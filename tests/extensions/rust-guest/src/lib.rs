//! Reference Rust extension used by the host tests (design §11.1 sample 1).
//!
//! It shows the round protocol a Rust SDK has to provide: `dispatch` starts
//! work and returns; host operations complete in a later `dispatch`.
use std::cell::RefCell;
mod memory_workspace;

wit_bindgen::generate!({
    path: "../../../wit/loog-extension",
    world: "extension-workspace",
    generate_all,
});

use exports::loog::extension::guest::Guest;
use loog::extension::host;
use loog::extension::types::{
    ApplyEditRequest, ByteRange, Completion, Diagnostic, DocumentEdits, ErrorKind, HostError,
    Inbound, ProviderCommand, ProviderKind, RoundStatus, TextEdit,
};
use loog::extension::types::{
    CodeAction, CompletionItem, DocumentSymbol, HoverAnswer, InlayHint, Location, ParameterInfo,
    SignatureAnswer, StorageScope,
};

#[derive(Default)]
struct State {
    ui_node: Option<u64>,
    widget_clicks: u8,
    relative_widget: bool,
    watch: Option<u64>,
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

fn run(
    command: &str,
    call: u64,
    snapshot: Option<u64>,
    selections: Vec<loog::extension::types::Selection>,
    precondition: Option<u64>,
) {
    if memory_workspace::command(command) {
        return;
    }
    if let Some(command) = command.strip_prefix("example.tools.launch:") {
        let value: serde_json::Value = serde_json::from_str(command).unwrap();
        let args: Vec<String> = serde_json::from_value(value["args"].clone()).unwrap();
        match loog::workspace::tools::launch(value["program"].as_str().unwrap(), &args) {
            Ok(process) => host::log(0, &format!("tool launched {process}")),
            Err(error) => host::log(2, &error),
        }
        return;
    }
    if let Some(command) = command.strip_prefix("example.workspace:") {
        match loog::workspace::session::submit(command) {
            Ok(ticket) => host::log(0, &format!("workspace submitted {ticket}")),
            Err(error) => host::log(2, &error),
        }
        return;
    }
    match command {
        "example.tools.workspace" => {
            let request = r#"{"kind":"register","descriptor":{"key":{"provider":"example.tools","instance":"memory"},"name":"Memory files","platform":"independent","capabilities":["files_read"]}}"#;
            match loog::workspace::session::submit(request) {
                Ok(ticket) => host::log(0, &format!("workspace submitted {ticket}")),
                Err(error) => host::log(2, &error),
            }
        }
        "example.tools.motion" => {
            use loog::extension::types::{UiDescription, UiKind, UiParent};
            use loog::ui::motion::{
                self, Animation, Direction, Easing, Keyframe, Property, Smoothing, Track,
            };
            let node = host::ui_create(
                &UiParent::Mount("ui.detached".into()),
                &UiDescription {
                    id: "motion-icon".into(),
                    kind: UiKind::Icon,
                    text: None,
                    icon: Some("sync".into()),
                    foreground: None,
                    background: None,
                    width: Some(16.0),
                    height: Some(16.0),
                    padding: 0.0,
                    gap: 0.0,
                    visible: true,
                    label: None,
                },
            )
            .unwrap();
            motion::smooth(
                node,
                Smoothing {
                    property: Property::TranslateX,
                    duration_ms: 120,
                    easing: Easing::Linear,
                },
            )
            .unwrap();
            motion::set(node, Property::TranslateX, 6.0).unwrap();
            motion::animate(
                node,
                &Animation {
                    tracks: vec![
                        Track {
                            property: Property::Rotation,
                            keyframes: vec![
                                Keyframe {
                                    offset: 0.0,
                                    value: 0.0,
                                },
                                Keyframe {
                                    offset: 1.0,
                                    value: 360.0,
                                },
                            ],
                            easing: Easing::Linear,
                        },
                        Track {
                            property: Property::Opacity,
                            keyframes: vec![
                                Keyframe {
                                    offset: 0.0,
                                    value: 1.0,
                                },
                                Keyframe {
                                    offset: 0.5,
                                    value: 0.5,
                                },
                                Keyframe {
                                    offset: 1.0,
                                    value: 1.0,
                                },
                            ],
                            easing: Easing::CubicBezier((0.42, 0.0, 0.58, 1.0)),
                        },
                    ],
                    duration_ms: 800,
                    delay_ms: 0,
                    iterations: None,
                    direction: Direction::Normal,
                    hold: false,
                },
            )
            .unwrap();
        }
        "example.tools.widget" | "example.tools.relative-widget" => {
            use loog::extension::types::{UiDescription, UiKind, UiParent};
            let relative = command == "example.tools.relative-widget";
            let description = UiDescription {
                id: if relative {
                    "relative-status"
                } else {
                    "ext.example.example-status"
                }
                .into(),
                kind: UiKind::Button,
                text: Some("Starting".into()),
                icon: None,
                foreground: None,
                background: Some(0x334455),
                width: None,
                height: None,
                padding: 4.0,
                gap: 0.0,
                visible: true,
                label: None,
            };
            let node = host::ui_create(
                &UiParent::Mount("statusbar.extensions".into()),
                &description,
            )
            .unwrap();
            if relative {
                use loog::ui::tree::{self, Placement, Relation, Target};
                tree::place(
                    node,
                    &Placement {
                        target: Target::PublicId("demo.cursor".into()),
                        relation: Relation::Before,
                        order: 100,
                    },
                )
                .unwrap();
            }
            STATE.with(|state| {
                let mut state = state.borrow_mut();
                state.ui_node = Some(node);
                state.widget_clicks = 0;
                state.relative_widget = relative;
            });
        }
        "example.tools.uppercase" => {
            let Some(snapshot) = snapshot else { return };
            let mut edits = Vec::new();
            for selection in &selections {
                match host::read_range(snapshot, selection.range) {
                    Ok(text) => edits.push(TextEdit {
                        range: selection.range,
                        text: text.to_uppercase(),
                    }),
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
        "example.tools.run" | "example.tools.check" | "example.tools.stream" => {
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
            let request = loog::extension::types::TaskRequest {
                call,
                root: first.id,
                program: spec["program"].as_str().unwrap_or_default().to_owned(),
                args: spec["args"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
                cwd: spec["cwd"].as_str().map(str::to_owned),
                env: Vec::new(),
                timeout_ms: Some(20_000),
            };
            if command == "example.tools.stream" {
                use loog::extension::types::{ProcessFraming, ProcessRequest};
                let process = host::process_spawn(&ProcessRequest {
                    root: Some(request.root),
                    program: request.program,
                    args: request.args,
                    cwd: request.cwd,
                    env: request.env,
                    framing: if spec["framed"].as_bool().unwrap_or(false) {
                        ProcessFraming::ContentLength
                    } else {
                        ProcessFraming::Raw
                    },
                })
                .unwrap();
                let accepted = if let Some(messages) = spec["input"].as_array() {
                    messages
                        .iter()
                        .map(|value| {
                            host::process_write(process, value.as_str().unwrap().as_bytes())
                                .unwrap()
                        })
                        .sum()
                } else {
                    host::process_write(process, &[0, 255, 1]).unwrap()
                };
                host::log(0, &format!("process started {process} accepted {accepted}"));
                if !spec["persistent"].as_bool().unwrap_or(false) {
                    host::process_close_stdin(process).unwrap();
                }
                return;
            }
            match host::task_start(&request) {
                Ok(task) => {
                    host::log(0, &format!("task started {task}"));
                    STATE.with(|state| {
                        let mut state = state.borrow_mut();
                        state.current_task = Some(task);
                        if command == "example.tools.check" {
                            state.checking = state
                                .last_doc
                                .clone()
                                .map(|(path, version)| (task, path, version));
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
                "workspace" => match loog::workspace::session::submit(&spec["command"].to_string())
                {
                    Ok(ticket) => format!("workspace submitted {ticket}"),
                    Err(error) => format!("workspace refused: {error}"),
                },
                "configure" => {
                    let configuration = loog::extension::types::ProviderConfiguration {
                        modes: None,
                        revision: None,
                        provider: spec["provider"].as_str().unwrap_or("").into(),
                        enabled: spec["enabled"].as_bool().unwrap_or(true),
                        resolve: true,
                        prepare: true,
                        triggers: vec![".".into()],
                        retriggers: vec![",".into()],
                        commands: vec!["run".into()],
                        aliases: vec!["test-formatter".into()],
                    };
                    match host::configure_provider(&configuration) {
                        Ok(()) => "configured".into(),
                        Err(error) => format!("configure refused: {}", error.message),
                    }
                }
                "capture" => match host::capture_document(spec["document"].as_u64().unwrap_or(1)) {
                    Ok(captured) => {
                        let text = host::read_range(
                            captured.snapshot,
                            ByteRange {
                                start: 0,
                                end: captured.length,
                            },
                        )
                        .unwrap();
                        host::release_snapshot(captured.snapshot);
                        format!(
                            "captured {} v{} {:?}",
                            captured.document.document, captured.document.version, text
                        )
                    }
                    Err(error) => format!("capture error: {}", error.message),
                },
                "workspace-edit" => {
                    use loog::extension::types::{
                        ColumnUnit, FileEdits, FileRange, FileTextEdit, TextPosition,
                        WorkspaceChange, WorkspaceEdit, WorkspaceEditRequest, WorkspaceOperation,
                    };
                    let changes = spec["paths"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|path| {
                            WorkspaceChange::Text(FileEdits {
                                path: path.as_str().unwrap().into(),
                                version: spec["version"].as_u64(),
                                edits: vec![FileTextEdit {
                                    range: FileRange {
                                        start: TextPosition { line: 0, column: 1 },
                                        end: TextPosition { line: 0, column: 3 },
                                        unit: ColumnUnit::Utf16,
                                    },
                                    text: spec["text"].as_str().unwrap_or("中").into(),
                                }],
                            })
                        })
                        .collect();
                    match host::perform_workspace_operation(&WorkspaceOperation::Edit(
                        WorkspaceEditRequest {
                            title: "Generic edit".into(),
                            edit: WorkspaceEdit { changes },
                        },
                    )) {
                        Ok(operation) => format!("workspace edit accepted {operation}"),
                        Err(error) => format!("workspace edit refused: {}", error.message),
                    }
                }
                "ui-create" | "ui-update" => {
                    use loog::extension::types::{UiDescription, UiKind, UiParent};
                    let description = UiDescription {
                        id: "status".into(),
                        kind: UiKind::Button,
                        text: Some(spec["text"].as_str().unwrap_or("Starting").into()),
                        icon: None,
                        foreground: Some(0xffffff),
                        background: Some(spec["color"].as_u64().unwrap_or(0x445566) as u32),
                        width: Some(100.0),
                        height: Some(28.0),
                        padding: 4.0,
                        gap: 0.0,
                        visible: true,
                        label: Some("Extension status".into()),
                    };
                    if spec["op"] == "ui-create" {
                        let node = host::ui_create(
                            &UiParent::Mount("statusbar.extensions".into()),
                            &description,
                        )
                        .unwrap();
                        STATE.with(|state| state.borrow_mut().ui_node = Some(node));
                        format!("ui created {node}")
                    } else {
                        let node = STATE.with(|state| state.borrow().ui_node.unwrap());
                        host::ui_update(node, &description).unwrap();
                        "ui updated".into()
                    }
                }
                "ui-remove" => {
                    let node = STATE.with(|state| state.borrow_mut().ui_node.take().unwrap());
                    host::ui_remove(node).unwrap();
                    "ui removed".into()
                }
                "ui-trap" => panic!("UI owner failed"),
                "watch" => {
                    let watch = host::watch_files(&loog::extension::types::WatchRequest {
                        root,
                        patterns: vec![spec["pattern"].as_str().unwrap_or("**/*").into()],
                    })
                    .unwrap();
                    STATE.with(|state| state.borrow_mut().watch = Some(watch));
                    format!("watch started {watch}")
                }
                "unwatch" => {
                    if let Some(watch) = STATE.with(|state| state.borrow_mut().watch.take()) {
                        host::unwatch_files(watch);
                    }
                    "watch stopped".into()
                }
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
                    use loog::extension::types::{
                        CreateOp, DeleteOp, FileOp, FileOpsRequest, RenameOp,
                    };
                    let text = |v: &serde_json::Value, k: &str| {
                        v[k].as_str().unwrap_or_default().to_owned()
                    };
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
        for event in loog::workspace::session::poll() {
            if !memory_workspace::event(&event) {
                host::log(0, &format!("workspace event {event}"));
            }
        }
        for message in batch {
            match message {
                Inbound::UiEvent(event) => {
                    if event.id == "motion-icon" {
                        loog::ui::motion::stop(event.node).unwrap();
                    }
                    host::log(
                        0,
                        &format!("ui event {} {} {:?}", event.node, event.id, event.payload),
                    );
                    if event.id == "relative-status" || event.id == "ext.example.example-status" {
                        STATE.with(|state| {
                            let mut state = state.borrow_mut();
                            state.widget_clicks += 1;
                            let node = state.ui_node.unwrap();
                            if state.widget_clicks == 2 {
                                host::ui_remove(node).unwrap();
                                state.ui_node = None;
                            } else if state.relative_widget {
                                use loog::ui::tree::{self, Placement, Relation, Target};
                                tree::place(
                                    node,
                                    &Placement {
                                        target: Target::PublicId("demo.encoding".into()),
                                        relation: Relation::After,
                                        order: 100,
                                    },
                                )
                                .unwrap();
                            } else {
                                use loog::extension::types::{UiDescription, UiKind};
                                host::ui_update(
                                    node,
                                    &UiDescription {
                                        id: event.id.clone(),
                                        kind: UiKind::Button,
                                        text: Some("Ready".into()),
                                        icon: None,
                                        foreground: None,
                                        background: Some(0x228844),
                                        width: None,
                                        height: None,
                                        padding: 4.0,
                                        gap: 0.0,
                                        visible: true,
                                        label: None,
                                    },
                                )
                                .unwrap();
                            }
                        });
                    }
                }
                Inbound::RunCommand(context) => run(
                    &context.command,
                    context.call,
                    context.snapshot,
                    context.selections,
                    context.selection_precondition,
                ),
                Inbound::OperationComplete((operation, completion)) => {
                    STATE.with(|state| {
                        state
                            .borrow_mut()
                            .pending
                            .retain(|pending| *pending != operation)
                    });
                    match completion {
                        Completion::Committed(receipt) => host::log(
                            0,
                            &format!("operation {operation} committed as {}", receipt.transaction),
                        ),
                        Completion::Failed(error) => host::log(
                            2,
                            &format!("operation {operation} failed: {}", error.message),
                        ),
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
                            exit.error
                                .as_ref()
                                .map(|e| e.message.as_str())
                                .unwrap_or("none")
                        ),
                    );
                    let checking = STATE.with(|state| {
                        let mut state = state.borrow_mut();
                        match state.checking.take() {
                            Some((task, path, version)) if task == exit.task => {
                                Some((path, version))
                            }
                            other => {
                                state.checking = other;
                                None
                            }
                        }
                    });
                    if let (Some((path, version)), Some(0)) = (checking, exit.code) {
                        let finding = Diagnostic {
                            documentation_url: None,
                            range: ByteRange { start: 0, end: 4 },
                            severity: 2,
                            message: "checker says hi".into(),
                            source: None,
                            code: None,
                        };
                        let _ = host::publish_diagnostics(
                            "example.tools.checker",
                            &path,
                            version,
                            &[finding],
                        );
                    }
                }
                Inbound::DocumentOpened(d) => {
                    STATE.with(|state| {
                        state.borrow_mut().last_doc = Some((d.path.clone(), d.version))
                    });
                    host::log(
                        0,
                        &format!(
                            "opened {} v{} life {} {}",
                            d.document, d.version, d.lifecycle, d.language
                        ),
                    );
                }
                Inbound::DocumentChanged(c) => host::log(
                    0,
                    &format!(
                        "changed {} {}->{} x{}",
                        c.document, c.from_version, c.to_version, c.edits
                    ),
                ),
                Inbound::DocumentResync(d) => {
                    host::log(0, &format!("resync {} v{}", d.document, d.version))
                }
                Inbound::DocumentClosed(d) => {
                    host::log(0, &format!("closed {} life {}", d.document, d.lifecycle))
                }
                Inbound::ProviderRequest(request)
                    if matches!(
                        request.kind,
                        ProviderKind::Completion | ProviderKind::CompletionResolve
                    ) =>
                {
                    match request.kind {
                        ProviderKind::Completion => {
                            let mut items = vec![
                                CompletionItem {
                                    label: "loog_hello".into(),
                                    label_detail: None,
                                    label_description: None,
                                    deprecated: None,
                                    preselect: None,
                                    insert_text_mode: None,
                                    detail: Some("from the extension".into()),
                                    documentation: None,
                                    kind: Some(3),
                                    insert_text: Some("loog_hello()".into()),
                                    text_edit: None,
                                    additional_text_edits: vec![],
                                    insert_text_format: None,
                                    commit_characters: vec![],
                                    command: None,
                                    sort_text: None,
                                    filter_text: None,
                                    data: Some("hello".into()),
                                },
                                CompletionItem {
                                    label: "loog_world".into(),
                                    label_detail: None,
                                    label_description: None,
                                    deprecated: None,
                                    preselect: None,
                                    insert_text_mode: None,
                                    detail: None,
                                    documentation: None,
                                    kind: Some(3),
                                    insert_text: None,
                                    text_edit: None,
                                    additional_text_edits: vec![],
                                    insert_text_format: None,
                                    commit_characters: vec![],
                                    command: None,
                                    sort_text: None,
                                    filter_text: None,
                                    data: Some("world".into()),
                                },
                            ];
                            if request.trigger.as_deref() == Some("!") {
                                let item = &mut items[0];
                                item.text_edit = Some(TextEdit {
                                    range: ByteRange {
                                        start: 0,
                                        end: request.length,
                                    },
                                    text: "loog_hello(${1:x})$0".into(),
                                });
                                item.additional_text_edits = vec![TextEdit {
                                    range: ByteRange {
                                        start: request.length,
                                        end: request.length,
                                    },
                                    text: "\n// imported".into(),
                                }];
                                item.insert_text_format = Some(2);
                                item.commit_characters = vec!["(".into()];
                                item.command = Some(ProviderCommand {
                                    name: "accepted".into(),
                                    arguments: "[42]".into(),
                                });
                            }
                            let _ = host::provide_completions(request.request, &items, false);
                        }
                        _ => {
                            if let Some(mut item) = request.item.clone() {
                                item.documentation = Some(format!(
                                    "docs for {}",
                                    item.data.clone().unwrap_or_default()
                                ));
                                let _ = host::provide_completions(request.request, &[item], false);
                            }
                        }
                    }
                }
                Inbound::WatchReady(watch) => host::log(0, &format!("watch ready {watch}")),
                Inbound::FileChanged(batch) => host::log(
                    0,
                    &format!("files changed {} {:?}", batch.watch, batch.changes),
                ),
                Inbound::WatchFailed(failure) => host::log(
                    0,
                    &format!("watch failed {} {}", failure.watch, failure.error.message),
                ),
                Inbound::RootsChanged(roots) => {
                    host::log(0, &format!("roots changed {}", roots.len()))
                }
                Inbound::ProcessOutput(output) => {
                    host::log(
                        0,
                        &format!("process output {} {:?}", output.process, output.bytes),
                    );
                    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.bytes) {
                        host::log(0, &format!("process message {}", value["id"]));
                    }
                }
                Inbound::ProcessWritable(writable) => host::log(
                    0,
                    &format!("process writable {} {}", writable.process, writable.bytes),
                ),
                Inbound::ProcessExited(exit) => host::log(
                    0,
                    &format!(
                        "process exited {} code {:?} killed {} error {:?}",
                        exit.process,
                        exit.code,
                        exit.killed,
                        exit.error.map(|error| error.message)
                    ),
                ),
                Inbound::ProviderCommand(call) => host::log(
                    0,
                    &format!(
                        "provider command {} {} {}",
                        call.provider, call.command.name, call.command.arguments
                    ),
                ),
                Inbound::ProviderRequest(request) => {
                    let text = host::read_range(
                        request.snapshot,
                        ByteRange {
                            start: 0,
                            end: request.length,
                        },
                    )
                    .unwrap_or_default();
                    match request.kind {
                        ProviderKind::Diagnostics => {
                            let mut items = Vec::new();
                            let mut at = 0;
                            while let Some(found) = text[at..].find("TODO") {
                                let start = at + found;
                                items.push(Diagnostic {
                                    documentation_url: None,
                                    range: ByteRange {
                                        start: start as u64,
                                        end: start as u64 + 4,
                                    },
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
                        ProviderKind::Formatting
                        | ProviderKind::FormatOnType
                        | ProviderKind::Save => {
                            if request.provider.ends_with(".a-prefix") {
                                let _ = host::provide_edits(
                                    request.request,
                                    &[TextEdit {
                                        range: ByteRange { start: 0, end: 0 },
                                        text: "😀 ".into(),
                                    }],
                                );
                                continue;
                            }
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
                        ProviderKind::Hover => {
                            let at = request.cursor.unwrap_or(0);
                            let answer = HoverAnswer {
                                contents: format!("hover at {at} of {} bytes", request.length),
                                range: Some(ByteRange {
                                    start: at,
                                    end: at + 1,
                                }),
                            };
                            let _ = host::provide_hover(request.request, Some(&answer));
                        }
                        ProviderKind::Definition => {
                            let place = Location {
                                path: "elsewhere.txt".into(),
                                line: 2,
                                column: 3,
                            };
                            let _ = host::provide_locations(request.request, &[place]);
                        }
                        ProviderKind::DocumentSymbols => {
                            let whole = ByteRange {
                                start: 0,
                                end: request.length,
                            };
                            let first = ByteRange {
                                start: 0,
                                end: request.length.min(2),
                            };
                            let items = [
                                DocumentSymbol {
                                    name: "outer".into(),
                                    deprecated: None,
                                    detail: None,
                                    kind: 2,
                                    range: whole,
                                    selection: first,
                                    parent: None,
                                },
                                DocumentSymbol {
                                    name: "inner".into(),
                                    deprecated: None,
                                    detail: Some("nested".into()),
                                    kind: 12,
                                    range: first,
                                    selection: first,
                                    parent: Some(0),
                                },
                            ];
                            let _ = host::provide_symbols(request.request, &items);
                        }
                        ProviderKind::SignatureHelp => {
                            let answer = SignatureAnswer {
                                label: "add(a, b)".into(),
                                documentation: None,
                                parameters: vec![
                                    ParameterInfo {
                                        label: "a".into(),
                                        documentation: None,
                                    },
                                    ParameterInfo {
                                        label: "b".into(),
                                        documentation: None,
                                    },
                                ],
                                active_parameter: Some(1),
                            };
                            let _ = host::provide_signature(request.request, Some(&answer));
                        }
                        ProviderKind::InlayHints => {
                            let hint = InlayHint {
                                offset: 0,
                                label: "hint:".into(),
                                kind: Some(2),
                                padding_left: false,
                                padding_right: true,
                                tooltip: None,
                            };
                            let _ = host::provide_inlay_hints(request.request, &[hint]);
                        }
                        ProviderKind::CodeActions => {
                            let range = request.range.unwrap_or(ByteRange { start: 0, end: 0 });
                            let action = CodeAction {
                                title: "Mark the selection".into(),
                                kind: Some("refactor".into()),
                                preferred: false,
                                edits: vec![TextEdit {
                                    range,
                                    text: "[marked]".into(),
                                }],
                            };
                            let _ = host::provide_code_actions(request.request, &[action]);
                        }
                        _ => {
                            use loog::extension::types::*;
                            let captured = host::capture_document(request.document).unwrap();
                            let path = captured.document.path;
                            host::release_snapshot(captured.snapshot);
                            let range = FileRange {
                                start: TextPosition { line: 0, column: 0 },
                                end: TextPosition { line: 0, column: 5 },
                                unit: ColumnUnit::Utf8,
                            };
                            let location = FileLocation {
                                path: path.clone(),
                                range,
                            };
                            let item = HierarchyItem {
                                name: "item".into(),
                                kind: 12,
                                detail: None,
                                location: location.clone(),
                                selection: range,
                                data: Some("owned-data".into()),
                            };
                            let context = request.context.as_ref();
                            let answer = match request.kind {
                                ProviderKind::References => LanguageAnswer::Places(vec![location]),
                                ProviderKind::WorkspaceSymbols => {
                                    LanguageAnswer::WorkspaceSymbols(vec![WorkspaceSymbol {
                                        name: context
                                            .and_then(|c| c.query.clone())
                                            .unwrap_or_else(|| "symbol".into()),
                                        kind: 12,
                                        container: None,
                                        location,
                                    }])
                                }
                                ProviderKind::DocumentHighlights => {
                                    LanguageAnswer::DocumentHighlights(vec![RangeKind {
                                        range: ByteRange { start: 0, end: 5 },
                                        kind: 2,
                                    }])
                                }
                                ProviderKind::PrepareRename => {
                                    LanguageAnswer::RenameTarget(Some(RenameTarget {
                                        range: Some(ByteRange { start: 0, end: 5 }),
                                        placeholder: Some("hello".into()),
                                        default_behavior: false,
                                    }))
                                }
                                ProviderKind::Rename => {
                                    LanguageAnswer::WorkspaceEdit(WorkspaceEdit {
                                        changes: vec![WorkspaceChange::Text(FileEdits {
                                            path,
                                            version: Some(request.version),
                                            edits: vec![FileTextEdit {
                                                range,
                                                text: context
                                                    .and_then(|c| c.name.clone())
                                                    .unwrap_or_else(|| "renamed".into()),
                                            }],
                                        })],
                                    })
                                }
                                ProviderKind::FoldingRanges => {
                                    LanguageAnswer::Folds(vec![FoldRange {
                                        start: 0,
                                        end: 1,
                                        start_column: None,
                                        end_column: None,
                                        kind: Some("region".into()),
                                        label: None,
                                    }])
                                }
                                ProviderKind::SelectionRanges => LanguageAnswer::Selections(vec![
                                    SelectionRange {
                                        range: ByteRange {
                                            start: 0,
                                            end: request.length,
                                        },
                                        parent: None,
                                    },
                                    SelectionRange {
                                        range: ByteRange { start: 0, end: 5 },
                                        parent: Some(0),
                                    },
                                ]),
                                ProviderKind::CallHierarchy
                                    if context.and_then(|c| c.mode)
                                        != Some(ProviderMode::Prepare) =>
                                {
                                    LanguageAnswer::Calls(vec![HierarchyEdge {
                                        item,
                                        sites: vec![range],
                                    }])
                                }
                                ProviderKind::CallHierarchy | ProviderKind::TypeHierarchy => {
                                    LanguageAnswer::Hierarchy(vec![item])
                                }
                                ProviderKind::CodeLens => LanguageAnswer::Lenses(vec![CodeLens {
                                    range: ByteRange { start: 0, end: 5 },
                                    title: None,
                                    command: None,
                                    data: Some("lens-data".into()),
                                }]),
                                ProviderKind::CodeLensResolve => {
                                    LanguageAnswer::Lenses(vec![CodeLens {
                                        range: ByteRange { start: 0, end: 5 },
                                        title: Some("Run".into()),
                                        command: Some(ProviderCommand {
                                            name: "run".into(),
                                            arguments: "[]".into(),
                                        }),
                                        data: None,
                                    }])
                                }
                                ProviderKind::CodeActionResolve => {
                                    LanguageAnswer::Actions(vec![ProviderAction {
                                        title: "Resolved".into(),
                                        kind: None,
                                        preferred: true,
                                        disabled: None,
                                        edit: None,
                                        command: Some(ProviderCommand {
                                            name: "run".into(),
                                            arguments: "[]".into(),
                                        }),
                                        data: None,
                                    }])
                                }
                                ProviderKind::ExecuteCommand => {
                                    host::log(
                                        0,
                                        &format!(
                                            "execute {:?} {:?}",
                                            context.and_then(|c| c.name.as_ref()),
                                            context.and_then(|c| c.data.as_ref())
                                        ),
                                    );
                                    LanguageAnswer::Completed
                                }
                                ProviderKind::Highlights => {
                                    LanguageAnswer::Highlights(vec![Highlight {
                                        range: ByteRange { start: 0, end: 5 },
                                        theme: "function".into(),
                                    }])
                                }
                                _ => continue,
                            };
                            host::provide_language(request.request, &answer).unwrap();
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
                Inbound::DocumentSaved(saved) => {
                    let text = host::read_range(
                        saved.snapshot,
                        ByteRange {
                            start: 0,
                            end: saved.length,
                        },
                    )
                    .unwrap();
                    host::release_snapshot(saved.snapshot);
                    host::log(
                        0,
                        &format!(
                            "saved {} v{} {:?}",
                            saved.document.document, saved.document.version, text
                        ),
                    );
                }
                Inbound::WorkspaceFilesChanged(changes) => {
                    host::log(0, &format!("workspace files {changes:?}"))
                }
                Inbound::ProviderCancelled(request) => {
                    host::log(0, &format!("provider cancelled {request}"))
                }
                Inbound::TimerElapsed(_) | Inbound::Shutdown => {}
            }
        }
        let waiting = STATE.with(|state| !state.borrow().pending.is_empty());
        let _ = (ByteRange { start: 0, end: 0 }, ErrorKind::Cancelled);
        Ok(if waiting {
            RoundStatus::Pending
        } else {
            RoundStatus::Idle
        })
    }
}

export!(Extension);
