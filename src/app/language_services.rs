//! The bridge from the application's documents to the language servers.
//!
//! After every change to the application state the bridge compares a cheap
//! fingerprint of what the servers care about (which documents are open, their
//! versions and languages, the folders, the server settings). Only a difference
//! produces a snapshot for the manager, so typing costs one hash over the open
//! documents and the manager's own thread does the rest. Because it looks at the
//! state rather than at individual commands, no way of changing a document can
//! be missed.
use super::state::AppState;
use crate::domain::language::Registry;
use crate::features::language_service::server_set::ServerSet;
use crate::features::language_service::sync::Stamp;
use crate::infrastructure::lsp::manager::{
    InstanceInfo, Manager, Options, Snapshot, SnapshotDocument,
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub(crate) struct LanguageServices {
    manager: Manager,
    sent: Mutex<Option<u64>>,
}

impl LanguageServices {
    #[cfg(test)]
    pub(super) fn with_manager(manager: Manager) -> Self {
        Self {
            manager,
            sent: Mutex::new(None),
        }
    }
    pub fn new(servers: Arc<ServerSet>, on_change: impl Fn() + Send + 'static) -> Self {
        Self::with_start_gate(servers, None, on_change)
    }

    /// `gate` is asked before any server process starts for a project root.
    pub fn with_start_gate(
        servers: Arc<ServerSet>,
        gate: Option<crate::infrastructure::lsp::manager::StartGate>,
        on_change: impl Fn() + Send + 'static,
    ) -> Self {
        Self {
            manager: Manager::start(
                Registry::builtin(),
                servers,
                crate::infrastructure::lsp_catalog::installer(),
                Options {
                    start_gate: gate,
                    ..Options::default()
                },
                on_change,
            ),
            sent: Mutex::new(None),
        }
    }

    /// Hands the manager the current documents if they differ from the last time.
    pub fn sync(&self, app: &AppState) {
        let fingerprint = fingerprint(app);
        {
            let mut sent = self.sent.lock().unwrap();
            if *sent == Some(fingerprint) {
                return;
            }
            *sent = Some(fingerprint);
        }
        self.manager.update(snapshot(app));
    }

    /// A document was written to disk.
    pub fn saved(&self, path: PathBuf, text: Arc<String>) {
        self.manager.saved(path, text);
    }

    /// Files were created, renamed or deleted by the application.
    pub fn files_changed(&self, change: crate::infrastructure::lsp::manager::FileChange) {
        self.manager.files_changed(change);
    }

    pub fn install(&self, server: &str) {
        self.manager.install(server);
    }

    pub fn cancel_install(&self, server: &str, generation: u64) {
        self.manager.cancel_install(server, generation);
    }

    pub fn publish_external(
        &self,
        source: u64,
        path: &std::path::Path,
        stamp: crate::features::language_service::sync::Stamp,
        text: Arc<String>,
        diagnostics: Arc<[crate::features::language_service::diagnostics::Diagnostic]>,
    ) {
        self.manager
            .publish_external(source, path, stamp, text, diagnostics);
    }

    pub fn clear_external(&self, source: u64) {
        self.manager.clear_external(source);
    }

    pub fn restart(&self) {
        self.manager.restart();
    }

    pub fn restart_instance(&self, id: u64) {
        self.manager.restart_instance(id);
    }

    pub fn cancel_task(&self, instance: u64, run: u64, task: u64) {
        self.manager.cancel_task(instance, run, task);
    }

    /// The server results that describe the document exactly as `stamp` says.
    pub fn results(
        &self,
        path: &std::path::Path,
        stamp: &crate::domain::document::DocumentStamp,
    ) -> crate::infrastructure::lsp::manager::DocumentResults {
        self.manager.results(
            path,
            Stamp {
                incarnation: stamp.incarnation,
                version: stamp.version,
            },
        )
    }

    /// Asks the running servers; see `Manager::request`.
    pub fn request(
        &self,
        spec: crate::infrastructure::lsp::manager::RequestSpec,
    ) -> crate::infrastructure::lsp::manager::RequestHandle {
        self.manager.request(spec)
    }

    /// What servers asked of the application since the last call.
    pub fn take_actions(&self) -> Vec<crate::infrastructure::lsp::manager::ServerAction> {
        self.manager.take_actions()
    }

    pub fn infos(&self) -> Vec<InstanceInfo> {
        self.manager.infos()
    }

    pub fn code_action_context(
        &self,
        path: &std::path::Path,
        stamp: Stamp,
    ) -> crate::infrastructure::lsp::manager::CodeActionContext {
        self.manager.code_action_context(path, stamp)
    }
}

fn fingerprint(app: &AppState) -> u64 {
    let mut hasher = DefaultHasher::new();
    let limit = app.preferences.values().lsp.max_document_bytes;
    limit.hash(&mut hasher);
    serde_json::to_string(&app.preferences.values().languages)
        .unwrap_or_default()
        .hash(&mut hasher);
    let mut ids: Vec<_> = app
        .workspace
        .documents
        .iter()
        .filter(|(_, d)| d.is_text() && d.buffer.len() <= limit)
        .map(|(_, d)| {
            (
                &d.meta.path,
                d.incarnation,
                d.buffer.version(),
                d.meta.language_generation,
            )
        })
        .collect();
    ids.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    ids.hash(&mut hasher);
    app.explorer.model.roots().hash(&mut hasher);
    let servers = &app.preferences.values().language_servers;
    servers.len().hash(&mut hasher);
    if !servers.is_empty() {
        serde_json::to_string(servers)
            .unwrap_or_default()
            .hash(&mut hasher);
    }
    hasher.finish()
}

fn snapshot(app: &AppState) -> Snapshot {
    // Documents larger than this are not sent to servers.
    let limit = app.preferences.values().lsp.max_document_bytes;
    let mut documents: Vec<SnapshotDocument> = app
        .workspace
        .documents
        .iter()
        .filter(|(_, d)| d.is_text() && d.buffer.len() <= limit)
        .map(|(_, d)| SnapshotDocument {
            path: d.meta.path.clone(),
            language: d.meta.lang,
            stamp: Stamp {
                incarnation: d.incarnation,
                version: d.buffer.version(),
            },
            text: d.buffer.text_snapshot(),
        })
        .collect();
    documents.sort_by(|a, b| a.path.cmp(&b.path));
    Snapshot {
        documents,
        workspace_roots: app.explorer.model.roots().to_vec(),
        servers: app.preferences.values().language_servers.clone(),
        languages: app.preferences.values().languages.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{runtime::AppRuntime, test_support::live};
    use crate::features::language_service::server_set::rust_analyzer_running;
    use crate::features::settings::servers::LanguageServerSetting;
    use serde_json::Value;
    use std::time::{Duration, Instant};

    fn fake_server() -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let dir = exe.parent().and_then(|deps| deps.parent()).unwrap();
        let name = if cfg!(windows) {
            "fake_lsp.exe"
        } else {
            "fake_lsp"
        };
        dir.join("examples").join(name)
    }

    fn methods(log: &std::path::Path) -> Vec<(String, Value)> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|m| Some((m["method"].as_str()?.to_owned(), m["params"].clone())))
            .collect()
    }

    fn wait(log: &std::path::Path, method: &str, count: usize) {
        let limit = Instant::now() + Duration::from_secs(15);
        while methods(log).iter().filter(|(m, _)| m == method).count() < count {
            assert!(
                Instant::now() < limit,
                "no {method} #{count}: {:?}",
                methods(log)
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn opening_editing_saving_and_closing_reach_the_server() {
        let dir = std::env::temp_dir().join(format!("loom-bridge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("server.log");
        let path = dir.join("a.rs");
        std::fs::write(&path, "fn a() {}").unwrap();

        let mut app = AppState::new();
        app.preferences.values_mut().language_servers = vec![LanguageServerSetting {
            args: Some(vec![
                "--sync".into(),
                "object".into(),
                "--diagnostics".into(),
                "--semantic".into(),
                "full".into(),
                "--log".into(),
                log.display().to_string(),
                "--mirror".into(),
                dir.join("mirror.json").display().to_string(),
            ]),
            ..LanguageServerSetting::named("rust-analyzer")
        }];
        let runtime = Arc::new(AppRuntime::default());
        runtime.install_language_services(LanguageServices::new(
            rust_analyzer_running(&fake_server().display().to_string()),
            || {},
        ));
        let state = live(app).with_runtime(&runtime);

        // Opening a file reaches the server without any language-service call.
        state.update(|app| {
            app.workspace.open_path(path.clone(), "fn a() {}".into());
        });
        wait(&log, "textDocument/didOpen", 1);
        let opened = methods(&log)
            .into_iter()
            .find(|(m, _)| m == "textDocument/didOpen")
            .unwrap();
        assert_eq!(opened.1["textDocument"]["languageId"], "rust");
        assert_eq!(opened.1["textDocument"]["text"], "fn a() {}");

        // What the server answers is available for exactly the state it described.
        let stamp = {
            let app = state.get();
            let (id, doc) = app
                .workspace
                .documents
                .iter()
                .find(|(_, d)| d.is_text())
                .unwrap();
            doc.stamp(*id)
        };
        let services = runtime.language_services().unwrap();
        let limit = Instant::now() + Duration::from_secs(15);
        let found = loop {
            let results = services.results(&path, &stamp);
            if let (Some(diagnostics), Some(semantic)) = (results.diagnostics, results.semantic) {
                break (diagnostics, semantic);
            }
            assert!(Instant::now() < limit, "no results");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(found.0[0].message, "fake error");
        assert_eq!(found.1[0].range, 0..2);

        // Any change to the buffer does, whichever way it was made.
        state.update(|app| {
            let doc = app
                .workspace
                .documents
                .values_mut()
                .find(|d| d.is_text())
                .unwrap();
            doc.buffer.reload("fn a() { 1 }".into());
        });
        wait(&log, "textDocument/didChange", 1);
        let changed = methods(&log)
            .into_iter()
            .find(|(m, _)| m == "textDocument/didChange")
            .unwrap();
        // The server was sent a range, and its copy is the editor's text.
        assert!(changed.1["contentChanges"][0]["range"].is_object());
        let mirrored = || {
            std::fs::read_to_string(dir.join("mirror.json"))
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .and_then(|map| {
                    map.as_object()?
                        .values()
                        .next()?
                        .as_str()
                        .map(str::to_owned)
                })
        };
        let limit = Instant::now() + Duration::from_secs(5);
        while mirrored().as_deref() != Some("fn a() { 1 }") {
            assert!(Instant::now() < limit, "mirror is {:?}", mirrored());
            std::thread::sleep(Duration::from_millis(20));
        }

        // An update that changes nothing about documents sends nothing.
        for _ in 0..5 {
            state.update(|app| {
                app.editor.syntax_revision = app.editor.syntax_revision.wrapping_add(1)
            });
        }
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            methods(&log)
                .iter()
                .filter(|(m, _)| m == "textDocument/didChange")
                .count(),
            1
        );

        let tasks = crate::app::test_support::UiTasks::default();
        runtime.set_application(tasks.application());
        assert!(crate::app::workspace_commands::save_active_document(&state));
        let deadline = Instant::now() + Duration::from_secs(10);
        while state.get().editor.saving.pending > 0 {
            tasks.run_all();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        wait(&log, "textDocument/didSave", 1);

        drop(state);
        drop(services);
        runtime.clear_language_services();
        let seen = methods(&log);
        assert!(
            seen.iter().any(|(m, _)| m == "textDocument/didClose"),
            "{seen:?}"
        );
        assert!(seen.iter().any(|(m, _)| m == "exit"), "{seen:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Like the real application: a document is opened, then edited in several
    /// steps while rust-analyzer is still loading. Needs LOOM_RA_FILE and LOOM_RA_ROOT.
    #[test]
    #[ignore = "needs rust-analyzer"]
    fn real_rust_analyzer_diagnostics_reach_the_current_state_after_edits() {
        let file = PathBuf::from(std::env::var("LOOM_RA_FILE").unwrap());
        let root = PathBuf::from(std::env::var("LOOM_RA_ROOT").unwrap());
        let original = std::fs::read_to_string(&file).unwrap();
        let mut app = AppState::new();
        app.explorer.model.restore(vec![root], Vec::new());
        app.preferences.values_mut().language_servers =
            vec![LanguageServerSetting::named("rust-analyzer")];
        let runtime = Arc::new(AppRuntime::default());
        runtime.install_language_services(LanguageServices::new(
            rust_analyzer_running(
                &std::env::var("LOOM_RUST_ANALYZER").unwrap_or_else(|_| "rust-analyzer".into()),
            ),
            || {},
        ));
        let state = live(app).with_runtime(&runtime);
        state.update(|app| {
            app.workspace.open_path(file.clone(), original.clone());
        });
        // Type a bad line one character at a time, as a person would.
        for step in 0..6 {
            std::thread::sleep(Duration::from_millis(400));
            state.update(|app| {
                let doc = app
                    .workspace
                    .documents
                    .values_mut()
                    .find(|d| d.is_text())
                    .unwrap();
                let text = original.replacen(
                    "fn main() {
",
                    &format!(
                        "fn main() {{
    {}
",
                        "s".repeat(step + 1)
                    ),
                    1,
                );
                doc.buffer.reload(text);
            });
        }
        let services = runtime.language_services().unwrap();
        let started = Instant::now();
        loop {
            let stamp = {
                let app = state.get();
                let (id, doc) = app
                    .workspace
                    .documents
                    .iter()
                    .find(|(_, d)| d.is_text())
                    .unwrap();
                doc.stamp(*id)
            };
            let found = services.results(&file, &stamp).diagnostics;
            println!(
                "{:>5.1}s {:?} {:?}",
                started.elapsed().as_secs_f32(),
                services
                    .infos()
                    .first()
                    .map(|i| (i.status.clone(), i.tasks.len())),
                found.as_ref().map(|d| d.len())
            );
            if found.is_some_and(|d| !d.is_empty()) {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(90),
                "no diagnostics for the current state"
            );
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}
