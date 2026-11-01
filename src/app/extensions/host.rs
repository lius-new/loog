//! Connects the extension service to the executor thread and the UI thread.
use super::{Effect, ExtensionService, LanguageAsk};
use crate::app::context::AppContext;
use crate::app::language_services::ExternalAnswers;
use crate::app::state::AppState;
use crate::domain::identity::FileId;
use crate::domain::language::Registry;
use crate::features::language_service::capabilities::PositionEncoding;
use crate::infrastructure::extensions::tasks;
use crate::infrastructure::extensions::trust_store::TrustStore;
use crate::infrastructure::extensions::{Event, Executor};
use crate::infrastructure::lsp::manager::{EXTERNAL_RANK, Part, PartSink, RequestSpec};
use lgui::ApplicationHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};

struct Inner {
    service: Mutex<ExtensionService>,
    executor: OnceLock<Executor>,
    runner: OnceLock<tasks::Runner>,
    /// The host's own handle, so background work can come back to it.
    self_ref: OnceLock<std::sync::Weak<Inner>>,
    trust: Arc<TrustStore>,
    user_settings: Option<PathBuf>,
    context: AppContext,
    application: ApplicationHandle,
    /// What to do when a save chain ends, by chain number.
    saves: Mutex<std::collections::HashMap<u64, SaveContinuation>>,
    /// The language servers last handed to the language service.
    published_servers: Mutex<Vec<crate::infrastructure::lsp::manager::Contributed>>,
    /// Where the answers of a language request go, with how many are still owed.
    language_sinks: Mutex<std::collections::HashMap<u64, (PartSink, usize)>>,
    next_language: std::sync::atomic::AtomicU64,
    hooked: std::sync::atomic::AtomicBool,
    /// The themes revision last published.
    themes_seen: std::sync::atomic::AtomicU64,
    /// The registry index and what is being done with it.
    registry: Mutex<registry::State>,
}

mod registry;
pub(crate) use registry::Suggestion;

/// Runs on the UI thread when the participants are done; `true` means write the files.
pub(crate) type SaveContinuation = Box<dyn FnOnce(&AppContext, bool) + Send>;

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(runner) = self.runner.get() {
            runner.cancel_all();
        }
    }
}

