//! M0 host tests against a real Rust component (design §7.1, §8, §11.1).
//!
//! The guest in `tests/extensions/rust-guest` is built for
//! `wasm32-unknown-unknown` and wrapped into a component with `wit-component`,
//! so it imports only `loom:extension/host`.
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

/// Build the guest once per test run and return the component bytes.
fn guest_component() -> &'static [u8] {
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(|| {
        let root = repo();
        let target = root.join("target/extension-guest");
        let status = Command::new("cargo")
            .args([
                "build",
                "--release",
                "--target",
                "wasm32-unknown-unknown",
                "--target-dir",
            ])
            .arg(&target)
            .current_dir(root.join("tests/extensions/rust-guest"))
            .status()
            .expect("cargo is available");
        assert!(status.success(), "the Rust guest failed to build");
        let module =
            std::fs::read(target.join("wasm32-unknown-unknown/release/loom_rust_guest.wasm"))
                .unwrap();
        wit_component::ComponentEncoder::default()
            .module(&module)
            .unwrap()
            .validate(true)
            .encode()
            .unwrap()
    })
}

fn runtime() -> (Runtime, Component) {
    let runtime = Runtime::new().unwrap();
    let component = runtime.compile(guest_component()).unwrap();
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

#[test]
fn a_command_starts_work_returns_and_resumes_when_the_host_completes_it() {
    let (runtime, component) = runtime();
    let mut instance = runtime.instantiate(&component, Limits::default()).unwrap();
    instance.put_snapshot(5, String::from("hello 中 world"));

    // Round 1: the guest reads the snapshot, asks for an edit and returns while it is pending.
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

    // The instance can serve another request while the first edit is unconfirmed.
    let status = instance
        .dispatch(&[command("example.tools.unknown", None, vec![])])
        .unwrap();
    assert_eq!(status, RoundStatus::Pending);

    // Round 2: the host reports the commit; the guest finishes.
    let done = Inbound::OperationComplete((
        accepted[0].operation,
        Completion::Committed(CommittedReceipt {
            transaction: 3,
            version: 4,
        }),
    ));
    assert_eq!(instance.dispatch(&[done]).unwrap(), RoundStatus::Idle);
    assert!(
        instance
            .state()
            .logs
            .iter()
            .any(|(_, text)| text.contains("committed as 3"))
    );
}

#[test]
fn the_guest_only_reads_what_the_snapshot_holds() {
    let (runtime, component) = runtime();
    let mut instance = runtime.instantiate(&component, Limits::default()).unwrap();
    instance.put_snapshot(5, String::from("abc"));
    instance
        .dispatch(&[command(
            "example.tools.uppercase",
            Some(5),
            vec![selection(0, 99)],
        )])
        .unwrap();
    assert!(
        instance.state().accepted[0].edit_count == 0,
        "an out-of-range read yields no edit"
    );
    assert!(
        instance
            .state()
            .logs
            .iter()
            .any(|(_, text)| text.contains("outside the snapshot"))
    );
}

#[test]
fn a_looping_guest_is_interrupted_by_the_epoch_deadline() {
    let (runtime, component) = runtime();
    let mut instance = runtime
        .instantiate(
            &component,
            Limits {
                round_ticks: 5,
                ..Limits::default()
            },
        )
        .unwrap();
    let started = Instant::now();
    let error = instance
        .dispatch(&[command("example.tools.spin", None, vec![])])
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::DeadlineExceeded, "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "interrupted promptly, took {:?}",
        started.elapsed()
    );
}

#[test]
fn memory_hoarding_hits_the_store_limit() {
    let (runtime, component) = runtime();
    let limits = Limits {
        memory_bytes: 64 * 1024 * 1024,
        round_ticks: 200,
        ..Limits::default()
    };
    let mut instance = runtime.instantiate(&component, limits).unwrap();
    let error = instance
        .dispatch(&[command("example.tools.hog", None, vec![])])
        .unwrap_err();
    assert!(
        matches!(
            error.kind,
            ErrorKind::LimitExceeded | ErrorKind::ExtensionFailed
        ),
        "{error}"
    );
}

#[test]
fn a_trap_fails_only_that_instance() {
    let (runtime, component) = runtime();
    let mut broken = runtime.instantiate(&component, Limits::default()).unwrap();
    let error = broken
        .dispatch(&[command("example.tools.trap", None, vec![])])
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::ExtensionFailed);

    // A fresh instance of the same component (a new activation generation) works.
    let mut fresh = runtime.instantiate(&component, Limits::default()).unwrap();
    assert_eq!(
        fresh
            .dispatch(&[command("example.tools.unknown", None, vec![])])
            .unwrap(),
        RoundStatus::Idle
    );
}

#[test]
fn pending_operation_capacity_is_bounded() {
    let (runtime, component) = runtime();
    let mut instance = runtime
        .instantiate(
            &component,
            Limits {
                pending_operations: 1,
                ..Limits::default()
            },
        )
        .unwrap();
    instance.put_snapshot(1, String::from("ab"));
    instance.put_snapshot(2, String::from("cd"));
    instance
        .dispatch(&[command(
            "example.tools.uppercase",
            Some(1),
            vec![selection(0, 1)],
        )])
        .unwrap();
    instance
        .dispatch(&[command(
            "example.tools.uppercase",
            Some(2),
            vec![selection(0, 1)],
        )])
        .unwrap();
    assert_eq!(instance.state().accepted.len(), 1);
    assert!(
        instance
            .state()
            .logs
            .iter()
            .any(|(_, text)| text.contains("too many pending"))
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
    })
}

