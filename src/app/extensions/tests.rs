//! End-to-end tests: real Rust component, real executor thread, real
//! workspace edit service. Events are pumped by hand on the test thread, which
//! plays the role of the UI thread.
use super::*;
use crate::infrastructure::extensions::tasks;
use crate::infrastructure::extensions::{Event, Executor};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
#[path = "../../../tests/fixtures/language_migration_metrics.rs"]
mod migration_metrics;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// Build the guest once and return the component bytes.
pub(crate) fn component() -> &'static [u8] {
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(|| {
        let root = repo();
        let target = root.join("target/extension-guest");
        let status = std::process::Command::new("cargo")
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

const MANIFEST: &str = r#"{
    "schema": 1,
    "id": "example.tools",
    "version": "0.1.0",
    "api": "loom:extension@0.1.0",
    "component": "extension.wasm",
    "activation": [
        { "kind": "command", "id": "example.tools.uppercase" },
        { "kind": "command", "id": "example.tools.trap" },
        { "kind": "command", "id": "example.tools.spin" }
    ],
    "capabilities": ["documents.read", "workspace.edit"],
    "contributes": { "commands": [
        { "id": "example.tools.uppercase", "title": "Uppercase Selection" },
        { "id": "example.tools.trap", "title": "Trap" },
        { "id": "example.tools.spin", "title": "Spin" }
    ] }
}"#;

fn package_dir(name: &str) -> PathBuf {
    package_dir_with(name, MANIFEST)
}

pub(crate) fn package_dir_with(name: &str, manifest: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("loom-ext-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("manifest.json"), manifest).unwrap();
    std::fs::write(dir.join("extension.wasm"), component()).unwrap();
    dir
}

/// A completion answer: its source and, if it came in time, the items and the incomplete flag.
type OfferedList = (u64, Option<(Vec<serde_json::Value>, bool)>);

struct Fixture {
    event_timeout: Duration,
    wake: Receiver<()>,
    trace: std::collections::VecDeque<String>,
    runner: tasks::Runner,
    task_events: Receiver<tasks::TaskEvent>,
    processes: crate::infrastructure::extensions::processes::Runner,
    process_events: Receiver<crate::infrastructure::extensions::processes::ProcessEvent>,
    watches: crate::infrastructure::extensions::watches::Runner,
    watch_events: Receiver<crate::infrastructure::extensions::watches::WatchEvent>,
    service: ExtensionService,
    executor: Executor,
    events: Receiver<Event>,
    app: AppState,
    path: PathBuf,
    published: Vec<(
        u64,
        Vec<crate::features::language_service::diagnostics::Diagnostic>,
    )>,
    cleared: Vec<u64>,
    offered: Vec<OfferedList>,
    ready: Vec<(u64, bool, Option<String>)>,
    changed: Vec<String>,
    submitted: Vec<Job>,
    resolved: Vec<(u64, serde_json::Value)>,
    /// Answers to language requests, by the token they were asked under.
    language: Vec<(u64, Option<super::LanguageAnswer>)>,
}

fn fixture(name: &str, text: &str, select: std::ops::Range<usize>) -> Fixture {
    fixture_with(name, text, select, MANIFEST)
}

fn fixture_with(name: &str, text: &str, select: std::ops::Range<usize>, manifest: &str) -> Fixture {
    fixture_package(name, text, select, &package_dir_with(name, manifest), None)
}

fn fixture_package(
    name: &str,
    text: &str,
    select: std::ops::Range<usize>,
    package: &Path,
    cache: Option<PathBuf>,
) -> Fixture {
    let event_timeout = Duration::from_secs(if cache.is_some() { 600 } else { 60 });
    let (wake_sender, wake) = channel();
    let executor_wake = wake_sender.clone();
    let (sender, events) = channel();
    let sender = Mutex::new(sender);
    let executor = Executor::spawn(Box::new(move |event| {
        let _ = sender.lock().unwrap().send(event);
        let _ = executor_wake.send(());
    }))
    .unwrap();
    let mut service = ExtensionService::new(cache.unwrap_or_else(|| {
        std::env::temp_dir().join(format!("loom-ext-cache-{}", std::process::id()))
    }));
    let id = service.load_package(package).unwrap();
    service.grant_declared(&id);
    let mut app = AppState::new();
    app.extension_widgets.begin_session(service.ui_session);
    let path = std::env::temp_dir().join(format!("loom-ext-doc-{}-{name}.txt", std::process::id()));
    app.workspace.open_path(path.clone(), text.into());
    {
        let mut editor = app.workspace.active_editor_mut().unwrap();
        editor.set_selection(crate::model::text::Selection {
            anchor: Some(select.start),
            cursor: select.end,
            ..crate::model::text::Selection::caret(select.end)
        });
    }
    let (task_sender, task_events) = channel();
    let task_sender = Mutex::new(task_sender);
    let task_wake = wake_sender.clone();
    let runner = tasks::Runner::new(move |event| {
        let _ = task_sender.lock().unwrap().send(event);
        let _ = task_wake.send(());
    });
    let (process_sender, process_events) = channel();
    let process_wake = wake_sender.clone();
    let processes = crate::infrastructure::extensions::processes::Runner::new(move |event| {
        let _ = process_sender.send(event);
        let _ = process_wake.send(());
    });
    let (watch_sender, watch_events) = channel();
    let watches = crate::infrastructure::extensions::watches::Runner::new(move |event| {
        let _ = watch_sender.send(event);
        let _ = wake_sender.send(());
    });
    Fixture {
        event_timeout,
        wake,
        trace: Default::default(),
        watches,
        watch_events,
        processes,
        process_events,
        runner,
        task_events,
        service,
        executor,
        events,
        app,
        path,
        published: Vec::new(),
        cleared: Vec::new(),
        offered: Vec::new(),
        ready: Vec::new(),
        changed: Vec::new(),
        submitted: Vec::new(),
        resolved: Vec::new(),
        language: Vec::new(),
    }
}

impl Fixture {
    /// The next executor event; task output and exits are handled on the way,
    /// as the UI thread would.
    fn recv(&mut self) -> Event {
        let limit = std::time::Instant::now() + self.event_timeout;
        loop {
            while let Ok(event) = self.watch_events.try_recv() {
                let effects = self.service.handle_watch_event(event);
                self.apply(effects);
            }
            while let Ok(event) = self.process_events.try_recv() {
                if std::env::var_os("LOOM_MIGRATION_TRACE").is_some() {
                    if let crate::infrastructure::extensions::processes::ProcessEvent::Output {
                        bytes,
                        ..
                    } = &event
                    {
                        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) {
                            self.trace.push_back(format!("in: {value}"));
                        }
                    }
                    while self.trace.len() > 12 {
                        self.trace.pop_front();
                    }
                }
                let effects = self.service.handle_process_event(event);
                self.apply(effects);
            }
            while let Ok(event) = self.task_events.try_recv() {
                let effects = self.service.handle_task_event(event);
                self.apply(effects);
            }
            if let Ok(event) = self.events.try_recv() {
                return event;
            }
            assert!(
                std::time::Instant::now() < limit,
                "the executor produced no event"
            );
            let _ = self
                .wake
                .recv_timeout(limit.saturating_duration_since(std::time::Instant::now()));
        }
    }

    fn text(&self) -> String {
        let id = self.app.workspace.file_id_for_path(&self.path).unwrap();
        self.app.workspace.documents[&id].buffer.text().to_owned()
    }

    /// Apply effects the way the host does; returns user-facing messages.
    fn apply(&mut self, effects: Vec<Effect>) -> Vec<String> {
        let mut messages = Vec::new();
        for effect in effects {
            match effect {
                Effect::WorkspaceOperation {
                    id,
                    generation,
                    operation,
                    action,
                } => {
                    let super::language::WorkspaceAction::Edit {
                        title,
                        edit,
                        stamps,
                    } = action
                    else {
                        panic!("native navigation is checked through its typed effect");
                    };
                    let loaded = crate::app::language_ui::edit::unopened(&self.app, &edit)
                        .unwrap()
                        .into_iter()
                        .map(|path| {
                            let text = std::fs::read_to_string(&path).unwrap();
                            (path, text)
                        })
                        .collect();
                    let request = crate::app::language_ui::edit::Request {
                        title,
                        edit,
                        stamps,
                        rect: Default::default(),
                        reply: None,
                    };
                    let result =
                        crate::app::language_ui::edit::apply_now(&mut self.app, &request, loaded);
                    self.executor.submit(Job::Complete {
                        id,
                        generation,
                        operation,
                        outcome: match result {
                            Ok(_) => Outcome::Committed {
                                transaction: 0,
                                version: 0,
                            },
                            Err(message) => Outcome::Failed(ExtensionError::new(
                                ErrorKind::InvalidEdit,
                                message,
                            )),
                        },
                    });
                }
                Effect::ClearUi { session, owner } => {
                    self.app.extension_widgets.remove(session, &owner)
                }
                Effect::StartWatch { watch, spec } => self.watches.start(watch, spec),
                Effect::StopWatch { watch } => self.watches.stop(watch),
                Effect::StartProcess { process, spec } => self.processes.start(process, spec),
                Effect::WriteProcess { process, bytes } => {
                    if std::env::var_os("LOOM_MIGRATION_TRACE").is_some() {
                        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                            self.trace.push_back(format!("out: {value}"));
                        }
                        while self.trace.len() > 12 {
                            self.trace.pop_front();
                        }
                    }
                    self.processes.write(process, bytes).unwrap();
                }
                Effect::CloseProcess { process } => {
                    self.processes.close_stdin(process).unwrap();
                }
                Effect::KillProcess { process } => self.processes.kill(process),
                Effect::Submit(job) => {
                    if matches!(job, Job::EndSave { .. }) {
                        self.submitted.push(Job::Shutdown);
                    }
                    assert!(self.executor.submit(job));
                }
                Effect::Toast(m) | Effect::Error(m) => messages.push(m),
                Effect::PublishHighlights { .. } => {}
                Effect::PublishDiagnostics { source, items, .. } => {
                    self.published.push((source, items))
                }
                Effect::ClearDiagnostics { source } => self.cleared.push(source),
                Effect::SaveReady { chain, ok, message } => self.ready.push((chain, ok, message)),
                Effect::RunFileOps { key, ops } => {
                    // The host runs this on a background thread; here it runs inline.
                    let outcome = crate::infrastructure::extensions::fileops::run(&ops, |_| {});
                    let effects = self.service.file_ops_done(&mut self.app, key, outcome);
                    messages.extend(self.apply(effects));
                }
                Effect::FilesChanged(changes) => {
                    self.changed
                        .extend(changes.iter().map(|c| format!("{c:?}")));
                }
                Effect::StartTask { task, spec } => self.runner.start(task, spec),
                Effect::CancelTask { task } => self.runner.cancel(task),
                Effect::OfferCompletion { source, items, .. } => self.offered.push((source, items)),
                Effect::ResolvedCompletion { source, item, .. } => {
                    self.resolved.push((source, item))
                }
                Effect::LanguageAnswer { token, answer } => self.language.push((token, answer)),
                Effect::OfferSnippets { source, items, .. } => {
                    self.offered.push((source, Some((items, false))))
                }
            }
        }
        messages
    }

    fn run(&mut self, command: &str) -> Vec<String> {
        let effects = self.service.run_command(&self.app, command);
        self.apply(effects)
    }

    /// Handle events until `done` says so (or time runs out).
    fn pump(&mut self, mut done: impl FnMut(&Fixture, &Event) -> bool) -> Vec<String> {
        let mut messages = Vec::new();
        loop {
            let event = self.recv();
            let stop = done(self, &event);
            let effects = self.service.handle_event(&mut self.app, event);
            messages.extend(self.apply(effects));
            if stop {
                return messages;
            }
        }
    }
}

#[test]
fn a_command_activates_on_demand_edits_the_captured_document_and_undoes_in_one_step() {
    let mut f = fixture("uppercase", "hello world", 0..5);
    assert_eq!(
        f.service
            .package("example.tools")
            .unwrap()
            .lifecycle
            .state(),
        State::Dormant
    );
    assert!(f.run("example.tools.uppercase").is_empty());
    assert_eq!(
        f.service
            .package("example.tools")
            .unwrap()
            .lifecycle
            .state(),
        State::Activating
    );

    // The guest reports the commit back to itself; wait for that log line.
    f.pump(|_, event| matches!(event, Event::Log { text, .. } if text.contains("committed as")));
    assert_eq!(f.text(), "HELLO world");
    assert_eq!(
        f.service
            .package("example.tools")
            .unwrap()
            .lifecycle
            .state(),
        State::Active
    );

    let id = f.app.workspace.file_id_for_path(&f.path).unwrap();
    f.app.workspace.document_editor_mut(id).unwrap().undo();
    assert_eq!(f.text(), "hello world");
}

#[test]
fn without_a_grant_nothing_starts() {
    let mut f = fixture("denied", "abc", 0..3);
    f.service = ExtensionService::new(std::env::temp_dir().join("loom-ext-cache-denied"));
    f.service.load_package(&package_dir("denied2")).unwrap();
    let messages = f.run("example.tools.uppercase");
    assert!(
        messages.iter().any(|m| m.contains("has not been granted")),
        "{messages:?}"
    );
    assert_eq!(
        f.service
            .package("example.tools")
            .unwrap()
            .lifecycle
            .state(),
        State::Dormant
    );
    assert_eq!(f.text(), "abc");
}

#[test]
fn typing_while_the_extension_works_makes_its_edit_stale_and_keeps_the_typing() {
    let mut f = fixture("stale", "hello world", 0..5);
    f.run("example.tools.uppercase");
    // The person types before the extension's edit comes back.
    let id = f.app.workspace.file_id_for_path(&f.path).unwrap();
    f.app
        .workspace
        .document_editor_mut(id)
        .unwrap()
        .set_cursor(11);
    loom_editor_core::normal::insert(&mut f.app.workspace.document_editor_mut(id).unwrap(), "!");
    f.pump(|_, event| matches!(event, Event::Log { text, .. } if text.contains("operation") && text.contains("failed")));
    assert_eq!(f.text(), "hello world!");
}