impl Inner {
    fn apply(&self, app: &mut AppState, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Submit(job) => {
                    if let Some(executor) = self.executor.get() {
                        executor.submit(job);
                    }
                }
                Effect::PublishDiagnostics {
                    source,
                    path,
                    stamp,
                    text,
                    items,
                } => {
                    if let Some(services) = self
                        .context
                        .services()
                        .and_then(|runtime| runtime.language_services())
                    {
                        services.publish_external(source, &path, stamp, text, items.into());
                    }
                }
                Effect::ClearDiagnostics { source } => {
                    if let Some(services) = self
                        .context
                        .services()
                        .and_then(|runtime| runtime.language_services())
                    {
                        services.clear_external(source);
                    }
                }
                Effect::OfferCompletion {
                    file,
                    incarnation,
                    word_start,
                    source,
                    rank,
                    items,
                } => crate::app::language_ui::completion::offer_external(
                    app,
                    file,
                    incarnation,
                    word_start,
                    source,
                    rank,
                    items,
                ),
                Effect::ResolvedCompletion { source, raw, item } => {
                    crate::app::language_ui::completion::offer_resolved(app, source, &raw, &item)
                }
                Effect::OfferSnippets {
                    file,
                    incarnation,
                    word_start,
                    source,
                    rank,
                    items,
                } => crate::app::language_ui::completion::offer_snippets(
                    app,
                    file,
                    incarnation,
                    word_start,
                    source,
                    rank,
                    items,
                ),
                Effect::RunFileOps { key, ops } => {
                    let inner = self.context.clone();
                    let application = self.application.clone();
                    let service = self.self_ref.get().cloned();
                    std::thread::spawn(move || {
                        let outcome = crate::infrastructure::extensions::fileops::run(&ops, |_| {});
                        application.post(move || {
                            inner.update(|app| {
                                if let Some(this) = service.and_then(|weak| weak.upgrade()) {
                                    let effects = this
                                        .service
                                        .lock()
                                        .unwrap()
                                        .file_ops_done(app, key, outcome);
                                    this.apply(app, effects);
                                }
                            });
                        });
                    });
                }
                Effect::FilesChanged(changes) => {
                    if let Some(services) = self
                        .context
                        .services()
                        .and_then(|runtime| runtime.language_services())
                    {
                        for change in changes {
                            services.files_changed(change);
                        }
                    }
                }
                Effect::SaveReady { chain, ok, message } => {
                    if let Some(message) = message {
                        app.show_error(format!(
                            "Not saved: {message}. Use Save Without Extensions to skip them."
                        ));
                    }
                    if let Some(then) = self.saves.lock().unwrap().remove(&chain) {
                        // Not inside this update: the continuation updates the state itself.
                        let context = self.context.clone();
                        self.application.post(move || then(&context, ok));
                    }
                }
                Effect::StartTask { task, spec } => {
                    if let Some(runner) = self.runner.get() {
                        runner.start(task, spec);
                    }
                }
                Effect::CancelTask { task } => {
                    if let Some(runner) = self.runner.get() {
                        runner.cancel(task);
                    }
                }
                Effect::LanguageAnswer { token, answer } => {
                    // One request may wait on several providers; the sink goes
                    // when the last of them has answered.
                    let mut sinks = self.language_sinks.lock().unwrap();
                    if let Some((sink, remaining)) = sinks.get_mut(&token) {
                        sink.deliver(answer.map(|answer| Part {
                            instance: answer.source,
                            server: answer.provider.into(),
                            rank: EXTERNAL_RANK,
                            encoding: PositionEncoding::Utf8,
                            value: answer.value,
                        }));
                        *remaining -= 1;
                        if *remaining == 0 {
                            sinks.remove(&token);
                        }
                    }
                }
                Effect::Toast(message) => app.show_toast(message),
                Effect::Error(message) => app.show_error(message),
            }
        }
    }
}

/// Lets hover, definition, outline, signature and inlay requests of the
/// language UI reach extension providers (see `LanguageServices::request`).
struct LanguageHook {
    inner: std::sync::Weak<Inner>,
}

impl ExternalAnswers for LanguageHook {
    fn expected(&self, spec: &RequestSpec) -> usize {
        let Some(inner) = self.inner.upgrade() else {
            return 0;
        };
        let registry = Registry::builtin();
        let language = registry
            .definition_of(registry.resolve_path(&spec.path))
            .name
            .to_string();
        inner
            .service
            .lock()
            .unwrap()
            .language_providers(spec.feature, &language)
            .len()
    }

    fn ask(&self, spec: RequestSpec, expected: usize, sink: PartSink) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        // The request is made on the UI thread, possibly inside an update; the
        // providers are asked from the next turn of the application.
        let application = inner.application.clone();
        application.post(move || {
            let context = inner.context.clone();
            context.update(|app| {
                let token = inner.next_language.fetch_add(1, Ordering::Relaxed);
                let ask = LanguageAsk {
                    feature: spec.feature,
                    path: spec.path.clone(),
                    stamp: spec.stamp,
                    start: spec.start,
                    end: spec.end,
                };
                let mut effects = Vec::new();
                let asked =
                    inner
                        .service
                        .lock()
                        .unwrap()
                        .request_language(app, &ask, token, &mut effects);
                // The tokens of providers that were not asked still owe an answer.
                for _ in asked..expected {
                    sink.deliver(None);
                }
                if asked > 0 {
                    inner
                        .language_sinks
                        .lock()
                        .unwrap()
                        .insert(token, (sink, asked));
                }
                inner.apply(app, effects);
            });
        });
    }
}

