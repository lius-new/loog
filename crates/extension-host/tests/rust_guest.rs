//! M0 host tests against a real Rust component (design §7.1, §8, §11.1).
//!
//! The guest in `tests/extensions/rust-guest` is built for
//! `wasm32-unknown-unknown` and wrapped into a component with `wit-component`,
//! so it imports only `loom:extension/host`.
use loom_extension_api::ErrorKind;
use loom_extension_host::{
    ByteRange, CommandContext, CommittedReceipt, Completion, Inbound, Limits, RoundStatus, Runtime,
    Selection,
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
