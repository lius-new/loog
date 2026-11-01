//! The extension registry in the application: the index the Available list is
//! made from, fetching it, and installing or updating from it.
//!
//! What is shown comes from the cached or shipped index at once (no network), and
//! the newest index is fetched on request. Installing plans what the extension
//! requires, asks the user first when the plan asks for anything that matters, then
//! downloads and installs in the background and loads the result.
use super::ExtensionHost;
use crate::app::context::AppContext;
use crate::app::state::AppState;
use crate::features::extensions::registry::{
    Entry as IndexEntry, Installed, VersionEntry, current_platform, plan_install, updates,
};
use crate::infrastructure::extensions::registry::{self as fetch, Loaded};
use crate::infrastructure::extensions::{install, package::Imported};
use crate::ui::extensions_binding::{Contribution, Entry, Frame, RegistryStatus, Table, Update};
use std::collections::{BTreeMap, BTreeSet};

const LOOM: &str = env!("CARGO_PKG_VERSION");

/// What the registry can do about a file whose language Loom does not know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Suggestion {
    /// Install this extension.
    Install { id: String, name: String },
    /// It is installed; languages are read at startup.
    Restart { name: String },
}

#[derive(Default)]
pub(super) struct State {
    loaded: Option<Loaded>,
    /// The cache and the shipped index were looked at.
    local_tried: bool,
    refreshing: bool,
    /// A fetch was started in this run, so opening the list does not start another.
    refreshed: bool,
    problem: Option<String>,
    installing: BTreeSet<String>,
}

/// What an extension asks for, in words. Only what the user would want to know first.
fn asks(version: &VersionEntry) -> Vec<String> {
    let mut asks = Vec::new();
    for capability in &version.capabilities {
        let text = match capability.as_str() {
            "process.execute-local" => "Run programs on your computer, such as a language server",
            "network" => "Connect to the internet",
            "workspace.edit" => "Change the text of your open files",
            "workspace.files.write" => "Create, rename and delete files in your folders",
            "workspace.files.read" => "Read files in your folders",
            "tasks" => "Run background tasks",
            _ => continue,
        };
        asks.push(text.to_owned());
    }
    for requirement in &version.requires {
        asks.push(format!(
            "Install {requirement} with it, which it needs to run"
        ));
    }
    asks
}

/// The file types a file name can be recognized by, longest first: `a.d.ts` is
/// `d.ts` and `ts`; a name without a dot (`Dockerfile`, `.gitignore`) stands for itself.
fn file_types(name: &str) -> Vec<String> {
    let lower = name.to_lowercase();
    let trimmed = lower.trim_start_matches('.');
    let parts: Vec<&str> = trimmed.split('.').collect();
    if parts.len() == 1 {
        return vec![trimmed.to_owned()];
    }
    (1..parts.len())
        .map(|from| parts[from..].join("."))
        .collect()
}

/// The language extension of `index` that recognizes a file called `name`, longest
/// file type first; `installed` says which extensions are installed already.
fn suggestion(
    index: &crate::features::extensions::registry::Index,
    name: &str,
    platform: &str,
    installed: impl Fn(&str) -> bool,
) -> Option<Suggestion> {
    for file_type in file_types(name) {
        for entry in index.for_file_type(&file_type) {
            if !entry.id.starts_with("loom.lang.") {
                continue;
            }
            let display = entry.name.trim_end_matches(" Language").to_owned();
            if installed(&entry.id) {
                return Some(Suggestion::Restart { name: display });
            }
            if entry.best_for(LOOM, platform).is_some() {
                return Some(Suggestion::Install {
                    id: entry.id.clone(),
                    name: display,
                });
            }
        }
    }
    None
}

fn index_entry(entry: &IndexEntry, version: &VersionEntry) -> Entry {
    let mut contributions = Vec::new();
    if !entry.languages.is_empty() {
        let rows: Vec<[String; 3]> = entry
            .languages
            .iter()
            .map(|language| {
                [
                    language.name.clone(),
                    language.file_types.join(", "),
                    String::new(),
                ]
            })
            .collect();
        contributions.push(Contribution::Table(Table {
            icon: "extensions",
            title: "Languages",
            total: rows.len(),
            head: ["Language", "File types", ""],
            mono: [true, true, false],
            rows,
            more: 0,
        }));
    }
    Entry {
        id: entry.id.clone(),
        name: entry.name.clone(),
        publisher: entry.publisher.clone(),
        version: version.version.clone(),
        tagline: entry.description.clone(),
        updated: String::new(),
        icon: if entry.categories.iter().any(|c| c == "Themes") {
            "extension-theme"
        } else {
            "extension-code"
        },
        tint: 0x38bdf8,
        icon_image: None,
        categories: entry.categories.clone(),
        readme: Vec::new(),
        readme_document: None,
        contributions,
        changelog: Vec::new(),
        changelog_document: None,
        installed: None,
    }
}

