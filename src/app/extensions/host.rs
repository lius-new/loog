//! Connects the extension service to the executor thread and the UI thread.
use super::{Effect, ExtensionService};
use crate::app::context::AppContext;
use crate::app::state::AppState;
use crate::infrastructure::extensions::trust_store::TrustStore;
use crate::infrastructure::extensions::{Event, Executor};
use lgui::ApplicationHandle;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

struct Inner {
    service: Mutex<ExtensionService>,
    executor: OnceLock<Executor>,
    trust: Arc<TrustStore>,
    user_settings: Option<PathBuf>,
    context: AppContext,
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
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
            trust,
            user_settings,
            context: context.clone(),
        });
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
