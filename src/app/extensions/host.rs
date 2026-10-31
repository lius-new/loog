//! Connects the extension service to the executor thread and the UI thread.
use super::{Effect, ExtensionService};
use crate::app::context::AppContext;
use crate::app::state::AppState;
use crate::domain::identity::FileId;
use crate::infrastructure::extensions::tasks;
use crate::infrastructure::extensions::trust_store::TrustStore;
use crate::infrastructure::extensions::{Event, Executor};
use lgui::ApplicationHandle;
use std::path::{Path, PathBuf};
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
}

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
                Effect::Toast(message) => app.show_toast(message),
                Effect::Error(message) => app.show_error(message),
            }
        }
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
        Ok(Self { inner })
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
        let effects = self.inner.service.lock().unwrap().observe(app);
        for effect in effects {
            if let (Effect::Submit(job), Some(executor)) = (effect, self.inner.executor.get()) {
                executor.submit(job);
            }
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

    pub(crate) fn report(&self) -> String {
        self.inner.service.lock().unwrap().report()
    }

    pub(crate) fn retry(&self, id: &str) -> Result<(), String> {
        self.inner.service.lock().unwrap().retry(id)
    }
}