/// The running extension subsystem of one mounted application.
pub(crate) struct ExtensionHost {
    inner: Arc<Inner>,
}

impl ExtensionHost {
    /// Start the executor. Events are handled on the UI thread through `application`.
    pub(crate) fn start(
        context: AppContext,
        application: ApplicationHandle,
        cache: PathBuf,
        trust: Arc<TrustStore>,
        user_settings: Option<PathBuf>,
    ) -> Result<Self, String> {
        let inner = Arc::new(Inner {
            service: Mutex::new(ExtensionService::new(cache)),
            executor: OnceLock::new(),
            runner: OnceLock::new(),
            self_ref: OnceLock::new(),
            trust,
            user_settings,
            context: context.clone(),
            application: application.clone(),
            saves: Mutex::new(std::collections::HashMap::new()),
            published_servers: Mutex::new(Vec::new()),
            language_sinks: Mutex::new(std::collections::HashMap::new()),
            next_language: std::sync::atomic::AtomicU64::new(1),
            hooked: std::sync::atomic::AtomicBool::new(false),
            themes_seen: std::sync::atomic::AtomicU64::new(0),
            registry: Mutex::new(registry::State::default()),
        });
        let _ = inner.self_ref.set(Arc::downgrade(&inner));
        inner.service.lock().unwrap().set_trust(inner.trust.clone());
        // Output and exits of tasks reach their guest through the UI thread, where
        // the service knows which extension and generation a task belongs to.
        let task_inner = inner.clone();
        let task_context = context.clone();
        let task_application = application.clone();
        let _ = inner.runner.set(tasks::Runner::new(move |event| {
            let inner = task_inner.clone();
            let context = task_context.clone();
            task_application.post(move || {
                context.update(|app| {
                    let effects = inner.service.lock().unwrap().handle_task_event(event);
                    inner.apply(app, effects);
                });
            });
        }));
        let sink_inner = inner.clone();
        let executor = Executor::spawn(Box::new(move |event: Event| {
            let inner = sink_inner.clone();
            let context = context.clone();
            application.post(move || {
                context.update(|app| {
                    let effects = inner.service.lock().unwrap().handle_event(app, event);
                    inner.apply(app, effects);
                });
            });
        }))?;
        let _ = inner.executor.set(executor);
        let host = Self { inner };
        host.attach_language_hook();
        Ok(host)
    }

    /// Let the language service ask extension providers too. Retried on every
    /// update until the language service exists.
    fn attach_language_hook(&self) {
        if self.inner.hooked.load(Ordering::Relaxed) {
            return;
        }
        if let Some(services) = self
            .inner
            .context
            .services()
            .and_then(|runtime| runtime.language_services())
        {
            services.set_external(Arc::new(LanguageHook {
                inner: Arc::downgrade(&self.inner),
            }));
            self.inner.hooked.store(true, Ordering::Relaxed);
        }
    }

    pub(crate) fn load_package(&self, dir: &Path) -> Result<String, String> {
        let mut service = self.inner.service.lock().unwrap();
        let id = service.load_package(dir)?;
        // Local development packages are granted what they declare; a packaged
        // install flow would ask the user instead (design §9).
        service.grant_declared(&id);
        Ok(id)
    }

    pub(crate) fn commands(&self) -> Vec<(String, String)> {
        self.inner.service.lock().unwrap().commands()
    }

    pub(crate) fn frame(&self) -> crate::ui::extensions_binding::Frame {
        let mut frame =
            crate::app::extensions_view::installed_frame(&self.inner.service.lock().unwrap());
        self.add_registry(&mut frame);
        frame
    }

    pub(crate) fn needs_attention(&self) -> bool {
        self.inner
            .service
            .lock()
            .unwrap()
            .packages()
            .any(|package| package.lifecycle.state() == super::State::Failed)
    }