#[test]
fn a_trap_fails_the_extension_until_an_explicit_retry_makes_a_new_generation() {
    let mut f = fixture("trap", "abc", 0..3);
    f.run("example.tools.trap");
    let messages = f.pump(|_, event| matches!(event, Event::Failed { .. }));
    assert!(
        messages.iter().any(|m| m.contains("stopped")),
        "{messages:?}"
    );
    let package = f.service.package("example.tools").unwrap();
    assert_eq!(package.lifecycle.state(), State::Failed);
    let failed_generation = package.lifecycle.generation();

    // No automatic restart: the next command is refused until the user retries.
    let messages = f.run("example.tools.uppercase");
    assert!(messages.iter().any(|m| m.contains("retry")), "{messages:?}");

    f.service.retry("example.tools").unwrap();
    assert!(
        f.service
            .package("example.tools")
            .unwrap()
            .lifecycle
            .generation()
            > failed_generation
    );
    f.run("example.tools.uppercase");
    f.pump(|_, event| matches!(event, Event::Log { text, .. } if text.contains("committed as")));
    assert_eq!(f.text(), "ABC");
}

#[test]
fn a_busy_loop_is_stopped_by_the_epoch_deadline() {
    let mut f = fixture("spin", "abc", 0..3);
    f.service.set_guest_limits(Limits {
        round_ticks: 10,
        ..Limits::default()
    });
    f.run("example.tools.spin");
    let messages = f.pump(|_, event| matches!(event, Event::Failed { .. }));
    assert!(
        messages.iter().any(|m| m.contains("deadline-exceeded")),
        "{messages:?}"
    );
    assert_eq!(f.text(), "abc");
}

#[test]
fn the_second_start_loads_precompiled_code_from_the_cache() {
    let mut first = fixture("cache1", "abc", 0..3);
    first.run("example.tools.uppercase");
    first.pump(|_, event| matches!(event, Event::Loaded { .. }));
    let mut second = fixture("cache2", "abc", 0..3);
    second.run("example.tools.uppercase");
    let mut hit = false;
    second.pump(|_, event| {
        if let Event::Loaded { cache_hit, .. } = event {
            hit = *cache_hit;
            true
        } else {
            false
        }
    });
    assert!(hit, "the same component bytes were compiled once");
}

#[test]
fn the_report_shows_state_permissions_and_failures_without_document_text() {
    let mut f = fixture("report", "secret text", 0..6);
    let before = f.service.report();
    assert!(before.contains("example.tools 0.1.0 [Dormant]"), "{before}");
    assert!(
        before.contains("documents.read, workspace.edit"),
        "{before}"
    );
    f.run("example.tools.trap");
    f.pump(|_, event| matches!(event, Event::Failed { .. }));
    let after = f.service.report();
    assert!(
        after.contains("[Failed]") && after.contains("failed at: Execution"),
        "{after}"
    );
    assert!(!after.contains("secret"), "{after}");
}

#[test]
fn a_reload_moves_the_generation_on_and_old_instance_events_cannot_act() {
    let mut f = fixture("reload", "hello world", 0..5);
    f.run("example.tools.uppercase");
    f.pump(|_, event| matches!(event, Event::Log { text, .. } if text.contains("committed as")));
    let old = f
        .service
        .package("example.tools")
        .unwrap()
        .lifecycle
        .generation();

    let effects = f.service.reload("example.tools").unwrap();
    f.apply(effects);
    let package = f.service.package("example.tools").unwrap();
    assert!(package.lifecycle.generation() > old);
    assert_eq!(package.lifecycle.state(), State::Dormant);

    // An edit request from the old instance finds no ground to stand on.
    let stale = loom_extension_host::AcceptedEdit {
        operation: 99,
        title: "late".into(),
        call: 1,
        sequence: 50,
        edit_count: 0,
        selection_precondition: None,
        documents: Vec::new(),
    };
    let effects = f.service.handle_event(
        &mut f.app,
        Event::EditRequested {
            id: "example.tools".into(),
            generation: old,
            edit: stale,
        },
    );
    assert!(effects.is_empty());

    // Unchanged content keeps its grants, so the reloaded extension runs again.
    f.run("example.tools.uppercase");
    f.pump(|_, event| matches!(event, Event::Log { text, .. } if text.contains("committed as")));
}

#[test]
fn a_package_that_changed_on_disk_must_be_granted_again_after_a_reload() {
    let mut f = fixture("regrant", "abc", 0..3);
    let dir = f.service.package("example.tools").unwrap().dir.clone();
    let manifest = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        manifest.replace("\"version\": \"0.1.0\"", "\"version\": \"0.2.0\""),
    )
    .unwrap();
    let effects = f.service.reload("example.tools").unwrap();
    let messages = f.apply(effects);
    assert!(
        messages.iter().any(|m| m.contains("granted again")),
        "{messages:?}"
    );
    let denied = f.run("example.tools.uppercase");
    assert!(
        denied.iter().any(|m| m.contains("has not been granted")),
        "{denied:?}"
    );
}

const EVENT_MANIFEST: &str = r#"{
    "schema": 1,
    "id": "example.tools",
    "version": "0.1.0",
    "api": "loom:extension@0.1.0",
    "component": "extension.wasm",
    "activation": [
        { "kind": "event", "name": "document.open" },
        { "kind": "event", "name": "document.change" },
        { "kind": "event", "name": "document.close" }
    ],
    "capabilities": ["events"]
}"#;

impl Fixture {
    fn observe(&mut self) {
        let effects = self.service.observe(&self.app);
        self.apply(effects);
    }

    fn wait_for_log(&mut self, needle: &str) {
        let needle = needle.to_owned();
        self.pump(
            move |_, event| matches!(event, Event::Log { text, .. } if text.contains(&needle)),
        );
    }
}

#[test]
fn document_events_wake_a_dormant_extension_and_arrive_in_order_with_a_baseline_first() {
    let mut f = fixture_with("events", "one", 0..0, EVENT_MANIFEST);
    assert_eq!(
        f.service
            .package("example.tools")
            .unwrap()
            .lifecycle
            .state(),
        State::Dormant
    );
    f.observe();
    assert_eq!(
        f.service
            .package("example.tools")
            .unwrap()
            .lifecycle
            .state(),
        State::Activating
    );
    // The document was open before the extension started: it still gets its opened event first.
    f.wait_for_log("opened 1 v");

    let id = f.app.workspace.file_id_for_path(&f.path).unwrap();
    f.app
        .workspace
        .document_editor_mut(id)
        .unwrap()
        .set_cursor(3);
    loom_editor_core::normal::insert(&mut f.app.workspace.document_editor_mut(id).unwrap(), "!");
    f.observe();
    f.wait_for_log("changed 1");

    // Two quick changes reach the extension as one contiguous message.
    loom_editor_core::normal::insert(&mut f.app.workspace.document_editor_mut(id).unwrap(), "?");
    loom_editor_core::normal::insert(&mut f.app.workspace.document_editor_mut(id).unwrap(), "#");
    f.observe();
    f.wait_for_log("x");
    let logs = f.service.package("example.tools").unwrap().logs.clone();
    let texts: Vec<_> = logs
        .iter()
        .map(|(_, t)| t.as_str())
        .filter(|text| !text.starts_with("roots changed"))
        .collect();
    assert!(texts[0].starts_with("opened 1"), "{texts:?}");
    assert!(
        texts.iter().any(|t| t.starts_with("changed 1")),
        "{texts:?}"
    );
}

#[test]
fn a_closed_document_is_reported_and_a_reopened_one_gets_a_new_identity() {
    let mut f = fixture_with("lives", "abc", 0..0, EVENT_MANIFEST);
    f.observe();
    f.wait_for_log("opened 1");
    let id = f.app.workspace.file_id_for_path(&f.path).unwrap();
    f.app.workspace.close(id);
    f.observe();
    f.wait_for_log("closed 1 life 1");
    f.app.workspace.open_path(f.path.clone(), "abc".into());
    f.observe();
    f.wait_for_log("opened 2");
}

#[test]
fn extensions_without_event_declarations_are_never_woken_by_documents() {
    let mut f = fixture("quiet", "abc", 0..0);
    f.observe();
    f.observe();
    assert_eq!(
        f.service
            .package("example.tools")
            .unwrap()
            .lifecycle
            .state(),
        State::Dormant
    );
}

const SETTINGS_MANIFEST: &str = r#"{
    "schema": 1,
    "id": "example.tools",
    "version": "0.1.0",
    "api": "loom:extension@0.1.0",
    "component": "extension.wasm",
    "activation": [{ "kind": "command", "id": "example.tools.uppercase" }],
    "capabilities": ["documents.read", "workspace.edit"],
    "contributes": {
        "commands": [{ "id": "example.tools.uppercase", "title": "Uppercase Selection" }],
        "settings": [
            { "key": "example.tools.width", "kind": "number", "default": 80 },
            { "key": "example.tools.token", "kind": "string", "default": "", "scope": "user" }
        ]
    }
}"#;

#[test]
fn running_extensions_are_told_which_settings_changed_and_project_files_stay_in_their_lane() {
    use serde_json::json;
    let mut f = fixture_with("settings", "hello", 0..5, SETTINGS_MANIFEST);
    f.run("example.tools.uppercase");
    f.wait_for_log("committed as");

    // A user value reaches the running extension.
    let effects = f
        .service
        .set_setting_sources(json!({ "example.tools.width": 100 }), None, false);
    f.apply(effects);
    f.wait_for_log("setting example.tools.width=100");

    // A project value in an untrusted folder changes nothing and is reported.
    let project = json!({ "example.tools.width": 120, "example.tools.token": "stolen" });
    let effects = f.service.set_setting_sources(
        json!({ "example.tools.width": 100 }),
        Some(project.clone()),
        false,
    );
    assert!(
        effects.iter().all(
            |e| !matches!(e, Effect::Submit(Job::Settings { changed, .. }) if !changed.is_empty())
        ),
        "{effects:?}"
    );
    f.apply(effects);
    let report = f.service.report();
    assert!(
        report.contains("setting ignored: example.tools.width (the folder is not trusted)"),
        "{report}"
    );
    assert!(
        report.contains("example.tools.token (this setting can only be set by the user)"),
        "{report}"
    );

    // Once the folder is trusted the project value applies, except the user-only key.
    let effects =
        f.service
            .set_setting_sources(json!({ "example.tools.width": 100 }), Some(project), true);
    f.apply(effects);
    f.wait_for_log("setting example.tools.width=120");
}

#[test]
fn a_manifest_default_of_the_wrong_type_is_refused() {
    let bad = SETTINGS_MANIFEST.replace("\"default\": 80", "\"default\": \"eighty\"");
    assert!(matches!(
        Manifest::parse(&bad),
        Err(crate::features::extensions::manifest::ManifestError::BadSettingDefault(_))
    ));
}

#[test]
fn a_resource_only_package_loads_without_a_component_and_bad_resources_are_dropped_alone() {
    let dir = std::env::temp_dir().join(format!("loom-ext-res-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("themes")).unwrap();
    std::fs::write(dir.join("themes/dark.json"), r#"{"name":"Night"}"#).unwrap();
    std::fs::write(dir.join("themes/broken.json"), "{ not json").unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        r#"{ "schema": 1, "id": "acme.night", "version": "1.0.0",
            "contributes": { "themes": [
                { "id": "acme.night.dark", "path": "themes/dark.json" },
                { "id": "acme.night.broken", "path": "themes/broken.json" },
                { "id": "acme.night.missing", "path": "themes/missing.json" }
            ] } }"#,
    )
    .unwrap();
    let mut service = ExtensionService::new(std::env::temp_dir().join("loom-ext-cache-res"));
    let id = service.load_package(&dir).unwrap();
    let package = service.package(&id).unwrap();
    assert!(!package.manifest.has_behaviour());
    assert_eq!(package.resources.themes, ["acme.night.dark"]);
    assert_eq!(package.resources.rejected.len(), 2);
    let report = service.report();
    assert!(
        report.contains("1 theme(s)") && report.contains("acme.night.broken (invalid JSON"),
        "{report}"
    );
    assert!(
        report.contains("acme.night.missing (the file does not exist)"),
        "{report}"
    );
    // A package without behaviour never activates, whatever happens to its resources.
    assert!(service.commands().is_empty());
}

#[test]
fn package_snippets_are_offered_for_their_language_only_and_a_bad_file_is_rejected_whole() {
    let probe = std::env::temp_dir().join(format!("loom-ext-doc-{}-snip.txt", std::process::id()));
    let language = language_of(&probe);
    let dir = std::env::temp_dir().join(format!("loom-ext-snippets-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("snippets")).unwrap();
    let file = |language: &str| {
        format!(
            r#"{{ "language": "{language}", "snippets": [
                {{ "prefix": "hello", "body": ["Hello, ${{1:name}}!", "$0"], "description": "Greeting" }}
            ] }}"#
        )
    };
    std::fs::write(dir.join("snippets/here.json"), file(&language)).unwrap();
    std::fs::write(
        dir.join("snippets/elsewhere.json"),
        file("no-such-language"),
    )
    .unwrap();
    std::fs::write(
        dir.join("snippets/bad.json"),
        r#"{ "language": "x", "snippets": [{ "prefix": "a" }] }"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        r#"{ "schema": 1, "id": "acme.snip", "version": "1.0.0",
            "contributes": { "snippets": [
                { "id": "acme.snip.here", "path": "snippets/here.json" },
                { "id": "acme.snip.elsewhere", "path": "snippets/elsewhere.json" },
                { "id": "acme.snip.bad", "path": "snippets/bad.json" }
            ] } }"#,
    )
    .unwrap();
    let mut f = fixture_with("snip", "he", 0..0, &provider_manifest(&language));
    let id = f.service.load_package(&dir).unwrap();
    let package = f.service.package(&id).unwrap();
    assert_eq!(package.resources.snippets.len(), 2);
    assert!(
        package.resources.rejected[0]
            .1
            .contains("snippet 1 needs a \"body\"")
    );

    let effects = f.service.snippet_offers(&f.app, 0);
    assert_eq!(effects.len(), 1, "only the file for this language");
    f.apply(effects);
    let (source, items) = f.offered.last().cloned().unwrap();
    assert!(source & crate::features::providers::EXTERNAL_SOURCE != 0);
    let (items, incomplete) = items.unwrap();
    assert!(!incomplete);
    assert_eq!(items[0]["label"], "hello");
    assert_eq!(items[0]["insertText"], "Hello, ${1:name}!\n$0");
    assert_eq!(items[0]["insertTextFormat"], 2);

    // A stopped package offers nothing.
    f.service.stop("acme.snip");
    assert!(f.service.snippet_offers(&f.app, 0).is_empty());
}

