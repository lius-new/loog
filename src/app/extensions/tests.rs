//! End-to-end tests: real Rust component, real executor thread, real
//! workspace edit service. Events are pumped by hand on the test thread, which
//! plays the role of the UI thread.
use super::*;
use crate::infrastructure::extensions::tasks;
use crate::infrastructure::extensions::{Event, Executor};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

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
    runner: tasks::Runner,
    task_events: Receiver<tasks::TaskEvent>,
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
    resolved: Vec<(u64, serde_json::Value)>,
}

fn fixture(name: &str, text: &str, select: std::ops::Range<usize>) -> Fixture {
    fixture_with(name, text, select, MANIFEST)
}

fn fixture_with(name: &str, text: &str, select: std::ops::Range<usize>, manifest: &str) -> Fixture {
    let (sender, events) = channel();
    let sender = Mutex::new(sender);
    let executor = Executor::spawn(Box::new(move |event| {
        let _ = sender.lock().unwrap().send(event);
    }))
    .unwrap();
    let mut service = ExtensionService::new(
        std::env::temp_dir().join(format!("loom-ext-cache-{}", std::process::id())),
    );
    let id = service
        .load_package(&package_dir_with(name, manifest))
        .unwrap();
    service.grant_declared(&id);
    let mut app = AppState::new();
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
    let runner = tasks::Runner::new(move |event| {
        let _ = task_sender.lock().unwrap().send(event);
    });
    Fixture {
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
        resolved: Vec::new(),
    }
}

impl Fixture {
    /// The next executor event; task output and exits are handled on the way,
    /// as the UI thread would.
    fn recv(&mut self) -> Event {
        let limit = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            while let Ok(event) = self.task_events.try_recv() {
                let effects = self.service.handle_task_event(event);
                self.apply(effects);
            }
            if let Ok(event) = self.events.recv_timeout(Duration::from_millis(10)) {
                return event;
            }
            assert!(
                std::time::Instant::now() < limit,
                "the executor produced no event"
            );
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
                Effect::Submit(job) => assert!(self.executor.submit(job)),
                Effect::Toast(m) | Effect::Error(m) => messages.push(m),
                Effect::PublishDiagnostics { source, items, .. } => {
                    self.published.push((source, items))
                }
                Effect::ClearDiagnostics { source } => self.cleared.push(source),
                Effect::StartTask { task, spec } => self.runner.start(task, spec),
                Effect::CancelTask { task } => self.runner.cancel(task),
                Effect::OfferCompletion { source, items, .. } => self.offered.push((source, items)),
                Effect::ResolvedCompletion { source, item, .. } => {
                    self.resolved.push((source, item))
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
    let texts: Vec<_> = logs.iter().map(|(_, t)| t.as_str()).collect();
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

pub(crate) fn provider_manifest(language: &str) -> String {
    r#"{
    "schema": 1,
    "id": "example.tools",
    "version": "0.1.0",
    "api": "loom:extension@0.1.0",
    "component": "extension.wasm",
    "capabilities": ["documents.read", "workspace.edit", "languages"],
    "contributes": { "providers": [
        { "id": "example.tools.todo", "kind": "diagnostics", "selector": { "languages": ["LANG"] } },
        { "id": "example.tools.trim", "kind": "formatting", "selector": { "languages": ["LANG"] } },
        { "id": "example.tools.complete", "kind": "completion", "selector": { "languages": ["LANG"] } }
    ] }
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
    assert!(source & crate::infrastructure::lsp::manager::EXTERNAL_SOURCE != 0);
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
    assert!(source & crate::infrastructure::lsp::manager::EXTERNAL_SOURCE != 0);
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
            .request_completion_resolve(&f.app, source | 0x40, &item)
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
    fn tasks(name: &str, task: String, trusted: bool, allowed: bool) -> (Fixture, PathBuf) {
        let mut f = fixture_with(name, "TODO text", 0..0, TASK_MANIFEST);
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
        if allowed {
            assert_eq!(f.service.allow_tools(&folder), 1);
        }
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
fn without_trust_or_a_tool_grant_a_task_is_refused_before_anything_starts() {
    for (trusted, allowed, expected) in [
        (false, false, "not trusted"),
        (true, false, "workspace::AllowExtensionTools"),
    ] {
        let (mut f, _) = Fixture::tasks(
            &format!("denied-{trusted}"),
            task_json("echo hi", "echo hi"),
            trusted,
            allowed,
        );
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
            logs.iter().any(|l| l.contains(expected)),
            "{expected}: {logs:?}"
        );
        assert_eq!(f.runner.running(), 0);
    }
}

#[test]
fn an_allowed_task_streams_output_and_reports_its_exit_once() {
    let (mut f, _) = Fixture::tasks("allowed", task_json("echo hello", "echo hello"), true, true);
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
    let (mut f, _) = Fixture::tasks("escape", task, true, true);
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
    let (mut f, _) = Fixture::tasks("cancel", task, true, true);
    f.start_with_roots();
    f.run("example.tools.run");
    f.pump_until(|f| !f.service.tasks.is_empty());
    f.run("example.tools.cancel");
    f.wait_for_log("cancelled true");
}

#[test]
fn removing_a_tool_grant_or_a_folder_stops_what_runs_there() {
    let task = task_json("ping -n 30 127.0.0.1 >nul", "sleep 30");
    let (mut f, folder) = Fixture::tasks("revoke", task.clone(), true, true);
    f.start_with_roots();
    f.run("example.tools.run");
    f.pump_until(|f| !f.service.tasks.is_empty());
    let effects = f.service.revoke_tools(&folder);
    assert_eq!(effects.len(), 1);
    f.apply(effects);
    f.wait_for_log("cancelled true");

    let (mut g, folder) = Fixture::tasks("removed", task, true, true);
    g.start_with_roots();
    g.run("example.tools.run");
    g.pump_until(|g| !g.service.tasks.is_empty());
    g.app.explorer.model.remove_root(&folder);
    g.observe();
    g.wait_for_log("cancelled true");
}

#[test]
fn a_checker_publishes_findings_for_the_state_it_checked_and_not_for_a_changed_one() {
    let (mut f, _) = Fixture::tasks("check", task_json("echo ok", "echo ok"), true, true);
    f.start_with_roots();
    f.wait_for_log("opened 1");
    f.run("example.tools.check");
    f.pump_until(|f| !f.published.is_empty());
    let (source, items) = &f.published[0];
    assert!(source & crate::infrastructure::lsp::manager::EXTERNAL_SOURCE != 0);
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
