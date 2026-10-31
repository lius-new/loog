//! Host tests against a real component written in TypeScript with the SDK
//! (`sdk/typescript`, design §10). The component is built by
//! `node sdk/typescript/build.mjs`, which also type-checks the sources.
use loom_extension_host::{
    ByteRange, CommandContext, CommittedReceipt, Completion, Inbound, Limits, Provided,
    ProviderKind, ProviderRequest, RoundStatus, Runtime, Selection,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::SystemTime;
use wasmtime::component::Component;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The newest change under `path`, to know whether the component is stale.
fn newest(path: &Path) -> SystemTime {
    let mut latest = SystemTime::UNIX_EPOCH;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == "node_modules") {
                continue;
            }
            let modified = if path.is_dir() {
                newest(&path)
            } else {
                entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH)
            };
            latest = latest.max(modified);
        }
    }
    latest
}

fn guest_component() -> &'static [u8] {
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(|| {
        let root = repo();
        let wasm = root.join("target/extension-guest/ts_guest.wasm");
        let sources = newest(&root.join("sdk/typescript")).max(newest(&root.join("wit")));
        let stale = std::fs::metadata(&wasm)
            .and_then(|m| m.modified())
            .map(|built| built < sources)
            .unwrap_or(true);
        if stale {
            let status = Command::new("node")
                .arg(root.join("sdk/typescript/build.mjs"))
                .current_dir(&root)
                .status()
                .expect("node is available");
            assert!(status.success(), "the TypeScript guest failed to build");
        }
        std::fs::read(wasm).unwrap()
    })
}

fn limits() -> Limits {
    Limits {
        memory_bytes: 512 * 1024 * 1024,
        round_ticks: 500,
        ..Limits::default()
    }
}

fn instance() -> (Runtime, Component) {
    let runtime = Runtime::new().unwrap();
    let component = runtime.compile(guest_component()).unwrap();
    (runtime, component)
}

fn provider(kind: ProviderKind, id: &str, text: &str, cursor: Option<u64>) -> Inbound {
    Inbound::ProviderRequest(ProviderRequest {
        request: 41,
        provider: id.into(),
        kind,
        snapshot: 9,
        document: 1,
        version: 3,
        length: text.len() as u64,
        range: None,
        cursor,
        trigger: None,
        item: None,
    })
}

#[test]
fn a_typescript_command_awaits_a_host_operation_across_rounds() {
    let (runtime, component) = instance();
    let mut instance = runtime.instantiate(&component, limits()).unwrap();
    instance.put_snapshot(5, String::from("hello 中 world"));
    let run = Inbound::RunCommand(CommandContext {
        call: 7,
        command: "example.tools.uppercase".into(),
        snapshot: Some(5),
        selections: vec![Selection {
            range: ByteRange { start: 0, end: 5 },
            reversed: false,
        }],
        selection_precondition: Some(99),
    });
    // Work started in this round: the SDK asks for an empty round to see where it settled.
    assert_eq!(instance.dispatch(&[run]).unwrap(), RoundStatus::Runnable);
    assert_eq!(instance.dispatch(&[]).unwrap(), RoundStatus::Pending);
    let accepted = instance.state().accepted.clone();
    assert_eq!(accepted.len(), 1, "{:?}", instance.state().logs);
    assert_eq!(accepted[0].title, "Uppercase Selection");
    assert_eq!(
        (
            accepted[0].call,
            accepted[0].sequence,
            accepted[0].selection_precondition
        ),
        (7, 1, Some(99))
    );
    assert_eq!(accepted[0].documents[0].edits[0].text, "HELLO");

    let done = Inbound::OperationComplete((
        accepted[0].operation,
        Completion::Committed(CommittedReceipt {
            transaction: 3,
            version: 4,
        }),
    ));
    assert_eq!(instance.dispatch(&[done]).unwrap(), RoundStatus::Runnable);
    assert_eq!(instance.dispatch(&[]).unwrap(), RoundStatus::Idle);
    assert!(
        instance
            .state()
            .logs
            .iter()
            .any(|(_, text)| text.contains("committed as 3")),
        "{:?}",
        instance.state().logs
    );
}

#[test]
fn offsets_are_utf8_bytes_even_though_javascript_counts_utf16() {
    let (runtime, component) = instance();
    let mut instance = runtime.instantiate(&component, limits()).unwrap();
    // 中 is one UTF-16 unit but three bytes; the emoji is two units and four bytes.
    let text = "中😀 TODO\nTODO";
    instance.put_snapshot(9, text.to_owned());
    instance
        .dispatch(&[provider(
            ProviderKind::Diagnostics,
            "example.tools.todo",
            text,
            None,
        )])
        .unwrap();
    instance.dispatch(&[]).unwrap();
    let provided = instance.take_provided();
    let [Provided::Diagnostics { request, items }] = provided.as_slice() else {
        panic!("{provided:?} {:?}", instance.state().logs);
    };
    assert_eq!(*request, 41);
    let starts: Vec<u64> = items.iter().map(|d| d.range.start).collect();
    assert_eq!(
        starts,
        [8, 13],
        "3 + 4 + 1 bytes before the first; it ends at 12, the line break is 12..13"
    );
}

#[test]
fn a_typescript_hover_provider_answers_with_a_typed_result() {
    let (runtime, component) = instance();
    let mut instance = runtime.instantiate(&component, limits()).unwrap();
    let text = "let value = 1;";
    instance.put_snapshot(9, text.to_owned());
    instance
        .dispatch(&[provider(
            ProviderKind::Hover,
            "example.tools.hover",
            text,
            Some(4),
        )])
        .unwrap();
    instance.dispatch(&[]).unwrap();
    let provided = instance.take_provided();
    let [Provided::Hover { request, answer }] = provided.as_slice() else {
        panic!("{provided:?} {:?}", instance.state().logs);
    };
    assert_eq!(*request, 41);
    let answer = answer.as_ref().expect("an answer");
    assert_eq!(answer.contents, "**14** bytes, caret at 4");
    assert_eq!(
        (
            answer.range.as_ref().unwrap().start,
            answer.range.as_ref().unwrap().end
        ),
        (4, 4)
    );
}

#[test]
fn a_request_no_handler_matches_is_logged_and_the_snapshot_released() {
    let (runtime, component) = instance();
    let mut instance = runtime.instantiate(&component, limits()).unwrap();
    instance.put_snapshot(9, String::from("x"));
    instance
        .dispatch(&[provider(
            ProviderKind::Hover,
            "example.tools.nobody",
            "x",
            Some(0),
        )])
        .unwrap();
    assert!(instance.take_provided().is_empty());
    assert!(
        instance
            .state()
            .logs
            .iter()
            .any(|(_, text)| text.contains("no hover handler")),
        "{:?}",
        instance.state().logs
    );
}
