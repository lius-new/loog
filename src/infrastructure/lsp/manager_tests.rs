//! The coordinator against real fake-server processes (`examples/fake_lsp.rs`).
use super::manager::{Manager, Options, Snapshot, SnapshotDocument, Status};
use crate::domain::language::Registry;
use crate::features::language_service::server_set::ServerSet;
use crate::features::language_service::sync::Stamp;
use crate::features::settings::servers::LanguageServerSetting;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let plain = r#"{ "name": "plaintext", "display_name": "Plain", "badge": "T",
            "variants": [{ "id": "plaintext" }] }"#;
        let fake = r#"{ "name": "fakelang", "display_name": "Fake", "badge": "F",
            "variants": [{ "id": "fakelang" }],
            "files": [{ "extensions": ["fk"], "variant": "fakelang" }] }"#;
        Registry::from_sources(&[("plaintext", plain), ("fakelang", fake)], &[])
            .expect("test registry")
    })
}

/// A server for `fakelang`; `priority` decides which of several is preferred.
fn fake_definition(name: &str, language_id: &str, priority: i32) -> String {
    definition_running(
        &fake_server().display().to_string(),
        name,
        language_id,
        priority,
    )
}

fn definition_running(command: &str, name: &str, language_id: &str, priority: i32) -> String {
    let command = serde_json::to_string(command).unwrap();
    format!(
        r#"{{ "name": "{name}", "display_name": "{name}", "command": {command},
            "priority": {priority},
            "root": {{ "markers": ["fake.toml"], "strategy": "nearest",
                      "fallback": "file_directory" }},
            "languages": [{{ "language": "fakelang", "language_id": "{language_id}" }}] }}"#
    )
}

/// Servers like `servers`, whose program is `command`.
fn servers_running(command: &str, definitions: &[(&str, &str, i32)]) -> Arc<ServerSet> {
    let sources: Vec<(String, String)> = definitions
        .iter()
        .map(|(name, id, priority)| {
            (
                name.to_string(),
                definition_running(command, name, id, *priority),
            )
        })
        .collect();
    let borrowed: Vec<(&str, &str)> = sources
        .iter()
        .map(|(name, json)| (name.as_str(), json.as_str()))
        .collect();
    Arc::new(ServerSet::from_sources(&borrowed).expect("test servers"))
}

fn servers(definitions: &[(&str, &str, i32)]) -> Arc<ServerSet> {
    let sources: Vec<(String, String)> = definitions
        .iter()
        .map(|(name, id, priority)| (name.to_string(), fake_definition(name, id, *priority)))
        .collect();
    let borrowed: Vec<(&str, &str)> = sources
        .iter()
        .map(|(name, json)| (name.as_str(), json.as_str()))
        .collect();
    Arc::new(ServerSet::from_sources(&borrowed).expect("test servers"))
}

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

static RUNS: AtomicUsize = AtomicUsize::new(0);

mod registration {
    use super::*;
    use crate::features::language_service::{request::Feature, uri};
    use crate::infrastructure::lsp::manager::{Failure, Progress, RequestSpec};

    fn fixture(script: Value, features: &str) -> Fixture {
        let mut fixture = Fixture::new(&["--sync", "object", "--features", features]);
        let path = fixture.dir.join("registrations.json");
        std::fs::write(&path, serde_json::to_vec(&script).unwrap()).unwrap();
        fixture.settings[0].args.as_mut().unwrap().extend([
            "--registration-script".into(),
            path.to_string_lossy().into_owned(),
        ]);
        fixture
    }

    fn register(id: u64, step: u64, registrations: Value) -> Value {
        json!({"id":id,"step":step,"method":"client/registerCapability","params":{"registrations":registrations}})
    }

    fn unregister(id: u64, step: u64, name: &str, method: &str) -> Value {
        json!({"id":id,"step":step,"method":"client/unregisterCapability","params":{"unregisterations":[{"id":name,"method":method}]}})
    }

    fn answer(f: &Fixture, id: u64) -> Value {
        f.wait_for("registration reply", || {
            f.log()
                .into_iter()
                .find(|message| message["id"] == id && message.get("method").is_none())
        })
    }

    fn accepted(f: &Fixture, id: u64) {
        let reply = answer(f, id);
        assert_eq!(reply.get("result"), Some(&Value::Null), "{reply}");
    }

    fn request(f: &Fixture, feature: Feature, file: &str) -> Progress {
        let handle = f.mgr().request(RequestSpec::new(
            feature,
            f.dir.join(file),
            Stamp {
                incarnation: 1,
                version: 0,
            },
            0,
        ));
        f.wait_for("feature reply", || {
            handle.is_finished().then(|| handle.poll())
        })
    }

    fn save(f: &Fixture) {
        f.mgr().saved(f.dir.join("a.fk"), Arc::new("x".into()));
    }