#[test]
fn a_package_theme_is_read_completely_offered_by_id_and_withdrawn_with_its_package() {
    let _turn = crate::features::settings::theme_id::REGISTRY_TURN
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("loom-ext-theme-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("themes")).unwrap();
    std::fs::write(
        dir.join("themes/dark.json"),
        r##"{ "name": "Acme Night", "colors": { "bg": "#101418" }, "syntax": { "keyword": "#ff8800" } }"##,
    )
    .unwrap();
    std::fs::write(
        dir.join("themes/typo.json"),
        r##"{ "colors": { "bgg": "#101418" } }"##,
    )
    .unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        r#"{ "schema": 1, "id": "acme.look", "version": "1.0.0",
            "contributes": { "themes": [
                { "id": "acme.look.night", "path": "themes/dark.json" },
                { "id": "acme.look.typo", "path": "themes/typo.json" }
            ] } }"#,
    )
    .unwrap();
    let mut service = ExtensionService::new(std::env::temp_dir().join("loom-ext-cache-theme"));
    let id = service.load_package(&dir).unwrap();
    let package = service.package(&id).unwrap();
    assert_eq!(package.resources.themes, ["acme.look.night"]);
    assert!(
        package.resources.rejected[0]
            .1
            .contains("unknown color \"bgg\""),
        "{:?}",
        package.resources.rejected
    );

    let themes = service.themes();
    assert_eq!(themes.len(), 1);
    assert_eq!(
        (themes[0].0.as_str(), themes[0].1.as_str()),
        ("acme.look.night", "Acme Night")
    );
    assert_eq!(themes[0].2.bg.0, 0x101418);

    // Offered by id, drawn with its own palette, and gone with the package.
    crate::ui::theme::set_package_themes(themes);
    let chosen = crate::features::settings::theme_id::ThemeId::parse("acme.look.night").unwrap();
    assert_eq!(chosen.theme().palette.bg.0, 0x101418);
    let revision = service.themes_revision();
    service.stop("acme.look");
    assert!(service.themes_revision() > revision);
    crate::ui::theme::set_package_themes(service.themes());
    assert!(crate::features::settings::theme_id::ThemeId::parse("acme.look.night").is_none());
    assert_eq!(
        chosen.theme().id,
        "aura-dark",
        "a gone theme draws as the default"
    );
}

pub(crate) fn provider_manifest(language: &str) -> String {
    r#"{
    "schema": 1,
    "id": "example.tools",
    "version": "0.1.0",
    "api": "loom:extension@0.1.0",
    "component": "extension.wasm",
    "capabilities": ["documents.read", "workspace.edit", "languages"],
    "contributes": { "language_service": { "providers": [
        { "id": "example.tools.todo", "kind": "diagnostics", "selector": { "languages": ["LANG"] } },
        { "id": "example.tools.trim", "kind": "formatting", "selector": { "languages": ["LANG"] } },
        { "id": "example.tools.complete", "kind": "completion", "selector": { "languages": ["LANG"] } }
    ] } }
}"#
    .replace("LANG", language)
}

pub(crate) fn language_of(path: &Path) -> String {
    let mut app = AppState::new();
    app.workspace.open_path(path.to_path_buf(), String::new());
    let id = app.workspace.file_id_for_path(path).unwrap();
    app.workspace.documents[&id]
        .meta
        .lang
        .definition()
        .name
        .to_string()
}

impl Fixture {
    /// Handle executor events one at a time until \`done\` holds.
    fn pump_until(&mut self, mut done: impl FnMut(&Fixture) -> bool) {
        while !done(self) {
            let event = self.recv();
            let effects = self.service.handle_event(&mut self.app, event);
            self.apply(effects);
        }
    }

    fn provider_fixture(name: &str, text: &str) -> Fixture {
        let probe =
            std::env::temp_dir().join(format!("loom-ext-doc-{}-{name}.txt", std::process::id()));
        let manifest = provider_manifest(&language_of(&probe));
        fixture_with(name, text, 0..0, &manifest)
    }
}

#[test]
fn a_diagnostics_provider_is_woken_by_a_matching_document_and_its_findings_are_published_for_that_state()
 {
    let mut f = Fixture::provider_fixture("diag", "fine\nTODO: later\n");
    assert_eq!(
        f.service
            .package("example.tools")
            .unwrap()
            .lifecycle
            .state(),
        State::Dormant
    );
    f.observe();
    assert_eq!(
        f.service
            .package("example.tools")
            .unwrap()
            .lifecycle
            .state(),
        State::Activating,
        "the document's language is the declared reason to wake it"
    );
    f.pump_until(|f| !f.published.is_empty());
    let (source, items) = &f.published[0];
    assert!(source & crate::features::providers::EXTERNAL_SOURCE != 0);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].range, 5..9);
    assert_eq!(items[0].source.as_deref(), Some("example.tools.todo"));
}

#[test]
fn an_answer_for_an_older_state_is_never_shown_and_the_new_state_is_asked_again() {
    let mut f = Fixture::provider_fixture("stale-diag", "TODO one\n");
    f.observe();
    // The person edits before the first answer is handled.
    let id = f.app.workspace.file_id_for_path(&f.path).unwrap();
    f.app
        .workspace
        .document_editor_mut(id)
        .unwrap()
        .set_cursor(0);
    loom_editor_core::normal::insert(&mut f.app.workspace.document_editor_mut(id).unwrap(), "x");
    f.pump_until(|f| !f.published.is_empty());
    // Whatever was published describes the edited text ("xTODO one"), never the old one.
    assert!(
        f.published
            .iter()
            .all(|(_, items)| items.iter().all(|d| d.range == (1..5))),
        "{:?}",
        f.published
    );
}

#[test]
fn only_the_latest_diagnostics_request_per_document_stays_in_flight() {
    let mut f = Fixture::provider_fixture("latest", "TODO\n");
    f.observe();
    let first: Vec<_> = f.service.provider_calls.keys().copied().collect();
    let id = f.app.workspace.file_id_for_path(&f.path).unwrap();
    f.app
        .workspace
        .document_editor_mut(id)
        .unwrap()
        .set_cursor(0);
    loom_editor_core::normal::insert(&mut f.app.workspace.document_editor_mut(id).unwrap(), "x");
    f.observe();
    let calls: Vec<_> = f.service.provider_calls.keys().copied().collect();
    assert_eq!(calls.len(), 1, "{first:?} -> {calls:?}");
    assert_ne!(calls, first);
}

#[test]
fn a_formatting_provider_edits_the_document_as_one_undo_step() {
    let mut f = Fixture::provider_fixture("format", "a  \nb\r\nc \t\n");
    let effects = f
        .service
        .request_format(&f.app, false)
        .expect("a provider is registered");
    f.apply(effects);
    f.pump_until(|f| f.text() != "a  \nb\r\nc \t\n");
    assert_eq!(f.text(), "a\nb\r\nc\n");
    let id = f.app.workspace.file_id_for_path(&f.path).unwrap();
    f.app.workspace.document_editor_mut(id).unwrap().undo();
    assert_eq!(f.text(), "a  \nb\r\nc \t\n");
}

#[test]
fn range_formatting_leaves_the_rest_of_the_document_alone() {
    let mut f = Fixture::provider_fixture("range", "a  \nb  \nc  \n");
    {
        let mut editor = f.app.workspace.active_editor_mut().unwrap();
        editor.set_selection(crate::model::text::Selection {
            anchor: Some(4),
            cursor: 8,
            ..crate::model::text::Selection::caret(8)
        });
    }
    let effects = f.service.request_format(&f.app, true).unwrap();
    f.apply(effects);
    f.pump_until(|f| f.text() != "a  \nb  \nc  \n");
    assert_eq!(f.text(), "a  \nb\nc  \n");
}

#[test]
fn a_language_without_a_provider_is_left_to_the_language_server() {
    let mut f = fixture_with("other", "x  ", 0..0, &provider_manifest("no-such-language"));
    assert!(f.service.request_format(&f.app, false).is_none());
    f.observe();
    assert_eq!(
        f.service
            .package("example.tools")
            .unwrap()
            .lifecycle
            .state(),
        State::Dormant
    );
}

#[test]
fn stopping_or_failing_an_extension_withdraws_what_it_published() {
    let mut f = Fixture::provider_fixture("withdraw", "TODO\n");
    f.observe();
    f.pump_until(|f| !f.published.is_empty());
    let source = f.published[0].0;
    let effects = f.service.stop("example.tools");
    f.apply(effects);
    assert_eq!(f.cleared, [source]);
}