#[test]
fn a_diagnostics_provider_answers_the_request_it_was_given() {
    let (runtime, component) = runtime();
    let mut instance = runtime.instantiate(&component, Limits::default()).unwrap();
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
    let mut instance = runtime.instantiate(&component, Limits::default()).unwrap();
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
    let mut instance = runtime.instantiate(&component, Limits::default()).unwrap();
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

#[test]
fn a_guest_without_the_ports_is_denied_storage_and_files() {
    let (runtime, component) = runtime();
    let mut instance = runtime.instantiate(&component, Limits::default()).unwrap();
    instance.put_settings(std::collections::BTreeMap::from([(
        "example.tools.spec".to_owned(),
        r#"{"op":"set","scope":"user","key":"a","value":"b"}"#.to_owned(),
    )]));
    instance
        .dispatch(&[command("example.tools.op", None, vec![])])
        .unwrap();
    let logs: Vec<_> = instance.state().logs.iter().map(|l| l.1.clone()).collect();
    assert!(logs.iter().any(|l| l.contains("not granted")), "{logs:?}");
}

#[test]
fn a_guest_can_start_and_cancel_a_task_and_see_only_the_folders_it_was_given() {
    use loom_extension_host::RootRef;
    use std::collections::BTreeMap;
    let (runtime, component) = runtime();
    let mut instance = runtime.instantiate(&component, Limits::default()).unwrap();
    instance.put_roots(vec![RootRef {
        id: 7,
        name: "project".into(),
    }]);
    instance.put_settings(BTreeMap::from([(
        "example.tools.task".to_owned(),
        r#"{"program":"tool","args":["--check"],"cwd":"sub"}"#.to_owned(),
    )]));
    instance
        .dispatch(&[command("example.tools.run", None, vec![])])
        .unwrap();
    let started = instance.take_started_tasks();
    assert_eq!(started.len(), 1, "logs: {:?}", instance.state().logs);
    let request = &started[0].request;
    assert_eq!(
        (
            request.root,
            request.program.as_str(),
            request.args.as_slice()
        ),
        (7, "tool", ["--check".to_owned()].as_slice())
    );
    assert_eq!(request.cwd.as_deref(), Some("sub"));

    instance
        .dispatch(&[command("example.tools.cancel", None, vec![])])
        .unwrap();
    assert_eq!(instance.take_cancelled_tasks(), [started[0].task]);
}

#[test]
fn an_instance_cannot_have_more_tasks_running_than_its_limit() {
    use loom_extension_host::RootRef;
    use std::collections::BTreeMap;
    let (runtime, component) = runtime();
    let mut instance = runtime
        .instantiate(
            &component,
            Limits {
                tasks: 1,
                ..Limits::default()
            },
        )
        .unwrap();
    instance.put_roots(vec![RootRef {
        id: 1,
        name: "p".into(),
    }]);
    instance.put_settings(BTreeMap::from([(
        "example.tools.task".to_owned(),
        r#"{"program":"t"}"#.to_owned(),
    )]));
    instance
        .dispatch(&[command("example.tools.run", None, vec![])])
        .unwrap();
    instance
        .dispatch(&[command("example.tools.run", None, vec![])])
        .unwrap();
    assert_eq!(instance.take_started_tasks().len(), 1);
    assert!(
        instance
            .state()
            .logs
            .iter()
            .any(|(_, t)| t.contains("too many running tasks")),
        "{:?}",
        instance.state().logs
    );
    // Freeing the slot lets the next one in.
    instance.task_ended();
    instance
        .dispatch(&[command("example.tools.run", None, vec![])])
        .unwrap();
    assert_eq!(instance.take_started_tasks().len(), 1);
}

#[test]
fn side_effects_are_refused_while_a_save_participant_runs_and_allowed_again_after_its_answer() {
    let (runtime, component) = runtime();
    let mut instance = runtime.instantiate(&component, Limits::default()).unwrap();
    instance.put_snapshot(5, String::from("hello"));

    // Another callback tries to edit while a save request (99) is open.
    instance.begin_save(99);
    instance
        .dispatch(&[command(
            "example.tools.uppercase",
            Some(5),
            vec![selection(0, 5)],
        )])
        .unwrap();
    assert!(instance.take_accepted().is_empty());
    assert!(
        instance
            .state()
            .logs
            .iter()
            .any(|(_, text)| text.contains("not allowed while a save participant runs")),
        "{:?}",
        instance.state().logs
    );

    // Ending the request (a timeout, say) lifts the restriction.
    instance.end_save(99);
    instance.put_snapshot(5, String::from("hello"));
    instance
        .dispatch(&[command(
            "example.tools.uppercase",
            Some(5),
            vec![selection(0, 5)],
        )])
        .unwrap();
    assert_eq!(instance.take_accepted().len(), 1);
}

#[test]
fn a_save_participant_answers_with_edits_and_that_answer_lifts_the_restriction() {
    let (runtime, component) = runtime();
    let mut instance = runtime.instantiate(&component, Limits::default()).unwrap();
    let text = "a  \nb\n";
    instance.put_snapshot(9, String::from(text));
    instance.begin_save(41);
    instance
        .dispatch(&[provider_request(ProviderKind::Save, text)])
        .unwrap();
    let provided = instance.take_provided();
    let [Provided::Edits { request, edits }] = provided.as_slice() else {
        panic!("{provided:?}");
    };
    assert_eq!((*request, edits.len()), (41, 1));
    instance.put_snapshot(5, String::from("hello"));
    instance
        .dispatch(&[command(
            "example.tools.uppercase",
            Some(5),
            vec![selection(0, 5)],
        )])
        .unwrap();
    assert_eq!(
        instance.take_accepted().len(),
        1,
        "the answer ended the restriction"
    );
}
