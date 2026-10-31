//! End-to-end tests: real Rust component, real executor thread, real
//! workspace edit service. Events are pumped by hand on the test thread, which
//! plays the role of the UI thread.
use super::*;
use crate::infrastructure::extensions::{Event, Executor};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// Build the guest once and return the component bytes.
fn component() -> &'static [u8] {
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

fn package_dir_with(name: &str, manifest: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("loom-ext-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("manifest.json"), manifest).unwrap();
    std::fs::write(dir.join("extension.wasm"), component()).unwrap();
    dir
}

struct Fixture {
    service: ExtensionService,
    executor: Executor,
    events: Receiver<Event>,
    app: AppState,
    path: PathBuf,
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
    Fixture {
        service,
        executor,
        events,
        app,
        path,
    }
}

impl Fixture {
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
            let event = self
                .events
                .recv_timeout(Duration::from_secs(60))
                .expect("the executor produced an event");
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