#[test]
fn a_completion_provider_answers_with_items_that_carry_their_origin_and_resolve_back_to_it() {
    let mut f = Fixture::provider_fixture("complete", "lo");
    f.service.set_clock(|| 0); // cold-start time is not what this test measures
    let (asked, effects) = f.service.request_completion(&f.app, 0, 2, None);
    assert_eq!(asked, 1);
    f.apply(effects);
    f.pump_until(|f| !f.offered.is_empty());
    let (source, items) = f.offered[0].clone();
    assert!(source & crate::features::providers::EXTERNAL_SOURCE != 0);
    let (items, incomplete) = items.expect("an answer in time");
    assert!(!incomplete);
    let labels: Vec<_> = items.iter().map(|i| i["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["loom_hello", "loom_world"]);
    assert_eq!(items[1]["x-loom"]["extension"], "example.tools");
    assert_eq!(items[1]["x-loom"]["data"], "world");

    // The item goes back to the extension that offered it, with its own data.
    let effects = f
        .service
        .request_completion_resolve(&f.app, source, &items[1]);
    f.apply(effects);
    f.pump_until(|f| !f.resolved.is_empty());
    assert_eq!(f.resolved[0].1["documentation"], "docs for world");
}

#[test]
fn language_providers_answer_with_the_replies_a_server_would_give_and_only_for_the_asked_state() {
    use crate::features::language_service::request::Feature;
    let probe =
        std::env::temp_dir().join(format!("loom-ext-doc-{}-language.txt", std::process::id()));
    let language = language_of(&probe);
    let kinds = [
        ("hover", "hover"),
        ("definition", "definition"),
        ("document-symbols", "outline"),
        ("signature-help", "signature"),
        ("inlay-hints", "hints"),
        ("code-actions", "actions"),
    ];
    let entries: String = kinds
        .iter()
        .map(|(kind, name)| {
            format!(
                "{{ \"id\": \"example.tools.{name}\", \"kind\": \"{kind}\", \"selector\": {{ \"languages\": [\"{language}\"] }} }},"
            )
        })
        .collect();
    let manifest = provider_manifest(&language)
        .replace("\"providers\": [", &format!("\"providers\": [{entries}"));
    let mut f = fixture_with("language", "hello world", 0..0, &manifest);
    f.service.set_clock(|| 0); // cold-start time is not what this test measures
    let file = f.app.workspace.file_id_for_path(&f.path).unwrap();
    let now = f.app.workspace.documents[&file].stamp(file);
    let stamp = crate::features::language_service::sync::Stamp {
        incarnation: now.incarnation,
        version: now.version,
    };
    let answer_to = |f: &mut Fixture, token: u64, feature, start, end| {
        let mut effects = Vec::new();
        let asked = f.service.request_language(
            &f.app,
            &LanguageAsk {
                stamp,
                path: f.path.clone(),
                feature,
                start,
                end,
                extra: serde_json::Value::Null,
                target: None,
                server: None,
                timeout: None,
            },
            token,
            &mut effects,
        );
        assert_eq!(asked, 1, "{feature:?}");
        f.apply(effects);
        f.pump_until(|f| f.language.iter().any(|(t, _)| *t == token));
        let (_, answer) = f.language.iter().find(|(t, _)| *t == token).unwrap();
        let answer = answer.as_ref().expect("an answer in time");
        assert!(answer.source & crate::features::providers::EXTERNAL_SOURCE != 0);
        answer.value.clone()
    };

    let hover = answer_to(&mut f, 1, Feature::Hover, 6, 6);
    assert_eq!(hover["contents"]["value"], "hover at 6 of 11 bytes");
    assert_eq!(hover["range"]["start"]["character"], 6);

    let places = answer_to(&mut f, 2, Feature::Definition, 0, 0);
    assert_eq!(places[0]["range"]["start"]["line"], 2);
    assert_eq!(places[0]["range"]["start"]["character"], 3);

    let outline = answer_to(&mut f, 3, Feature::DocumentSymbol, 0, 0);
    assert_eq!(outline[0]["name"], "outer");
    assert_eq!(outline[0]["children"][0]["name"], "inner");

    let signature = answer_to(&mut f, 4, Feature::SignatureHelp, 3, 3);
    assert_eq!(signature["activeParameter"], 1);
    assert_eq!(
        signature["signatures"][0]["parameters"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let hints = answer_to(&mut f, 5, Feature::InlayHint, 0, 11);
    assert_eq!(hints[0]["label"], "hint:");
    assert_eq!(hints[0]["kind"], 2);

    let actions = answer_to(&mut f, 6, Feature::CodeAction, 0, 5);
    assert_eq!(actions[0]["title"], "Mark the selection");
    let uri = crate::features::language_service::uri::file_uri(&f.path).unwrap();
    assert_eq!(
        actions[0]["edit"]["changes"][&uri][0]["newText"],
        "[marked]"
    );
    assert_eq!(
        actions[0]["edit"]["changes"][&uri][0]["range"]["end"]["character"],
        5
    );

    // A request for a state the document has left is not asked at all.
    let mut stale = LanguageAsk {
        feature: Feature::Hover,
        path: f.path.clone(),
        stamp,
        start: 0,
        end: 0,
        extra: serde_json::Value::Null,
        target: None,
        server: None,
        timeout: None,
    };
    stale.stamp.version += 1;
    let mut effects = Vec::new();
    assert_eq!(
        f.service.request_language(&f.app, &stale, 9, &mut effects),
        0
    );
    assert!(effects.is_empty());
    // And a feature no provider declares has nobody to ask.
    let providers = f.service.language_providers(Feature::References, &language);
    assert!(providers.is_empty());
}

#[test]
fn one_extension_cannot_hold_more_requests_open_than_the_cap_allows() {
    use crate::features::language_service::request::Feature;
    let probe = std::env::temp_dir().join(format!("loom-ext-doc-{}-cap.txt", std::process::id()));
    let language = language_of(&probe);
    let manifest = provider_manifest(&language).replace(
        "\"providers\": [",
        &format!(
            "\"providers\": [{{ \"id\": \"example.tools.hover\", \"kind\": \"hover\", \"selector\": {{ \"languages\": [\"{language}\"] }} }},"
        ),
    );
    let mut f = fixture_with("cap", "x", 0..0, &manifest);
    f.service.set_clock(|| 0);
    let mut paths = Vec::new();
    for n in 0..40 {
        let path =
            std::env::temp_dir().join(format!("loom-ext-doc-{}-cap-{n}.txt", std::process::id()));
        f.app.workspace.open_path(path.clone(), "x".into());
        paths.push(path);
    }
    let mut asked = 0;
    for path in &paths {
        let file = f.app.workspace.file_id_for_path(path).unwrap();
        let now = f.app.workspace.documents[&file].stamp(file);
        let ask = LanguageAsk {
            feature: Feature::Hover,
            path: path.clone(),
            stamp: crate::features::language_service::sync::Stamp {
                incarnation: now.incarnation,
                version: now.version,
            },
            start: 0,
            end: 0,
            extra: serde_json::Value::Null,
            target: None,
            server: None,
            timeout: None,
        };
        let mut effects = Vec::new();
        asked += f.service.request_language(&f.app, &ask, 1, &mut effects);
        // Nothing is submitted: the answers are not what this test is about.
    }
    assert_eq!(asked, 32, "the rest were refused, not queued");
}

#[test]
fn an_item_filed_under_another_source_is_never_sent_for_resolving() {
    let mut f = Fixture::provider_fixture("forged", "lo");
    f.service.set_clock(|| 0); // cold-start time is not what this test measures
    let (_, effects) = f.service.request_completion(&f.app, 0, 2, None);
    f.apply(effects);
    f.pump_until(|f| !f.offered.is_empty());
    let (source, items) = f.offered[0].clone();
    let item = items.unwrap().0[0].clone();
    assert!(
        f.service
            .request_completion_resolve(&f.app, source ^ 0x40, &item)
            .is_empty()
    );
    let mut forged = item.clone();
    forged["x-loom"]["extension"] = serde_json::json!("other.ext");
    assert!(
        f.service
            .request_completion_resolve(&f.app, source, &forged)
            .is_empty()
    );
}

#[test]
fn a_completion_answer_that_misses_its_deadline_is_reported_as_no_items() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    let mut f = Fixture::provider_fixture("slow", "lo");
    let now = Arc::new(AtomicU64::new(0));
    let clock = now.clone();
    f.service.set_clock(move || clock.load(Ordering::Relaxed));
    let (_, effects) = f.service.request_completion(&f.app, 0, 2, None);
    f.apply(effects);
    now.store(60_000, Ordering::Relaxed); // the cold start took far too long
    f.pump_until(|f| !f.offered.is_empty());
    assert!(
        f.offered[0].1.is_none(),
        "the session is told to stop waiting"
    );
}

#[test]
fn languages_without_a_completion_provider_ask_nobody() {
    let mut f = fixture_with("nobody", "lo", 0..0, &provider_manifest("no-such-language"));
    let (asked, effects) = f.service.request_completion(&f.app, 0, 2, None);
    assert_eq!(asked, 0);
    assert!(effects.is_empty());
}

const TASK_MANIFEST: &str = r#"{
    "schema": 1,
    "id": "example.tools",
    "version": "0.1.0",
    "api": "loom:extension@0.1.0",
    "component": "extension.wasm",
    "activation": [
        { "kind": "command", "id": "example.tools.run" },
        { "kind": "command", "id": "example.tools.check" },
        { "kind": "command", "id": "example.tools.cancel" },
        { "kind": "event", "name": "document.open" }
    ],
    "capabilities": ["documents.read", "workspace.edit", "process.execute-local", "events"],
    "contributes": {
        "commands": [
            { "id": "example.tools.run", "title": "Run" },
            { "id": "example.tools.check", "title": "Check" },
            { "id": "example.tools.cancel", "title": "Cancel" }
        ],
        "settings": [{ "key": "example.tools.task", "kind": "string", "default": "" }]
    }
}"#;

fn task_json(script_windows: &str, script_unix: &str) -> String {
    let (program, flag, script) = if cfg!(windows) {
        ("cmd", "/c", script_windows)
    } else {
        ("sh", "-c", script_unix)
    };
    serde_json::json!({ "program": program, "args": [flag, script] }).to_string()
}

impl Fixture {
    /// A fixture with a workspace folder, the task setting and (optionally) trust.
    fn tasks(name: &str, task: String, trusted: bool) -> (Fixture, PathBuf) {
        Self::tasks_with(TASK_MANIFEST, name, task, trusted)
    }

    fn tasks_with(manifest: &str, name: &str, task: String, trusted: bool) -> (Fixture, PathBuf) {
        let mut f = fixture_with(name, "TODO text", 0..0, manifest);
        let folder =
            std::env::temp_dir().join(format!("loom-task-root-{}-{name}", std::process::id()));
        std::fs::create_dir_all(folder.join("sub")).unwrap();
        f.app.explorer.model.add_root(
            &crate::infrastructure::explorer_filesystem::ExplorerFilesystem,
            folder.clone(),
        );
        let store = std::sync::Arc::new(
            crate::infrastructure::extensions::trust_store::TrustStore::default(),
        );
        if trusted {
            store.trust(&folder).unwrap();
        }
        f.service.set_trust(store);
        let effects = f.service.set_setting_sources(
            serde_json::json!({ "example.tools.task": task }),
            None,
            false,
        );
        f.apply(effects);
        (f, folder)
    }

    /// Activate the extension and let it learn the folders.
    fn start_with_roots(&mut self) {
        let effects = self.service.observe(&self.app);
        self.apply(effects);
        self.pump_until(|f| {
            f.service
                .package("example.tools")
                .unwrap()
                .lifecycle
                .state()
                == State::Active
        });
    }
}

#[test]
fn retired_server_declarations_cannot_register_a_host_process() {
    let manifest = TASK_MANIFEST.replace(
        "\"settings\": [",
        "\"language_service\": { \"servers\": [{ \"id\": \"example.tools.lsp\", \"languages\": [\"rust\"], \
         \"command\": \"tools/lsp\", \"root_markers\": [\"Cargo.toml\"] }] }, \"settings\": [",
    );
    assert!(crate::features::extensions::manifest::Manifest::parse(&manifest).is_err());
}

#[test]
fn without_trust_a_task_is_refused_before_anything_starts() {
    let (mut f, _) = Fixture::tasks("denied-untrusted", task_json("echo hi", "echo hi"), false);
    f.start_with_roots();
    f.run("example.tools.run");
    f.wait_for_log("task exited 1");
    let logs: Vec<_> = f
        .service
        .package("example.tools")
        .unwrap()
        .logs
        .iter()
        .map(|l| l.1.clone())
        .collect();
    assert!(logs.iter().any(|l| l.contains("not trusted")), "{logs:?}");
    assert_eq!(f.runner.running(), 0);
}

#[test]
fn a_persistent_process_round_trips_binary_frames_through_the_real_guest() {
    let manifest = TASK_MANIFEST.replace("example.tools.run", "example.tools.stream");
    for framed in [false, true] {
        let spec = serde_json::json!({"program":"node", "args":["-e", "process.stdin.pipe(process.stdout)"], "framed":framed}).to_string();
        let (mut f, _) = Fixture::tasks_with(
            &manifest,
            if framed {
                "framed-process"
            } else {
                "raw-process"
            },
            spec,
            true,
        );
        f.start_with_roots();
        f.run("example.tools.stream");
        f.wait_for_log("process exited 1 code Some(0)");
        let logs = &f.service.package("example.tools").unwrap().logs;
        assert!(
            logs.iter()
                .any(|(_, text)| text.contains("process output 1 [0, 255, 1]")),
            "{logs:?}"
        );
        assert!(
            logs.iter()
                .any(|(_, text)| text.contains("process writable 1 3"))
        );
        assert_eq!(
            logs.iter()
                .filter(|(_, text)| text.contains("process exited 1"))
                .count(),
            1
        );
        assert!(f.service.processes.is_empty());
    }
}

#[test]
fn a_guest_exchanges_length_framed_messages_with_the_fake_language_server() {
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join(if cfg!(windows) {
            "fake_lsp.exe"
        } else {
            "fake_lsp"
        });
    assert!(
        binary.exists(),
        "build the fixture with cargo build --example fake_lsp"
    );
    let input = vec![
        serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{"capabilities":{}}}).to_string(),
        serde_json::json!({"jsonrpc":"2.0", "id":2, "method":"shutdown", "params":null}).to_string(),
        serde_json::json!({"jsonrpc":"2.0", "method":"exit", "params":null}).to_string(),
    ];
    let spec =
        serde_json::json!({"program":binary, "args":["--chunk","3"], "framed":true, "input":input})
            .to_string();
    let manifest = TASK_MANIFEST.replace("example.tools.run", "example.tools.stream");
    let (mut f, _) = Fixture::tasks_with(&manifest, "fake-process", spec, true);
    f.start_with_roots();
    f.run("example.tools.stream");
    f.wait_for_log("process exited 1 code Some(0)");
    let logs = &f.service.package("example.tools").unwrap().logs;
    for id in [1, 2] {
        assert!(
            logs.iter()
                .any(|(_, text)| text == &format!("process message {id}")),
            "{logs:?}"
        );
    }
}

#[test]
fn persistent_processes_share_trust_and_are_reclaimed_on_revoke_and_root_removal() {
    let manifest = TASK_MANIFEST.replace("example.tools.run", "example.tools.stream");
    let spec = serde_json::json!({"program":"node", "args":["-e", "process.stdin.resume();setInterval(()=>{},1000)"], "persistent":true}).to_string();
    let (mut denied, _) = Fixture::tasks_with(&manifest, "process-denied", spec.clone(), false);
    denied.start_with_roots();
    denied.run("example.tools.stream");
    denied.wait_for_log("process exited 1");
    assert!(denied.service.processes.is_empty());
    assert!(
        denied
            .service
            .package("example.tools")
            .unwrap()
            .logs
            .iter()
            .any(|(_, text)| text.contains("trust"))
    );
    for revoke in [false, true] {
        let (mut f, folder) = Fixture::tasks_with(
            &manifest,
            if revoke {
                "process-revoke"
            } else {
                "process-remove"
            },
            spec.clone(),
            true,
        );
        f.start_with_roots();
        f.run("example.tools.stream");
        f.pump_until(|f| !f.service.processes.is_empty());
        let effects = if revoke {
            f.service.stop_tools_in(&folder)
        } else {
            f.app.explorer.model.remove_root(&folder);
            f.service.observe(&f.app)
        };
        f.apply(effects);
        f.wait_for_log("killed true");
        assert!(f.service.processes.is_empty());
        if !revoke {
            assert!(
                f.service
                    .package("example.tools")
                    .unwrap()
                    .logs
                    .iter()
                    .any(|(_, text)| text == "roots changed 0")
            );
        }
    }
}

#[test]
fn a_task_cannot_enter_an_untrusted_nested_workspace_folder() {
    let (mut f, folder) = Fixture::tasks("nested-cwd", task_json("echo x", "echo x"), true);
    let nested = folder.join("sub");
    f.app.explorer.model.add_root(
        &crate::infrastructure::explorer_filesystem::ExplorerFilesystem,
        nested.clone(),
    );
    f.service
        .trust
        .as_ref()
        .unwrap()
        .set_workspace(&[folder.clone(), nested.clone()]);
    let effects = f.service.sync_roots(&f.app);
    f.apply(effects);
    let root = f
        .service
        .roots
        .iter()
        .find(|(_, path)| path == &folder)
        .unwrap()
        .0;
    let request = loom_extension_host::TaskRequest {
        call: 0,
        root,
        program: "node".into(),
        args: vec![],
        cwd: Some("sub".into()),
        env: vec![],
        timeout_ms: None,
    };
    assert!(
        f.service
            .plan_task("example.tools", &request)
            .unwrap_err()
            .message
            .contains("untrusted")
    );
    f.service.trust.as_ref().unwrap().trust(&nested).unwrap();
    assert!(f.service.plan_task("example.tools", &request).is_ok());
}

#[test]
fn an_allowed_task_streams_output_and_reports_its_exit_once() {
    let (mut f, _) = Fixture::tasks("allowed", task_json("echo hello", "echo hello"), true);
    f.start_with_roots();
    f.run("example.tools.run");
    f.wait_for_log("task exited 1 code Some(0)");
    let logs: Vec<_> = f
        .service
        .package("example.tools")
        .unwrap()
        .logs
        .iter()
        .map(|l| l.1.clone())
        .collect();
    assert!(
        logs.iter().any(|l| l.contains("task started 1")),
        "{logs:?}"
    );
    assert!(
        logs.iter().any(|l| l.starts_with("task output 1: hello")),
        "{logs:?}"
    );
    assert_eq!(
        logs.iter().filter(|l| l.starts_with("task exited")).count(),
        1
    );
}

#[test]
fn a_working_directory_cannot_leave_the_folder() {
    let task = serde_json::json!({
        "program": if cfg!(windows) { "cmd" } else { "sh" },
        "args": [if cfg!(windows) { "/c" } else { "-c" }, "echo hi"],
        "cwd": "../..",
    })
    .to_string();
    let (mut f, _) = Fixture::tasks("escape", task, true);
    f.start_with_roots();
    f.run("example.tools.run");
    f.wait_for_log("task exited 1");
    let logs: Vec<_> = f
        .service
        .package("example.tools")
        .unwrap()
        .logs
        .iter()
        .map(|l| l.1.clone())
        .collect();
    assert!(
        logs.iter().any(|l| l.contains("inside the folder")),
        "{logs:?}"
    );
}

#[test]
fn the_guest_can_cancel_its_task_and_still_hears_one_exit() {
    let task = task_json("ping -n 30 127.0.0.1 >nul", "sleep 30");
    let (mut f, _) = Fixture::tasks("cancel", task, true);
    f.start_with_roots();
    f.run("example.tools.run");
    f.pump_until(|f| !f.service.tasks.is_empty());
    f.run("example.tools.cancel");
    f.wait_for_log("cancelled true");
}

#[test]
fn taking_trust_away_or_removing_a_folder_stops_what_runs_there() {
    let task = task_json("ping -n 30 127.0.0.1 >nul", "sleep 30");
    let (mut f, folder) = Fixture::tasks("revoke", task.clone(), true);
    f.start_with_roots();
    f.run("example.tools.run");
    f.pump_until(|f| !f.service.tasks.is_empty());
    let effects = f.service.stop_tools_in(&folder);
    assert_eq!(effects.len(), 1);
    f.apply(effects);
    f.wait_for_log("cancelled true");

    let (mut g, folder) = Fixture::tasks("removed", task, true);
    g.start_with_roots();
    g.run("example.tools.run");
    g.pump_until(|g| !g.service.tasks.is_empty());
    g.app.explorer.model.remove_root(&folder);
    g.observe();
    g.wait_for_log("cancelled true");
}

