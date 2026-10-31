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
    ApplyEditRequest, ByteRange, Completion, Diagnostic, DocumentEdits, ErrorKind, HostError, Inbound,
    ProviderKind, RoundStatus, TextEdit,
};
use loom::extension::types::{
    StorageScope,
    CompletionItem,
};

#[derive(Default)]
struct State {
    /// The last document the host told us about: path and version.
    last_doc: Option<(String, u64)>,
    current_task: Option<u64>,
    /// Version of the document a checker task was started for.
    checking: Option<(u64, String, u64)>,
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
        "example.tools.run" | "example.tools.check" => {
            let setting = host::get_setting("example.tools.task").unwrap_or_default();
            let mut spec: serde_json::Value = serde_json::from_str(&setting).unwrap_or_default();
            // A string setting holds the task as JSON text.
            if let Some(text) = spec.as_str() {
                spec = serde_json::from_str(text).unwrap_or_default();
            }
            let roots = host::list_roots();
            let Some(first) = roots.first() else {
                host::log(2, "no folder is open");
                return;
            };
            let request = loom::extension::types::TaskRequest {
                call,
                root: first.id,
                program: spec["program"].as_str().unwrap_or_default().to_owned(),
                args: spec["args"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
                    .unwrap_or_default(),
                cwd: spec["cwd"].as_str().map(str::to_owned),
                env: Vec::new(),
                timeout_ms: Some(20_000),
            };
            match host::task_start(&request) {
                Ok(task) => {
                    host::log(0, &format!("task started {task}"));
                    STATE.with(|state| {
                        let mut state = state.borrow_mut();
                        state.current_task = Some(task);
                        if command == "example.tools.check" {
                            state.checking = state.last_doc.clone().map(|(path, version)| (task, path, version));
                        }
                    });
                }
                Err(error) => host::log(2, &format!("task refused: {}", error.message)),
            }
        }
        "example.tools.op" => {
            // One storage or file operation described by the "example.tools.op" setting.
            let setting = host::get_setting("example.tools.spec").unwrap_or_default();
            let mut spec: serde_json::Value = serde_json::from_str(&setting).unwrap_or_default();
            if let Some(text) = spec.as_str() {
                spec = serde_json::from_str(text).unwrap_or_default();
            }
            let roots = host::list_roots();
            let root = roots.first().map(|r| r.id).unwrap_or(0);
            let scope = match spec["scope"].as_str() {
                Some("workspace") => StorageScope::Workspace,
                Some("root") => StorageScope::Root(root),
                _ => StorageScope::User,
            };
            let key = spec["key"].as_str().unwrap_or("k").to_owned();
            let value = spec["value"].as_str().unwrap_or_default().to_owned();
            let message = match spec["op"].as_str().unwrap_or_default() {
                "set" => match host::storage_set(scope, &key, &value) {
                    Ok(()) => "stored".to_owned(),
                    Err(e) => format!("store error: {}", e.message),
                },
                "get" => format!("got {:?}", host::storage_get(scope, &key)),
                "delete" => match host::storage_delete(scope, &key) {
                    Ok(()) => "deleted".to_owned(),
                    Err(e) => format!("delete error: {}", e.message),
                },
                "read" => match host::read_file(
                    root,
                    spec["path"].as_str().unwrap_or_default(),
                    spec["max"].as_u64().unwrap_or(1000) as u32,
                ) {
                    Ok(c) => format!("read {:?} truncated {}", c.text, c.truncated),
                    Err(e) => format!("read error: {}", e.message),
                },
                "find" => match host::find_files(
                    root,
                    spec["pattern"].as_str().unwrap_or("*"),
                    spec["limit"].as_u64().unwrap_or(100) as u32,
                ) {
                    Ok(found) => format!("found {}", found.join(",")),
                    Err(e) => format!("find error: {}", e.message),
                },
                "fileops" => {
                    use loom::extension::types::{CreateOp, DeleteOp, FileOp, FileOpsRequest, RenameOp};
                    let text = |v: &serde_json::Value, k: &str| v[k].as_str().unwrap_or_default().to_owned();
                    let ops: Vec<FileOp> = spec["ops"]
                        .as_array()
                        .map(|list| {
                            list.iter()
                                .filter_map(|o| match o["kind"].as_str()? {
                                    "create" => Some(FileOp::Create(CreateOp {
                                        path: text(o, "path"),
                                        text: o["text"].as_str().map(str::to_owned),
                                        overwrite: o["overwrite"].as_bool().unwrap_or(false),
                                    })),
                                    "rename" => Some(FileOp::Rename(RenameOp {
                                        source: text(o, "source"),
                                        target: text(o, "target"),
                                        overwrite: o["overwrite"].as_bool().unwrap_or(false),
                                    })),
                                    "delete" => Some(FileOp::Delete(DeleteOp {
                                        path: text(o, "path"),
                                        recursive: o["recursive"].as_bool().unwrap_or(false),
                                    })),
                                    _ => None,
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    match host::apply_file_ops(&FileOpsRequest { call: 0, root, ops }) {
                        Ok(op) => format!("file ops accepted {op}"),
                        Err(e) => format!("file ops error: {}", e.message),
                    }
                }
                "http" => match host::http_get(
                    spec["url"].as_str().unwrap_or_default(),
                    spec["max"].as_u64().unwrap_or(1000) as u32,
                ) {
                    Ok(r) => format!("http {} {:?} truncated {}", r.status, r.body, r.truncated),
                    Err(e) => format!("http error: {}", e.message),
                },
                other => format!("unknown op {other}"),
            };
            host::log(0, &format!("op result: {message}"));
        }
        "example.tools.cancel" => {
            if let Some(task) = STATE.with(|state| state.borrow().current_task) {
                host::task_cancel(task);
            }
        }
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
                Inbound::TaskOutput(output) => host::log(
                    0,
                    &format!("task output {}: {}", output.task, output.text.trim()),
                ),
                Inbound::TaskExited(exit) => {
                    host::log(
                        0,
                        &format!(
                            "task exited {} code {:?} cancelled {} timed_out {} error {}",
                            exit.task,
                            exit.code,
                            exit.cancelled,
                            exit.timed_out,
                            exit.error.as_ref().map(|e| e.message.as_str()).unwrap_or("none")
                        ),
                    );
                    let checking = STATE.with(|state| {
                        let mut state = state.borrow_mut();
                        match state.checking.take() {
                            Some((task, path, version)) if task == exit.task => Some((path, version)),
                            other => {
                                state.checking = other;
                                None
                            }
                        }
                    });
                    if let (Some((path, version)), Some(0)) = (checking, exit.code) {
                        let finding = Diagnostic {
                            range: ByteRange { start: 0, end: 4 },
                            severity: 2,
                            message: "checker says hi".into(),
                            source: None,
                            code: None,
                        };
                        let _ = host::publish_diagnostics("example.tools.checker", &path, version, &[finding]);
                    }
                }
                Inbound::DocumentOpened(d) => {
                    STATE.with(|state| state.borrow_mut().last_doc = Some((d.path.clone(), d.version)));
                    host::log(0, &format!("opened {} v{} life {} {}", d.document, d.version, d.lifecycle, d.language));
                }
                Inbound::DocumentChanged(c) => host::log(0, &format!("changed {} {}->{} x{}", c.document, c.from_version, c.to_version, c.edits)),
                Inbound::DocumentResync(d) => host::log(0, &format!("resync {} v{}", d.document, d.version)),
                Inbound::DocumentClosed(d) => host::log(0, &format!("closed {} life {}", d.document, d.lifecycle)),
                Inbound::ProviderRequest(request)
                    if matches!(
                        request.kind,
                        ProviderKind::Completion | ProviderKind::CompletionResolve
                    ) =>
                {
                    match request.kind {
                        ProviderKind::Completion => {
                            let items = vec![
                                CompletionItem {
                                    label: "loom_hello".into(),
                                    detail: Some("from the extension".into()),
                                    documentation: None,
                                    kind: Some(3),
                                    insert_text: Some("loom_hello()".into()),
                                    sort_text: None,
                                    filter_text: None,
                                    data: Some("hello".into()),
                                },
                                CompletionItem {
                                    label: "loom_world".into(),
                                    detail: None,
                                    documentation: None,
                                    kind: Some(3),
                                    insert_text: None,
                                    sort_text: None,
                                    filter_text: None,
                                    data: Some("world".into()),
                                },
                            ];
                            let _ = host::provide_completions(request.request, &items, false);
                        }
                        _ => {
                            if let Some(mut item) = request.item.clone() {
                                item.documentation =
                                    Some(format!("docs for {}", item.data.clone().unwrap_or_default()));
                                let _ = host::provide_completions(request.request, &[item], false);
                            }
                        }
                    }
                }
                Inbound::ProviderRequest(request) => {
                    let text = host::read_range(
                        request.snapshot,
                        ByteRange { start: 0, end: request.length },
                    )
                    .unwrap_or_default();
                    match request.kind {
                        ProviderKind::Diagnostics => {
                            let mut items = Vec::new();
                            let mut at = 0;
                            while let Some(found) = text[at..].find("TODO") {
                                let start = at + found;
                                items.push(Diagnostic {
                                    range: ByteRange { start: start as u64, end: start as u64 + 4 },
                                    severity: 2,
                                    message: "TODO found".into(),
                                    source: Some(request.provider.clone()),
                                    code: None,
                                });
                                at = start + 4;
                            }
                            let _ = host::provide_diagnostics(request.request, &items);
                        }
                        ProviderKind::Completion | ProviderKind::CompletionResolve => {}
                        ProviderKind::Formatting | ProviderKind::Save => {
                            let mut edits = Vec::new();
                            let mut offset = 0usize;
                            for line in text.split_inclusive('\n') {
                                let body = line.trim_end_matches(['\r', '\n']);
                                let trimmed = body.trim_end_matches([' ', '\t']);
                                if trimmed.len() != body.len() {
                                    edits.push(TextEdit {
                                        range: ByteRange {
                                            start: (offset + trimmed.len()) as u64,
                                            end: (offset + body.len()) as u64,
                                        },
                                        text: String::new(),
                                    });
                                }
                                offset += line.len();
                            }
                            let _ = host::provide_edits(request.request, &edits);
                        }
                    }
                    host::release_snapshot(request.snapshot);
                }
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