    pub(crate) fn run_command(&self, context: &AppContext, command: &str) {
        context.update(|app| {
            let effects = self.inner.service.lock().unwrap().run_command(app, command);
            self.inner.apply(app, effects);
        });
    }

    /// Read the user and project setting files again and tell running
    /// extensions which values changed. Project values only count in a trusted
    /// folder. Files are read when asked for (start, reload), not watched.
    pub(crate) fn refresh_settings(&self, app: &AppState) {
        let user = self
            .inner
            .user_settings
            .as_deref()
            .and_then(read_json)
            .unwrap_or_else(|| serde_json::json!({}));
        let root = app.explorer.model.roots().first().cloned();
        let project = root
            .as_ref()
            .and_then(|root| read_json(&root.join(".loom/extension-settings.json")));
        let trusted = root
            .as_ref()
            .is_some_and(|root| self.inner.trust.is_trusted(root));
        let effects = self
            .inner
            .service
            .lock()
            .unwrap()
            .set_setting_sources(user, project, trusted);
        for effect in effects {
            if let (Effect::Submit(job), Some(executor)) = (effect, self.inner.executor.get()) {
                executor.submit(job);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn service(&self) -> std::sync::MutexGuard<'_, ExtensionService> {
        self.inner.service.lock().unwrap()
    }

    /// Ask the extensions' completion providers about the word being typed.
    /// Returns how many were asked; their answers reach the open completion
    /// session through [`Effect::OfferCompletion`].
    pub(crate) fn request_completion(
        &self,
        app: &AppState,
        word_start: usize,
        cursor: usize,
        trigger: Option<char>,
    ) -> usize {
        let (asked, effects) = self
            .inner
            .service
            .lock()
            .unwrap()
            .request_completion(app, word_start, cursor, trigger);
        self.submit(effects);
        asked
    }

    /// Add the snippets packages contribute for this language to the open
    /// completion session.
    pub(crate) fn offer_snippets(&self, app: &mut AppState, word_start: usize) {
        let effects = self
            .inner
            .service
            .lock()
            .unwrap()
            .snippet_offers(app, word_start);
        self.inner.apply(app, effects);
    }

    /// Ask the extension that offered a completion item to complete it.
    pub(crate) fn request_completion_resolve(
        &self,
        app: &AppState,
        source: u64,
        raw: &serde_json::Value,
    ) {
        let effects = self
            .inner
            .service
            .lock()
            .unwrap()
            .request_completion_resolve(app, source, raw);
        self.submit(effects);
    }

    fn submit(&self, effects: Vec<Effect>) {
        for effect in effects {
            if let (Effect::Submit(job), Some(executor)) = (effect, self.inner.executor.get()) {
                executor.submit(job);
            }
        }
    }

    /// Tell subscribed extensions about document changes this update made.
    pub(crate) fn observe(&self, app: &AppState) {
        self.attach_language_hook();
        let (effects, servers, themes) = {
            let mut service = self.inner.service.lock().unwrap();
            let effects = service.observe(app);
            let revision = service.themes_revision();
            let themes = (self.inner.themes_seen.swap(revision, Ordering::Relaxed) != revision)
                .then(|| service.themes());
            (effects, service.language_servers(), themes)
        };
        if let Some(themes) = themes {
            self.publish_themes(themes);
        }
        for effect in effects {
            if let (Effect::Submit(job), Some(executor)) = (effect, self.inner.executor.get()) {
                executor.submit(job);
            }
        }
        self.publish_language_servers(servers);
    }

    /// Make the packages' themes available for choosing. A theme that settings
    /// named before its package loaded is chosen now, from the next turn of the
    /// application (this runs inside an update).
    fn publish_themes(&self, themes: Vec<super::PackageTheme>) {
        crate::ui::theme::set_package_themes(themes);
        if let Some(wanted) = crate::features::settings::theme_id::ThemeId::take_wanted() {
            let context = self.inner.context.clone();
            self.inner.application.post(move || {
                context.update(|app| {
                    if app.preferences.values().theme == Default::default() {
                        app.preferences.values_mut().theme = wanted;
                    }
                });
            });
        }
    }

    /// Hand the language service the servers packages declare when they, or the
    /// folders and permissions they run under, changed since the last time.
    fn publish_language_servers(
        &self,
        servers: Vec<crate::infrastructure::lsp::manager::Contributed>,
    ) {
        let mut last = self.inner.published_servers.lock().unwrap();
        if *last == servers {
            return;
        }
        // Without a language service nothing is recorded, so the list is offered again.
        if let Some(services) = self
            .inner
            .context
            .services()
            .and_then(|runtime| runtime.language_services())
        {
            last.clone_from(&servers);
            services.set_contributed(servers);
        }
    }

    /// Allow extensions to run programs for `root`; returns how many were allowed.
    pub(crate) fn allow_tools(&self, root: &Path) -> usize {
        self.inner.service.lock().unwrap().allow_tools(root)
    }

    /// A folder lost its trust or its tool permission: what runs there stops.
    pub(crate) fn revoke_tools(&self, root: &Path) {
        let effects = self.inner.service.lock().unwrap().revoke_tools(root);
        for effect in effects {
            if let (Effect::CancelTask { task }, Some(runner)) = (effect, self.inner.runner.get()) {
                runner.cancel(task);
            }
        }
    }

    /// Whether save participants would run for these files.
    pub(crate) fn participates(&self, app: &AppState, files: &[FileId]) -> bool {
        self.inner
            .service
            .lock()
            .unwrap()
            .has_participants(app, files)
    }

    /// Run the save participants over `files`, then call `then` on the UI
    /// thread with whether the files may be written. Without any participant
    /// it is called straight away.
    pub(crate) fn prepare_save(
        &self,
        context: &AppContext,
        files: Vec<FileId>,
        then: SaveContinuation,
    ) {
        let mut then = Some(then);
        context.update(|app| {
            let busy = {
                let service = self.inner.service.lock().unwrap();
                files.iter().any(|file| service.saving(*file))
            };
            if busy {
                app.show_error("A save is already waiting for the extensions");
                if let Some(then) = then.take() {
                    let context = self.inner.context.clone();
                    self.inner.application.post(move || then(&context, false));
                }
                return;
            }
            let (chain, effects) = self.inner.service.lock().unwrap().begin_save(app, files);
            if let Some(then) = then.take() {
                self.inner.saves.lock().unwrap().insert(chain, then);
            }
            self.inner.apply(app, effects);
            // A participant that never answers is cut off by the chain deadline.
            let inner = self.inner.clone();
            let context = self.inner.context.clone();
            let application = self.inner.application.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(
                    super::save::SAVE_DEADLINE_MS + 100,
                ));
                application.post(move || {
                    context.update(|app| {
                        let effects = inner.service.lock().unwrap().expire_save(chain);
                        inner.apply(app, effects);
                    });
                });
            });
        });
    }

    /// Format the active document with an extension provider if one is
    /// registered for its language. Returns whether a provider took the request.
    pub(crate) fn format(&self, context: &AppContext, selection_only: bool) -> bool {
        let mut handled = false;
        context.update(|app| {
            let request = self
                .inner
                .service
                .lock()
                .unwrap()
                .request_format(app, selection_only);
            if let Some(effects) = request {
                handled = true;
                self.inner.apply(app, effects);
            }
        });
        handled
    }

    /// Reload every package from disk; failures are shown, nothing is replayed.
    pub(crate) fn reload_all(&self, context: &AppContext) {
        context.update(|app| {
            let mut service = self.inner.service.lock().unwrap();
            let mut ids: Vec<String> = service.packages().map(|p| p.manifest.id.clone()).collect();
            ids.sort();
            let mut reloaded = 0;
            for id in ids {
                match service.reload(&id) {
                    Ok(effects) => {
                        reloaded += 1;
                        self.inner.apply(app, effects);
                    }
                    Err(error) => app.show_error(format!("{id}: {error}")),
                }
            }
            drop(service);
            self.refresh_settings(app);
            app.show_toast(format!("Reloaded {reloaded} extension(s)"));
        });
    }

    /// Install the extension package `archive` and start using it. The result is
    /// shown as a message: what was installed, or why it was not.
    pub(crate) fn import(&self, context: &AppContext, archive: &Path) {
        let Some(directory) =
            crate::infrastructure::language_packs::user_resource_directory("extensions")
        else {
            context
                .update(|app| app.show_error("There is no folder for extensions on this system"));
            return;
        };
        self.import_into(context, archive, &directory);
    }

    pub(super) fn import_into(&self, context: &AppContext, archive: &Path, directory: &Path) {
        let imported = crate::infrastructure::extensions::package::import(
            archive,
            directory,
            env!("CARGO_PKG_VERSION"),
        );
        context.update(|app| match imported {
            Err(error) => {
                app.show_error(format!("Could not import {}: {error}", archive.display()))
            }
            Ok(done) => {
                let mut service = self.inner.service.lock().unwrap();
                // A new version lives in a folder of its own, so the old one is
                // stopped and the new folder is loaded in its place.
                let loaded = service.adopt(&done.directory).map(|(_, effects)| {
                    self.inner.apply(app, effects);
                });
                let rejected = service
                    .package(&done.id)
                    .map(|package| {
                        package
                            .resources
                            .rejected
                            .iter()
                            .map(|(id, error)| format!("{id}: {error}"))
                            .collect::<Vec<_>>()
                            .join("; ")
                    })
                    .unwrap_or_default();
                drop(service);
                self.refresh_settings(app);
                match loaded {
                    Err(error) => app.show_error(format!(
                        "Imported {} but could not load it: {error}",
                        done.name
                    )),
                    Ok(()) => {
                        app.extensions_ui.show_installed(done.id);
                        app.apply(crate::features::keymap::action::Action::OpenExtensions);
                        if !rejected.is_empty() {
                            app.show_error(format!(
                                "Imported {} but some resources could not load: {rejected}",
                                done.name
                            ));
                            return;
                        }
                        app.show_success(format!(
                            "{} {} {}{}",
                            if done.replaced.is_some() {
                                "Updated"
                            } else {
                                "Imported"
                            },
                            done.name,
                            done.version,
                            if done.needs_restart {
                                " (restart Loom to use its language)"
                            } else {
                                ""
                            }
                        ));
                    }
                }
            }
        });
    }

    /// The folder installed packages live in, when this host has one.
    fn directory(&self) -> Option<&Path> {
        self.inner.user_settings.as_deref()?.parent()
    }

    /// Stop the extensions the user turned off in an earlier run, right after loading.
    pub(crate) fn restore_disabled(&self, app: &mut AppState) {
        let Some(directory) = self.directory() else {
            return;
        };
        let disabled = crate::infrastructure::extensions::state::disabled(directory);
        for id in disabled {
            let result = self.inner.service.lock().unwrap().disable(&id);
            if let Ok(effects) = result {
                self.inner.apply(app, effects);
            }
        }
    }

    /// Turn an extension off or on. The choice is kept for the next run; what a
    /// language contributes follows it after a restart. Nothing is announced.
    pub(crate) fn set_enabled(&self, context: &AppContext, id: &str, enabled: bool) {
        context.update(|app| {
            let (result, name) = {
                let mut service = self.inner.service.lock().unwrap();
                let name = service
                    .package(id)
                    .and_then(|package| package.manifest.name.clone())
                    .unwrap_or_else(|| id.to_owned());
                let result = if enabled {
                    service.enable(id).map(|()| Vec::new())
                } else {
                    service.disable(id)
                };
                (result, name)
            };
            let effects = match result {
                Ok(effects) => effects,
                Err(error) => return app.show_error(format!("{id}: {error}")),
            };
            self.inner.apply(app, effects);
            if let Some(directory) = self.directory() {
                if let Err(error) =
                    crate::infrastructure::extensions::state::set_enabled(directory, id, enabled)
                {
                    app.show_error(format!("Could not keep the choice for {name}: {error}"));
                }
            }
        });
    }

    /// Stop an installed extension and delete its folder. A package loaded from
    /// anywhere else is left alone: only what Loom installed is Loom's to delete.
    pub(crate) fn uninstall(&self, context: &AppContext, id: &str) {
        context.update(|app| {
            let Some(directory) = self.directory().map(Path::to_path_buf) else {
                return app.show_error("There is no folder for extensions on this system");
            };
            let mut service = self.inner.service.lock().unwrap();
            let Some(package) = service.package(id) else {
                return app.show_error(format!("{id}: unknown extension"));
            };
            let name = package
                .manifest
                .name
                .clone()
                .unwrap_or_else(|| id.to_owned());
            if crate::infrastructure::extensions::state::installed_id(&directory, &package.dir)
                .as_deref()
                != Some(id)
            {
                let place = package.dir.display().to_string();
                return app.show_error(format!(
                    "{name} was not installed by Loom; delete {place} yourself"
                ));
            }
            // Another extension that requires this one keeps it installed.
            let unused_runtimes =
                match crate::infrastructure::extensions::install::removal_check(&directory, id) {
                    Ok(unused) => unused,
                    Err(why) => return app.show_error(format!("Cannot uninstall {name}: {why}")),
                };
            let effects = match service.uninstall(id) {
                Ok((_, effects)) => effects,
                Err(error) => return app.show_error(format!("{id}: {error}")),
            };
            drop(service);
            self.inner.apply(app, effects);
            if app.extensions_ui.selected.as_deref() == Some(id) {
                app.extensions_ui.selected = None;
            }
            // Every installed version goes, and the record of it.
            if let Err(error) = crate::infrastructure::extensions::state::remove(&directory, id) {
                return app.show_error(format!(
                    "Uninstalled {name} but could not delete it: {error}"
                ));
            }
            if !unused_runtimes.is_empty() {
                app.show_toast(format!(
                    "Nothing uses {} any more; you can uninstall it",
                    unused_runtimes.join(", ")
                ));
            }
        });
    }

    /// Give every failed extension a new generation; it starts again when it is next needed.
    pub(crate) fn retry_failed(&self, context: &AppContext) {
        context.update(|app| {
            let mut service = self.inner.service.lock().unwrap();
            let failed: Vec<String> = service
                .packages()
                .filter(|p| p.lifecycle.state() == super::State::Failed)
                .map(|p| p.manifest.id.clone())
                .collect();
            let retried = failed.iter().filter(|id| service.retry(id).is_ok()).count();
            app.show_toast(match retried {
                0 => "No extension has failed".to_owned(),
                n => format!("Retrying {n} extension(s) when they are next needed"),
            });
        });
    }

    /// Stop every running extension, which also stops the language servers they
    /// declare. `workspace::ReloadExtensions` brings them back.
    pub(crate) fn stop_all(&self, context: &AppContext) {
        context.update(|app| {
            let mut service = self.inner.service.lock().unwrap();
            let running: Vec<String> = service
                .packages()
                .filter(|p| p.lifecycle.state() != super::State::Stopped)
                .map(|p| p.manifest.id.clone())
                .collect();
            let count = running.len();
            for id in running {
                let effects = service.stop(&id);
                self.inner.apply(app, effects);
            }
            app.show_toast(format!(
                "Stopped {count} extension(s); reload them with workspace::ReloadExtensions"
            ));
        });
    }

    pub(crate) fn report(&self) -> String {
        self.inner.service.lock().unwrap().report()
    }

    pub(crate) fn retry(&self, id: &str) -> Result<(), String> {
        self.inner.service.lock().unwrap().retry(id)
    }
}