#[test]
fn a_checker_publishes_findings_for_the_state_it_checked_and_not_for_a_changed_one() {
    let (mut f, _) = Fixture::tasks("check", task_json("echo ok", "echo ok"), true);
    f.start_with_roots();
    f.wait_for_log("opened 1");
    f.run("example.tools.check");
    f.pump_until(|f| !f.published.is_empty());
    let (source, items) = &f.published[0];
    assert!(source & crate::features::providers::EXTERNAL_SOURCE != 0);
    assert_eq!(items[0].range, 0..4);
    assert_eq!(items[0].source.as_deref(), Some("example.tools.checker"));

    // The same check, but the text changes before the tool finishes.
    f.published.clear();
    let slow = task_json("ping -n 2 127.0.0.1 >nul", "sleep 1");
    let effects = f.service.set_setting_sources(
        serde_json::json!({ "example.tools.task": slow }),
        None,
        false,
    );
    f.apply(effects);
    f.run("example.tools.check");
    f.wait_for_log("task started 2");
    let id = f.app.workspace.file_id_for_path(&f.path).unwrap();
    f.app
        .workspace
        .document_editor_mut(id)
        .unwrap()
        .set_cursor(0);
    loom_editor_core::normal::insert(&mut f.app.workspace.document_editor_mut(id).unwrap(), "x");
    f.wait_for_log("task exited 2 code Some(0)");
    assert!(f.published.is_empty(), "{:?}", f.published);
}

fn op_manifest(capabilities: &str) -> String {
    op_manifest_with(capabilities, "")
}

fn op_manifest_with(capabilities: &str, extra: &str) -> String {
    r#"{
    EXTRA
    "schema": 1,
    "id": "example.tools",
    "version": "0.1.0",
    "api": "loom:extension@0.1.0",
    "component": "extension.wasm",
    "activation": [{ "kind": "command", "id": "example.tools.op" }],
    "capabilities": CAPS,
    "contributes": {
        "commands": [{ "id": "example.tools.op", "title": "Op" }],
        "settings": [{ "key": "example.tools.spec", "kind": "string", "default": "" }]
    }
}"#
    .replace("CAPS", capabilities)
    .replace("EXTRA", extra)
}