    fn disk_events(f: &Fixture) -> Vec<Value> {
        f.log()
            .iter()
            .filter(|message| message["method"] == "workspace/didChangeWatchedFiles")
            .flat_map(|message| {
                message["params"]["changes"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            })
            .collect()
    }

    fn event(f: &Fixture, path: &std::path::Path, kind: u8) {
        let expected = json!({"uri":uri::file_uri(path).unwrap(),"type":kind});
        f.wait_for("watched file notification", || {
            disk_events(f).contains(&expected).then_some(())
        });
    }

    #[test]
    fn providers_are_selected_per_document_and_unregister_preserves_static_and_other_ids() {
        let f = fixture(
            json!([
                register(
                    7000,
                    0,
                    json!([
                        {"id":"a","method":"textDocument/hover","registerOptions":{"documentSelector":[{"language":"fake","scheme":"file","pattern":"a.fk"}]}},
                        {"id":"b","method":"textDocument/hover","registerOptions":{"documentSelector":[{"pattern":"b.fk"}]}}
                    ])
                ),
                unregister(7001, 1, "a", "textDocument/hover"),
                unregister(7002, 2, "b", "textDocument/hover")
            ]),
            "definition",
        );
        f.show(vec![
            f.doc("a.fk", 0, "x"),
            f.doc("b.fk", 0, "x"),
            f.doc("c.fk", 0, "x"),
        ]);
        accepted(&f, 7000);
        assert_eq!(request(&f, Feature::Hover, "a.fk").parts.len(), 1);
        assert_eq!(request(&f, Feature::Hover, "b.fk").parts.len(), 1);
        assert_eq!(
            request(&f, Feature::Hover, "c.fk").failure,
            Some(Failure::Unsupported)
        );
        save(&f);
        accepted(&f, 7001);
        assert_eq!(
            request(&f, Feature::Hover, "a.fk").failure,
            Some(Failure::Unsupported)
        );
        assert_eq!(request(&f, Feature::Hover, "b.fk").parts.len(), 1);
        assert_eq!(request(&f, Feature::Definition, "a.fk").parts.len(), 1);
        save(&f);
        accepted(&f, 7002);
        f.wait_for("registration panel clears", || {
            f.mgr().infos()[0].registrations.is_empty().then_some(())
        });
        assert!(!f.mgr().infos()[0].providers.as_ref().unwrap().hover);
        assert!(f.mgr().infos()[0].providers.as_ref().unwrap().definition);
    }

    #[test]
    fn unregister_invalidates_inflight_replies_and_unknown_methods_fail_atomically() {
        let mut unreg = unregister(7010, 99, "hover", "textDocument/hover");
        unreg["onMethod"] = json!("textDocument/hover");
        let f = fixture(
            json!([
                register(
                    7000,
                    0,
                    json!([{"id":"hover","method":"textDocument/hover"}])
                ),
                unreg,
                register(
                    7001,
                    1,
                    json!([{"id":"new","method":"textDocument/hover"},{"id":"unsupported","method":"textDocument/didOpen"}])
                ),
                register(7002, 2, json!([{"id":"new","method":"textDocument/hover"}]))
            ]),
            "",
        );
        f.show(vec![f.doc("a.fk", 0, "x")]);
        accepted(&f, 7000);
        let progress = request(&f, Feature::Hover, "a.fk");
        assert_eq!(progress.failure, Some(Failure::Stale));
        assert!(progress.parts.is_empty());
        accepted(&f, 7010);
        save(&f);
        assert_eq!(answer(&f, 7001)["error"]["code"], -32602);
        assert_eq!(
            request(&f, Feature::Hover, "a.fk").failure,
            Some(Failure::Unsupported)
        );
        save(&f);
        accepted(&f, 7002);
    }

    #[test]
    fn native_disk_events_include_closed_files_rename_git_masks_and_unregister_release() {
        let f = fixture(
            json!([
                register(
                    7000,
                    0,
                    json!([{"id":"files","method":"workspace/didChangeWatchedFiles","registerOptions":{"watchers":[
                        {"globPattern":"**/*.wat"}, {"globPattern":"**/*.wat"},
                        {"globPattern":"**/*.created","kind":1}, {"globPattern":"**/.git/config"}
                    ]}}])
                ),
                unregister(7001, 1, "files", "workspace/didChangeWatchedFiles")
            ]),
            "hover",
        );
        f.show(vec![f.doc("a.fk", 0, "unsaved")]);
        accepted(&f, 7000);
        let file = f.dir.join("closed.wat");
        std::fs::write(&file, "first").unwrap();
        event(&f, &file, 1);
        std::fs::write(&file, "changed and longer").unwrap();
        event(&f, &file, 2);
        let renamed = f.dir.join("renamed.wat");
        std::fs::rename(&file, &renamed).unwrap();
        event(&f, &file, 3);
        event(&f, &renamed, 1);
        std::fs::remove_file(&renamed).unwrap();
        event(&f, &renamed, 3);
        std::fs::create_dir(f.dir.join(".git")).unwrap();
        let config = f.dir.join(".git/config");
        std::fs::write(&config, "git").unwrap();
        event(&f, &config, 1);
        let masked = f.dir.join("a.created");
        std::fs::write(&masked, "created").unwrap();
        event(&f, &masked, 1);
        std::fs::write(&masked, "changed").unwrap();
        std::fs::remove_file(&masked).unwrap();
        save(&f);
        accepted(&f, 7001);
        let count = disk_events(&f).len();
        std::fs::write(f.dir.join("after.wat"), "no lease").unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(disk_events(&f).len(), count);
        assert!(
            disk_events(&f)
                .iter()
                .filter(|e| e["uri"] == uri::file_uri(&masked).unwrap())
                .all(|e| e["type"] == 1)
        );
        assert!(
            !f.methods()
                .iter()
                .any(|method| method == "textDocument/didChange"),
            "disk events must not synthesize unsaved document sync"
        );
        assert_eq!(
            disk_events(&f)
                .iter()
                .filter(|e| e["uri"] == uri::file_uri(&file).unwrap() && e["type"] == 1)
                .count(),
            1,
            "overlapping watches deduplicate"
        );
    }

    #[test]
    fn external_relative_roots_ack_after_setup_and_failed_setup_rolls_back_all_providers() {
        // A sibling tree exercises an extra lease outside the instance root.
        let outside = std::env::temp_dir().join(format!(
            "loom-extra-{}-{}",
            std::process::id(),
            RUNS.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&outside).unwrap();
        let base = uri::file_uri(&outside).unwrap();
        let f = fixture(
            json!([
                register(
                    7000,
                    0,
                    json!([
                        {"id":"hover","method":"textDocument/hover"},
                        {"id":"bad","method":"workspace/didChangeWatchedFiles","registerOptions":{"watchers":[{"globPattern":{"baseUri":uri::file_uri(&outside.join("missing")).unwrap(),"pattern":"**"}}]}}
                    ])
                ),
                register(
                    7001,
                    1,
                    json!([{"id":"extra","method":"workspace/didChangeWatchedFiles","registerOptions":{"watchers":[{"globPattern":{"baseUri":{"uri":base,"name":"extra"},"pattern":"**/*.wat"}}]}}])
                ),
                unregister(7002, 2, "extra", "workspace/didChangeWatchedFiles")
            ]),
            "",
        );
        f.show(vec![f.doc("a.fk", 0, "x")]);
        assert_eq!(answer(&f, 7000)["error"]["code"], -32603);
        assert_eq!(
            request(&f, Feature::Hover, "a.fk").failure,
            Some(Failure::Unsupported)
        );
        assert!(f.mgr().infos()[0].registrations.is_empty());
        save(&f);
        accepted(&f, 7001);
        let path = outside.join("closed.wat");
        std::fs::write(&path, "external").unwrap();
        event(&f, &path, 1);
        save(&f);
        accepted(&f, 7002);
        let count = disk_events(&f).len();
        std::fs::write(&path, "after unregistration").unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(disk_events(&f).len(), count);
        drop(f);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn restarting_a_server_discards_its_old_registrations_and_watch_callbacks() {
        let mut f = fixture(
            json!([register(
                7000,
                0,
                json!([
                    {"id":"hover","method":"textDocument/hover"},
                    {"id":"files","method":"workspace/didChangeWatchedFiles","registerOptions":{"watchers":[{"globPattern":"**/*.wat"}]}}
                ])
            )]),
            "",
        );
        f.show(vec![f.doc("a.fk", 0, "x")]);
        accepted(&f, 7000);
        let old = f.mgr().infos()[0].run;
        std::fs::write(f.dir.join("registrations.json"), "[]").unwrap();
        // The settings are unchanged, so only the explicit restart creates a run.
        f.mgr().restart();
        f.wait_for("new run", || {
            f.mgr()
                .infos()
                .into_iter()
                .find(|i| i.run != old && i.status == Status::Ready)
        });
        assert!(f.mgr().infos()[0].registrations.is_empty());
        assert_eq!(
            request(&f, Feature::Hover, "a.fk").failure,
            Some(Failure::Unsupported)
        );
        let count = disk_events(&f).len();
        std::fs::write(f.dir.join("new-run.wat"), "x").unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(disk_events(&f).len(), count);
        // Stop before the fixture's filesystem cleanup.
        drop(f.manager.take());
    }

    #[test]
    fn failed_watch_reconfiguration_resumes_the_previous_lease() {
        let missing = uri::file_uri(&std::env::temp_dir().join(format!(
            "loom-no-watch-{}-{}",
            std::process::id(),
            RUNS.fetch_add(1, Ordering::Relaxed)
        )))
        .unwrap();
        let f = fixture(
            json!([
                register(
                    7000,
                    0,
                    json!([{"id":"existing","method":"workspace/didChangeWatchedFiles","registerOptions":{"watchers":[{"globPattern":"**/*.wat"}]}}])
                ),
                register(
                    7001,
                    1,
                    json!([{"id":"bad","method":"workspace/didChangeWatchedFiles","registerOptions":{"watchers":[{"globPattern":{"baseUri":missing,"pattern":"**"}}]}}])
                )
            ]),
            "",
        );
        f.show(vec![f.doc("a.fk", 0, "x")]);
        accepted(&f, 7000);
        save(&f);
        assert_eq!(answer(&f, 7001)["error"]["code"], -32603);
        let path = f.dir.join("after-rollback.wat");
        std::fs::write(&path, "still watching").unwrap();
        event(&f, &path, 1);
        assert_eq!(f.mgr().infos()[0].registrations.len(), 1);
    }

    #[test]
    fn reusing_a_registration_id_replaces_its_watch_options_transactionally() {
        let f = fixture(
            json!([
                register(
                    7000,
                    0,
                    json!([{"id":"files","method":"workspace/didChangeWatchedFiles","registerOptions":{"watchers":[{"globPattern":"**/*.old"}]}}])
                ),
                register(
                    7001,
                    1,
                    json!([{"id":"files","method":"workspace/didChangeWatchedFiles","registerOptions":{"watchers":[{"globPattern":"**/*.new"}]}}])
                )
            ]),
            "",
        );
        f.show(vec![f.doc("a.fk", 0, "x")]);
        accepted(&f, 7000);
        let old = f.dir.join("before.old");
        std::fs::write(&old, "old").unwrap();
        event(&f, &old, 1);
        save(&f);
        accepted(&f, 7001);
        let new = f.dir.join("after.new");
        std::fs::write(&new, "new").unwrap();
        event(&f, &new, 1);
        std::fs::write(f.dir.join("ignored.old"), "ignored").unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(f.mgr().infos()[0].registrations.len(), 1);
        assert!(
            !disk_events(&f)
                .iter()
                .any(|event| event["uri"] == uri::file_uri(&f.dir.join("ignored.old")).unwrap())
        );
    }

    #[test]
    fn dynamic_watch_subscriptions_are_isolated_between_two_roots() {
        let f = fixture(
            json!([register(
                7000,
                0,
                json!([{"id":"files","method":"workspace/didChangeWatchedFiles","registerOptions":{"watchers":[{"globPattern":"**/*.wat"}]}}])
            )]),
            "",
        );
        for root in ["first", "second"] {
            std::fs::create_dir(f.dir.join(root)).unwrap();
        }
        f.show(vec![
            f.doc("first/a.fk", 0, "x"),
            f.doc("second/a.fk", 0, "x"),
        ]);
        f.wait_for("two watcher registrations", || {
            (f.log()
                .iter()
                .filter(|message| {
                    message["id"] == 7000 && message.get("result") == Some(&Value::Null)
                })
                .count()
                == 2)
                .then_some(())
        });
        let path = f.dir.join("first/closed.wat");
        std::fs::write(&path, "one root").unwrap();
        event(&f, &path, 1);
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(
            disk_events(&f)
                .iter()
                .filter(|event| event["uri"] == uri::file_uri(&path).unwrap())
                .count(),
            1
        );
    }
}

struct Fixture {
    manager: Option<Manager>,
    dir: PathBuf,
    /// The first server's log; `logs` has one for each server, in order.
    log: PathBuf,
    logs: Vec<PathBuf>,
    settings: Vec<LanguageServerSetting>,
    changes: Arc<AtomicUsize>,
}

fn options() -> Options {
    Options {
        timeouts: crate::infrastructure::lsp::client::Timeouts {
            initialize: Duration::from_secs(10),
            request: Duration::from_secs(5),
            exit_grace: Duration::from_millis(500),
        },
        idle_stop: Duration::from_millis(600),
        backoff: vec![Duration::from_millis(50), Duration::from_millis(50)],
        stable_after: Duration::from_secs(60),
        progress_timeout: Duration::from_secs(300),
        cancel_grace: Duration::from_millis(150),
        start_gate: None,
    }
}

impl Fixture {
    fn mgr(&self) -> &Manager {
        self.manager.as_ref().unwrap()
    }

    fn new(extra_args: &[&str]) -> Self {
        Self::with_servers(
            servers(&[("fake-server", "fake", 0)]),
            &[("fake-server", extra_args)],
        )
    }

    /// Runs the given servers, each with its own arguments and its own log.
    fn with_servers(servers: Arc<ServerSet>, runs: &[(&str, &[&str])]) -> Self {
        Self::with_options(servers, runs, options())
    }

    fn with_options(
        servers: Arc<ServerSet>,
        runs: &[(&str, &[&str])],
        manager_options: Options,
    ) -> Self {
        let n = RUNS.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("loom-mgr-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut logs = Vec::new();
        let mut settings = Vec::new();
        for (name, extra_args) in runs {
            let log = dir.join(format!("{name}.log"));
            let mut args: Vec<String> = if extra_args.contains(&"--sync") {
                Vec::new()
            } else {
                ["--sync", "full"].map(String::from).into()
            };
            args.extend(extra_args.iter().map(|a| a.to_string()));
            args.extend(["--log".into(), log.display().to_string()]);
            settings.push(LanguageServerSetting {
                args: Some(args),
                ..LanguageServerSetting::named(name)
            });
            logs.push(log);
        }
        let changes = Arc::new(AtomicUsize::new(0));
        let counter = changes.clone();
        let manager = Manager::start(registry(), servers, None, manager_options, move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        Self {
            manager: Some(manager),
            dir,
            log: logs[0].clone(),
            logs,
            settings,
            changes,
        }
    }

    fn doc(&self, name: &str, version: u64, text: &str) -> SnapshotDocument {
        let path = self.dir.join(name);
        SnapshotDocument {
            language: registry().resolve_path(&path),
            path,
            stamp: Stamp {
                incarnation: 1,
                version,
            },
            text: Arc::new(text.to_owned()),
        }
    }

    fn show(&self, documents: Vec<SnapshotDocument>) {
        self.mgr().update(Snapshot {
            languages: Default::default(),
            documents,
            workspace_roots: vec![self.dir.clone()],
            servers: self.settings.clone(),
        });
    }

    fn wait_for<T>(&self, what: &str, mut check: impl FnMut() -> Option<T>) -> T {
        let limit = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(found) = check() {
                return found;
            }
            assert!(
                Instant::now() < limit,
                "timed out waiting for {what}; infos {:?}",
                self.mgr().infos()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_status(&self, what: &str, want: impl Fn(&Status) -> bool) {
        self.wait_for(what, || {
            self.mgr()
                .infos()
                .iter()
                .any(|i| want(&i.status))
                .then_some(())
        });
    }

    fn log(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn methods(&self) -> Vec<String> {
        self.log()
            .iter()
            .filter_map(|m| m["method"].as_str().map(str::to_owned))
            .collect()
    }

    fn wait_method_count(&self, method: &str, count: usize) {
        self.wait_for(&format!("{count} x {method}"), || {
            (self.methods().iter().filter(|m| *m == method).count() >= count).then_some(())
        });
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.manager.take());
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn a_document_starts_its_server_and_is_opened_once_ready() {
    let f = Fixture::new(&[]);
    f.show(vec![f.doc("a.fk", 0, "hello")]);
    f.wait_status("ready", |s| *s == Status::Ready);
    f.wait_method_count("textDocument/didOpen", 1);
    let open = f
        .log()
        .into_iter()
        .find(|m| m["method"] == "textDocument/didOpen")
        .unwrap();
    assert_eq!(open["params"]["textDocument"]["languageId"], "fake");
    assert_eq!(open["params"]["textDocument"]["text"], "hello");
    assert!(
        f.changes.load(Ordering::Relaxed) >= 1,
        "status changes are announced"
    );
    let info = &f.mgr().infos()[0];
    assert_eq!(info.documents, 1);
    assert_eq!(info.root, f.dir, "no marker: the file's own folder");
}

#[test]
fn edits_and_closes_follow_the_snapshots() {
    let f = Fixture::new(&[]);
    f.show(vec![f.doc("a.fk", 0, "one"), f.doc("b.fk", 0, "bee")]);
    f.wait_method_count("textDocument/didOpen", 2);
    f.show(vec![f.doc("a.fk", 4, "one two"), f.doc("b.fk", 0, "bee")]);
    f.wait_method_count("textDocument/didChange", 1);
    let change = f
        .log()
        .into_iter()
        .find(|m| m["method"] == "textDocument/didChange")
        .unwrap();
    assert_eq!(
        change["params"]["contentChanges"],
        json!([{ "text": "one two" }])
    );
    assert_eq!(change["params"]["textDocument"]["version"], 3);
    // Same snapshot again: nothing new is sent.
    f.show(vec![f.doc("a.fk", 4, "one two"), f.doc("b.fk", 0, "bee")]);
    f.show(vec![f.doc("a.fk", 4, "one two")]);
    f.wait_method_count("textDocument/didClose", 1);
    assert_eq!(
        f.methods()
            .iter()
            .filter(|m| *m == "textDocument/didChange")
            .count(),
        1
    );
}

#[test]
fn files_the_server_does_not_serve_start_nothing() {
    let f = Fixture::new(&[]);
    f.show(vec![f.doc("notes.txt", 0, "plain")]);
    std::thread::sleep(Duration::from_millis(300));
    assert!(f.mgr().infos().is_empty());
    assert!(f.methods().is_empty());
}

#[test]
fn a_marker_file_chooses_the_project_root() {
    let f = Fixture::new(&[]);
    let project = f.dir.join("proj");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("fake.toml"), "").unwrap();
    f.show(vec![f.doc("proj/src/a.fk", 0, "x")]);
    f.wait_status("ready", |s| *s == Status::Ready);
    assert_eq!(f.mgr().infos()[0].root, project);
}

#[test]
fn an_untrusted_root_never_starts_its_server_until_the_gate_opens() {
    let open = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let gate_open = open.clone();
    let gate = crate::infrastructure::lsp::manager::StartGate(Arc::new(move |root| {
        if gate_open.load(Ordering::Relaxed) {
            Ok(())
        } else {
            Err(format!("{} is not trusted", root.display()))
        }
    }));
    let f = Fixture::with_options(
        servers(&[("fake-server", "fake", 0)]),
        &[("fake-server", &[])],
        Options {
            start_gate: Some(gate),
            ..options()
        },
    );
    f.show(vec![f.doc("a.fk", 0, "hello")]);
    f.wait_status(
        "refused",
        |s| matches!(s, Status::Unavailable(m) if m.contains("not trusted")),
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        f.methods().is_empty(),
        "no process ran, so nothing was logged"
    );

    // Trusting the folder and restarting lets the same document start its server.
    open.store(true, Ordering::Relaxed);
    f.mgr().restart();
    f.wait_status("ready", |s| *s == Status::Ready);
    f.wait_method_count("textDocument/didOpen", 1);
}

#[test]
fn a_server_an_extension_declares_runs_only_where_it_is_allowed_and_stops_when_withdrawn() {
    use crate::infrastructure::lsp::manager::Contributed;
    let n = RUNS.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("loom-ext-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut definition: Value = serde_json::from_str(&definition_running(
        &fake_server().display().to_string(),
        "ext-fake",
        "fake",
        0,
    ))
    .unwrap();
    definition["args"] = json!([
        "--sync",
        "full",
        "--log",
        dir.join("ext.log").display().to_string()
    ]);
    let declared = |allowed: Vec<PathBuf>| {
        vec![Contributed {
            name: "ext-fake".into(),
            definition: definition.clone(),
            allowed_roots: allowed,
            refusal: "acme.fake: running programs here has not been allowed".into(),
        }]
    };
    let manager = Manager::start(registry(), servers(&[]), None, options(), || {});
    let wait =
        |what: &str,
         check: &dyn Fn(&[crate::infrastructure::lsp::manager::InstanceInfo]) -> bool| {
            let limit = Instant::now() + Duration::from_secs(15);
            while !check(&manager.infos()) {
                assert!(
                    Instant::now() < limit,
                    "timed out waiting for {what}: {:?}",
                    manager.infos()
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        };
    let path = dir.join("a.fk");
    manager.update(Snapshot {
        documents: vec![SnapshotDocument {
            language: registry().resolve_path(&path),
            path,
            stamp: Stamp {
                incarnation: 1,
                version: 0,
            },
            text: Arc::new("x".to_owned()),
        }],
        workspace_roots: vec![dir.clone()],
        servers: Vec::new(),
        languages: Default::default(),
    });
    std::thread::sleep(Duration::from_millis(200));
    assert!(manager.infos().is_empty(), "nothing declares a server yet");

    // Declared but no folder is allowed: the instance says why and runs nothing.
    manager.set_contributed(declared(Vec::new()));
    wait("the refusal", &|infos| {
        infos.iter().any(
            |i| matches!(&i.status, Status::Unavailable(m) if m.contains("has not been allowed")),
        )
    });
    assert!(!dir.join("ext.log").exists(), "no process ran");

    // Allowing the folder starts the same server for the same document.
    manager.set_contributed(declared(vec![dir.clone()]));
    wait("ready", &|infos| {
        infos.iter().any(|i| i.status == Status::Ready)
    });

    // Withdrawing it stops the process at once, without waiting for the idle time.
    manager.set_contributed(Vec::new());
    wait("the instance to be gone", &|infos| infos.is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_extension_server_cannot_take_the_name_of_another_server() {
    use crate::infrastructure::lsp::manager::Contributed;
    let f = Fixture::new(&[]);
    // Same name as the catalogued test server, so the declaration is left out and
    // the catalogued one keeps serving.
    f.mgr().set_contributed(vec![Contributed {
        name: "fake-server".into(),
        definition: serde_json::from_str(&definition_running(
            "/bin/does-not-exist",
            "fake-server",
            "fake",
            0,
        ))
        .unwrap(),
        allowed_roots: Vec::new(),
        refusal: "never shown".into(),
    }]);
    f.show(vec![f.doc("a.fk", 0, "hello")]);
    f.wait_status("ready", |s| *s == Status::Ready);
}

#[test]
fn findings_from_an_extension_show_for_their_state_survive_server_housekeeping_and_can_be_withdrawn()
 {
    use crate::features::language_service::diagnostics::{Diagnostic, Severity};
    let f = Fixture::new(&[]);
    let document = f.doc("ext.fk", 3, "hello TODO");
    f.show(vec![document.clone()]);
    f.wait_status("ready", |s| *s == Status::Ready);

    let source = crate::infrastructure::lsp::manager::EXTERNAL_SOURCE | 7;
    let finding = Diagnostic {
        range: 6..10,
        severity: Severity::Warning,
        message: "TODO found".into(),
        source: Some("example.tools.todo".into()),
        code: None,
        extra: serde_json::Value::Null,
    };
    f.mgr().publish_external(
        source,
        &document.path,
        document.stamp,
        document.text.clone(),
        vec![finding.clone()].into(),
    );
    let shown = f.wait_for("the extension finding", || {
        f.mgr().results(&document.path, document.stamp).diagnostics
    });
    assert_eq!(shown.as_ref(), std::slice::from_ref(&finding));

    // For any other state of the document it is not shown as current.
    let later = crate::features::language_service::sync::Stamp {
        incarnation: document.stamp.incarnation,
        version: document.stamp.version + 1,
    };
    assert!(f.mgr().results(&document.path, later).diagnostics.is_none());

    // Server bookkeeping (documents changing hands) does not drop it.
    f.show(vec![f.doc("ext.fk", 3, "hello TODO")]);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        f.mgr()
            .results(&document.path, document.stamp)
            .diagnostics
            .is_some()
    );

    f.mgr().clear_external(source);
    assert!(
        f.mgr()
            .results(&document.path, document.stamp)
            .diagnostics
            .is_none()
    );
}

#[test]
fn a_missing_program_is_unavailable_and_not_retried() {
    let missing = std::env::temp_dir().join("loom-no-such-server");
    let f = Fixture::with_servers(
        servers_running(
            &missing.display().to_string(),
            &[("fake-server", "fake", 0)],
        ),
        &[("fake-server", &[])],
    );
    f.show(vec![f.doc("a.fk", 0, "x")]);
    f.wait_status(
        "unavailable",
        |s| matches!(s, Status::Unavailable(m) if m.contains("not found")),
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(matches!(f.mgr().infos()[0].status, Status::Unavailable(_)));
}

#[test]
fn a_command_from_an_uninstalled_pack_is_reported_and_never_started() {
    let f = Fixture::with_servers(
        servers_running("${pack}/tool", &[("fake-server", "fake", 0)]),
        &[("fake-server", &[])],
    );
    f.show(vec![f.doc("a.fk", 0, "x")]);
    f.wait_status(
        "not installed",
        |s| matches!(s, Status::Unavailable(m) if m.contains("not installed")),
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(f.methods().is_empty(), "nothing was started");
}

#[test]
fn a_crashing_server_is_restarted_then_given_up_on() {
    let f = Fixture::new(&["--crash-after-init"]);
    f.show(vec![f.doc("a.fk", 0, "x")]);
    // Initial run plus two restarts: three initialize requests in all.
    f.wait_status("failed", |s| matches!(s, Status::Failed(_)));
    let initializes = f.methods().iter().filter(|m| *m == "initialize").count();
    assert_eq!(initializes, 3, "{:?}", f.methods());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        f.methods().iter().filter(|m| *m == "initialize").count(),
        3,
        "no further tries"
    );
}

#[test]
fn restart_revives_a_failed_server_and_reopens_documents() {
    let f = Fixture::new(&["--crash-after-init"]);
    f.show(vec![f.doc("a.fk", 0, "x")]);
    f.wait_status("failed", |s| matches!(s, Status::Failed(_)));
    f.mgr().restart();
    f.wait_for("a fourth initialize", || {
        (f.methods().iter().filter(|m| *m == "initialize").count() >= 4).then_some(())
    });
}

#[test]
fn an_idle_server_is_stopped_and_exits_cleanly() {
    let f = Fixture::new(&[]);
    f.show(vec![f.doc("a.fk", 0, "x")]);
    f.wait_status("ready", |s| *s == Status::Ready);
    f.show(vec![]);
    f.wait_for("the instance to go", || {
        f.mgr().infos().is_empty().then_some(())
    });
    f.wait_method_count("exit", 1);
    assert!(f.methods().contains(&"shutdown".to_owned()));
}

#[test]
fn changing_the_launch_replaces_the_instance() {
    let mut f = Fixture::new(&[]);
    f.show(vec![f.doc("a.fk", 0, "x")]);
    f.wait_status("ready", |s| *s == Status::Ready);
    f.settings[0]
        .args
        .as_mut()
        .unwrap()
        .push("--stderr-noise".into());
    f.show(vec![f.doc("a.fk", 0, "x")]);
    f.wait_method_count("initialize", 2);
    f.wait_method_count("textDocument/didOpen", 2);
}

#[test]
fn turning_the_server_off_stops_it() {
    let mut f = Fixture::new(&[]);
    f.show(vec![f.doc("a.fk", 0, "x")]);
    f.wait_status("ready", |s| *s == Status::Ready);
    f.settings[0].enabled = false;
    f.show(vec![f.doc("a.fk", 0, "x")]);
    f.wait_for("the instance to go", || {
        f.mgr().infos().is_empty().then_some(())
    });
}

#[test]
fn saving_sends_did_save_to_servers_that_want_it() {
    let f = Fixture::new(&["--sync", "object"]);
    f.show(vec![f.doc("a.fk", 0, "x")]);
    f.wait_method_count("textDocument/didOpen", 1);
    f.mgr().saved(f.dir.join("a.fk"), Arc::new("x".into()));
    f.wait_method_count("textDocument/didSave", 1);
    let save = f
        .log()
        .into_iter()
        .find(|m| m["method"] == "textDocument/didSave")
        .unwrap();
    assert_eq!(save["params"]["text"], "x", "the fake asks for the text");
}

#[test]
fn dropping_the_manager_shuts_servers_down() {
    let f = Fixture::new(&[]);
    f.show(vec![f.doc("a.fk", 0, "x")]);
    f.wait_status("ready", |s| *s == Status::Ready);
    let mut f = f;
    drop(f.manager.take());
    let log = f.log.clone();
    let methods: Vec<String> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|m| m["method"].as_str().map(str::to_owned))
        .collect();
    assert!(
        methods.contains(&"textDocument/didClose".to_owned()),
        "{methods:?}"
    );
    assert!(methods.contains(&"shutdown".to_owned()), "{methods:?}");
    assert!(methods.contains(&"exit".to_owned()), "{methods:?}");
}

impl Fixture {
    fn results_at(&self, name: &str, version: u64) -> super::manager::DocumentResults {
        self.mgr().results(
            &self.dir.join(name),
            Stamp {
                incarnation: 1,
                version,
            },
        )
    }
}

#[test]
fn versioned_diagnostics_apply_to_the_state_they_describe() {
    let f = Fixture::new(&["--diagnostics"]);
    f.show(vec![f.doc("a.fk", 0, "fn a")]);
    let found = f.wait_for("diagnostics", || f.results_at("a.fk", 0).diagnostics);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].range, 0..2);
    assert_eq!(found[0].message, "fake error");

    // After an edit the old state's diagnostics no longer apply, and the server's
    // answer for the new version is attached to the new state.
    f.show(vec![f.doc("a.fk", 5, "fn b")]);
    f.wait_for("the new version's publication", || {
        (f.mgr().infos()[0].diagnostics.published == 2).then_some(())
    });
    assert!(f.results_at("a.fk", 5).diagnostics.is_some());
    assert!(f.results_at("a.fk", 0).diagnostics.is_none());
    assert!(
        f.results_at("a.fk", 4).diagnostics.is_none(),
        "never for a state it was not computed for"
    );
}

#[test]
fn reopened_documents_discard_diagnostics_from_their_previous_lifetime() {
    for close_first in [true, false] {
        let f = Fixture::new(&[
            "--sync",
            "object",
            "--diagnostics",
            "--diag-defer-until-save",
        ]);
        // Keep the server alive throughout, even when the tested document closes.
        let other = f.doc("keep.fk", 0, "fn keep");
        let old = f.doc("a.fk", 0, "fn a");
        f.show(vec![old.clone(), other.clone()]);
        f.wait_method_count("textDocument/didOpen", 2);
        let process = f.mgr().infos()[0].process_id;
        if close_first {
            f.show(vec![other.clone()]);
            f.wait_method_count("textDocument/didClose", 1);
        }
        let mut reopened = old.clone();
        reopened.stamp.incarnation += 1;
        // Identical text must not let a diagnostic cross document lifetimes.
        f.show(vec![reopened.clone(), other]);
        f.wait_method_count("textDocument/didOpen", 3);
        assert_eq!(f.mgr().infos()[0].diagnostics.published, 0);
        f.mgr().saved(reopened.path.clone(), reopened.text.clone());
        let counts = f.wait_for("all deferred diagnostics", || {
            let counts = f.mgr().infos().first()?.diagnostics;
            (counts.published == 3).then_some(counts)
        });
        assert_eq!(counts.outdated, 1, "close_first={close_first}: {counts:?}");
        assert_eq!(counts.shown, 2, "only the two current documents");
        assert_eq!(
            counts.carried, 0,
            "old text must not be carried into a new life"
        );
        assert_eq!(f.mgr().infos()[0].process_id, process);
        let found = f
            .mgr()
            .results(&reopened.path, reopened.stamp)
            .diagnostics
            .unwrap();
        assert_eq!(found.len(), 1, "the current publication still arrives");
        assert!(f.mgr().results(&old.path, old.stamp).diagnostics.is_none());
    }
}

#[test]
fn reopening_does_not_project_cached_diagnostics_into_the_new_lifetime() {
    let f = Fixture::new(&[
        "--sync",
        "object",
        "--diagnostics",
        "--diag-defer-until-save",
    ]);
    let old = f.doc("a.fk", 0, "fn a");
    f.show(vec![old.clone()]);
    f.wait_method_count("textDocument/didOpen", 1);
    f.mgr().saved(old.path.clone(), old.text.clone());
    f.wait_for("the cached diagnostic", || {
        f.mgr().results(&old.path, old.stamp).diagnostics
    });

    let mut reopened = old.clone();
    reopened.stamp.incarnation += 1;
    // Replacing a lifetime in one snapshot must clear the cached result even
    // when no closed snapshot was published and the text is exactly the same.
    f.show(vec![reopened.clone()]);
    f.wait_method_count("textDocument/didOpen", 2);
    assert!(
        f.mgr()
            .results(&reopened.path, reopened.stamp)
            .diagnostics
            .is_none()
    );
    f.mgr().saved(reopened.path.clone(), reopened.text.clone());
    f.wait_for("the new lifetime's diagnostic", || {
        f.mgr().results(&reopened.path, reopened.stamp).diagnostics
    });
}

#[test]
fn unversioned_diagnostics_apply_to_the_latest_state_only() {
    let f = Fixture::new(&["--diagnostics", "--diag-unversioned"]);
    f.show(vec![f.doc("a.fk", 0, "fn a")]);
    f.wait_for("diagnostics", || f.results_at("a.fk", 0).diagnostics);
    f.show(vec![f.doc("a.fk", 1, "fn ab")]);
    f.wait_for("diagnostics for the new state", || {
        f.results_at("a.fk", 1).diagnostics
    });
}

#[test]
fn semantic_tokens_are_requested_after_a_pause_and_decoded_for_their_state() {
    let f = Fixture::new(&["--semantic", "full"]);
    f.show(vec![f.doc("a.fk", 0, "fn a")]);
    let spans = f.wait_for("semantic tokens", || f.results_at("a.fk", 0).semantic);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].range, 0..2);
    assert_eq!(spans[0].class.map(|c| c.name()), Some("keyword"));
    f.show(vec![f.doc("a.fk", 3, "fn c")]);
    f.wait_for("tokens for the edit", || f.results_at("a.fk", 3).semantic);
    assert!(f.results_at("a.fk", 2).semantic.is_none());
}

#[test]
fn rapid_edits_are_one_request() {
    let f = Fixture::new(&["--semantic", "full"]);
    f.show(vec![f.doc("a.fk", 0, "fn a")]);
    f.wait_for("first tokens", || f.results_at("a.fk", 0).semantic);
    for v in 1..=8 {
        f.show(vec![f.doc("a.fk", v, "fn x")]);
    }
    f.wait_for("last tokens", || f.results_at("a.fk", 8).semantic);
    let requests = f
        .methods()
        .iter()
        .filter(|m| *m == "textDocument/semanticTokens/full")
        .count();
    assert!(requests <= 3, "debounced, got {requests}");
}

#[test]
fn a_null_answer_clears_the_layer() {
    let f = Fixture::new(&["--semantic", "full", "--semantic-null"]);
    f.show(vec![f.doc("a.fk", 0, "fn a")]);
    let spans = f.wait_for("an empty layer", || f.results_at("a.fk", 0).semantic);
    assert!(spans.is_empty());
}

#[test]
fn a_range_only_server_gets_a_stamped_document_coverage_request() {
    let f = Fixture::new(&["--semantic", "range"]);
    f.show(vec![f.doc("a.fk", 0, "fn a")]);
    f.wait_method_count("textDocument/didOpen", 1);
    f.wait_method_count("textDocument/semanticTokens/range", 1);
    assert!(
        !f.methods()
            .contains(&"textDocument/semanticTokens/full".to_owned())
    );
    let spans = f.wait_for("range tokens", || f.results_at("a.fk", 0).semantic);
    assert_eq!(spans[0].range, 0..2);
    f.show(vec![f.doc("a.fk", 1, "fn abc")]);
    f.wait_method_count("textDocument/semanticTokens/range", 2);
    assert!(f.results_at("a.fk", 0).semantic.is_none());
    f.wait_for("new range coverage", || f.results_at("a.fk", 1).semantic);
}

#[test]
fn a_refresh_request_asks_again_even_though_the_text_is_unchanged() {
    let f = Fixture::new(&[
        "--semantic",
        "full",
        "--sync",
        "object",
        "--refresh-on-save",
    ]);
    f.show(vec![f.doc("a.fk", 0, "fn a")]);
    f.wait_method_count("textDocument/semanticTokens/full", 1);
    f.mgr().saved(f.dir.join("a.fk"), Arc::new("fn a".into()));
    f.wait_method_count("textDocument/semanticTokens/full", 2);
}

#[test]
fn results_vanish_with_the_document_and_the_server() {
    let f = Fixture::new(&["--diagnostics", "--semantic", "full"]);
    f.show(vec![f.doc("a.fk", 0, "fn a")]);
    f.wait_for("both layers", || {
        let r = f.results_at("a.fk", 0);
        (r.diagnostics.is_some() && r.semantic.is_some()).then_some(())
    });
    f.show(vec![]);
    f.wait_for("results to go", || {
        let r = f.results_at("a.fk", 0);
        (r.diagnostics.is_none() && r.semantic.is_none()).then_some(())
    });
}

impl Fixture {
    /// The messages a server logged, by its position in the run list.
    fn methods_of(&self, server: usize) -> Vec<Value> {
        std::fs::read_to_string(&self.logs[server])
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn wait_ready(&self, count: usize) {
        self.wait_for(&format!("{count} ready instances"), || {
            let infos = self.mgr().infos();
            (infos.iter().filter(|i| i.status == Status::Ready).count() == count).then_some(())
        });
    }
}

fn two_servers() -> Arc<ServerSet> {
    servers(&[("alpha", "alpha-id", 10), ("beta", "beta-id", 0)])
}

#[test]
fn a_document_is_held_by_every_server_of_its_language_with_its_own_language_id() {
    let f = Fixture::with_servers(two_servers(), &[("alpha", &[]), ("beta", &[])]);
    f.show(vec![f.doc("a.fk", 0, "hello")]);
    f.wait_ready(2);
    f.wait_for("both servers to open the document", || {
        (0..2)
            .all(|n| {
                f.methods_of(n)
                    .iter()
                    .any(|m| m["method"] == "textDocument/didOpen")
            })
            .then_some(())
    });
    for (n, id) in [(0, "alpha-id"), (1, "beta-id")] {
        let open = f
            .methods_of(n)
            .into_iter()
            .find(|m| m["method"] == "textDocument/didOpen")
            .unwrap();
        assert_eq!(open["params"]["textDocument"]["languageId"], id);
        assert_eq!(open["params"]["textDocument"]["text"], "hello");
    }
    let infos = f.mgr().infos();
    assert_eq!(infos.len(), 2);
    assert!(infos.iter().all(|i| i.documents == 1));
}

#[test]
fn diagnostics_of_every_server_are_merged_and_cleared_independently() {
    let mut f = Fixture::with_servers(
        two_servers(),
        &[
            ("alpha", &["--diagnostics", "--diag-message", "alpha says"]),
            ("beta", &["--diagnostics", "--diag-message", "beta says"]),
        ],
    );
    f.show(vec![f.doc("a.fk", 0, "fn a")]);
    let found = f.wait_for("both servers' diagnostics", || {
        f.results_at("a.fk", 0)
            .diagnostics
            .filter(|found| found.len() == 2)
    });
    let messages: Vec<_> = found.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(messages, ["alpha says", "beta says"], "the servers' order");

    // Turning one server off takes only its findings away.
    f.settings[1].enabled = false;
    f.show(vec![f.doc("a.fk", 0, "fn a")]);
    let found = f.wait_for("only alpha's diagnostics", || {
        f.results_at("a.fk", 0)
            .diagnostics
            .filter(|found| found.len() == 1)
    });
    assert_eq!(found[0].message, "alpha says");
}

#[test]
fn a_server_that_keeps_crashing_does_not_take_the_other_down() {
    let f = Fixture::with_servers(
        two_servers(),
        &[
            ("alpha", &["--crash-after-init"]),
            ("beta", &["--diagnostics", "--diag-message", "beta says"]),
        ],
    );
    f.show(vec![f.doc("a.fk", 0, "fn a")]);
    f.wait_status("alpha to give up", |s| matches!(s, Status::Failed(_)));
    let found = f.wait_for("beta's diagnostics", || f.results_at("a.fk", 0).diagnostics);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].message, "beta says");
    assert!(
        f.mgr()
            .infos()
            .iter()
            .any(|i| i.server == "beta" && i.status == Status::Ready)
    );
}

#[test]
fn semantic_tokens_come_from_the_preferred_server_and_fall_back_when_it_goes() {
    let mut f = Fixture::with_servers(
        two_servers(),
        &[
            ("alpha", &["--semantic", "full", "--semantic-length", "3"]),
            ("beta", &["--semantic", "full", "--semantic-length", "2"]),
        ],
    );
    f.show(vec![f.doc("a.fk", 0, "fn a")]);
    f.wait_for("alpha's tokens to win", || {
        let spans = f.results_at("a.fk", 0).semantic?;
        (spans.len() == 1 && spans[0].range == (0..3)).then_some(())
    });
    f.settings[0].enabled = false;
    f.show(vec![f.doc("a.fk", 0, "fn a")]);
    f.wait_for("beta's tokens once alpha is gone", || {
        let spans = f.results_at("a.fk", 0).semantic?;
        (spans.len() == 1 && spans[0].range == (0..2)).then_some(())
    });
}

/// Needs a real rust-analyzer (set `LOOM_RUST_ANALYZER` to its path, or have it on
/// PATH) and cargo. Run with `cargo test --bin Loom real_rust_analyzer -- --ignored --nocapture`.
///
/// It records the facts P4b-3 asks for: the server's version, and whether it
/// notices a file created on disk while that file is not open in the editor.
#[test]
#[ignore = "needs rust-analyzer"]
fn real_rust_analyzer_sees_files_changed_outside_the_editor() {
    let program = std::env::var("LOOM_RUST_ANALYZER").unwrap_or_else(|_| "rust-analyzer".into());
    let version = std::process::Command::new(&program)
        .arg("--version")
        .output();
    println!(
        "rust-analyzer: {:?}",
        version.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
    );

    let dir = std::env::temp_dir().join(format!("loom-ra-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"smoke\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    let lib = "mod other;\npub fn f() {}\n";
    std::fs::write(dir.join("src/lib.rs"), lib).unwrap();

    let manager = Manager::start(
        Registry::builtin(),
        crate::features::language_service::server_set::rust_analyzer_running(&program),
        None,
        Options::default(),
        || {},
    );
    let path = dir.join("src/lib.rs");
    let document = SnapshotDocument {
        language: Registry::builtin().resolve_path(&path),
        path: path.clone(),
        stamp: Stamp {
            incarnation: 1,
            version: 0,
        },
        text: Arc::new(lib.to_owned()),
    };
    let servers = vec![LanguageServerSetting::named("rust-analyzer")];
    manager.update(Snapshot {
        languages: Default::default(),
        documents: vec![document],
        workspace_roots: vec![dir.clone()],
        servers,
    });

    let wait = |what: &str, check: &dyn Fn() -> bool| {
        let limit = Instant::now() + Duration::from_secs(180);
        while !check() {
            assert!(
                Instant::now() < limit,
                "timed out: {what}; {:?}",
                manager.infos()
            );
            std::thread::sleep(Duration::from_millis(250));
        }
    };
    let stamp = Stamp {
        incarnation: 1,
        version: 0,
    };
    let diagnostics = || manager.results(&path, stamp).diagnostics;
    wait("an unresolved-module diagnostic", &|| {
        diagnostics().is_some_and(|d| d.iter().any(|d| d.message.contains("unresolved module")))
    });
    println!(
        "before: {:?}",
        diagnostics().map(|d| d.iter().map(|d| d.message.clone()).collect::<Vec<_>>())
    );

    wait("semantic tokens", &|| {
        manager
            .results(&path, stamp)
            .semantic
            .is_some_and(|t| !t.is_empty())
    });
    println!(
        "semantic spans: {:?}",
        manager.results(&path, stamp).semantic.map(|t| t.len())
    );

    // Create the missing file behind the editor's back.
    std::fs::write(dir.join("src/other.rs"), "pub fn g() {}\n").unwrap();
    wait(
        "the diagnostic to clear after the file appeared on disk",
        &|| {
            diagnostics()
                .is_some_and(|d| !d.iter().any(|d| d.message.contains("unresolved module")))
        },
    );
    println!("after: {:?}", diagnostics().map(|d| d.len()));
    drop(manager);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Opens `LOOM_RA_FILE` with one invalid line appended and prints what rust-analyzer
/// reports and when. `cargo test --bin Loom real_rust_analyzer_reports_a_syntax_error -- --ignored --nocapture`
#[test]
#[ignore = "needs rust-analyzer and LOOM_RA_FILE"]
fn real_rust_analyzer_reports_a_syntax_error() {
    let file = PathBuf::from(std::env::var("LOOM_RA_FILE").expect("LOOM_RA_FILE"));
    let root = PathBuf::from(std::env::var("LOOM_RA_ROOT").expect("LOOM_RA_ROOT"));
    let mut text = std::fs::read_to_string(&file).unwrap();
    if std::env::var("LOOM_RA_AS_IS").is_err() {
        text = text.replacen("fn main() {\n", "fn main() {\n    s\n", 1);
    }
    let sent = text.clone();
    let manager = Manager::start(
        Registry::builtin(),
        crate::features::language_service::server_set::rust_analyzer_running(
            &std::env::var("LOOM_RUST_ANALYZER").unwrap_or_else(|_| "rust-analyzer".into()),
        ),
        None,
        Options::default(),
        || {},
    );
    let stamp = Stamp {
        incarnation: 1,
        version: 0,
    };
    manager.update(Snapshot {
        languages: Default::default(),
        documents: vec![SnapshotDocument {
            language: Registry::builtin().resolve_path(&file),
            path: file.clone(),
            stamp,
            text: Arc::new(text),
        }],
        workspace_roots: vec![root],
        servers: vec![LanguageServerSetting::named("rust-analyzer")],
    });
    let started = Instant::now();
    let mut printed = 0;
    while started.elapsed() < Duration::from_secs(120) {
        let r = manager.results(&file, stamp);
        let found = r.diagnostics.as_ref().is_some_and(|d| !d.is_empty());
        println!(
            "{:>5.1}s {:?} tasks={:?} diagnostics={:?}",
            started.elapsed().as_secs_f32(),
            manager.infos().first().map(|i| i.status.clone()),
            manager.infos().first().map(|i| i
                .tasks
                .iter()
                .map(|t| format!("{} {:?}", t.title, t.percentage))
                .collect::<Vec<_>>()),
            r.diagnostics
                .as_ref()
                .map(|d| d.iter().map(|d| d.message.clone()).collect::<Vec<_>>())
        );
        if found {
            let all = manager.results(&file, stamp).diagnostics.unwrap();
            if all.len() != printed {
                printed = all.len();
                for d in all.iter() {
                    println!(
                        "DIAG {:?} {:?} {:?} {:?}",
                        d.severity,
                        d.range,
                        &sent[d.range.clone()],
                        d.message
                    );
                }
            }
            // Keep listening a while: compiler diagnostics come later.
            if started.elapsed() > Duration::from_secs(45) {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
fn background_tasks_logs_and_the_command_are_reported() {
    let f = Fixture::new(&["--progress", "--log-message", "--sync", "object"]);
    f.show(vec![f.doc("a.fk", 0, "x")]);
    let info = f.wait_for("a running task", || {
        f.mgr().infos().into_iter().find(|i| !i.tasks.is_empty())
    });
    assert_eq!(info.status, Status::Ready);
    assert_eq!(info.tasks.len(), 1);
    assert_eq!(info.tasks[0].title, "Loading project");
    f.wait_for("the progress report", || {
        let t = f
            .mgr()
            .infos()
            .into_iter()
            .next()?
            .tasks
            .into_iter()
            .next()?;
        (t.percentage == Some(30) && t.message.as_deref() == Some("3/10")).then_some(())
    });
    let info = f.mgr().infos().remove(0);
    assert!(info.process_id.is_some());
    assert!(info.command.contains("fake_lsp"), "{}", info.command);
    f.wait_for("the log message", || {
        f.mgr().infos()[0]
            .log
            .iter()
            .any(|l| l == "info: hello from the fake server")
            .then_some(())
    });

    // The server ends the task.
    f.mgr().saved(f.dir.join("a.fk"), Arc::new("x".into()));
    f.wait_for("the task to end", || {
        f.mgr().infos()[0].tasks.is_empty().then_some(())
    });
}

#[test]
fn each_instance_reports_only_its_own_workspace_folder() {
    let f = Fixture::new(&["--ask-folders"]);
    for dir in ["first", "second"] {
        std::fs::create_dir_all(f.dir.join(dir)).unwrap();
        std::fs::write(f.dir.join(dir).join("fake.toml"), "").unwrap();
    }
    f.show(vec![
        f.doc("first/a.fk", 0, "one"),
        f.doc("second/b.fk", 0, "two"),
    ]);
    f.wait_ready(2);
    f.wait_for("both folder replies", || {
        (f.log()
            .iter()
            .filter(|m| m["id"] == 9004 && m.get("result").is_some())
            .count()
            == 2)
            .then_some(())
    });
    let log = f.log();
    let folder_sets: Vec<_> = log
        .iter()
        .filter(|m| m["method"] == "initialize")
        .map(|m| m["params"]["workspaceFolders"].clone())
        .collect();
    assert_eq!(folder_sets.len(), 2);
    for folder in &folder_sets {
        let folders = folder.as_array().unwrap();
        assert_eq!(folders.len(), 1);
        assert!(["first", "second"].contains(&folders[0]["name"].as_str().unwrap()));
        assert!(
            log.iter()
                .any(|m| m["id"] == 9004 && m["result"] == *folder)
        );
    }
    assert_ne!(folder_sets[0], folder_sets[1]);
    assert!(
        !f.methods()
            .contains(&"workspace/didChangeWorkspaceFolders".to_owned())
    );
}

#[test]
fn progress_cancellation_preserves_numeric_and_string_tokens() {
    let f = Fixture::new(&["--progress-pair", "--progress-cancellable"]);
    f.show(vec![f.doc("a.fk", 0, "x")]);
    let info = f.wait_for("two distinct tasks", || {
        f.mgr().infos().into_iter().find(|i| i.tasks.len() == 2)
    });
    assert_ne!(info.tasks[0].id, info.tasks[1].id);
    f.mgr().cancel_task(info.id, info.run, info.tasks[0].id);
    f.wait_for("only the numeric task ended", || {
        let current = f.mgr().infos().into_iter().next()?;
        (current.tasks.len() == 1 && current.tasks[0].id == info.tasks[1].id).then_some(())
    });
    f.mgr().cancel_task(info.id, info.run, info.tasks[1].id);
    f.wait_for("the string task ended", || {
        f.mgr().infos()[0].tasks.is_empty().then_some(())
    });
    let tokens: Vec<_> = f
        .log()
        .into_iter()
        .filter(|m| m["method"] == "window/workDoneProgress/cancel")
        .map(|m| m["params"]["token"].clone())
        .collect();
    assert_eq!(tokens, [json!(7), json!("7")]);
}

#[test]
fn noncancellable_tasks_and_stale_panel_actions_send_no_cancellation() {
    for cancellable in [false, true] {
        let mut args = vec!["--sync", "object", "--progress"];
        if cancellable {
            args.push("--progress-cancellable");
        }
        let f = Fixture::new(&args);
        f.show(vec![f.doc("a.fk", 0, "x")]);
        let old = f.wait_for("a task", || {
            f.mgr().infos().into_iter().find(|i| !i.tasks.is_empty())
        });
        if cancellable {
            f.mgr().restart();
            f.wait_for("a new server run", || {
                f.mgr()
                    .infos()
                    .into_iter()
                    .find(|i| i.run != old.run && !i.tasks.is_empty())
            });
        }
        f.mgr().cancel_task(old.id, old.run, old.tasks[0].id);
        f.mgr().saved(f.dir.join("a.fk"), Arc::new("x".into()));
        f.wait_method_count("textDocument/didSave", 1);
        assert!(
            !f.methods()
                .contains(&"window/workDoneProgress/cancel".to_owned())
        );
    }
}

#[test]
fn ignored_progress_cancellation_and_missing_end_reports_are_bounded() {
    let f = Fixture::new(&[
        "--progress",
        "--progress-cancellable",
        "--ignore-progress-cancel",
    ]);
    let now = Instant::now();
    f.mgr().set_progress_time(now);
    // Observe the final scripted report before advancing expiry boundaries.
    f.show(vec![f.doc("a.fk", 0, "x")]);
    let info = f.wait_for("a cancellable task", || {
        f.mgr()
            .infos()
            .into_iter()
            .find(|i| i.tasks.iter().any(|task| task.percentage == Some(30)))
    });
    f.mgr().cancel_task(info.id, info.run, info.tasks[0].id);
    f.mgr().cancel_task(info.id, info.run, info.tasks[0].id);
    f.wait_method_count("window/workDoneProgress/cancel", 1);
    // Deliberately miss the old 150ms sampling window. Expiry is controlled by
    // the coordinator's progress clock, not by how soon this observer runs.
    std::thread::sleep(Duration::from_millis(450));
    f.wait_for("cancelling is visible", || {
        f.mgr().infos()[0]
            .tasks
            .iter()
            .any(|t| t.cancelling)
            .then_some(())
    });
    f.mgr().set_progress_time(now + Duration::from_millis(149));
    assert!(!f.mgr().infos()[0].tasks.is_empty());
    f.mgr().set_progress_time(now + Duration::from_millis(150));
    f.wait_for("unacknowledged cancellation stops being displayed", || {
        let info = f.mgr().infos().into_iter().next()?;
        (info.tasks.is_empty()
            && info
                .log
                .iter()
                .any(|l| l.contains("cancellation was not acknowledged")))
        .then_some(())
    });
    assert_eq!(
        f.methods()
            .iter()
            .filter(|m| *m == "window/workDoneProgress/cancel")
            .count(),
        1
    );

    let f = Fixture::with_options(
        servers(&[("fake-server", "fake", 0)]),
        &[("fake-server", &["--progress"])],
        Options {
            progress_timeout: Duration::from_millis(300),
            ..options()
        },
    );
    let now = Instant::now();
    f.mgr().set_progress_time(now);
    f.show(vec![f.doc("a.fk", 0, "x")]);
    f.wait_method_count("textDocument/didOpen", 1);
    // Inject observer latency longer than the former 300ms task lifetime.
    std::thread::sleep(Duration::from_millis(450));
    f.wait_for("a task before its timeout", || {
        f.mgr()
            .infos()
            .into_iter()
            .find(|i| i.tasks.iter().any(|task| task.percentage == Some(30)))
    });
    f.mgr().set_progress_time(now + Duration::from_millis(299));
    assert!(!f.mgr().infos()[0].tasks.is_empty());
    f.mgr().set_progress_time(now + Duration::from_millis(300));
    f.wait_for("a missing end is not shown forever", || {
        let info = f.mgr().infos().into_iter().next()?;
        (info.tasks.is_empty()
            && info
                .log
                .iter()
                .any(|l| l.contains("no progress report before timeout")))
        .then_some(())
    });
}

#[test]
fn diagnostics_for_an_older_version_are_moved_along_the_edits_not_lost() {
    // The server answers late and never republishes: what it said about the text it
    // saw must still show once the text has moved on.
    let f = Fixture::new(&[
        "--sync",
        "object",
        "--diagnostics",
        "--diag-once",
        "--diag-defer-until-save",
    ]);
    f.show(vec![f.doc("a.fk", 0, "fn a")]);
    f.wait_method_count("textDocument/didOpen", 1);
    // Release the old publication only after the edit has reached the server.
    f.show(vec![f.doc("a.fk", 5, "fn a plus more")]);
    f.wait_method_count("textDocument/didChange", 1);
    f.mgr()
        .saved(f.dir.join("a.fk"), Arc::new("fn a plus more".into()));
    let found = f.wait_for("carried diagnostics", || {
        f.results_at("a.fk", 5).diagnostics
    });
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].range,
        0..2,
        "the underline is where the text it described still is"
    );
    // Results become visible before the coordinator publishes its status snapshot.
    let counts = f.wait_for("diagnostic counts to be published", || {
        let counts = f.mgr().infos()[0].diagnostics;
        (counts.published == 1).then_some(counts)
    });
    assert_eq!((counts.published, counts.carried, counts.shown), (1, 1, 0));
}

#[test]
fn a_diagnostic_on_text_that_was_edited_before_the_server_answered_is_dropped() {
    let f = Fixture::new(&[
        "--sync",
        "object",
        "--diagnostics",
        "--diag-once",
        "--diag-defer-until-save",
    ]);
    f.show(vec![f.doc("a.fk", 0, "fn a")]);
    f.wait_method_count("textDocument/didOpen", 1);
    // Replace the diagnosed characters before releasing the old publication.
    f.show(vec![f.doc("a.fk", 5, "xx a")]);
    f.wait_method_count("textDocument/didChange", 1);
    f.mgr().saved(f.dir.join("a.fk"), Arc::new("xx a".into()));
    f.wait_for("the publication", || {
        (f.mgr().infos()[0].diagnostics.published == 1).then_some(())
    });
    assert!(
        f.results_at("a.fk", 5)
            .diagnostics
            .is_none_or(|d| d.is_empty())
    );
}

#[test]
fn underlines_follow_the_typing_while_the_server_has_nothing_new_to_say() {
    let f = Fixture::new(&["--diagnostics", "--diag-once"]);
    f.show(vec![f.doc("a.fk", 0, "fn a")]);
    f.wait_for("the first diagnostics", || {
        f.results_at("a.fk", 0).diagnostics
    });
    // The server never answers again, yet every later state has the diagnostic,
    // moved to where its text now is.
    f.show(vec![f.doc("a.fk", 1, "// top\nfn a")]);
    let moved = f.wait_for("the moved diagnostic", || {
        f.results_at("a.fk", 1).diagnostics
    });
    assert_eq!(moved[0].range, 7..9);
    f.show(vec![f.doc("a.fk", 2, "// top\nfn a b")]);
    let still = f.wait_for("it follows again", || f.results_at("a.fk", 2).diagnostics);
    assert_eq!(still[0].range, 7..9);
}

mod installing {
    use super::*;
    use crate::features::settings::servers::InstallPolicy;
    use crate::infrastructure::lsp_catalog::Catalog;
    use crate::infrastructure::lsp_install::{Download, Installer, Smoke};
    use ring::digest::{Context, SHA256};
    use std::io::{Cursor, Read, Write};
    use std::sync::Mutex;

    const URL: &str =
        "https://github.com/owner/loom/releases/download/lsp-servers-1/demo-any.tar.gz";

    /// A pack whose server program is the fake language server itself.
    fn pack() -> Vec<u8> {
        let program = std::fs::read(fake_server()).expect("build the fake_lsp example first");
        let name = if cfg!(windows) {
            "demo/bin/demo.exe"
        } else {
            "demo/bin/demo"
        };
        let mut header = tar::Header::new_gnu();
        header.set_path(name).unwrap();
        header.set_size(program.len() as u64);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        let mut builder = tar::Builder::new(Vec::new());
        builder.append(&header, program.as_slice()).unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&builder.into_inner().unwrap()).unwrap();
        encoder.finish().unwrap()
    }

    struct World {
        manager: Manager,
        dir: PathBuf,
        downloads: Arc<AtomicUsize>,
        served: Arc<Mutex<Option<Vec<u8>>>>,
        servers: Vec<LanguageServerSetting>,
        _clean: Clean,
    }

    fn world() -> World {
        let n = RUNS.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("loom-mgr-install-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bytes = pack();
        let mut context = Context::new(&SHA256);
        context.update(&bytes);
        let sha: String = context
            .finish()
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let text = format!(
            r#"
schema = 1
[distribution]
repository = "owner/loom"
[packs.demo]
version = "1.0.0"
license = "MIT"
[packs.demo.platforms.any]
asset = "demo-any.tar.gz"
release = "lsp-servers-1"
sha256 = "{sha}"
size = {}
[servers.demo]
name = "Demo"
pack = "demo"
command = "${{pack}}/bin/demo"
args = ["--sync", "full"]
[servers.demo.root]
markers = ["fake.toml"]
fallback = "file_directory"
[[servers.demo.languages]]
language = "fakelang"
language_id = "fakelang"
"#,
            bytes.len()
        );
        let catalog = Catalog::parse(&text).unwrap();
        let servers = Arc::new(catalog.servers().unwrap());
        let served = Arc::new(Mutex::new(Some(bytes)));
        let downloads = Arc::new(AtomicUsize::new(0));
        let (served_for, count) = (served.clone(), downloads.clone());
        let download: Arc<Download> = Arc::new(move |url: &str| {
            count.fetch_add(1, Ordering::SeqCst);
            match (url == URL, served_for.lock().unwrap().clone()) {
                (true, Some(bytes)) => Ok(Box::new(Cursor::new(bytes)) as Box<dyn Read + Send>),
                _ => Err(format!("404 {url}")),
            }
        });
        let smoke: Arc<Smoke> = Arc::new(|_| Ok(()));
        let installer = Installer::new(
            Arc::new(catalog),
            dir.join("managed"),
            "linux-x86_64".into(),
            download,
            smoke,
        );
        let manager = Manager::start(registry(), servers, Some(installer), options(), || {});
        World {
            manager,
            dir: dir.clone(),
            downloads,
            served,
            servers: Vec::new(),
            _clean: Clean(dir.clone()),
        }
    }

    impl World {
        fn show(&self) {
            let path = self.dir.join("a.fk");
            self.manager.update(Snapshot {
                languages: Default::default(),
                documents: vec![SnapshotDocument {
                    language: registry().resolve_path(&path),
                    path,
                    stamp: Stamp {
                        incarnation: 1,
                        version: 0,
                    },
                    text: Arc::new("x".to_owned()),
                }],
                workspace_roots: vec![self.dir.clone()],
                servers: self.servers.clone(),
            });
        }

        fn status(&self) -> Option<Status> {
            self.manager.infos().into_iter().next().map(|i| i.status)
        }

        fn wait(&self, what: &str, check: impl Fn(&Status) -> bool) {
            let limit = Instant::now() + Duration::from_secs(20);
            loop {
                if self.status().as_ref().is_some_and(&check) {
                    return;
                }
                assert!(
                    Instant::now() < limit,
                    "timed out waiting for {what}: {:?}",
                    self.status()
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    /// Removes the scratch directory after the manager has stopped its servers.
    struct Clean(PathBuf);
    impl Drop for Clean {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn by_default_a_missing_pack_waits_until_the_person_installs_it() {
        let w = world();
        w.show();
        w.wait("not installed", |s| matches!(s, Status::NotInstalled));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            w.downloads.load(Ordering::SeqCst),
            0,
            "asking never downloads"
        );
        w.manager.install("demo");
        w.wait("ready", |s| matches!(s, Status::Ready));
        assert_eq!(w.downloads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn auto_installs_and_starts_without_being_asked() {
        let mut w = world();
        w.servers.push(LanguageServerSetting {
            install: Some(InstallPolicy::Auto),
            ..LanguageServerSetting::named("demo")
        });
        w.show();
        w.wait("ready", |s| matches!(s, Status::Ready));
    }

    #[test]
    fn multiple_project_roots_each_wait_for_the_person_to_install_the_pack() {
        let w = world();
        let documents = ["one/a.fk", "two/a.fk"]
            .iter()
            .map(|name| {
                let path = w.dir.join(name);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                SnapshotDocument {
                    language: registry().resolve_path(&path),
                    path,
                    stamp: Stamp {
                        incarnation: 1,
                        version: 0,
                    },
                    text: Arc::new("x".into()),
                }
            })
            .collect();
        w.manager.update(Snapshot {
            documents,
            workspace_roots: vec![w.dir.clone()],
            ..Snapshot::default()
        });
        w.wait("not installed", |s| matches!(s, Status::NotInstalled));
        assert_eq!(w.manager.infos().len(), 2);
        w.manager.restart();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(w.downloads.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_failed_install_is_reported_and_retry_recovers() {
        let mut w = world();
        w.servers.push(LanguageServerSetting {
            install: Some(InstallPolicy::Auto),
            ..LanguageServerSetting::named("demo")
        });
        let good = w.served.lock().unwrap().take();
        w.show();
        w.wait("install failed", |s| matches!(s, Status::InstallFailed(_)));
        *w.served.lock().unwrap() = good;
        w.manager.retry_installs();
        w.wait("ready", |s| matches!(s, Status::Ready));
    }

    #[test]
    fn a_server_the_person_registered_needs_no_install_and_runs_from_its_path() {
        let mut w = world();
        let program = fake_server().display().to_string();
        let own: LanguageServerSetting = serde_json::from_value(serde_json::json!({
            "command": program,
            "args": ["--sync", "full"],
            "languages": [{ "language": "fakelang", "language_id": "fakelang" }],
            "root": { "markers": ["fake.toml"], "fallback": "file_directory" },
            "priority": 100,
        }))
        .unwrap();
        w.servers.push(LanguageServerSetting {
            server: "mine".into(),
            ..own
        });
        // Turn the catalogued server off, so only the registered one serves the file.
        w.servers.push(LanguageServerSetting {
            enabled: false,
            ..LanguageServerSetting::named("demo")
        });
        w.show();
        w.wait("ready", |s| matches!(s, Status::Ready));
        assert_eq!(
            w.downloads.load(Ordering::SeqCst),
            0,
            "nothing is installed"
        );
        let infos = w.manager.infos();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].server_id, "mine");
    }
}

mod requests {
    use super::*;
    use crate::features::language_service::reply;
    use crate::features::language_service::request::Feature;
    use crate::infrastructure::lsp::manager::{Failure, RequestHandle, RequestSpec};

    const FEATURES: &str = "hover,definition,references,highlight,symbols,workspaceSymbols,completion,\
signature,rename,codeAction,formatting,inlay,folding,selection,callHierarchy,codeLens,execute";

    fn stamp(version: u64) -> Stamp {
        Stamp {
            incarnation: 1,
            version,
        }
    }

    fn featured(extra: &[&str]) -> Fixture {
        let mut args = vec!["--features", FEATURES];
        args.extend(extra);
        Fixture::new(&args)
    }

    impl Fixture {
        fn ask(&self, feature: Feature, version: u64, offset: usize) -> RequestHandle {
            self.mgr().request(RequestSpec::new(
                feature,
                self.dir.join("a.fk"),
                stamp(version),
                offset,
            ))
        }

        fn finish(&self, handle: &RequestHandle) -> crate::infrastructure::lsp::manager::Progress {
            self.wait_for("the request to finish", || {
                let progress = handle.poll();
                progress.finished.then_some(progress)
            })
        }

        /// Asks once the server is ready; a request made earlier would find nobody.
        fn ready_then_ask(&self, feature: Feature, version: u64, offset: usize) -> RequestHandle {
            self.wait_status("ready", |s| matches!(s, Status::Ready));
            self.wait_method_count("textDocument/didOpen", 1);
            self.ask(feature, version, offset)
        }
    }

    #[test]
    fn hover_asks_for_the_position_of_the_offset_and_hears_back() {
        let f = featured(&[]);
        f.show(vec![f.doc("a.fk", 0, "ab\ncd😀ef")]);
        // Byte 9 is the e after the emoji: line 1, UTF-16 column 4.
        let handle = f.ready_then_ask(Feature::Hover, 0, 9);
        let progress = f.finish(&handle);
        assert_eq!(progress.failure, None);
        assert_eq!(progress.parts.len(), 1);
        let hover = reply::hover(&progress.parts[0].value, progress.parts[0].encoding).unwrap();
        assert_eq!(hover.markdown, "hover 1:4 from fake");
        let sent = f
            .log()
            .into_iter()
            .find(|m| m["method"] == "textDocument/hover")
            .unwrap();
        assert_eq!(
            sent["params"]["position"],
            json!({ "line": 1, "character": 4 })
        );
        assert_eq!(progress.parts[0].server.as_ref(), "fake-server");
    }

    #[test]
    fn versioned_workspace_edits_must_match_the_instances_current_synced_document() {
        for (version, valid) in [("1", true), ("999", false)] {
            let f = featured(&["--edit-version", version]);
            f.show(vec![f.doc("a.fk", 0, "abc")]);
            let handle = f.ready_then_ask(Feature::Rename, 0, 1);
            let progress = f.finish(&handle);
            assert_eq!(!progress.parts.is_empty(), valid);
            if !valid {
                assert_eq!(progress.failure, Some(Failure::Stale));
            }
        }
    }

    #[test]
    fn a_request_for_an_old_state_is_stale_and_sends_nothing() {
        let f = featured(&[]);
        f.show(vec![f.doc("a.fk", 0, "one")]);
        f.wait_status("ready", |s| matches!(s, Status::Ready));
        f.show(vec![f.doc("a.fk", 1, "one two")]);
        let handle = f.ask(Feature::Hover, 0, 1);
        let progress = f.finish(&handle);
        assert_eq!(progress.failure, Some(Failure::Stale));
        assert!(progress.parts.is_empty());
        std::thread::sleep(Duration::from_millis(100));
        assert!(!f.methods().contains(&"textDocument/hover".to_owned()));
    }

    #[test]
    fn a_server_without_the_feature_is_not_asked() {
        let f = Fixture::new(&["--features", "definition"]);
        f.show(vec![f.doc("a.fk", 0, "x")]);
        let handle = f.ready_then_ask(Feature::Hover, 0, 0);
        assert_eq!(f.finish(&handle).failure, Some(Failure::Unsupported));
        let handle = f.ask(Feature::Definition, 0, 0);
        assert_eq!(f.finish(&handle).parts.len(), 1);
    }

    #[test]
    fn a_server_that_is_not_ready_yet_cannot_answer() {
        let f = featured(&["--ignore-initialize"]);
        f.show(vec![f.doc("a.fk", 0, "x")]);
        let handle = f.ask(Feature::Hover, 0, 0);
        assert_eq!(f.finish(&handle).failure, Some(Failure::Unsupported));
    }

    #[test]
    fn dropping_the_handle_cancels_the_request_at_the_server() {
        let f = featured(&["--ignore-feature-success"]);
        f.show(vec![f.doc("a.fk", 0, "x")]);
        let handle = f.ready_then_ask(Feature::Hover, 0, 0);
        f.wait_method_count("textDocument/hover", 1);
        drop(handle);
        f.wait_method_count("$/cancelRequest", 1);
    }

    #[test]
    fn a_newer_request_of_the_same_kind_replaces_the_older() {
        let f = featured(&["--ignore-first-feature-success"]);
        f.show(vec![f.doc("a.fk", 0, "abcdef")]);
        let first = f.ready_then_ask(Feature::Hover, 0, 1);
        f.wait_method_count("textDocument/hover", 1);
        assert!(!first.is_finished());
        let second = f.ask(Feature::Hover, 0, 4);
        let old = f.finish(&first);
        assert!(old.parts.is_empty(), "the older request was given up on");
        let new = f.finish(&second);
        let hover = reply::hover(&new.parts[0].value, new.parts[0].encoding).unwrap();
        assert_eq!(hover.markdown, "hover 0:4 from fake");
        f.wait_method_count("$/cancelRequest", 1);
    }

    #[test]
    fn content_modified_means_stale_not_failed() {
        let f = featured(&["--feature-error"]);
        f.show(vec![f.doc("a.fk", 0, "x")]);
        let handle = f.ready_then_ask(Feature::Hover, 0, 0);
        assert_eq!(f.finish(&handle).failure, Some(Failure::Stale));
        assert_eq!(
            f.methods()
                .iter()
                .filter(|m| *m == "textDocument/hover")
                .count(),
            2
        );
    }

    #[test]
    fn transient_content_modified_and_server_cancelled_queries_retry_once() {
        for code in ["-32801", "-32802"] {
            let f = featured(&["--feature-error-once", "--feature-error-code", code]);
            f.show(vec![f.doc("a.fk", 0, "alpha")]);
            let handle = f.ready_then_ask(Feature::Hover, 0, 2);
            let progress = f.finish(&handle);
            assert_eq!(progress.failure, None, "{progress:?}");
            assert_eq!(progress.parts.len(), 1);
            assert_eq!(
                f.methods()
                    .iter()
                    .filter(|m| *m == "textDocument/hover")
                    .count(),
                2
            );
        }
    }

    #[test]
    fn a_retry_never_replays_a_command_or_an_outdated_document() {
        let f = featured(&["--feature-error-once"]);
        f.show(vec![f.doc("a.fk", 0, "alpha")]);
        f.wait_status("ready", |s| matches!(s, Status::Ready));
        f.wait_method_count("textDocument/didOpen", 1);
        let instance = f.mgr().infos()[0].id;
        let handle = f.mgr().request(
            RequestSpec::new(Feature::ExecuteCommand, f.dir.join("a.fk"), stamp(0), 0)
                .target(instance)
                .extra(json!({ "command": "fake.run" })),
        );
        assert_eq!(f.finish(&handle).failure, Some(Failure::Stale));
        assert_eq!(
            f.methods()
                .iter()
                .filter(|m| *m == "workspace/executeCommand")
                .count(),
            1
        );

        let f = featured(&[
            "--sync",
            "object",
            "--feature-error-once",
            "--defer-feature-replies",
        ]);
        f.show(vec![f.doc("a.fk", 0, "alpha")]);
        let handle = f.ready_then_ask(Feature::Hover, 0, 2);
        f.wait_method_count("textDocument/hover", 1);
        f.show(vec![f.doc("a.fk", 1, "different")]);
        f.wait_method_count("textDocument/didChange", 1);
        f.mgr()
            .saved(f.dir.join("a.fk"), Arc::new("different".into()));
        f.wait_method_count("textDocument/didSave", 1);
        assert_eq!(f.finish(&handle).failure, Some(Failure::Stale));
        assert_eq!(
            f.methods()
                .iter()
                .filter(|m| *m == "textDocument/hover")
                .count(),
            1
        );
    }

    #[test]
    fn the_server_can_forbid_retriggering() {
        let f = featured(&[
            "--feature-error-once",
            "--feature-error-code",
            "-32802",
            "--do-not-retrigger",
        ]);
        f.show(vec![f.doc("a.fk", 0, "alpha")]);
        let handle = f.ready_then_ask(Feature::Hover, 0, 2);
        assert_eq!(f.finish(&handle).failure, Some(Failure::Stale));
        assert_eq!(
            f.methods()
                .iter()
                .filter(|m| *m == "textDocument/hover")
                .count(),
            1
        );
    }

    #[test]
    fn a_retry_keeps_the_original_request_budget() {
        let f = featured(&["--feature-error-once", "--slow-ms", "150"]);
        f.show(vec![f.doc("a.fk", 0, "alpha")]);
        f.wait_status("ready", |s| matches!(s, Status::Ready));
        f.wait_method_count("textDocument/didOpen", 1);
        let handle = f.mgr().request(
            RequestSpec::new(Feature::Hover, f.dir.join("a.fk"), stamp(0), 2)
                .timeout(Duration::from_millis(240)),
        );
        let progress = f.finish(&handle);
        assert!(
            progress.parts.is_empty(),
            "a reset budget would let the second 150ms reply succeed"
        );
        // A loaded runner may process the first stale reply after the budget
        // has expired. In that case no retry is sent and Stale is valid too.
        assert!(
            matches!(progress.failure, Some(Failure::Stale | Failure::Failed(_))),
            "{progress:?}"
        );
        f.wait_method_count("textDocument/hover", 1);
        let requests = f
            .methods()
            .iter()
            .filter(|m| *m == "textDocument/hover")
            .count();
        assert!((1..=2).contains(&requests), "{requests} requests");
    }

    #[test]
    fn only_the_stale_server_is_retried() {
        let f = Fixture::with_servers(
            two_servers(),
            &[
                (
                    "alpha",
                    &[
                        "--features",
                        "hover",
                        "--feature-error-once",
                        "--slow-ms",
                        "100",
                    ],
                ),
                ("beta", &["--features", "hover"]),
            ],
        );
        f.show(vec![f.doc("a.fk", 0, "alpha")]);
        f.wait_ready(2);
        let handle = f.ask(Feature::Hover, 0, 2);
        let progress = f.finish(&handle);
        assert_eq!(progress.parts.len(), 2);
        for (n, expected) in [2, 1].into_iter().enumerate() {
            let log = std::fs::read_to_string(&f.logs[n]).unwrap();
            let count = log
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter(|m| m["method"] == "textDocument/hover")
                .count();
            assert_eq!(count, expected);
        }
    }

    #[test]
    fn dropping_a_retried_handle_cancels_the_retried_request() {
        // Keep the retry pending: a short delay can elapse before the test
        // thread runs again on CI, leaving an already-finished request to drop.
        let f = featured(&["--feature-error-once", "--ignore-feature-success"]);
        f.show(vec![f.doc("a.fk", 0, "alpha")]);
        let handle = f.ready_then_ask(Feature::Hover, 0, 2);
        f.wait_method_count("textDocument/hover", 2);
        assert!(!handle.is_finished());
        drop(handle);
        f.wait_method_count("$/cancelRequest", 1);
        let messages = f.log();
        let requests: Vec<_> = messages
            .iter()
            .filter(|m| m["method"] == "textDocument/hover")
            .collect();
        assert_eq!(requests.len(), 2);
        assert_ne!(requests[0]["id"], requests[1]["id"]);
        let cancel = messages
            .iter()
            .find(|m| m["method"] == "$/cancelRequest")
            .unwrap();
        assert_eq!(cancel["params"]["id"], requests[1]["id"]);
    }

    #[test]
    fn a_server_that_dies_mid_request_fails_it_rather_than_hanging() {
        let f = featured(&["--crash-on", "textDocument/hover"]);
        f.show(vec![f.doc("a.fk", 0, "x")]);
        let handle = f.ready_then_ask(Feature::Hover, 0, 0);
        let progress = f.finish(&handle);
        assert!(
            matches!(progress.failure, Some(Failure::Failed(_))),
            "{progress:?}"
        );
    }

    #[test]
    fn every_server_answers_and_the_first_non_empty_one_wins_where_that_is_the_rule() {
        let f = Fixture::with_servers(
            two_servers(),
            &[
                (
                    "alpha",
                    &[
                        "--features",
                        "hover,highlight",
                        "--tag",
                        "alpha",
                        "--hover-null",
                    ],
                ),
                ("beta", &["--features", "hover,highlight", "--tag", "beta"]),
            ],
        );
        f.show(vec![f.doc("a.fk", 0, "abc")]);
        f.wait_ready(2);
        f.wait_for("both servers to open the document", || {
            (0..2)
                .all(|n| {
                    f.methods_of(n)
                        .iter()
                        .any(|m| m["method"] == "textDocument/didOpen")
                })
                .then_some(())
        });
        // Hover merges: alpha has nothing to say (null is not a part), beta does.
        let handle = f.ask(Feature::Hover, 0, 1);
        let progress = f.finish(&handle);
        assert_eq!(progress.parts.len(), 1);
        assert_eq!(progress.parts[0].server.as_ref(), "beta");
        // Highlights: first non-empty in listing order (beta has the lower priority number? both answer).
        let handle = f.ask(Feature::DocumentHighlight, 0, 1);
        let progress = f.finish(&handle);
        assert_eq!(progress.parts.len(), 2);
        let usable = progress.usable(Feature::DocumentHighlight);
        assert_eq!(usable.len(), 1);
        assert_eq!(
            usable[0].rank, progress.parts[0].rank,
            "the preferred server"
        );
    }

    #[test]
    fn a_follow_up_goes_only_to_the_server_that_produced_the_item() {
        let f = Fixture::with_servers(
            two_servers(),
            &[
                ("alpha", &["--features", "completion", "--tag", "alpha"]),
                ("beta", &["--features", "completion", "--tag", "beta"]),
            ],
        );
        f.show(vec![f.doc("a.fk", 0, "abc")]);
        f.wait_ready(2);
        f.wait_for("both servers to open the document", || {
            (0..2)
                .all(|n| {
                    f.methods_of(n)
                        .iter()
                        .any(|m| m["method"] == "textDocument/didOpen")
                })
                .then_some(())
        });
        let handle = f.ask(Feature::Completion, 0, 3);
        let progress = f.finish(&handle);
        assert_eq!(progress.parts.len(), 2);
        let beta = progress
            .parts
            .iter()
            .find(|p| p.server.as_ref() == "beta")
            .unwrap();
        let list = reply::completion(&beta.value, beta.encoding);
        let item = list.items.iter().find(|i| i.label == "beta").unwrap();
        let resolve = f.mgr().request(
            RequestSpec::new(Feature::CompletionResolve, f.dir.join("a.fk"), stamp(0), 0)
                .extra(item.raw.clone())
                .target(beta.instance),
        );
        let answer = f.finish(&resolve);
        assert_eq!(answer.parts.len(), 1);
        assert_eq!(answer.parts[0].instance, beta.instance);
        let resolved =
            reply::completion_item(&answer.parts[0].value, answer.parts[0].encoding).unwrap();
        assert_eq!(resolved.documentation.as_deref(), Some("resolved docs"));
        let alpha_resolves = f
            .methods_of(0)
            .iter()
            .filter(|m| m["method"] == "completionItem/resolve")
            .count();
        assert_eq!(alpha_resolves, 0, "alpha was not asked");
    }

    #[test]
    fn workspace_requests_need_no_document() {
        let f = featured(&[]);
        f.show(vec![f.doc("a.fk", 0, "x")]);
        f.wait_status("ready", |s| matches!(s, Status::Ready));
        let handle = f.mgr().request(
            RequestSpec::new(Feature::WorkspaceSymbol, f.dir.join("a.fk"), stamp(0), 0)
                .extra(json!({ "query": "ab" })),
        );
        let progress = f.finish(&handle);
        let found = reply::symbols(&progress.parts[0].value, progress.parts[0].encoding);
        assert_eq!(found[0].name, "sym-ab");
    }

    #[test]
    fn typing_reaches_a_server_that_wants_ranges_as_ranges_and_leaves_its_copy_identical() {
        let dir_mirror = |f: &Fixture| f.dir.join("mirror.json");
        let mut f = Fixture::new(&["--sync", "incremental", "--features", "hover"]);
        let mirror = dir_mirror(&f).display().to_string();
        f.settings[0]
            .args
            .as_mut()
            .unwrap()
            .extend(["--mirror".to_owned(), mirror]);
        f.show(vec![f.doc(
            "a.fk",
            0,
            "fn main() {\r\n    let é = 1;\r\n}\r\n",
        )]);
        f.wait_method_count("textDocument/didOpen", 1);
        let texts = [
            "fn main() {\r\n    let é = 1;\r\n}\r\n!",
            "fn main() {\r\n    let éé = 12;\r\n}\r\n!",
            "fn main() {\r\n    let éé = 12;\r\n\r\n}\r\n!",
            "fn mn() {\r\n    let éé = 12;\r\n\r\n}\r\n!",
        ];
        for (n, text) in texts.iter().enumerate() {
            f.show(vec![f.doc("a.fk", n as u64 + 1, text)]);
            f.wait_method_count("textDocument/didChange", n + 1);
            let want = text.to_string();
            f.wait_for("the server's copy to match", || {
                std::fs::read_to_string(dir_mirror(&f))
                    .ok()
                    .and_then(|m| serde_json::from_str::<Value>(&m).ok())
                    .and_then(|m| m.as_object()?.values().next()?.as_str().map(str::to_owned))
                    .filter(|mirrored| *mirrored == want)
            });
        }
        let changes: Vec<Value> = f
            .log()
            .into_iter()
            .filter(|m| m["method"] == "textDocument/didChange")
            .collect();
        assert!(
            changes
                .iter()
                .all(|c| c["params"]["contentChanges"][0]["range"].is_object())
        );
    }
}

mod file_operations {
    use super::*;
    use crate::infrastructure::lsp::manager::FileChange;

    #[test]
    fn servers_hear_about_the_files_their_filters_accept() {
        let f = Fixture::new(&["--features", "fileOps"]);
        f.show(vec![f.doc("a.fk", 0, "x")]);
        f.wait_status("ready", |s| matches!(s, Status::Ready));
        f.wait_method_count("textDocument/didOpen", 1);
        let a = f.dir.join("one.rs");
        let b = f.dir.join("two.rs");
        f.mgr().files_changed(FileChange::Created(a.clone()));
        f.mgr().files_changed(FileChange::Renamed {
            from: a.clone(),
            to: b.clone(),
        });
        f.mgr().files_changed(FileChange::Deleted(b));
        // A file the filters do not cover is not announced.
        f.mgr()
            .files_changed(FileChange::Created(f.dir.join("notes.txt")));
        f.wait_method_count("workspace/didDeleteFiles", 1);
        let methods = f.methods();
        assert_eq!(
            methods
                .iter()
                .filter(|m| *m == "workspace/didCreateFiles")
                .count(),
            1
        );
        assert_eq!(
            methods
                .iter()
                .filter(|m| *m == "workspace/didRenameFiles")
                .count(),
            1
        );
        let rename = f
            .log()
            .into_iter()
            .find(|m| m["method"] == "workspace/didRenameFiles")
            .unwrap();
        assert!(
            rename["params"]["files"][0]["oldUri"]
                .as_str()
                .unwrap()
                .ends_with("one.rs")
        );
        assert!(
            rename["params"]["files"][0]["newUri"]
                .as_str()
                .unwrap()
                .ends_with("two.rs")
        );
    }

    #[test]
    fn a_server_that_did_not_ask_hears_nothing() {
        let f = Fixture::new(&[]);
        f.show(vec![f.doc("a.fk", 0, "x")]);
        f.wait_status("ready", |s| matches!(s, Status::Ready));
        f.mgr()
            .files_changed(FileChange::Created(f.dir.join("x.rs")));
        std::thread::sleep(Duration::from_millis(200));
        assert!(!f.methods().iter().any(|m| m.starts_with("workspace/did")));
    }
}

#[test]
fn a_server_that_offers_deltas_is_asked_for_differences_after_the_first_result() {
    let f = Fixture::new(&["--semantic", "delta"]);
    f.show(vec![f.doc("a.fk", 0, "hello world")]);
    let first = f.wait_for("the first tokens", || f.results_at("a.fk", 0).semantic);
    assert_eq!(first[0].range, 0..2);
    f.show(vec![f.doc("a.fk", 1, "hello brave world")]);
    // The second answer is a difference to the first, and it is applied.
    let second = f.wait_for("the tokens of the new text", || {
        f.results_at("a.fk", 1).semantic
    });
    assert_eq!(second[0].range, 0..3);
    let methods = f.methods();
    assert!(methods.contains(&"textDocument/semanticTokens/full".to_owned()));
    assert!(methods.contains(&"textDocument/semanticTokens/full/delta".to_owned()));
    let delta = f
        .log()
        .into_iter()
        .find(|m| m["method"] == "textDocument/semanticTokens/full/delta")
        .unwrap();
    assert_eq!(delta["params"]["previousResultId"], "r1");
}

#[test]
fn a_server_that_answers_pull_requests_is_asked_for_problems_after_edits() {
    let f = Fixture::new(&["--features", "pull", "--pull-diagnostic", "pulled warning"]);
    f.show(vec![f.doc("a.fk", 0, "one")]);
    let found = f.wait_for("the pulled problems", || {
        f.results_at("a.fk", 0).diagnostics
    });
    assert_eq!(found[0].message, "pulled warning");
    // After an edit it asks again with the last result id; "unchanged" keeps them.
    f.show(vec![f.doc("a.fk", 1, "one two")]);
    f.wait_method_count("textDocument/diagnostic", 2);
    let found = f.wait_for("the problems of the new text", || {
        f.results_at("a.fk", 1).diagnostics
    });
    assert_eq!(found[0].message, "pulled warning");
    let asked: Vec<_> = f
        .log()
        .into_iter()
        .filter(|m| m["method"] == "textDocument/diagnostic")
        .collect();
    assert!(asked.len() >= 2, "{asked:?}");
    assert_eq!(asked[1]["params"]["previousResultId"], "d1");
}

#[test]
fn diagnostic_refresh_repulls_all_open_files_without_an_editor_change() {
    let f = Fixture::new(&[
        "--sync",
        "object",
        "--features",
        "pull",
        "--pull-refresh-on-save",
    ]);
    f.show(vec![f.doc("a.fk", 0, "one"), f.doc("b.fk", 0, "two")]);
    for file in ["a.fk", "b.fk"] {
        f.wait_for("initial pulled diagnostics", || {
            f.results_at(file, 0).diagnostics.filter(|d| !d.is_empty())
        });
    }
    f.mgr().saved(f.dir.join("a.fk"), Arc::new("one".into()));
    for file in ["a.fk", "b.fk"] {
        f.wait_for("refreshed diagnostics at the same stamp", || {
            f.results_at(file, 0).diagnostics.filter(|d| d.is_empty())
        });
    }
    f.wait_method_count("textDocument/diagnostic", 4);
    assert!(!f.methods().contains(&"textDocument/didChange".to_owned()));
    assert!(
        f.log()
            .iter()
            .any(|m| m["id"] == 9101 && m["result"].is_null())
    );
}