impl ExtensionHost {
    fn root(&self) -> Option<std::path::PathBuf> {
        self.directory().map(std::path::Path::to_path_buf)
    }

    /// Read the cached or shipped index, once. No network.
    fn read_local(&self) {
        let mut state = self.inner.registry.lock().unwrap();
        if state.local_tried {
            return;
        }
        state.local_tried = true;
        if state.loaded.is_some() {
            return;
        }
        let now = fetch::now();
        let cached = self
            .root()
            .and_then(|root| fetch::load_cached(&fetch::cache_dir(&root), fetch::KEYS, now));
        let shipped =
            fetch::shipped_dir().and_then(|dir| fetch::load_snapshot(&dir, fetch::KEYS, now));
        state.loaded = [cached, shipped]
            .into_iter()
            .flatten()
            .max_by_key(|loaded| loaded.index.sequence);
    }

    /// Add to the installed entries what is recorded about their versions: whether
    /// the version is kept, and which earlier one they can go back to.
    pub(super) fn add_management(&self, frame: &mut Frame) {
        use crate::infrastructure::extensions::state;
        let Some(root) = self.root() else {
            return;
        };
        let recorded = state::read(&root);
        for entry in &mut frame.installed {
            let (Some(installed), Some(kept)) =
                (&mut entry.installed, recorded.extensions.get(&entry.id))
            else {
                continue;
            };
            installed.pinned = kept.pinned;
            installed.previous = kept.previous.clone().filter(|previous| {
                state::version_dir(&root, &entry.id, previous)
                    .join("manifest.json")
                    .is_file()
            });
        }
    }

    /// Keep `id` at its version (or stop keeping it there).
    pub(crate) fn set_pinned(&self, context: &AppContext, id: &str, pinned: bool) {
        let Some(root) = self.root() else {
            return;
        };
        let id = id.to_owned();
        context.update(move |app| {
            match crate::infrastructure::extensions::state::set_pinned(&root, &id, pinned) {
                Ok(()) => app.show_toast(if pinned {
                    format!("{id} stays at this version")
                } else {
                    format!("{id} may be updated again")
                }),
                Err(error) => app.show_error(format!("{id}: {error}")),
            }
        });
    }

    /// Go back to the version of `id` in use before the current one, and keep it there:
    /// the newer one would otherwise be offered at once.
    pub(crate) fn roll_back(&self, context: &AppContext, id: &str) {
        use crate::infrastructure::extensions::state;
        let Some(root) = self.root() else {
            return;
        };
        let host = self.clone_handle();
        let id = id.to_owned();
        context.update(move |app| {
            let (from, to) = match state::roll_back(&root, &id) {
                Ok(versions) => versions,
                Err(error) => return app.show_error(format!("{id}: {error}")),
            };
            let _ = state::set_pinned(&root, &id, true);
            let dir = state::version_dir(&root, &id, &to);
            let adopted = host.inner.service.lock().unwrap().adopt(&dir);
            match adopted {
                Ok((_, effects)) => host.inner.apply(app, effects),
                Err(error) => return app.show_error(format!("{id}: {error}")),
            }
            host.refresh_settings(app);
            app.show_success(format!(
                "{id} is back at {to} (it was {from}), and stays there"
            ));
        });
    }

