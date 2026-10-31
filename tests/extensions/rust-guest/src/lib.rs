//! Reference Rust extension used by the host tests (design §11.1 sample 1).
//!
//! It shows the round protocol a Rust SDK has to provide: `dispatch` starts
//! work and returns; host operations complete in a later `dispatch`.
use std::cell::RefCell;

wit_bindgen::generate!({
    path: "../../../wit/loom-extension",
    world: "extension",
});

use exports::loom::extension::guest::Guest;
use loom::extension::host;
use loom::extension::types::{
    ApplyEditRequest, ByteRange, Completion, DocumentEdits, ErrorKind, HostError, Inbound, RoundStatus, TextEdit,
};

#[derive(Default)]
struct State {
    sequence: u64,
    pending: Vec<u64>,
    hoard: Vec<Vec<u8>>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

struct Extension;

fn run(command: &str, call: u64, snapshot: Option<u64>, selections: Vec<loom::extension::types::Selection>, precondition: Option<u64>) {
    match command {
        "example.tools.uppercase" => {
            let Some(snapshot) = snapshot else { return };
            let mut edits = Vec::new();
            for selection in &selections {
                match host::read_range(snapshot, selection.range) {
                    Ok(text) => edits.push(TextEdit { range: selection.range, text: text.to_uppercase() }),
                    Err(error) => host::log(2, &error.message),
                }
            }
            let sequence = STATE.with(|state| {
                let mut state = state.borrow_mut();
                state.sequence += 1;
                state.sequence
            });
            let request = ApplyEditRequest {
                title: "Uppercase Selection".into(),
                call,
                sequence,
                selection_precondition: precondition,
                documents: vec![DocumentEdits { snapshot, edits }],
            };
            match host::apply_edit(&request) {
                Ok(operation) => STATE.with(|state| state.borrow_mut().pending.push(operation)),
                Err(error) => host::log(2, &error.message),
            }
            host::release_snapshot(snapshot);
        }
        // Failure fixtures: each must fail the instance, not the application.
        "example.tools.spin" => {
            let mut counter = 0u64;
            loop {
                counter = std::hint::black_box(counter.wrapping_add(1));
            }
        }
        "example.tools.hog" => loop {
            let block = vec![1u8; 16 * 1024 * 1024];
            STATE.with(|state| state.borrow_mut().hoard.push(block));
        },
        "example.tools.trap" => panic!("deliberate trap"),
        other => host::log(1, &format!("unknown command {other}")),
    }
}

impl Guest for Extension {
    fn dispatch(batch: Vec<Inbound>) -> Result<RoundStatus, HostError> {
        for message in batch {
            match message {
                Inbound::RunCommand(context) => run(
                    &context.command,
                    context.call,
                    context.snapshot,
                    context.selections,
                    context.selection_precondition,
                ),
                Inbound::OperationComplete((operation, completion)) => {
                    STATE.with(|state| state.borrow_mut().pending.retain(|pending| *pending != operation));
                    match completion {
                        Completion::Committed(receipt) => {
                            host::log(0, &format!("operation {operation} committed as {}", receipt.transaction))
                        }
                        Completion::Failed(error) => host::log(2, &format!("operation {operation} failed: {}", error.message)),
                    }
                }
                Inbound::DocumentOpened(d) => host::log(0, &format!("opened {} v{} life {} {}", d.document, d.version, d.lifecycle, d.language)),
                Inbound::DocumentChanged(c) => host::log(0, &format!("changed {} {}->{} x{}", c.document, c.from_version, c.to_version, c.edits)),
                Inbound::DocumentResync(d) => host::log(0, &format!("resync {} v{}", d.document, d.version)),
                Inbound::DocumentClosed(d) => host::log(0, &format!("closed {} life {}", d.document, d.lifecycle)),
                Inbound::SettingsChanged(keys) => {
                    for key in keys {
                        let value = host::get_setting(&key).unwrap_or_default();
                        host::log(0, &format!("setting {key}={value}"));
                    }
                }
                Inbound::Cancel(call) => host::log(0, &format!("call {call} cancelled")),
                Inbound::Shutdown => {}
            }
        }
        let waiting = STATE.with(|state| !state.borrow().pending.is_empty());
        let _ = (ByteRange { start: 0, end: 0 }, ErrorKind::Cancelled);
        Ok(if waiting { RoundStatus::Pending } else { RoundStatus::Idle })
    }
}

export!(Extension);
