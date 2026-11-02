//! M0 host tests against a real JavaScript component (design §7.1 gate).
//!
//! The guest in `tests/extensions/js-guest` is built with the pinned jco /
//! ComponentizeJS (`node tests/extensions/js-guest/build.mjs`, run on demand).
use loom_extension_api::ErrorKind;
use loom_extension_host::{
    ByteRange, CommandContext, CommittedReceipt, Completion, Inbound, Limits, Provided,
    ProviderKind, ProviderRequest, RoundStatus, Runtime, Selection, WireCompletionItem,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use wasmtime::component::Component;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn guest_component() -> &'static [u8] {
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(|| {
        let root = repo();
        let wasm = root.join("target/extension-guest/js_guest.wasm");
        if !wasm.exists() {
            let status = Command::new("node")
                .arg(root.join("tests/extensions/js-guest/build.mjs"))
                .current_dir(&root)
                .status()
                .expect("node is available");
            assert!(status.success(), "the JS guest failed to build");
        }
        std::fs::read(wasm).unwrap()
    })
}

fn runtime() -> (Runtime, Component) {
    let runtime = Runtime::new().unwrap();
    let (component, _) = runtime
        .compile_cached(
            guest_component(),
            &repo().join("target/test-js-guest-cache"),
        )
        .unwrap();
    (runtime, component)
}

fn command(name: &str, snapshot: Option<u64>, selections: Vec<Selection>) -> Inbound {
    Inbound::RunCommand(CommandContext {
        call: 7,
        command: name.into(),
        snapshot,
        selections,
        selection_precondition: Some(99),
    })
}

fn selection(start: u64, end: u64) -> Selection {
    Selection {
        range: ByteRange { start, end },
        reversed: false,
    }
}

fn limits() -> Limits {
    // StarlingMonkey needs far more memory and time than a Rust guest.
    Limits {
        memory_bytes: 512 * 1024 * 1024,
        round_ticks: 500,
        ..Limits::default()
    }
}

