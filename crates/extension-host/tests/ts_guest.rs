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
    let (component, _) = runtime
        .compile_cached(
            guest_component(),
            &repo().join("target/test-ts-guest-cache"),
        )
        .unwrap();
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
        context: None,
    })
}

#[test]
fn a_typescript_widget_uses_general_node_operations_and_receives_its_events() {
    use loom_extension_host::{UiEvent, UiEventPayload};
    let (runtime, component) = instance();
    let mut instance = runtime.instantiate(&component, limits()).unwrap();
    instance
        .dispatch(&[Inbound::RunCommand(CommandContext {
            call: 7,
            command: "example.tools.widget".into(),
            snapshot: None,
            selections: vec![],
            selection_precondition: None,
        })])
        .unwrap();
    instance.dispatch(&[]).unwrap();
    let nodes = instance.take_ui_snapshot().unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].description.background, Some(0x334455));
    let node = nodes[0].node;
    let click = || {
        Inbound::UiEvent(UiEvent {
            node,
            id: "ext.example.example-status".into(),
            payload: UiEventPayload::Click,
        })
    };
    instance.dispatch(&[click()]).unwrap();
    instance.dispatch(&[]).unwrap();
    let updated = instance.take_ui_snapshot().unwrap();
    assert_eq!(updated[0].description.text.as_deref(), Some("Ready"));
    assert_eq!(updated[0].description.background, Some(0x228844));
    instance.dispatch(&[click()]).unwrap();
    instance.dispatch(&[]).unwrap();
    assert!(instance.take_ui_snapshot().unwrap().is_empty());
}

#[test]
fn typed_references_and_saved_snapshots_cross_the_real_typescript_component_boundary() {
    use loom_extension_host::{ColumnUnit, DocumentRef, DocumentView, WireLanguageAnswer};
    use std::sync::Arc;
    let (runtime, component) = instance();
    let mut instance = runtime.instantiate(&component, limits()).unwrap();
    let text = Arc::new("中😀 TODO".to_owned());
    let reference = DocumentRef {
        document: 1,
        lifecycle: 1,
        version: 7,
        path: std::env::temp_dir()
            .join("loom-ts-reference.tsx")
            .display()
            .to_string(),
        language: "typescript".into(),
        variant: Some("tsx".into()),
    };
    instance.put_documents(
        vec![DocumentView {
            reference: reference.clone(),
            text: Some(text.clone()),
        }],
        &[],
    );
    instance.put_snapshot(9, text.clone());
    instance
        .dispatch(&[provider(
            ProviderKind::References,
            "example.tools.references",
            &text,
            None,
        )])
        .unwrap();
    instance.dispatch(&[]).unwrap();
    let answers = instance.take_provided();
    let [
        Provided::Language {
            answer: WireLanguageAnswer::Places(places),
            ..
        },
    ] = answers.as_slice()
    else {
        panic!("{answers:?} {:?}", instance.state().logs);
    };
    assert_eq!(places.len(), 1);
    assert_eq!(places[0].path, reference.path);
    assert_eq!(places[0].range.unit, ColumnUnit::Utf16);
    assert_eq!(places[0].range.start.column, 4);
    assert_eq!(places[0].range.end.column, 8);
    // More than the capture-handle cap: the SDK must release every saved snapshot.
    for _ in 0..40 {
        let saved = instance
            .saved_snapshot(reference.clone(), text.clone())
            .unwrap();
        instance.dispatch(&[Inbound::DocumentSaved(saved)]).unwrap();
        instance.dispatch(&[]).unwrap();
    }
    assert!(
        instance
            .state()
            .logs
            .iter()
            .any(|(_, text)| text == "saved 7: 中😀 TODO")
    );
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