    /// Add what the registry offers to `frame`: the extensions to install, the
    /// updates of installed ones, and how the registry stands.
    pub(super) fn add_registry(&self, frame: &mut Frame) {
        self.read_local();
        let state = self.inner.registry.lock().unwrap();
        frame.registry = RegistryStatus {
            refreshing: state.refreshing,
            problem: state
                .problem
                .clone()
                .or_else(|| state.loaded.as_ref().and_then(|l| l.problem.clone())),
            expired: state.loaded.as_ref().is_some_and(|l| l.expired),
            installing: state.installing.iter().cloned().collect(),
            loaded: state.loaded.is_some(),
        };
        let Some(loaded) = &state.loaded else {
            return;
        };
        // An expired index offers nothing new.
        let Some(platform) = current_platform().filter(|_| !loaded.expired) else {
            return;
        };
        let recorded = self
            .root()
            .map(|root| crate::infrastructure::extensions::state::read(&root))
            .unwrap_or_default();
        let mut installed = BTreeMap::new();
        let mut known = BTreeSet::new();
        for package in self.inner.service.lock().unwrap().packages() {
            known.insert(package.manifest.id.clone());
            // What ships with Loom is updated with Loom.
            if !package.is_bundled() {
                installed.insert(
                    package.manifest.id.clone(),
                    Installed {
                        version: package.manifest.version.clone(),
                        requires: package.manifest.requires.clone(),
                        pinned: recorded
                            .extensions
                            .get(&package.manifest.id)
                            .is_some_and(|entry| entry.pinned),
                    },
                );
            }
        }
        for (id, _, to) in updates(&loaded.index, &installed, LOOM, platform) {
            let Some(entry) = frame.installed.iter_mut().find(|entry| entry.id == id) else {
                continue;
            };
            if let Some(installed) = &mut entry.installed {
                installed.update = Some(Update {
                    version: to.clone(),
                    notes: String::new(),
                });
            }
            if let Some(version) = loaded
                .index
                .entry(&id)
                .and_then(|e| e.versions.iter().find(|v| v.version == to))
            {
                let asks = asks(version);
                if !asks.is_empty() {
                    frame.asks.insert(id, asks);
                }
            }
        }
        for entry in &loaded.index.extensions {
            if known.contains(&entry.id) {
                continue;
            }
            if let Some((version, _)) = entry.best_for(LOOM, platform) {
                frame.available.push(index_entry(entry, version));
                let asks = asks(version);
                if !asks.is_empty() {
                    frame.asks.insert(entry.id.clone(), asks);
                }
            }
        }
        frame.available.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.id.cmp(&b.id))
        });
    }

    /// Fetch the newest index in the background. Without `force`, only the first time
    /// in a run.
    pub(crate) fn refresh_registry(&self, context: &AppContext, force: bool) {
        let Some(root) = self.root() else {
            return;
        };
        {
            let mut state = self.inner.registry.lock().unwrap();
            if state.refreshing || (state.refreshed && !force) {
                return;
            }
            state.refreshing = true;
            state.refreshed = true;
        }
        let inner = self.inner.clone();
        let context = context.clone();
        let started = std::thread::Builder::new()
            .name("loom-registry".into())
            .spawn({
                let inner = inner.clone();
                move || {
                    let result = fetch::current(
                        &fetch::cache_dir(&root),
                        fetch::shipped_dir().as_deref(),
                        fetch::BASE,
                        fetch::KEYS,
                        fetch::now(),
                        &fetch::http,
                    );
                    let application = inner.application.clone();
                    application.post(move || {
                        {
                            let mut state = inner.registry.lock().unwrap();
                            state.refreshing = false;
                            match result {
                                Ok(loaded) => {
                                    state.problem = loaded.problem.clone();
                                    state.loaded = Some(loaded);
                                }
                                Err(problem) => state.problem = Some(problem),
                            }
                        }
                        // Repaint with the new index.
                        context.update(|_| {});
                    });
                }
            });
        if started.is_err() {
            self.inner.registry.lock().unwrap().refreshing = false;
        }
    }

    /// What would give a file called `name` the syntax it lacks: a language extension of
    /// the registry to install, or one that is installed already and waits for a restart.
    /// `None` when the registry knows nothing for the file's type.
    pub(crate) fn suggest_for(&self, name: &str) -> Option<Suggestion> {
        self.read_local();
        let state = self.inner.registry.lock().unwrap();
        let loaded = state.loaded.as_ref().filter(|loaded| !loaded.expired)?;
        let platform = current_platform()?;
        let service = self.inner.service.lock().unwrap();
        suggestion(&loaded.index, name, platform, |id| {
            service.package(id).is_some()
        })
    }

    /// Show what the registry offers for a file called `name`: the Extensions page,
    /// on the extensions that recognize its type.
    pub(crate) fn find_for_file(&self, context: &AppContext, name: &str) {
        let types = file_types(name);
        let mut frame = Frame {
            installed: crate::app::extensions_view::installed_frame(
                &self.inner.service.lock().unwrap(),
            )
            .installed,
            ..Frame::default()
        };
        self.add_registry(&mut frame);
        let found: Vec<String> = {
            let state = self.inner.registry.lock().unwrap();
            state
                .loaded
                .iter()
                .flat_map(|loaded| types.iter().flat_map(|t| loaded.index.for_file_type(t)))
                .map(|entry| entry.id.clone())
                .filter(|id| frame.available.iter().any(|offered| offered.id == *id))
                .collect()
        };
        let loaded = frame.registry.loaded;
        let search = types.first().cloned().unwrap_or_default();
        let name = name.to_owned();
        context.update(move |app| {
            if found.is_empty() && loaded {
                return app.show_toast(format!(
                    "No extension in the registry adds anything for {name}"
                ));
            }
            app.extensions_ui
                .show_installed(found.first().cloned().unwrap_or_default());
            app.extensions_ui.filter = crate::ui::extensions_binding::StateFilter::Available;
            app.extensions_ui.search.set_text(search);
            app.apply(crate::features::keymap::action::Action::OpenExtensions);
        });
        self.refresh_registry(context, false);
    }

    /// What installing or updating `id` would ask the user to accept.
    fn asks_for(&self, id: &str) -> Vec<String> {
        // The installed entries are only needed to find the updates.
        let mut frame = Frame {
            installed: crate::app::extensions_view::installed_frame(
                &self.inner.service.lock().unwrap(),
            )
            .installed,
            ..Frame::default()
        };
        self.add_registry(&mut frame);
        frame.asks.remove(id).unwrap_or_default()
    }

    /// The user pressed Install or Update for `id`. When the install asks for
    /// something that matters, the first press only shows what it asks for.
    pub(crate) fn install(&self, context: &AppContext, id: &str) {
        let asks = self.asks_for(id);
        let host = self.clone_handle();
        let context = context.clone();
        let id = id.to_owned();
        context.clone().update(move |app| {
            if !asks.is_empty() && app.extensions_ui.confirming.as_deref() != Some(id.as_str()) {
                app.extensions_ui.confirming = Some(id);
                return;
            }
            app.extensions_ui.confirming = None;
            host.install_now(&context, app, &id);
        });
    }

    fn clone_handle(&self) -> ExtensionHost {
        ExtensionHost {
            inner: self.inner.clone(),
        }
    }

    fn install_now(&self, context: &AppContext, app: &mut AppState, id: &str) {
        let Some(root) = self.root() else {
            return app.show_error("There is no folder for extensions on this system");
        };
        let plan = {
            let state = self.inner.registry.lock().unwrap();
            let Some(loaded) = state.loaded.as_ref().filter(|loaded| !loaded.expired) else {
                return app.show_error("The extension registry is out of date; refresh it first");
            };
            let Some(platform) = current_platform() else {
                return app.show_error("There are no extensions for this kind of computer");
            };
            plan_install(
                &loaded.index,
                &install::installed(&root),
                id,
                LOOM,
                platform,
            )
        };
        let steps = match plan {
            Ok(steps) if steps.is_empty() => {
                return app.show_toast(format!("{id} is up to date"));
            }
            Ok(steps) => steps,
            Err(error) => return app.show_error(format!("Cannot install {id}: {error}")),
        };
        let ids: Vec<String> = steps.iter().map(|step| step.id.clone()).collect();
        self.inner
            .registry
            .lock()
            .unwrap()
            .installing
            .extend(ids.iter().cloned());
        let inner = self.inner.clone();
        let context = context.clone();
        let planned = ids.clone();
        let spawned = std::thread::Builder::new()
            .name("loom-extension-install".into())
            .spawn(move || {
                let ids = planned;
                let result = install::install(&root, &steps, LOOM, &install::download);
                let application = inner.application.clone();
                application.post(move || {
                    context.update(|app| {
                        ExtensionHost { inner }.finish_install(app, &ids, result);
                    });
                });
            });
        if spawned.is_err() {
            let mut state = self.inner.registry.lock().unwrap();
            for id in &ids {
                state.installing.remove(id);
            }
            app.show_error("Could not start the installation");
        }
    }

    /// The installation ended: load what was installed, or say why not.
    fn finish_install(
        &self,
        app: &mut AppState,
        ids: &[String],
        result: Result<Vec<Imported>, String>,
    ) {
        {
            let mut state = self.inner.registry.lock().unwrap();
            for id in ids {
                state.installing.remove(id);
            }
        }
        let done = match result {
            Ok(done) => done,
            Err(error) => return app.show_error(format!("Could not install: {error}")),
        };
        let mut problems = Vec::new();
        for imported in &done {
            let adopted = self
                .inner
                .service
                .lock()
                .unwrap()
                .adopt(&imported.directory);
            match adopted {
                Ok((_, effects)) => self.inner.apply(app, effects),
                Err(error) => problems.push(format!("{}: {error}", imported.id)),
            }
        }
        self.refresh_settings(app);
        // The extension that was asked for is the last of the plan.
        let Some(asked) = done.last() else {
            return;
        };
        app.extensions_ui.show_installed(asked.id.clone());
        if !problems.is_empty() {
            return app.show_error(format!(
                "Installed {} but could not load: {}",
                asked.name,
                problems.join("; ")
            ));
        }
        let also = done.len() - 1;
        let suggested = self.recommended_after(asked);
        app.show_success(format!(
            "{} {} installed{}{}{}",
            asked.name,
            asked.version,
            if also > 0 {
                format!(", with {also} it needs")
            } else {
                String::new()
            },
            if done.iter().any(|imported| imported.needs_restart) {
                " (restart Loom to use its language)"
            } else {
                ""
            },
            if suggested.is_empty() {
                String::new()
            } else {
                format!(". You may also want {}", suggested.join(", "))
            }
        ));
    }

    /// The names of the extensions the installed `imported` recommends that the
    /// registry offers and that are not installed.
    fn recommended_after(&self, imported: &Imported) -> Vec<String> {
        let recommends = std::fs::read_to_string(imported.directory.join("manifest.json"))
            .ok()
            .and_then(|text| crate::features::extensions::manifest::Manifest::parse(&text).ok())
            .map(|manifest| manifest.recommends)
            .unwrap_or_default();
        if recommends.is_empty() {
            return Vec::new();
        }
        let state = self.inner.registry.lock().unwrap();
        let Some(loaded) = state.loaded.as_ref() else {
            return Vec::new();
        };
        let service = self.inner.service.lock().unwrap();
        recommends
            .iter()
            .filter(|id| service.package(id).is_none())
            .filter_map(|id| loaded.index.entry(id))
            .map(|entry| entry.name.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(capabilities: &[&str], requires: &[&str]) -> VersionEntry {
        VersionEntry {
            version: "1.0.0".into(),
            engines: None,
            capabilities: capabilities.iter().map(|c| (*c).to_owned()).collect(),
            requires: requires.iter().map(|r| (*r).to_owned()).collect(),
            recommends: Vec::new(),
            artifacts: Vec::new(),
        }
    }

    #[test]
    fn a_file_without_syntax_is_offered_the_language_extension_or_a_restart() {
        use crate::features::extensions::registry::Index;
        let sha = "a".repeat(64);
        let index = Index::parse(
            format!(
                r#"{{ "schema": 1, "generated": "2026-11-01T00:00:00.000Z",
                  "expires": "2026-12-01T00:00:00.000Z", "sequence": 1, "keyId": "k",
                  "extensions": [
                    {{ "id": "loom.lang.rust", "name": "Rust Language",
                       "languages": [{{ "name": "rust", "fileTypes": ["rs"] }}],
                       "versions": [{{ "version": "1.0.0", "artifacts": [
                         {{ "platform": "any", "url": "https://x/r", "sha256": "{sha}", "size": 3 }}] }}] }},
                    {{ "id": "loom.lsp.rust-analyzer", "name": "rust-analyzer",
                       "languages": [{{ "name": "rust", "fileTypes": ["rs"] }}],
                       "versions": [{{ "version": "1.0.0", "artifacts": [
                         {{ "platform": "any", "url": "https://x/a", "sha256": "{sha}", "size": 3 }}] }}] }}
                  ] }}"#
            )
            .as_bytes(),
        )
        .unwrap();
        let none = |_: &str| false;
        assert_eq!(
            suggestion(&index, "lib.rs", "linux-x86_64", none),
            Some(Suggestion::Install {
                id: "loom.lang.rust".into(),
                name: "Rust".into()
            })
        );
        // Installed already: the language is read at startup.
        assert_eq!(
            suggestion(&index, "lib.RS", "linux-x86_64", |id| id
                == "loom.lang.rust"),
            Some(Suggestion::Restart {
                name: "Rust".into()
            })
        );
        // Nothing for another type, and only languages are offered for syntax.
        assert_eq!(suggestion(&index, "main.zig", "linux-x86_64", none), None);
    }

    #[test]
    fn a_file_name_is_recognized_by_its_suffixes_longest_first() {
        assert_eq!(file_types("lib.rs"), ["rs"]);
        assert_eq!(file_types("types.D.ts"), ["d.ts", "ts"]);
        assert_eq!(file_types("Dockerfile"), ["dockerfile"]);
        assert_eq!(file_types(".gitignore"), ["gitignore"]);
        assert_eq!(file_types("a.b.c"), ["b.c", "c"]);
    }

    #[test]
    fn only_what_matters_is_put_to_the_user() {
        assert!(asks(&version(&[], &[])).is_empty());
        assert!(asks(&version(&["documents.read", "events", "settings"], &[])).is_empty());
        let asked = asks(&version(
            &["process.execute-local", "events"],
            &["loom.runtime.node@^22"],
        ));
        assert_eq!(asked.len(), 2);
        assert!(asked[0].contains("Run programs"));
        assert!(asked[1].contains("loom.runtime.node@^22"));
    }
}