#[test]
fn an_awaiting_callback_survives_the_round_and_resumes_on_completion() {
    let (runtime, component) = runtime();
    let mut instance = runtime.instantiate(&component, limits()).unwrap();
    instance.put_snapshot(5, String::from("hello 中 world"));

    // Round 1: `await applyEdit(...)` is pending inside the engine; the export returned.
    let status = instance
        .dispatch(&[command(
            "example.tools.uppercase",
            Some(5),
            vec![selection(0, 5), selection(6, 9)],
        )])
        .unwrap();
    assert_eq!(status, RoundStatus::Pending);
    let accepted = instance.state().accepted.clone();
    assert_eq!(accepted.len(), 1);
    assert_eq!(
        (
            accepted[0].call,
            accepted[0].sequence,
            accepted[0].edit_count
        ),
        (7, 1, 2)
    );
    assert!(
        instance
            .state()
            .logs
            .iter()
            .all(|(_, text)| !text.contains("committed")),
        "nothing completed yet"
    );

    // The instance accepts another request while the first is unconfirmed.
    assert_eq!(
        instance
            .dispatch(&[command("example.tools.unknown", None, vec![])])
            .unwrap(),
        RoundStatus::Pending
    );

    // Round 2: completion re-enters; the microtask continuation runs and `finally` releases the snapshot.
    let done = Inbound::OperationComplete((
        accepted[0].operation,
        Completion::Committed(CommittedReceipt {
            transaction: 3,
            version: 4,
        }),
    ));
    let status = instance.dispatch(&[done]).unwrap();
    eprintln!(
        "status right after completion: {status:?}; logs: {:?}",
        instance.state().logs
    );
    // Microtasks may still be queued when the export returns; an empty round lets them run.
    let settled = instance.dispatch(&[]).unwrap();
    eprintln!(
        "status after an empty round: {settled:?}; logs: {:?}",
        instance.state().logs
    );
    assert_eq!(settled, RoundStatus::Idle);
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
fn an_uncaught_async_failure_stays_inside_the_instance() {
    let (runtime, component) = runtime();
    let mut instance = runtime.instantiate(&component, limits()).unwrap();
    instance
        .dispatch(&[command("example.tools.trap", None, vec![])])
        .unwrap();
    assert_eq!(instance.dispatch(&[]).unwrap(), RoundStatus::Idle);
    assert!(
        instance
            .state()
            .logs
            .iter()
            .any(|(_, text)| text.contains("command failed"))
    );
    assert_eq!(
        instance
            .dispatch(&[command("example.tools.unknown", None, vec![])])
            .unwrap(),
        RoundStatus::Idle
    );
}

#[test]
fn a_busy_loop_is_interrupted() {
    let (runtime, component) = runtime();
    let mut instance = runtime
        .instantiate(
            &component,
            Limits {
                round_ticks: 500,
                ..limits()
            },
        )
        .unwrap();
    // Warm up so the deadline measures the loop, not engine start-up.
    instance
        .dispatch(&[command("example.tools.unknown", None, vec![])])
        .unwrap();
    let mut instance2 = runtime
        .instantiate(
            &component,
            Limits {
                round_ticks: 30,
                ..limits()
            },
        )
        .unwrap();
    let started = Instant::now();
    let error = instance2
        .dispatch(&[command("example.tools.spin", None, vec![])])
        .unwrap_err();
    assert!(matches!(error.kind, ErrorKind::DeadlineExceeded), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn a_microtask_flood_is_interrupted() {
    let (runtime, component) = runtime();
    let mut instance = runtime
        .instantiate(
            &component,
            Limits {
                round_ticks: 60,
                ..limits()
            },
        )
        .unwrap();
    let started = Instant::now();
    let error = instance
        .dispatch(&[command("example.tools.flood", None, vec![])])
        .unwrap_err();
    assert!(matches!(error.kind, ErrorKind::DeadlineExceeded), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn the_wasi_imports_the_toolchain_needs_are_denied_not_granted() {
    // The component still declares wasi:io / wasi:http imports (see design §9).
    let (runtime, component) = runtime();
    let imports: Vec<String> = component
        .component_type()
        .imports(runtime.engine())
        .map(|(name, _)| name.to_owned())
        .collect();
    assert!(
        imports.iter().any(|name| name.starts_with("wasi:")),
        "{imports:?}"
    );
    assert!(
        imports
            .iter()
            .any(|name| name == "loom:extension/host@0.1.0"),
        "{imports:?}"
    );
}

fn provider_request(kind: ProviderKind, text: &str) -> Inbound {
    Inbound::ProviderRequest(ProviderRequest {
        request: 41,
        provider: "example.tools.provider".into(),
        kind,
        snapshot: 9,
        document: 1,
        version: 3,
        length: text.len() as u64,
        range: None,
        cursor: None,
        trigger: None,
        item: None,
        context: None,
    })
}

#[test]
fn a_diagnostics_provider_answers_the_request_it_was_given() {
    let (runtime, component) = runtime();
    let mut instance = runtime.instantiate(&component, limits()).unwrap();
    let text = "fine\nTODO: later\nTODO again\n";
    instance.put_snapshot(9, String::from(text));
    instance
        .dispatch(&[provider_request(ProviderKind::Diagnostics, text)])
        .unwrap();
    let provided = instance.take_provided();
    let [Provided::Diagnostics { request, items }] = provided.as_slice() else {
        panic!("{provided:?}");
    };
    assert_eq!(*request, 41, "the answer names its request");
    let starts: Vec<u64> = items.iter().map(|d| d.range.start).collect();
    assert_eq!(starts, [5, 17]);
    assert!(
        items
            .iter()
            .all(|d| d.severity == 2 && d.message == "TODO found")
    );
}

#[test]
fn a_formatting_provider_answers_with_edits_against_the_snapshot() {
    let (runtime, component) = runtime();
    let mut instance = runtime.instantiate(&component, limits()).unwrap();
    let text = "a  \nb\r\nc \t\n";
    instance.put_snapshot(9, String::from(text));
    instance
        .dispatch(&[provider_request(ProviderKind::Formatting, text)])
        .unwrap();
    let provided = instance.take_provided();
    let [Provided::Edits { request, edits }] = provided.as_slice() else {
        panic!("{provided:?}");
    };
    assert_eq!(*request, 41);
    let ranges: Vec<(u64, u64)> = edits.iter().map(|e| (e.range.start, e.range.end)).collect();
    assert_eq!(ranges, [(1, 3), (8, 10)]);
    assert!(edits.iter().all(|e| e.text.is_empty()));
}

#[test]
fn a_completion_provider_offers_items_and_resolves_one_back_to_its_origin() {
    let (runtime, component) = runtime();
    let mut instance = runtime.instantiate(&component, limits()).unwrap();
    instance.put_snapshot(9, String::from("lo"));
    let mut ask = provider_request(ProviderKind::Completion, "lo");
    if let Inbound::ProviderRequest(request) = &mut ask {
        request.cursor = Some(2);
        request.trigger = Some(".".into());
    }
    instance.dispatch(&[ask]).unwrap();
    let provided = instance.take_provided();
    let [
        Provided::Completions {
            request,
            items,
            incomplete,
        },
    ] = provided.as_slice()
    else {
        panic!("{provided:?}");
    };
    assert_eq!(*request, 41);
    assert!(!incomplete);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, ["loom_hello", "loom_world"]);
    assert_eq!(items[0].insert_text.as_deref(), Some("loom_hello()"));

    // The opaque data comes back unchanged with the resolve request.
    let mut resolve = provider_request(ProviderKind::CompletionResolve, "lo");
    if let Inbound::ProviderRequest(request) = &mut resolve {
        request.request = 42;
        request.item = Some(items[1].clone());
    }
    instance.put_snapshot(9, String::from("lo"));
    instance.dispatch(&[resolve]).unwrap();
    let provided = instance.take_provided();
    let [
        Provided::Completions {
            request,
            items: resolved,
            ..
        },
    ] = provided.as_slice()
    else {
        panic!("{provided:?}");
    };
    assert_eq!(*request, 42);
    assert_eq!(resolved[0].label, "loom_world");
    assert_eq!(resolved[0].documentation.as_deref(), Some("docs for world"));
    let _: Option<&WireCompletionItem> = resolved.first();
}