impl Fixture {
    /// A fixture with a workspace folder, its own storage directory and the given capabilities.
    fn ops(name: &str, capabilities: &str) -> (Fixture, PathBuf) {
        let mut f = fixture_with(name, "text", 0..0, &op_manifest(capabilities));
        let base = std::env::temp_dir().join(format!("loom-ops-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let folder = base.join("project");
        std::fs::create_dir_all(folder.join("src")).unwrap();
        std::fs::write(folder.join("src/lib.rs"), "fn main() {}\n").unwrap();
        std::fs::write(folder.join("notes.txt"), "0123456789").unwrap();
        std::fs::write(folder.join("blob.bin"), [0u8, 1, 2]).unwrap();
        f.app.explorer.model.add_root(
            &crate::infrastructure::explorer_filesystem::ExplorerFilesystem,
            folder.clone(),
        );
        f.service.set_storage_dir(base.join("storage"));
        (f, folder)
    }

    /// Run one operation in the guest and return what it reported.
    fn op(&mut self, spec: serde_json::Value) -> String {
        let before = self.op_results().len();
        let effects = self.service.set_setting_sources(
            serde_json::json!({ "example.tools.spec": spec.to_string() }),
            None,
            false,
        );
        self.apply(effects);
        // Settings reach the guest before the command, and a running guest answers in order.
        let observed = self.service.observe(&self.app);
        self.apply(observed);
        self.run("example.tools.op");
        self.pump_until(|f| f.op_results().len() > before);
        self.op_results().pop().unwrap()
    }

    fn op_results(&self) -> Vec<String> {
        self.service
            .package("example.tools")
            .unwrap()
            .logs
            .iter()
            .filter_map(|(_, l)| l.strip_prefix("op result: ").map(str::to_owned))
            .collect()
    }
}

#[test]
fn stored_values_live_in_their_scope_survive_a_restart_and_follow_the_set_of_folders() {
    let (mut f, _) = Fixture::ops(
        "store",
        r#"["documents.read", "workspace.edit", "storage"]"#,
    );
    assert_eq!(
        f.op(serde_json::json!({"op":"set","scope":"user","key":"a","value":"1"})),
        "stored"
    );
    assert_eq!(
        f.op(serde_json::json!({"op":"set","scope":"root","key":"a","value":"2"})),
        "stored"
    );
    assert_eq!(
        f.op(serde_json::json!({"op":"set","scope":"workspace","key":"a","value":"3"})),
        "stored"
    );
    assert_eq!(
        f.op(serde_json::json!({"op":"get","scope":"user","key":"a"})),
        "got Some(\"1\")"
    );
    assert_eq!(
        f.op(serde_json::json!({"op":"get","scope":"root","key":"a"})),
        "got Some(\"2\")"
    );
    assert_eq!(
        f.op(serde_json::json!({"op":"get","scope":"workspace","key":"a"})),
        "got Some(\"3\")"
    );

    // A second folder changes the workspace's identity; the old value is not visible.
    let other = f.service.storage_dir.parent().unwrap().join("other");
    std::fs::create_dir_all(&other).unwrap();
    f.app.explorer.model.add_root(
        &crate::infrastructure::explorer_filesystem::ExplorerFilesystem,
        other,
    );
    assert_eq!(
        f.op(serde_json::json!({"op":"get","scope":"workspace","key":"a"})),
        "got None"
    );
    assert_eq!(
        f.op(serde_json::json!({"op":"get","scope":"user","key":"a"})),
        "got Some(\"1\")"
    );

    assert_eq!(
        f.op(serde_json::json!({"op":"delete","scope":"user","key":"a"})),
        "deleted"
    );
    assert_eq!(
        f.op(serde_json::json!({"op":"get","scope":"user","key":"a"})),
        "got None"
    );

    // A fresh extension instance (a restart) of the same folder reads what was stored on disk.
    let saved = f
        .service
        .storage_dir
        .parent()
        .unwrap()
        .with_file_name(format!("saved-storage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&saved);
    copy_dir(&f.service.storage_dir, &saved);
    drop(f);
    let (mut g, _) = Fixture::ops(
        "store",
        r#"["documents.read", "workspace.edit", "storage"]"#,
    );
    g.service.set_storage_dir(saved);
    assert_eq!(
        g.op(serde_json::json!({"op":"get","scope":"root","key":"a"})),
        "got Some(\"2\")"
    );
}

#[test]
fn storage_is_bounded_and_needs_its_capability() {
    let (mut f, _) = Fixture::ops(
        "quota",
        r#"["documents.read", "workspace.edit", "storage"]"#,
    );
    let big = "x".repeat(65 * 1024);
    let result = f.op(serde_json::json!({"op":"set","scope":"user","key":"big","value":big}));
    assert!(result.contains("too large"), "{result}");
    let result =
        f.op(serde_json::json!({"op":"set","scope":"user","key":"k".repeat(300),"value":"v"}));
    assert!(result.contains("too long"), "{result}");

    let (mut g, _) = Fixture::ops("nostorage", r#"["documents.read", "workspace.edit"]"#);
    let result = g.op(serde_json::json!({"op":"set","scope":"user","key":"a","value":"1"}));
    assert!(result.contains("not granted"), "{result}");
}

#[test]
fn files_are_read_and_found_inside_the_folder_only() {
    let (mut f, _) = Fixture::ops(
        "files",
        r#"["documents.read", "workspace.edit", "workspace.files.read"]"#,
    );
    assert_eq!(
        f.op(serde_json::json!({"op":"read","path":"notes.txt","max":4})),
        "read \"0123\" truncated true"
    );
    assert_eq!(
        f.op(serde_json::json!({"op":"read","path":"src/lib.rs"})),
        "read \"fn main() {}\\n\" truncated false"
    );
    for path in ["../project/notes.txt", "/etc/hosts", "..\\x"] {
        let result = f.op(serde_json::json!({"op":"read","path":path}));
        assert!(result.contains("inside the folder"), "{path}: {result}");
    }
    assert!(
        f.op(serde_json::json!({"op":"read","path":"blob.bin"}))
            .contains("not a text file")
    );
    assert_eq!(
        f.op(serde_json::json!({"op":"find","pattern":"**/*.rs"})),
        "found src/lib.rs"
    );

    let (mut g, _) = Fixture::ops("nofiles", r#"["documents.read", "workspace.edit"]"#);
    let result = g.op(serde_json::json!({"op":"read","path":"notes.txt"}));
    assert!(result.contains("not granted"), "{result}");
}

/// Copy a directory tree (test helper).
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn serve_hello() -> u16 {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut buffer = [0u8; 2048];
            let _ = stream.read(&mut buffer);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\nconnection: close\r\n\r\nhello",
            );
        }
    });
    port
}

#[test]
fn network_reaches_only_the_declared_and_granted_hosts() {
    let port = serve_hello();
    let manifest = op_manifest_with(
        r#"["documents.read", "workspace.edit", "network"]"#,
        r#""network": { "domains": ["127.0.0.1"] },"#,
    );
    let mut f = fixture_with("net", "text", 0..0, &manifest);
    let result =
        f.op(serde_json::json!({"op":"http","url":format!("http://127.0.0.1:{port}/x"),"max":3}));
    assert_eq!(result, "http 200 \"hel\" truncated true");
    let result = f.op(serde_json::json!({"op":"http","url":format!("http://localhost:{port}/x")}));
    assert!(result.contains("not one of the hosts"), "{result}");

    // Without the capability the declared hosts are not reachable either.
    let manifest = op_manifest_with(r#"["documents.read", "workspace.edit"]"#, "");
    let mut g = fixture_with("nonet", "text", 0..0, &manifest);
    let result = g.op(serde_json::json!({"op":"http","url":format!("http://127.0.0.1:{port}/x")}));
    assert!(result.contains("not granted"), "{result}");
}

#[test]
fn a_network_declaration_needs_the_capability_and_plain_host_names() {
    let with = |caps: &str, domains: &str| {
        Manifest::parse(&op_manifest_with(
            caps,
            &format!(r#""network": {{ "domains": {domains} }},"#),
        ))
    };
    assert!(
        with(
            r#"["network"]"#,
            r#"["api.example.com", "*.cdn.example.com"]"#
        )
        .is_ok()
    );
    for bad in [
        r#"[]"#,
        r#"["https://api.example.com"]"#,
        r#"["api.example.com:8080"]"#,
        r#"["*"]"#,
        r#"["Api.Example.com"]"#,
        r#"["a/b"]"#,
    ] {
        assert!(with(r#"["network"]"#, bad).is_err(), "{bad}");
    }
    assert!(
        with(r#"["documents.read"]"#, r#"["api.example.com"]"#).is_err(),
        "declared without the capability"
    );
}

fn save_manifest(language: &str) -> String {
    r#"{
    "schema": 1,
    "id": "example.tools",
    "version": "0.1.0",
    "api": "loom:extension@0.1.0",
    "component": "extension.wasm",
    "capabilities": ["documents.read", "workspace.edit"],
    "contributes": { "language_service": { "providers": [
        { "id": "example.tools.trim", "kind": "save-participant", "selector": { "languages": ["LANG"] } }
    ] } }
}"#
    .replace("LANG", language)
}

impl Fixture {
    fn saver(name: &str, text: &str) -> Fixture {
        let probe =
            std::env::temp_dir().join(format!("loom-ext-doc-{}-{name}.txt", std::process::id()));
        let mut f = fixture_with(name, text, 0..0, &save_manifest(&language_of(&probe)));
        f.service.set_clock(|| 0); // cold start time is not what these tests measure
        f
    }

    fn begin_save(&mut self) -> u64 {
        let file = self.app.workspace.file_id_for_path(&self.path).unwrap();
        let (chain, effects) = self.service.begin_save(&mut self.app, vec![file]);
        self.apply(effects);
        chain
    }
}

#[test]
fn participants_change_the_text_in_one_undoable_step_after_all_of_them_answered() {
    let mut f = Fixture::saver("chain", "a  \nb  \nkeep\n");
    let file = f.app.workspace.file_id_for_path(&f.path).unwrap();
    assert!(f.service.has_participants(&f.app, &[file]));
    let chain = f.begin_save();
    assert_eq!(
        f.text(),
        "a  \nb  \nkeep\n",
        "nothing changes before the participants answered"
    );
    f.pump_until(|f| !f.ready.is_empty());
    assert_eq!(f.ready, [(chain, true, None)]);
    assert_eq!(f.text(), "a\nb\nkeep\n");
    f.app.workspace.document_editor_mut(file).unwrap().undo();
    assert_eq!(f.text(), "a  \nb  \nkeep\n", "one undo step");
}

#[test]
fn a_file_with_nothing_to_change_is_ready_without_an_edit() {
    let mut f = Fixture::saver("clean", "already clean\n");
    let before = f.app.workspace.documents[&f.app.workspace.file_id_for_path(&f.path).unwrap()]
        .buffer
        .version();
    f.begin_save();
    f.pump_until(|f| !f.ready.is_empty());
    assert!(f.ready[0].1);
    let after = f.app.workspace.documents[&f.app.workspace.file_id_for_path(&f.path).unwrap()]
        .buffer
        .version();
    assert_eq!(before, after);
}

#[test]
fn a_language_without_a_participant_has_none() {
    let f = fixture_with(
        "other-lang",
        "x  ",
        0..0,
        &save_manifest("no-such-language"),
    );
    let file = f.app.workspace.file_id_for_path(&f.path).unwrap();
    assert!(!f.service.has_participants(&f.app, &[file]));
}

#[test]
fn typing_while_the_participants_work_makes_them_run_again_on_the_new_text() {
    let mut f = Fixture::saver("typing", "a  \n");
    f.begin_save();
    let id = f.app.workspace.file_id_for_path(&f.path).unwrap();
    f.app
        .workspace
        .document_editor_mut(id)
        .unwrap()
        .set_cursor(4);
    loom_editor_core::normal::insert(&mut f.app.workspace.document_editor_mut(id).unwrap(), "b  ");
    f.pump_until(|f| !f.ready.is_empty());
    assert!(f.ready[0].1, "{:?}", f.ready);
    assert_eq!(
        f.text(),
        "a\nb",
        "the person's typing is kept and also cleaned"
    );
}

#[test]
fn an_answer_after_the_deadline_discards_everything_and_releases_the_participant() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    let mut f = Fixture::saver("late", "a  \n");
    let now = Arc::new(AtomicU64::new(0));
    let clock = now.clone();
    f.service.set_clock(move || clock.load(Ordering::Relaxed));
    f.begin_save();
    now.store(60_000, Ordering::Relaxed);
    f.pump_until(|f| !f.ready.is_empty());
    assert!(!f.ready[0].1);
    assert!(
        f.ready[0].2.as_deref().unwrap().contains("too late"),
        "{:?}",
        f.ready
    );
    assert_eq!(f.text(), "a  \n", "the real document is untouched");
}

#[test]
fn a_stopped_participant_ends_the_chain_with_a_reason_and_changes_nothing() {
    let mut f = Fixture::saver("stopped", "a  \n");
    // A failed extension stays down until retried (a dormant one would simply be woken).
    f.service
        .packages
        .get_mut("example.tools")
        .unwrap()
        .lifecycle
        .fail(crate::features::extensions::lifecycle::FailureStage::Execution);
    f.begin_save();
    assert_eq!(f.ready.len(), 1);
    assert!(!f.ready[0].1);
    assert!(
        f.ready[0].2.as_deref().unwrap().contains("not running"),
        "{:?}",
        f.ready
    );
    assert_eq!(f.text(), "a  \n");
}

#[test]
fn the_timer_cuts_off_a_chain_whose_participant_never_answers() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    let mut f = Fixture::saver("timer", "a  \n");
    let now = Arc::new(AtomicU64::new(0));
    let clock = now.clone();
    f.service.set_clock(move || clock.load(Ordering::Relaxed));
    let chain = f.begin_save();
    assert!(f.service.expire_save(chain).is_empty(), "not due yet");
    now.store(10_000, Ordering::Relaxed);
    let effects = f.service.expire_save(chain);
    f.apply(effects);
    assert_eq!(
        f.ready,
        [(
            chain,
            false,
            Some("a save participant did not answer in time".into())
        )]
    );
    assert!(
        !f.submitted.is_empty(),
        "the restriction on the silent participant was lifted"
    );
    assert!(f.service.expire_save(chain).is_empty(), "a chain ends once");
}

#[test]
fn saving_runs_the_participants_before_writing_and_closing_waits_for_them() {
    use crate::app::saving;
    use crate::app::test_support::SavingUi;
    let dir = std::env::temp_dir().join(format!("loom-save-chain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("file.txt");
    std::fs::write(&path, "x  \ny  \n").unwrap();
    let mut app = AppState::new();
    let id = app.workspace.open_path(path.clone(), "x  \ny  \n".into());
    {
        // Make the document dirty so a save has something to write.
        let mut editor = app.workspace.document_editor_mut(id).unwrap();
        editor.select_to(usize::MAX, false);
        editor
            .replace_selection("x  \ny  \nz  \n", loom_editor_core::text::EditKind::Other)
            .unwrap();
    }
    let ui = SavingUi::new(app);
    let host = host::ExtensionHost::start(
        ui.state.clone(),
        ui.tasks.application(),
        std::env::temp_dir().join(format!("loom-save-chain-cache-{}", std::process::id())),
        ui.runtime.trust().clone(),
        None,
    )
    .unwrap();
    host.service().set_clock(|| 0);
    host.load_package(&package_dir_with(
        "save-live",
        &save_manifest(&language_of(&path)),
    ))
    .unwrap();
    ui.runtime.install_extensions(host);

    assert!(saving::document(&ui.state, id, |_, _| {}));
    assert!(
        ui.state.get().editor.saving.pending > 0,
        "the window cannot close while participants work"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "x  \ny  \n",
        "nothing is written yet"
    );
    ui.wait();
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(
        !written.contains("  ") && written.ends_with("z\n"),
        "{written:?}"
    );
    assert_eq!(
        ui.state.get().workspace.documents[&id].buffer.text(),
        written
    );
    assert!(!ui.state.get().workspace.is_dirty(id));

    // The way out: saving without the extensions writes the text as it is.
    ui.state.update(|app| {
        let mut editor = app.workspace.document_editor_mut(id).unwrap();
        editor.select_to(usize::MAX, false);
        editor
            .replace_selection("raw  \n", loom_editor_core::text::EditKind::Other)
            .unwrap();
    });
    assert!(saving::document_without_participants(
        &ui.state,
        id,
        |_, _| {}
    ));
    ui.wait();
    assert!(std::fs::read_to_string(&path).unwrap().contains("raw  \n"));
    ui.runtime.clear_extensions();
}

#[test]
fn a_failing_participant_keeps_the_file_from_being_written() {
    use crate::app::saving;
    use crate::app::test_support::SavingUi;
    let dir = std::env::temp_dir().join(format!("loom-save-fail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("file.txt");
    std::fs::write(&path, "old\n").unwrap();
    let mut app = AppState::new();
    let id = app.workspace.open_path(path.clone(), "old\n".into());
    {
        let mut editor = app.workspace.document_editor_mut(id).unwrap();
        editor.select_to(usize::MAX, false);
        editor
            .replace_selection("new  \n", loom_editor_core::text::EditKind::Other)
            .unwrap();
    }
    let ui = SavingUi::new(app);
    let host = host::ExtensionHost::start(
        ui.state.clone(),
        ui.tasks.application(),
        std::env::temp_dir().join(format!("loom-save-fail-cache-{}", std::process::id())),
        ui.runtime.trust().clone(),
        None,
    )
    .unwrap();
    let extension = host
        .load_package(&package_dir_with(
            "save-fail",
            &save_manifest(&language_of(&path)),
        ))
        .unwrap();
    host.service()
        .packages
        .get_mut(&extension)
        .unwrap()
        .lifecycle
        .fail(crate::features::extensions::lifecycle::FailureStage::Execution);
    ui.runtime.install_extensions(host);

    let reported = std::sync::Arc::new(std::sync::Mutex::new(None));
    let sink = reported.clone();
    assert!(saving::document(&ui.state, id, move |_, ok| *sink
        .lock()
        .unwrap() =
        Some(ok)));
    ui.wait();
    assert_eq!(*reported.lock().unwrap(), Some(false));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "old\n",
        "nothing was written"
    );
    assert!(
        ui.state.get().workspace.is_dirty(id),
        "the person's changes are still there"
    );
    ui.runtime.clear_extensions();
}

impl Fixture {
    fn file_ops(&mut self, ops: serde_json::Value) {
        let accepted = self.op(serde_json::json!({ "op": "fileops", "ops": ops }));
        assert!(accepted.starts_with("file ops accepted"), "{accepted}");
    }

    fn log_lines(&self) -> Vec<String> {
        self.service
            .package("example.tools")
            .unwrap()
            .logs
            .iter()
            .filter(|(_, text)| !text.starts_with("roots changed"))
            .map(|l| l.1.clone())
            .collect()
    }
}

const WRITER: &str = r#"["documents.read", "workspace.edit", "workspace.files.write"]"#;

/// Run after `node .tmp/loom-extensions/language-clients/stage.mjs`.
/// This uses the real executor, owner grants/trust, frame transport, WASM SDK and
/// native result conversion. Slow component compilation is excluded from warmed
/// request measurements and retained in a persistent local cache.
#[test]
#[ignore = "requires locally built official plugins and real language servers"]
fn official_language_components_reach_native_consumers_without_host_servers() {
    use crate::features::language_service::request::Feature;
    use std::time::Instant;
    let packages = repo().join(".tmp/loom-extensions/language-clients/.work/packages");
    let fixture_bytes: usize = std::env::var("LOOM_MIGRATION_BYTES")
        .ok()
        .map(|value| value.parse().unwrap())
        .unwrap_or(0);
    assert!([0, 1024 * 1024, 8 * 1024 * 1024].contains(&fixture_bytes));
    let mut exceeded = Vec::new();
    let cases = [
        (
            "rust",
            "loom.lsp.rust-analyzer",
            "src/main.rs",
            "Cargo.toml",
            "[workspace]\n[package]\nname=\"loom_native\"\nversion=\"0.1.0\"\nedition=\"2021\"\n",
            "fn add(a: i32, b: i32) -> i32 { a + b }\nfn main() { let value = add(1, 2); println!(\"{}\", value); }\n",
        ),
        (
            "go",
            "loom.lsp.gopls",
            "main.go",
            "go.mod",
            "module example.com/loomnative\n\ngo 1.26\n",
            "package main\nimport \"fmt\"\nfunc add(a int, b int) int { return a+b }\nfunc main() { value := add(1,2); fmt.Println(value) }\n",
        ),
        (
            "javascript",
            "loom.lsp.typescript-language-server",
            "main.js",
            "jsconfig.json",
            "{\"compilerOptions\":{\"checkJs\":true,\"target\":\"ES2022\"}}",
            "/** @param {number} a @param {number} b */\nfunction add(a, b) { return a+b; }\nconst value = add(1,2);\nconsole.log(value);\n",
        ),
        (
            "typescript",
            "loom.lsp.typescript-language-server",
            "main.ts",
            "tsconfig.json",
            "{\"compilerOptions\":{\"strict\":true,\"target\":\"ES2022\"}}",
            "function add(a: number, b: number): number { return a+b; }\nconst value = add(1,2);\nconsole.log(value);\n",
        ),
    ];
    for (language, extension, filename, marker, project, text) in cases {
        if std::env::var("LOOM_MIGRATION_LANGUAGE")
            .ok()
            .is_some_and(|name| name != language)
        {
            continue;
        }
        let mut content = text.to_owned();
        if fixture_bytes > 0 {
            content.push_str("/*");
            let padding = "中😀\r\n";
            content.push_str(&padding.repeat((fixture_bytes - content.len() - 2) / padding.len()));
            content.push_str(&"x".repeat(fixture_bytes - content.len() - 2));
            content.push_str("*/");
            assert_eq!(content.len(), fixture_bytes);
        }
        let text = content.as_str();
        let package = packages.join(extension);
        assert!(
            package.join("extension.wasm").is_file(),
            "build/stage {extension} first"
        );
        let root = repo()
            .join("target/language-migration/projects")
            .join(if fixture_bytes == 0 {
                language.to_owned()
            } else {
                format!("{language}-{fixture_bytes}")
            });
        let _ = std::fs::remove_file(root.join("native-observed.json"));
        let path = root.join(filename);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(root.join(marker), project).unwrap();
        std::fs::write(&path, text).unwrap();
        let mut f = fixture_package(
            &format!("official-{language}"),
            text,
            0..0,
            &package,
            Some(repo().join("target/language-migration/cache")),
        );
        f.app.workspace = AppState::new().workspace;
        f.path = path.clone();
        f.app.workspace.open_path(path.clone(), text.into());
        assert_eq!(
            f.app.workspace.documents[&f.app.workspace.active_document().unwrap()]
                .meta
                .lang
                .definition()
                .name
                .as_ref(),
            language,
            "acceptance metadata must recognize {filename}"
        );
        f.app.explorer.model.add_root(
            &crate::infrastructure::explorer_filesystem::ExplorerFilesystem,
            root.clone(),
        );
        let trust = std::sync::Arc::new(
            crate::infrastructure::extensions::trust_store::TrustStore::default(),
        );
        trust.set_workspace(std::slice::from_ref(&root));
        trust.trust(&root).unwrap();
        f.service.set_trust(trust);
        f.service.set_clock(|| 0);
        let effects = f.service.set_setting_sources(
            serde_json::json!({format!("{extension}.performance"): true}),
            None,
            false,
        );
        f.apply(effects);
        if extension.ends_with("typescript-language-server") {
            f.service
                .load_package(&packages.join("loom.runtime.node"))
                .unwrap();
        }
        eprintln!("official {language}: loading component and starting server");
        f.observe();
        let ready = format!("{extension}.completion");
        f.pump_until(|f| {
            let package = f.service.package(extension).unwrap();
            package.lifecycle.state() == State::Failed
                || package.logs.iter().any(|(level, _)| *level >= 2)
                || package
                    .provider_configurations
                    .get(&ready)
                    .is_some_and(|configuration| configuration.enabled)
        });
        assert_eq!(
            f.service.package(extension).unwrap().lifecycle.state(),
            State::Active,
            "{:?}",
            f.service.package(extension).unwrap().logs
        );
        assert!(
            f.service
                .package(extension)
                .unwrap()
                .provider_configurations
                .get(&ready)
                .is_some_and(|configuration| configuration.enabled),
            "{language}: server failed to initialize: {:?}",
            f.service.package(extension).unwrap().logs
        );
        let cursor = text.rfind("add(").unwrap() + 1;
        let mut token = 1000;
        let mut peak_memory = migration_metrics::snapshot();
        let mut ask = |f: &mut Fixture, feature, extra| {
            token += 1;
            let file = f.app.workspace.active_document().unwrap();
            let document = &f.app.workspace.documents[&file];
            let ask = LanguageAsk {
                feature,
                path: path.clone(),
                stamp: Stamp {
                    incarnation: document.incarnation,
                    version: document.buffer.version(),
                },
                start: cursor,
                end: cursor,
                extra,
                target: None,
                server: None,
                timeout: Some(Duration::from_secs(5)),
            };
            let mut effects = Vec::new();
            assert_eq!(
                f.service
                    .request_language(&f.app, &ask, token, &mut effects),
                1,
                "{language}: {feature:?}; state: {:?}; error: {:?}; logs: {:?}",
                f.service.package(extension).unwrap().lifecycle.state(),
                f.service.package(extension).unwrap().last_error,
                f.service.package(extension).unwrap().logs
            );
            let started = Instant::now();
            f.apply(effects);
            f.pump_until(|f| f.language.iter().any(|(asked, _)| *asked == token));
            let at = f
                .language
                .iter()
                .position(|(asked, _)| *asked == token)
                .unwrap();
            let answer = f
                .language
                .remove(at)
                .1
                .map(|answer| answer.value)
                .unwrap_or(serde_json::Value::Null);
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            migration_metrics::sample(&mut peak_memory);
            (answer, elapsed)
        };
        // Indexing may still be active after initialize. Warm-up has its own
        // deadline and is never silently counted as a successful request sample.
        let indexing = Instant::now() + Duration::from_secs(60);
        let mut warm_attempts = 0;
        let warm_started = Instant::now();
        let mut stable_probes = 0;
        loop {
            warm_attempts += 1;
            let (completion, _) = ask(
                &mut f,
                Feature::Completion,
                serde_json::json!({"context":{"triggerKind":1}}),
            );
            let has_completion = completion
                .get("items")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|items| !items.is_empty())
                || completion.as_array().is_some_and(|items| !items.is_empty());
            let mut usable = false;
            if has_completion {
                let (hover, _) = ask(&mut f, Feature::Hover, serde_json::Value::Null);
                let (definition, _) = ask(&mut f, Feature::Definition, serde_json::Value::Null);
                if !hover.is_null() && definition.as_array().is_some_and(|items| !items.is_empty())
                {
                    usable = true;
                }
            }
            stable_probes = if usable { stable_probes + 1 } else { 0 };
            if stable_probes >= 10 && warm_started.elapsed() >= Duration::from_secs(2) {
                break;
            }
            assert!(
                Instant::now() < indexing,
                "{language}: indexing produced no completions"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        eprintln!("official {language}: ready after {warm_attempts} indexing probes");
        let idle_memory = migration_metrics::snapshot();
        let (hover, _) = ask(&mut f, Feature::Hover, serde_json::Value::Null);
        assert!(!hover.is_null(), "{language}: hover");
        let (definition, _) = ask(&mut f, Feature::Definition, serde_json::Value::Null);
        assert!(
            !definition.as_array().unwrap().is_empty(),
            "{language}: definition"
        );
        let (rename, _) = ask(
            &mut f,
            Feature::Rename,
            serde_json::json!({"newName":"sum"}),
        );
        assert!(
            rename.get("documentChanges").is_some() || rename.get("changes").is_some(),
            "{language}: rename"
        );
        let mut timings = serde_json::Map::new();
        for feature in [Feature::Completion, Feature::Hover, Feature::Definition] {
            for _ in 0..10 {
                ask(&mut f, feature, serde_json::Value::Null);
            }
            let mut samples = Vec::new();
            for _ in 0..100 {
                let (answer, elapsed) = ask(&mut f, feature, serde_json::Value::Null);
                assert!(
                    !answer.is_null(),
                    "{language}: measured request failed at sample {}: {feature:?}; state: {:?}; error: {:?}; logs: {:?}; trace: {:?}",
                    samples.len(),
                    f.service.package(extension).unwrap().lifecycle.state(),
                    f.service.package(extension).unwrap().last_error,
                    f.service.package(extension).unwrap().logs,
                    f.trace
                );
                samples.push(elapsed);
            }
            samples.sort_by(f64::total_cmp);
            timings.insert(format!("{feature:?}"), serde_json::json!({"count":samples.len(),"p50_ms":samples[49],"p95_ms":samples[94]}));
        }
        let cpu: Vec<_> = f
            .service
            .package(extension)
            .unwrap()
            .logs
            .iter()
            .filter_map(|(_, text)| {
                text.strip_prefix("language-metrics ")
                    .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
            })
            .collect();
        let restart_started = Instant::now();
        let old_process = *f.service.processes.keys().next().unwrap();
        f.processes.kill(old_process);
        f.pump_until(|f| {
            !f.service.processes.is_empty()
                && !f.service.processes.contains_key(&old_process)
                && f.service
                    .package(extension)
                    .unwrap()
                    .provider_configurations
                    .get(&ready)
                    .is_some_and(|c| c.enabled)
        });
        loop {
            if !ask(&mut f, Feature::Hover, serde_json::Value::Null)
                .0
                .is_null()
            {
                break;
            }
            assert!(
                restart_started.elapsed() < Duration::from_secs(30),
                "{language}: restart failed"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let restart_ms = restart_started.elapsed().as_secs_f64() * 1000.0;
        let stop_started = Instant::now();
        let effects = f.service.stop(extension);
        f.apply(effects);
        let stopped = Instant::now() + Duration::from_secs(5);
        while !f.service.processes.is_empty() && Instant::now() < stopped {
            while let Ok(event) = f.process_events.try_recv() {
                let effects = f.service.handle_process_event(event);
                f.apply(effects);
            }
            while let Ok(event) = f.events.try_recv() {
                let effects = f.service.handle_event(&mut f.app, event);
                f.apply(effects);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            f.service.processes.is_empty(),
            "{language}: processes reclaimed"
        );
        std::fs::write(root.join("native-observed.json"), serde_json::to_vec_pretty(&serde_json::json!({"language":language,"document_bytes":text.len(),"route":"native-executor-wasm-provider-consumer","indexing_probes":warm_attempts,"profile":if cfg!(debug_assertions){"debug"}else{"release"},"small_document":timings,"guest_cpu":cpu,"idle_memory":idle_memory,"observed_peak_memory":peak_memory,"restart_ms":restart_ms,"exit_ms":stop_started.elapsed().as_secs_f64()*1000.0})).unwrap()).unwrap();
        if std::env::var_os("LOOM_MIGRATION_COMPARE").is_some() {
            let baseline: serde_json::Value = serde_json::from_slice(
                &std::fs::read(root.join("baseline-observed.json"))
                    .expect("collect the matching original-host baseline first"),
            )
            .unwrap();
            for operation in ["Completion", "Hover", "Definition"] {
                let reference = baseline["small_document"][operation]["p95_ms"]
                    .as_f64()
                    .unwrap();
                let measured = timings[operation]["p95_ms"].as_f64().unwrap();
                let budget = reference + 10.0f64.max(reference * 0.2);
                if measured > budget {
                    exceeded.push(format!(
                        "{language} {operation}: {measured:.3} ms > {budget:.3} ms"
                    ));
                }
            }
        }
    }
    assert!(
        exceeded.is_empty(),
        "performance budgets exceeded: {exceeded:?}"
    );
}

#[test]
fn a_guest_captures_current_document_text_without_a_provider_request() {
    let (mut f, _) = Fixture::ops(
        "capture-document",
        r#"["documents.read", "workspace.edit"]"#,
    );
    assert_eq!(
        f.op(serde_json::json!({"op":"capture"})),
        "captured 1 v0 \"text\""
    );
    f.app
        .workspace
        .active_editor_mut()
        .unwrap()
        .replace_selection("中😀", crate::model::text::EditKind::Typing)
        .unwrap();
    let captured = f.op(serde_json::json!({"op":"capture"}));
    assert!(
        captured.starts_with("captured 1 v1") && captured.contains("中😀"),
        "{captured}"
    );
    f.app
        .preferences
        .values_mut()
        .language_features
        .max_document_bytes = 3;
    assert!(
        f.op(serde_json::json!({"op":"capture"}))
            .contains("synchronization limit")
    );
    let file = f.app.workspace.active_document().unwrap();
    f.app.workspace.close(file);
    f.app
        .workspace
        .open_path(f.path.with_extension("other.txt"), "other".into());
    assert!(
        f.op(serde_json::json!({"op":"capture"}))
            .contains("no longer open")
    );
}

#[test]
fn a_saved_event_wakes_the_guest_with_the_saved_version_even_after_more_typing() {
    let manifest = EVENT_MANIFEST
        .replace("\"document.close\"", "\"document.save\"")
        .replace("[\"events\"]", "[\"events\",\"documents.read\"]");
    let mut f = fixture_with("saved-snapshot", "old", 0..0, &manifest);
    let file = f.app.workspace.active_document().unwrap();
    let saved = f.app.workspace.shared_save_snapshot(file).unwrap();
    f.app.workspace.active_editor_mut().unwrap().set_cursor(3);
    f.app
        .workspace
        .active_editor_mut()
        .unwrap()
        .replace_selection("new", crate::model::text::EditKind::Typing)
        .unwrap();
    let effects = f.service.document_saved(&f.app, saved);
    f.apply(effects);
    f.wait_for_log("saved 1 v0 \"old\"");
    assert_eq!(f.text(), "oldnew");
}

#[test]
fn provider_preferences_filter_by_kind_and_targeted_resolve_keeps_its_owner() {
    use crate::features::providers::Feature;
    let mut f = Fixture::provider_fixture("provider-preferences", "hello");
    f.observe();
    f.pump_until(|f| !f.published.is_empty());
    let language = language_of(&f.path);
    let choice = crate::features::settings::language::LanguageSetting {
        providers: [("formatting".into(), vec!["uninstalled.provider".into()])].into(),
        ..Default::default()
    };
    f.app
        .preferences
        .values_mut()
        .languages
        .insert(language.clone(), choice);
    f.observe();
    assert!(
        f.service
            .matching_language_providers(Feature::Formatting, &language, None, None)
            .is_empty()
    );
    assert!(
        !f.service
            .matching_language_providers(Feature::Completion, &language, None, None)
            .is_empty()
    );
    assert_eq!(
        f.service
            .matching_language_providers(
                Feature::Formatting,
                &language,
                None,
                Some("example.tools.trim")
            )
            .len(),
        1
    );
    f.app
        .preferences
        .values_mut()
        .languages
        .get_mut(&language)
        .unwrap()
        .providers
        .get_mut("formatting")
        .unwrap()
        .push("...".into());
    f.observe();
    assert_eq!(
        f.service
            .matching_language_providers(Feature::Formatting, &language, None, None)
            .len(),
        1
    );
}

#[test]
fn negotiated_provider_changes_disable_work_and_named_formatters_and_noops_are_stable() {
    use crate::features::language_service::request::Feature;
    let mut f = Fixture::provider_fixture("provider-config", "hello");
    f.service.set_clock(|| 0);
    f.observe();
    f.pump_until(|f| !f.published.is_empty());
    let language = language_of(&f.path);
    let generation = f
        .service
        .package("example.tools")
        .unwrap()
        .lifecycle
        .generation();
    let configuration = |provider: &str, enabled| loom_extension_host::ProviderConfiguration {
        provider: provider.into(),
        enabled,
        modes: None,
        revision: None,
        resolve: true,
        prepare: true,
        triggers: vec![".".into()],
        retriggers: vec![],
        commands: vec![],
        aliases: vec!["named-formatter".into()],
    };
    let effects = f.service.handle_event(
        &mut f.app,
        Event::ProvidersConfigured {
            id: "example.tools".into(),
            generation,
            configurations: vec![
                configuration("example.tools.complete", false),
                configuration("example.tools.trim", true),
            ],
        },
    );
    f.apply(effects);
    assert!(
        f.service
            .language_providers(Feature::Completion, &language)
            .is_empty()
    );
    assert_eq!(
        f.service
            .matching_language_providers(
                Feature::Formatting,
                &language,
                None,
                Some("named-formatter")
            )
            .len(),
        1
    );
    let revision = f
        .service
        .package("example.tools")
        .unwrap()
        .provider_revision;
    let effects = f.service.handle_event(
        &mut f.app,
        Event::ProvidersConfigured {
            id: "example.tools".into(),
            generation,
            configurations: vec![configuration("example.tools.trim", true)],
        },
    );
    f.apply(effects);
    assert_eq!(
        f.service
            .package("example.tools")
            .unwrap()
            .provider_revision,
        revision
    );
    let mut range_only = configuration("example.tools.trim", true);
    range_only.modes = Some(vec![loom_extension_host::ProviderMode::Range]);
    let effects = f.service.handle_event(
        &mut f.app,
        Event::ProvidersConfigured {
            id: "example.tools".into(),
            generation,
            configurations: vec![range_only],
        },
    );
    f.apply(effects);
    assert!(
        f.service
            .language_providers(Feature::Formatting, &language)
            .is_empty()
    );
    assert_eq!(
        f.service
            .language_providers(Feature::RangeFormatting, &language)
            .len(),
        1
    );
}

#[test]
fn application_file_notifications_wake_only_authorized_subscribers() {
    use crate::features::providers::FileChange;
    let (mut f, folder) = Fixture::ops(
        "workspace-events",
        r#"["documents.read","workspace.edit","workspace.files.read","events"]"#,
    );
    f.service
        .packages
        .get_mut("example.tools")
        .unwrap()
        .manifest
        .activation
        .push(crate::features::extensions::manifest::Activation::Event {
            name: "workspace.files".into(),
        });
    let effects = f.service.workspace_files_changed(
        &f.app,
        FileChange::Renamed {
            from: folder.join("a"),
            to: folder.join("b"),
        },
    );
    assert!(!effects.is_empty());
    f.apply(effects);
    f.wait_for_log("workspace files");
    assert!(
        f.service
            .package("example.tools")
            .unwrap()
            .logs
            .iter()
            .any(|(_, text)| text.contains("Renamed") && text.contains("a") && text.contains("b"))
    );
    assert!(
        f.service
            .workspace_files_changed(
                &f.app,
                FileChange::Created(std::env::temp_dir().join("outside-workspace"))
            )
            .is_empty()
    );
}

#[test]
fn a_guest_applies_utf16_multi_file_edits_as_one_undoable_transaction_and_cannot_leave_the_workspace()
 {
    let (mut f, folder) = Fixture::ops("workspace-edit", r#"["documents.read","workspace.edit"]"#);
    let current = f.app.workspace.active_document().unwrap();
    f.app
        .workspace
        .documents
        .get_mut(&current)
        .unwrap()
        .buffer
        .reload("a😀b".into());
    let other = folder.join("other.txt");
    std::fs::write(&other, "a😀b").unwrap();
    let first = f.path.clone();
    assert_eq!(
        f.op(serde_json::json!({"op":"workspace-edit","paths":[first,other]})),
        "workspace edit accepted 1"
    );
    f.wait_for_log("operation 1 committed");
    let second = f.app.workspace.file_id_for_path(&other).unwrap();
    assert_eq!(f.text(), "a中b");
    assert_eq!(f.app.workspace.documents[&second].buffer.text(), "a中b");
    crate::app::language_ui::edit::undo_redo(&mut f.app, true)
        .unwrap()
        .unwrap();
    assert_eq!(f.text(), "a😀b");
    assert_eq!(f.app.workspace.documents[&second].buffer.text(), "a😀b");
    let outside = folder.parent().unwrap().join("outside.txt");
    std::fs::write(&outside, "a😀b").unwrap();
    assert_eq!(
        f.op(serde_json::json!({"op":"workspace-edit","paths":[outside]})),
        "workspace edit accepted 2"
    );
    f.wait_for_log("operation 2 failed");
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "a😀b");
}

#[test]
fn typed_providers_cover_navigation_rename_hierarchies_lenses_and_their_origin_routing() {
    use crate::features::language_service::{
        capabilities::PositionEncoding, reply, request::Feature as F,
    };
    let language = language_of(&std::env::temp_dir().join("typed-provider.txt"));
    let kinds = [
        "references",
        "workspace-symbols",
        "document-highlights",
        "rename",
        "folding-ranges",
        "selection-ranges",
        "call-hierarchy",
        "type-hierarchy",
        "code-lens",
        "format-on-type",
        "execute-command",
        "code-actions",
    ];
    let entries = kinds.iter().enumerate().map(|(i,kind)|format!(r#"{{"id":"example.tools.p{i}","kind":"{kind}","selector":{{"languages":["{language}"]}}}},"#)).collect::<String>();
    let manifest = provider_manifest(&language)
        .replace("\"providers\": [", &format!("\"providers\": [{entries}"));
    let mut f = fixture_with("typed-provider", "hello world\nline2\n", 0..0, &manifest);
    f.service.set_clock(|| 0);
    let ask = |f: &mut Fixture, token, feature, extra, target| {
        let file = f.app.workspace.active_document().unwrap();
        let d = &f.app.workspace.documents[&file];
        let request = LanguageAsk {
            feature,
            path: d.meta.path.clone(),
            stamp: Stamp {
                incarnation: d.incarnation,
                version: d.buffer.version(),
            },
            start: 0,
            end: 5,
            extra,
            target,
            server: None,
            timeout: None,
        };
        let mut effects = Vec::new();
        assert_eq!(
            f.service
                .request_language(&f.app, &request, token, &mut effects),
            1,
            "{feature:?}"
        );
        f.apply(effects);
        f.pump_until(|f| f.language.iter().any(|(t, _)| *t == token));
        f.language
            .iter()
            .find(|(t, _)| *t == token)
            .unwrap()
            .1
            .clone()
            .expect("typed answer")
    };
    let references = ask(&mut f, 101, F::References, serde_json::Value::Null, None);
    assert_eq!(
        reply::locations(&references.value, PositionEncoding::Utf8).len(),
        1
    );
    let symbols = ask(
        &mut f,
        102,
        F::WorkspaceSymbol,
        serde_json::json!({"query":"needle"}),
        None,
    );
    assert_eq!(symbols.value[0]["name"], "needle");
    let highlights = ask(
        &mut f,
        103,
        F::DocumentHighlight,
        serde_json::Value::Null,
        None,
    );
    assert_eq!(highlights.value[0]["kind"], 2);
    let prepared = ask(&mut f, 104, F::PrepareRename, serde_json::Value::Null, None);
    assert_eq!(
        reply::prepare_rename(&prepared.value, PositionEncoding::Utf8)
            .unwrap()
            .placeholder
            .as_deref(),
        Some("hello")
    );
    let folds = ask(&mut f, 105, F::FoldingRange, serde_json::Value::Null, None);
    assert_eq!(reply::folding_ranges(&folds.value).len(), 1);
    let selections = ask(
        &mut f,
        106,
        F::SelectionRange,
        serde_json::Value::Null,
        None,
    );
    assert!(selections.value[0]["parent"].is_object());
    let calls = ask(
        &mut f,
        107,
        F::PrepareCallHierarchy,
        serde_json::Value::Null,
        None,
    );
    assert_eq!(calls.value[0]["data"], "owned-data");
    let incoming = ask(
        &mut f,
        108,
        F::IncomingCalls,
        calls.value[0].clone(),
        Some(calls.source),
    );
    assert_eq!(
        reply::calls(&incoming.value, "from", PositionEncoding::Utf8).len(),
        1
    );
    let types = ask(
        &mut f,
        109,
        F::PrepareTypeHierarchy,
        serde_json::Value::Null,
        None,
    );
    let supers = ask(
        &mut f,
        110,
        F::Supertypes,
        types.value[0].clone(),
        Some(types.source),
    );
    assert_eq!(
        reply::hierarchy_items(&supers.value, PositionEncoding::Utf8).len(),
        1
    );
    let lenses = ask(&mut f, 111, F::CodeLens, serde_json::Value::Null, None);
    assert_eq!(lenses.value[0]["data"], "lens-data");
    let lens = ask(
        &mut f,
        112,
        F::CodeLensResolve,
        lenses.value[0].clone(),
        Some(lenses.source),
    );
    assert_eq!(lens.value["command"]["command"], "run");
    let command = ask(
        &mut f,
        113,
        F::ExecuteCommand,
        serde_json::json!({"command":"run","arguments":[7]}),
        Some(lenses.source),
    );
    assert!(command.value.is_null());
    let actions = ask(&mut f, 114, F::CodeAction, serde_json::Value::Null, None);
    let action = ask(
        &mut f,
        115,
        F::CodeActionResolve,
        serde_json::json!({"data":"owned"}),
        Some(actions.source),
    );
    assert_eq!(action.value["command"]["command"], "run");
    assert!(action.value.get("disabled").is_none());
    let renamed = ask(
        &mut f,
        116,
        F::Rename,
        serde_json::json!({"newName":"中文"}),
        Some(prepared.source),
    );
    let edit = reply::workspace_edit(&renamed.value, PositionEncoding::Utf8).unwrap();
    let request = crate::app::language_ui::edit::Request {
        title: "Rename".into(),
        edit,
        stamps: crate::app::language_ui::edit::stamps(&f.app),
        rect: Default::default(),
        reply: None,
    };
    crate::app::language_ui::edit::apply_now(&mut f.app, &request, HashMap::new()).unwrap();
    assert_eq!(f.text(), "中文 world\nline2\n");
    f.app.workspace.active_editor_mut().unwrap().undo();
    assert_eq!(f.text(), "hello world\nline2\n");
}

#[test]
fn a_guest_creates_updates_and_removes_its_own_colored_widget_and_receives_events() {
    use crate::features::extensions::ui::Payload;
    let (mut f, _) = Fixture::ops("widgets", r#"["documents.read", "workspace.edit"]"#);
    assert_eq!(
        f.op(serde_json::json!({"op":"ui-create", "text":"Starting", "color":0x334455})),
        "ui created 1"
    );
    f.pump_until(|f| !f.app.extension_widgets.roots().is_empty());
    let root = f.app.extension_widgets.roots()[0].clone();
    assert_eq!(root.properties.background, Some(0x334455));
    let effects = f
        .service
        .ui_event(&f.app, root.origin.clone(), Payload::Click);
    f.apply(effects);
    f.wait_for_log("Click");
    assert_eq!(
        f.op(serde_json::json!({"op":"ui-update", "text":"Ready", "color":0x228844})),
        "ui updated"
    );
    f.pump_until(|f| {
        f.app.extension_widgets.roots()[0]
            .properties
            .text
            .as_deref()
            == Some("Ready")
    });
    assert_eq!(
        f.app.extension_widgets.roots()[0].properties.background,
        Some(0x228844)
    );
    assert_eq!(f.op(serde_json::json!({"op":"ui-remove"})), "ui removed");
    f.pump_until(|f| f.app.extension_widgets.roots().is_empty());
    assert!(
        f.service
            .ui_event(&f.app, root.origin.clone(), Payload::Click)
            .is_empty()
    );
    assert_eq!(f.op(serde_json::json!({"op":"ui-create"})), "ui created 2");
    f.pump_until(|f| !f.app.extension_widgets.roots().is_empty());
    let effects = f.service.set_setting_sources(
        serde_json::json!({"example.tools.spec": serde_json::json!({"op":"ui-trap"}).to_string()}),
        None,
        false,
    );
    f.apply(effects);
    f.run("example.tools.op");
    f.pump_until(|f| {
        f.service
            .package("example.tools")
            .unwrap()
            .lifecycle
            .state()
            == State::Failed
    });
    assert!(
        f.app.extension_widgets.roots().is_empty(),
        "a failed owner cannot leave widgets behind"
    );
}

#[test]
fn disk_watch_events_reach_the_guest_and_unwatch_releases_the_subscription() {
    let (mut f, folder) = Fixture::ops(
        "watch-files",
        r#"["documents.read", "workspace.edit", "workspace.files.read"]"#,
    );
    assert_eq!(
        f.op(serde_json::json!({"op":"watch", "pattern":"**/*.txt"})),
        "watch started 1"
    );
    f.wait_for_log("watch ready 1");
    std::fs::write(folder.join("watched.txt"), "hello").unwrap();
    f.wait_for_log("files changed 1");
    assert!(
        f.service
            .package("example.tools")
            .unwrap()
            .logs
            .iter()
            .any(|(_, text)| text.contains("watched.txt"))
    );
    assert_eq!(f.op(serde_json::json!({"op":"unwatch"})), "watch stopped");
    f.pump_until(|f| f.service.watches.is_empty());
    assert!(f.service.watches.is_empty());
}

#[test]
fn files_are_created_renamed_and_deleted_in_order_and_the_guest_hears_the_outcome() {
    let (mut f, dir) = Fixture::ops("fileops-ok", WRITER);
    f.file_ops(serde_json::json!([
        { "kind": "create", "path": "made.txt", "text": "hello" },
        { "kind": "rename", "source": "notes.txt", "target": "docs/notes.txt" },
        { "kind": "delete", "path": "blob.bin" },
    ]));
    f.pump_until(|f| f.log_lines().iter().any(|l| l.contains("committed as")));
    assert_eq!(
        std::fs::read_to_string(dir.join("made.txt")).unwrap(),
        "hello"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("docs/notes.txt")).unwrap(),
        "0123456789"
    );
    assert!(!dir.join("notes.txt").exists() && !dir.join("blob.bin").exists());
    assert_eq!(f.changed.len(), 3, "{:?}", f.changed);
    assert!(
        f.changed[0].starts_with("Created")
            && f.changed[1].starts_with("Renamed")
            && f.changed[2].starts_with("Deleted")
    );
}

#[test]
fn a_failing_step_undoes_the_earlier_ones_and_says_so() {
    let (mut f, dir) = Fixture::ops("fileops-undo", WRITER);
    f.file_ops(serde_json::json!([
        { "kind": "create", "path": "made.txt", "text": "hello" },
        { "kind": "rename", "source": "does-not-exist.txt", "target": "x.txt" },
    ]));
    f.pump_until(|f| f.log_lines().iter().any(|l| l.contains("failed")));
    assert!(!dir.join("made.txt").exists(), "the first step was undone");
    assert!(dir.join("notes.txt").exists());
    let logs = f.log_lines();
    assert!(
        logs.iter()
            .any(|l| l.contains("everything before it was undone")),
        "{logs:?}"
    );
    assert!(f.changed.is_empty());
}

#[test]
fn open_documents_follow_a_rename_and_close_with_a_delete() {
    let (mut f, dir) = Fixture::ops("fileops-docs", WRITER);
    let notes = dir.join("notes.txt");
    let src = dir.join("src/lib.rs");
    f.app
        .workspace
        .open_path(notes.clone(), "0123456789".into());
    f.app
        .workspace
        .open_path(src.clone(), "fn main() {}\n".into());
    f.file_ops(serde_json::json!([
        { "kind": "rename", "source": "notes.txt", "target": "renamed.txt" },
        { "kind": "delete", "path": "src/lib.rs" },
    ]));
    f.pump_until(|f| f.log_lines().iter().any(|l| l.contains("committed as")));
    assert!(
        f.app
            .workspace
            .file_id_for_path(&dir.join("renamed.txt"))
            .is_some(),
        "the open document moved"
    );
    assert!(f.app.workspace.file_id_for_path(&notes).is_none());
    assert!(
        f.app.workspace.file_id_for_path(&src).is_none(),
        "the deleted file's document closed"
    );
}

#[test]
fn a_file_with_unsaved_changes_is_never_touched() {
    let (mut f, dir) = Fixture::ops("fileops-dirty", WRITER);
    let notes = dir.join("notes.txt");
    let id = f
        .app
        .workspace
        .open_path(notes.clone(), "0123456789".into());
    f.app
        .workspace
        .document_editor_mut(id)
        .unwrap()
        .set_cursor(0);
    loom_editor_core::normal::insert(
        &mut f.app.workspace.document_editor_mut(id).unwrap(),
        "typed ",
    );
    f.file_ops(serde_json::json!([{ "kind": "delete", "path": "notes.txt" }]));
    f.pump_until(|f| f.log_lines().iter().any(|l| l.contains("failed")));
    assert!(
        f.log_lines()
            .iter()
            .any(|l| l.contains("has unsaved changes")),
        "{:?}",
        f.log_lines()
    );
    assert!(notes.exists());
    assert!(f.app.workspace.file_id_for_path(&notes).is_some());
}

#[test]
fn paths_outside_the_folder_protected_names_and_a_missing_grant_are_refused() {
    let (mut f, dir) = Fixture::ops("fileops-refuse", WRITER);
    for path in [
        "../outside.txt",
        "/abs.txt",
        ".git/config",
        "sub/.loom-edit-1/x",
    ] {
        let before = f.log_lines().len();
        f.file_ops(serde_json::json!([{ "kind": "create", "path": path }]));
        f.pump_until(|f| f.log_lines().len() > before + 1);
        assert!(
            f.log_lines()
                .iter()
                .any(|l| l.contains("inside the folder") || l.contains("cannot be changed")),
            "{path}: {:?}",
            f.log_lines()
        );
    }
    assert!(!dir.join("..").join("outside.txt").exists());

    let (mut g, _) = Fixture::ops(
        "fileops-ungranted",
        r#"["documents.read", "workspace.edit"]"#,
    );
    g.op(serde_json::json!({ "op": "fileops", "ops": [{ "kind": "create", "path": "a.txt" }] }));
    g.pump_until(|g| g.log_lines().iter().any(|l| l.contains("failed")));
    assert!(
        g.log_lines()
            .iter()
            .any(|l| l.contains("not declared") || l.contains("not been granted")),
        "{:?}",
        g.log_lines()
    );
}

#[test]
fn a_note_left_by_an_interrupted_operation_is_reported_when_the_folder_opens() {
    let (mut f, dir) = Fixture::ops("fileops-note", WRITER);
    let note = dir.join(format!(
        "{}1-1.txt",
        crate::infrastructure::extensions::fileops::NOTE_PREFIX
    ));
    std::fs::write(&note, "rename a -> b").unwrap();
    // The folder was added to the app before the service first looked at the roots.
    let effects = f.service.observe(&f.app);
    let messages = f.apply(effects);
    assert!(
        messages.iter().any(|m| m.contains("did not finish")),
        "{messages:?}"
    );
    // Reported once, not at every update.
    let again = f.service.observe(&f.app);
    assert!(f.apply(again).iter().all(|m| !m.contains("did not finish")));
}
