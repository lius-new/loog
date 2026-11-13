//! Connects the extension service to the executor thread and the UI thread.
use super::{Effect, ExtensionService, LanguageAsk};
use crate::app::context::AppContext;
use crate::app::language_services::ExternalAnswers;
use crate::app::state::AppState;
use crate::domain::identity::FileId;
use crate::domain::language::Registry;
use crate::features::language_service::capabilities::PositionEncoding;
use crate::features::providers::{Cancellation, Part, PartSink, RequestSpec};
use crate::infrastructure::extensions::processes;
use crate::infrastructure::extensions::tasks;
use crate::infrastructure::extensions::trust_store::TrustStore;
use crate::infrastructure::extensions::watches;
use crate::infrastructure::extensions::{Event, Executor, Job, Outcome};
use crate::infrastructure::language_packs::{self, ExtensionSources};
use lgui::ApplicationHandle;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};

struct Inner {
    session: u64,
    service: Mutex<ExtensionService>,
    executor: OnceLock<Executor>,
    runner: OnceLock<tasks::Runner>,
    processes: OnceLock<processes::Runner>,
    watches: OnceLock<watches::Runner>,
    /// The host's own handle, so background work can come back to it.
    self_ref: OnceLock<std::sync::Weak<Inner>>,
    trust: Arc<TrustStore>,
    user_settings: Option<PathBuf>,
    context: AppContext,
    application: ApplicationHandle,
    /// What to do when a save chain ends, by chain number.
    saves: Mutex<std::collections::HashMap<u64, SaveContinuation>>,
    resource_callbacks:
        Mutex<BTreeMap<loog_workspace_api::RequestId, (bool, ResourceContinuation)>>,
    resource_observers: Mutex<
        BTreeMap<
            (
                loog_workspace_api::ScopeId,
                loog_workspace_api::Capability,
                u64,
            ),
            ResourceObserver,
        >,
    >,
    /// Where the answers of a language request go, with how many are still owed.
    language_sinks: Mutex<std::collections::HashMap<u64, (PartSink, usize)>>,
    next_language: std::sync::atomic::AtomicU64,
    hooked: std::sync::atomic::AtomicBool,
    /// The resource revision last published.
    resources_seen: std::sync::atomic::AtomicU64,
    language_sources: OnceLock<Mutex<Option<BTreeMap<String, u64>>>>,
    /// The registry index and what is being done with it.
    registry: Mutex<registry::State>,
}

mod registry;
#[cfg(test)]
mod resource_acceptance;

/// Runs on the UI thread when the participants are done; `true` means write the files.
pub(crate) type SaveContinuation = Box<dyn FnOnce(&AppContext, bool) + Send>;
pub(crate) type ResourceContinuation = Box<
    dyn FnOnce(&mut AppState, Result<loog_workspace_api::Answer, loog_workspace_api::Failure>)
        + Send,
>;
pub(crate) type ResourceObserver = Arc<
    dyn Fn(Result<loog_workspace_api::Notification, loog_workspace_api::Failure>) + Send + Sync,
>;

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

impl Drop for Inner {
    fn drop(&mut self) {
        for observer in std::mem::take(self.resource_observers.get_mut().unwrap()).into_values() {
            observer(Err(loog_workspace_api::Failure::new(
                loog_workspace_api::FailureKind::Unavailable,
                "Resource provider stopped",
            )));
        }
        if let Some(runner) = self.watches.get() {
            runner.cancel_all();
        }
        if let Some(runner) = self.processes.get() {
            runner.cancel_all();
        }
        if let Some(runner) = self.runner.get() {
            runner.cancel_all();
        }
    }
}

impl Inner {
    /// Hand what the user did in the palette to the extension it is for.
    fn palette_event(&self, id: &str, generation: u64, event: loog_extension_host::PaletteEvent) {
        if let Some(executor) = self.executor.get() {
            executor.submit(Job::PaletteEvent {
                id: id.to_owned(),
                generation,
                event,
            });
        }
    }

    /// Show the system file dialog an extension asked for and answer it. The
    /// dialog blocks, so it runs on the application thread outside any state update.
    fn pick_file(&self, id: &str, generation: u64, request: u64, title: String) {
        let weak = self.self_ref.get().cloned();
        let id = id.to_owned();
        self.application.post(move || {
            let options = lgui::dialogs::FileDialogOptions::new().title(title);
            let path = lgui::dialogs::system_file_dialogs()
                .pick_file(&options)
                .map(|path| path.to_string_lossy().into_owned());
            if let Some(inner) = weak.and_then(|weak| weak.upgrade()) {
                inner.palette_event(
                    &id,
                    generation,
                    loog_extension_host::PaletteEvent::File(loog_extension_host::PaletteFilePicked {
                        request,
                        path,
                    }),
                );
            }
        });
    }

    fn apply(&self, app: &mut AppState, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::ResourceBookmarks(bookmarks) => {
                    if app.explorer.resources.session == self.session {
                        app.explorer.resources.bookmarks = bookmarks;
                    }
                }
                Effect::ResourceRoots(roots) => {
                    if app.explorer.resources.session != self.session {
                        continue;
                    }
                    let added = roots
                        .iter()
                        .any(|root| app.explorer.resources.root(root.scope).is_none());
                    app.explorer.resources.sync(roots);
                    let scopes: Vec<_> = app
                        .explorer
                        .resources
                        .roots
                        .iter()
                        .map(|root| root.descriptor.scope)
                        .collect();
                    let weak = self.self_ref.get().cloned();
                    self.application.post(move || {
                        if let Some(inner) = weak.and_then(|weak| weak.upgrade()) {
                            let removed: Vec<_> = {
                                let mut observers = inner.resource_observers.lock().unwrap();
                                let keys: Vec<_> = observers
                                    .keys()
                                    .filter(|(scope, _, _)| !scopes.contains(scope))
                                    .copied()
                                    .collect();
                                keys.into_iter()
                                    .filter_map(|key| observers.remove(&key))
                                    .collect()
                            };
                            for observer in removed {
                                observer(Err(loog_workspace_api::Failure::new(
                                    loog_workspace_api::FailureKind::Unavailable,
                                    "Resource environment disconnected",
                                )));
                            }
                        }
                    });
                    if added {
                        let context = self.context.clone();
                        self.application.post(move || {
                            crate::app::resources::reconnect_active(&context);
                            crate::app::resource_watch::start(&context);
                        });
                    }
                }
                Effect::ResourceDelivery(loog_workspace_api::Delivery::Complete {
                    request,
                    result,
                }) => {
                    let callback = self.resource_callbacks.lock().unwrap().remove(&request);
                    if let Some((changes_state, callback)) = callback {
                        // Effects can be applied during package retirement while
                        // the service is borrowed. Consumers may submit their
                        // next chunk, so always resume them on a later UI turn.
                        let context = self.context.clone();
                        let session = self.session;
                        self.application.post(move || {
                            context.update(|app| {
                                let result = if app.explorer.resources.session == session {
                                    result
                                } else {
                                    Err(loog_workspace_api::Failure::new(
                                        if changes_state {
                                            loog_workspace_api::FailureKind::UnknownOutcome
                                        } else {
                                            loog_workspace_api::FailureKind::Unavailable
                                        },
                                        "Resource session was replaced",
                                    ))
                                };
                                callback(app, result);
                            })
                        });
                    }
                }
                Effect::ResourceDelivery(loog_workspace_api::Delivery::Notification {
                    scope,
                    event,
                    ..
                }) => {
                    let context = self.context.clone();
                    let session = self.session;
                    let weak = self.self_ref.get().cloned();
                    self.application.post(move || {
                        if context.get().explorer.resources.session == session {
                            if let Some(inner) = weak.and_then(|weak| weak.upgrade()) {
                                use loog_workspace_api::Notification as N;
                                let (handle, ended) = match &event {
                                    N::TerminalOutput { handle, .. }
                                    | N::TerminalDirectory { handle, .. }
                                    | N::TerminalWritable { handle, .. }
                                    | N::Output { handle, .. }
                                    | N::Writable { handle, .. } => (*handle, false),
                                    N::TerminalExited { handle, .. } | N::Exited { handle, .. } => {
                                        (*handle, true)
                                    }
                                    N::Changed { watch, .. } => (*watch, false),
                                    N::WatchStopped { watch, .. } => (*watch, true),
                                };
                                let key = (scope, event.capability(), handle);
                                let observer = {
                                    let mut observers = inner.resource_observers.lock().unwrap();
                                    if ended {
                                        observers.remove(&key)
                                    } else {
                                        observers.get(&key).cloned()
                                    }
                                };
                                if let Some(observer) = observer {
                                    observer(Ok(event.clone()));
                                }
                            }
                            crate::app::resource_watch::notify(&context, scope, event);
                        }
                    });
                }
                Effect::ResourceDelivery(_) => {}
                Effect::ClearUi { session, owner } => app.extension_widgets.remove(session, &owner),
                Effect::Palette {
                    id,
                    generation,
                    commands,
                } => {
                    let dialogs =
                        crate::app::palette::apply_commands(app, &id, generation, commands);
                    // A session this replaced is answered as cancelled.
                    for answer in crate::app::palette::take_answers(app) {
                        self.palette_event(&answer.owner, answer.generation, answer.event);
                    }
                    for (request, title) in dialogs {
                        self.pick_file(&id, generation, request, title);
                    }
                }
                Effect::StartWatch { watch, spec } => {
                    if let Some(runner) = self.watches.get() {
                        runner.start(watch, spec);
                    }
                }
                Effect::StopWatch { watch } => {
                    if let Some(runner) = self.watches.get() {
                        runner.stop(watch);
                    }
                }
                Effect::StartProcess { process, spec } => {
                    if let Some(runner) = self.processes.get() {
                        runner.start(process, spec);
                    }
                }
                Effect::WriteProcess { process, bytes } => {
                    if let Some(runner) = self.processes.get() {
                        let length = bytes.len();
                        match runner.write(process, bytes) {
                            Ok(accepted) if accepted == length => {}
                            Ok(_) => {
                                runner.fail(process, "process input admission was exhausted".into())
                            }
                            Err(error) => runner.fail(process, error),
                        }
                    }
                }
                Effect::CloseProcess { process } => {
                    if let Some(runner) = self.processes.get()
                        && let Err(error) = runner.close_stdin(process)
                    {
                        runner.fail(process, error);
                    }
                }
                Effect::KillProcess { process } => {
                    if let Some(runner) = self.processes.get() {
                        runner.kill(process);
                    }
                }
                Effect::ResourceWorkspaceEdit {
                    owner,
                    ticket,
                    title,
                    edit,
                    stamps,
                } => {
                    let Some(weak) = self.self_ref.get().cloned() else {
                        continue;
                    };
                    let context = self.context.clone();
                    self.application.post(move || {
                        let Some(inner) = weak.upgrade() else {
                            return;
                        };
                        let valid = {
                            let weak = weak.clone();
                            let owner = owner.clone();
                            Arc::new(move |app: &AppState| {
                                weak.upgrade().is_some_and(|inner| {
                                    inner
                                        .service
                                        .lock()
                                        .unwrap()
                                        .resource_edit_current(app, &owner, ticket)
                                })
                            })
                        };
                        let completion = Arc::new(move |result: Result<usize, String>| {
                            let Some(inner) = weak.upgrade() else {
                                return;
                            };
                            let result = result
                                .map(|changed| loog_workspace_api::session::Reply::EditsApplied {
                                    transaction: if changed == 0 {
                                        0
                                    } else {
                                        inner.context.get().editor.language.next_transaction
                                    },
                                    changed: changed as u32,
                                })
                                .map_err(|reason| {
                                    loog_workspace_api::Failure::new(
                                        loog_workspace_api::FailureKind::Conflict,
                                        reason,
                                    )
                                });
                            let effects = inner
                                .service
                                .lock()
                                .unwrap()
                                .finish_resource_edit(&owner, ticket, result);
                            inner.context.update(|app| inner.apply(app, effects));
                        });
                        let rect = context.get().editor.language.rect.unwrap_or_default();
                        crate::app::language_ui::resource_edits::apply_guarded(
                            &context,
                            crate::app::language_ui::edit::Request {
                                title,
                                edit,
                                stamps,
                                rect,
                                reply: Some(crate::app::language_ui::edit::Reply::Native(
                                    completion,
                                )),
                            },
                            valid,
                        );
                        drop(inner);
                    });
                }
                Effect::WorkspaceOperation {
                    id,
                    generation,
                    operation,
                    action,
                } => {
                    let Some(weak) = self.self_ref.get().cloned() else {
                        continue;
                    };
                    let context = self.context.clone();
                    self.application.post(move || {
                        let Some(inner) = weak.upgrade() else {
                            return;
                        };
                        if !inner
                            .service
                            .lock()
                            .unwrap()
                            .package(&id)
                            .is_some_and(|p| p.lifecycle.accepts(generation))
                        {
                            return;
                        }
                        let completion = {
                            let weak = Arc::downgrade(&inner);
                            let id = id.clone();
                            Arc::new(move |result: Result<usize, String>| {
                                let Some(inner) = weak.upgrade() else {
                                    return;
                                };
                                if !inner
                                    .service
                                    .lock()
                                    .unwrap()
                                    .package(&id)
                                    .is_some_and(|p| p.lifecycle.accepts(generation))
                                {
                                    return;
                                }
                                if let Some(executor) = inner.executor.get() {
                                    executor.submit(Job::Complete {
                                        id: id.clone(),
                                        generation,
                                        operation,
                                        outcome: match result {
                                            Ok(changed) => {
                                                let app = inner.context.get();
                                                let transaction = if changed == 0 {
                                                    0
                                                } else {
                                                    app.editor.language.next_transaction
                                                };
                                                let version = app
                                                    .editor
                                                    .language
                                                    .transactions
                                                    .get(&transaction)
                                                    .filter(|documents| documents.len() == 1)
                                                    .and_then(|documents| {
                                                        app.workspace.documents.get(&documents[0])
                                                    })
                                                    .map_or(0, |document| {
                                                        document.buffer.version()
                                                    });
                                                Outcome::Committed {
                                                    transaction,
                                                    version,
                                                }
                                            }
                                            Err(message) => Outcome::Failed(
                                                loog_extension_api::ExtensionError::new(
                                                    loog_extension_api::ErrorKind::InvalidEdit,
                                                    message,
                                                ),
                                            ),
                                        },
                                    });
                                }
                            })
                        };
                        let rect = context.get().editor.language.rect.unwrap_or_default();
                        match action {
                            super::language::WorkspaceAction::Edit {
                                title,
                                edit,
                                stamps,
                            } => crate::app::language_ui::edit::apply(
                                &context,
                                crate::app::language_ui::edit::Request {
                                    title,
                                    edit,
                                    stamps,
                                    rect,
                                    reply: Some(crate::app::language_ui::edit::Reply::Native(
                                        completion,
                                    )),
                                },
                            ),
                            super::language::WorkspaceAction::Show { path, span } => {
                                crate::app::language_ui::navigate::go(
                                    &context,
                                    crate::app::language_ui::navigate::Place::span(path, span),
                                    rect,
                                    true,
                                );
                                completion(Ok(0));
                            }
                        }
                    });
                }
                Effect::Submit(job) => {
                    if let Some(executor) = self.executor.get() {
                        executor.submit(job);
                    }
                }
                Effect::PublishDiagnostics {
                    source,
                    resource,
                    stamp,
                    text,
                    items,
                } => {
                    if let Some(services) = self
                        .context
                        .services()
                        .and_then(|runtime| runtime.language_services())
                    {
                        services.publish_external(source, resource, stamp, text, items.into());
                    }
                }
                Effect::PublishFindings {
                    source,
                    path,
                    items,
                } => {
                    if let Some(services) = self
                        .context
                        .services()
                        .and_then(|runtime| runtime.language_services())
                    {
                        services.publish_findings(source, path, items);
                    }
                }
                Effect::ClearFindings { source } => {
                    if let Some(services) = self
                        .context
                        .services()
                        .and_then(|runtime| runtime.language_services())
                    {
                        services.clear_findings(source);
                    }
                }
                Effect::ClearLocalDiagnostics { source } => {
                    if let Some(services) = self
                        .context
                        .services()
                        .and_then(|runtime| runtime.language_services())
                    {
                        services.clear_findings(source);
                        services.clear_external_matching(source, |r| r.local_path().is_some());
                    }
                }
                Effect::ClearResourceDiagnostics { source, resource } => {
                    if let Some(services) = self
                        .context
                        .services()
                        .and_then(|runtime| runtime.language_services())
                    {
                        services.clear_external_matching(source, |r| r == &resource);
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
                Effect::PublishHighlights {
                    source,
                    resource,
                    stamp,
                    length,
                    items,
                } => {
                    if let Some(services) = self
                        .context
                        .services()
                        .and_then(|runtime| runtime.language_services())
                    {
                        services.publish_highlights(source, resource, stamp, length, items);
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
                            rank: answer.rank,
                            encoding: PositionEncoding::Utf8,
                            value: answer.value,
                        }));
                        *remaining -= 1;
                        if *remaining == 0 {
                            sinks.remove(&token);
                        }
                    }
                }
                Effect::LanguageFailure { token, message } => {
                    let mut sinks = self.language_sinks.lock().unwrap();
                    if let Some((sink, remaining)) = sinks.get_mut(&token) {
                        sink.fail(message);
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
    fn document_languages(&self, languages: Vec<(PathBuf, String)>) {
        if let Some(inner) = self.inner.upgrade() {
            inner.service.lock().unwrap().document_languages = languages.into_iter().collect();
        }
    }
    fn files_changed(&self, change: crate::features::providers::FileChange) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let context = inner.context.clone();
        let weak = self.inner.clone();
        inner.application.clone().post(move || {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            context.update(|app| {
                let effects = inner
                    .service
                    .lock()
                    .unwrap()
                    .workspace_files_changed(app, change);
                inner.apply(app, effects);
            });
        });
    }
    fn saved(&self, saved: crate::domain::document::SaveSnapshot) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let context = inner.context.clone();
        let weak = self.inner.clone();
        inner.application.clone().post(move || {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            context.update(|app| {
                let effects = inner.service.lock().unwrap().document_saved(app, saved);
                inner.apply(app, effects);
            });
        });
    }
    fn capabilities(
        &self,
        path: Option<&loog_workspace_api::ResourceRef>,
    ) -> Vec<crate::app::language_services::CapabilitySource> {
        let Some(inner) = self.inner.upgrade() else {
            return Vec::new();
        };
        inner.service.lock().unwrap().capability_sources(path)
    }
    fn expected(&self, spec: &RequestSpec) -> usize {
        let Some(inner) = self.inner.upgrade() else {
            return 0;
        };
        let registry = Registry::builtin();
        let service = inner.service.lock().unwrap();
        let language = if let Some(path) = spec.resource.local_path() {
            service
                .document_languages
                .get(path)
                .cloned()
                .unwrap_or_else(|| registry.resolve_path(path).definition().name.to_string())
        } else {
            let Some(language) = service.resource_documents.language(&spec.resource) else {
                return 0;
            };
            language.to_owned()
        };
        service
            .matching_language_providers_for(
                spec.feature,
                &language,
                spec.target,
                spec.server.as_deref(),
                Some(&spec.resource),
            )
            .len()
    }

    fn ask(&self, spec: RequestSpec, expected: usize, sink: PartSink) -> Option<Cancellation> {
        let inner = self.inner.upgrade()?;
        let token = inner.next_language.fetch_add(1, Ordering::Relaxed);
        // The request is made on the UI thread, possibly inside an update; the
        // providers are asked from the next turn of the application.
        let application = inner.application.clone();
        let requested = self.inner.clone();
        application.post(move || {
            if !sink.is_live() {
                return;
            }
            let Some(inner) = requested.upgrade() else {
                return;
            };
            let context = inner.context.clone();
            context.update(|app| {
                let ask = LanguageAsk {
                    feature: spec.feature,
                    resource: spec.resource.clone(),
                    stamp: spec.stamp,
                    start: spec.start,
                    end: spec.end,
                    extra: spec.extra.clone(),
                    target: spec.target,
                    server: spec.server.clone(),
                    timeout: spec.timeout,
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
            crate::app::language_ui::tick(&context);
        });
        let cancelled = self.inner.clone();
        Some(Arc::new(move || {
            let cancelled = cancelled.clone();
            application.post(move || {
                let Some(inner) = cancelled.upgrade() else {
                    return;
                };
                let context = inner.context.clone();
                context.update(|app| {
                    let mut effects = Vec::new();
                    {
                        let mut service = inner.service.lock().unwrap();
                        let requests = service
                            .language_calls
                            .iter()
                            .filter(|(_, t)| **t == token)
                            .map(|(request, _)| *request)
                            .collect::<Vec<_>>();
                        for request in requests {
                            service.cancel_provider(request, &mut effects);
                        }
                    }
                    inner.apply(app, effects);
                    inner.language_sinks.lock().unwrap().remove(&token);
                });
            });
        }))
    }
}

/// The running extension subsystem of one mounted application.
pub(crate) struct ExtensionHost {
    inner: Arc<Inner>,
}

impl Drop for ExtensionHost {
    fn drop(&mut self) {
        let session = self.inner.service.lock().unwrap().ui_session;
        let context = self.inner.context.clone();
        let saves = std::mem::take(&mut *self.inner.saves.lock().unwrap());
        let resources = std::mem::take(&mut *self.inner.resource_callbacks.lock().unwrap());
        self.inner.application.post(move || {
            context.update(|app| {
                app.extension_widgets.end_session(session);
                if app.explorer.resources.session == session {
                    app.explorer.resources.sync(Vec::new());
                }
                for (changes_state, callback) in resources.into_values() {
                    callback(
                        app,
                        Err(loog_workspace_api::Failure::new(
                            if changes_state {
                                loog_workspace_api::FailureKind::UnknownOutcome
                            } else {
                                loog_workspace_api::FailureKind::Unavailable
                            },
                            "Resource subsystem stopped",
                        )),
                    );
                }
            });
            for then in saves.into_values() {
                then(&context, false);
            }
        });
    }
}

impl ExtensionHost {
    /// Tell an extension what the user did in the command palette.
    pub(crate) fn palette_event(
        &self,
        id: &str,
        generation: u64,
        event: loog_extension_host::PaletteEvent,
    ) {
        self.inner.palette_event(id, generation, event);
    }

    pub(crate) fn refresh_resource_grants(&self) {
        let effects = self.inner.service.lock().unwrap().pump_workspaces();
        self.inner
            .context
            .update(|app| self.inner.apply(app, effects));
    }
    pub(crate) fn restore_resource(&self, bookmark: loog_workspace_api::session::ResourceBookmark) {
        let result = self
            .inner
            .service
            .lock()
            .unwrap()
            .restore_resource(bookmark);
        self.inner.context.update(|app| match result {
            Ok(effects) => self.inner.apply(app, effects),
            Err(error) => app.show_error(error),
        });
    }
    pub(crate) fn restore_resources(&self) {
        let bookmarks = self
            .inner
            .service
            .lock()
            .unwrap()
            .workspace_bookmarks
            .clone();
        for bookmark in bookmarks {
            self.restore_resource(bookmark);
        }
    }
    pub(crate) fn forget_resource(&self, resource: &loog_workspace_api::ResourceRef) {
        let effects = {
            let mut service = self.inner.service.lock().unwrap();
            service
                .workspace_bookmarks
                .retain(|bookmark| &bookmark.resource != resource);
            service.workspace_bookmarks_dirty = true;
            service.pump_workspaces()
        };
        self.inner
            .context
            .update(|app| self.inner.apply(app, effects));
    }
    pub(crate) fn request_resource(
        &self,
        scope: loog_workspace_api::ScopeId,
        operation: loog_workspace_api::Operation,
        callback: ResourceContinuation,
    ) -> Result<loog_workspace_api::RequestId, loog_workspace_api::Failure> {
        let mut service = self.inner.service.lock().unwrap();
        let changes_state = operation.changes_state();
        let request = service.resource_request(scope, operation)?;
        self.inner
            .resource_callbacks
            .lock()
            .unwrap()
            .insert(request, (changes_state, callback));
        // Deliver on a later UI turn, including immediate provider failures.
        let weak = Arc::downgrade(&self.inner);
        self.inner.application.post(move || {
            if let Some(inner) = weak.upgrade() {
                let effects = inner.service.lock().unwrap().pump_workspaces();
                inner.context.update(|app| inner.apply(app, effects));
            }
        });
        Ok(request)
    }
    pub(crate) fn observe_resource_handle(
        &self,
        scope: loog_workspace_api::ScopeId,
        capability: loog_workspace_api::Capability,
        handle: u64,
        observer: ResourceObserver,
    ) {
        self.inner
            .resource_observers
            .lock()
            .unwrap()
            .insert((scope, capability, handle), observer);
    }
    pub(crate) fn forget_resource_handle(
        &self,
        scope: loog_workspace_api::ScopeId,
        capability: loog_workspace_api::Capability,
        handle: u64,
    ) {
        self.inner
            .resource_observers
            .lock()
            .unwrap()
            .remove(&(scope, capability, handle));
    }
    pub(crate) fn cancel_resource(&self, request: loog_workspace_api::RequestId) {
        self.inner.service.lock().unwrap().resource_cancel(request);
        let weak = Arc::downgrade(&self.inner);
        self.inner.application.post(move || {
            if let Some(inner) = weak.upgrade() {
                let effects = inner.service.lock().unwrap().pump_workspaces();
                inner.context.update(|app| inner.apply(app, effects));
            }
        });
    }
    /// Start the executor. Events are handled on the UI thread through `application`.
    pub(crate) fn start(
        context: AppContext,
        application: ApplicationHandle,
        cache: PathBuf,
        trust: Arc<TrustStore>,
        user_settings: Option<PathBuf>,
    ) -> Result<Self, String> {
        let mut service = ExtensionService::new(cache);
        service.workspace_bookmarks = context.get().explorer.resources.bookmarks.clone();
        let session = service.ui_session;
        let inner = Arc::new(Inner {
            session,
            service: Mutex::new(service),
            executor: OnceLock::new(),
            runner: OnceLock::new(),
            processes: OnceLock::new(),
            watches: OnceLock::new(),
            self_ref: OnceLock::new(),
            trust,
            user_settings,
            context: context.clone(),
            application: application.clone(),
            saves: Mutex::new(std::collections::HashMap::new()),
            resource_callbacks: Mutex::new(BTreeMap::new()),
            resource_observers: Mutex::new(BTreeMap::new()),
            language_sinks: Mutex::new(std::collections::HashMap::new()),
            next_language: std::sync::atomic::AtomicU64::new(1),
            hooked: std::sync::atomic::AtomicBool::new(false),
            resources_seen: std::sync::atomic::AtomicU64::new(0),
            language_sources: OnceLock::new(),
            registry: Mutex::new(registry::State::default()),
        });
        let _ = inner.self_ref.set(Arc::downgrade(&inner));
        let ui_context = context.clone();
        application.post(move || {
            ui_context.update(|app| {
                app.extension_widgets.begin_session(session);
                if app.explorer.resources.session < session {
                    app.explorer.resources.session = session;
                    app.explorer.resources.sync(Vec::new());
                    app.explorer.resources.open_generation += 1;
                    app.explorer.resources.pending_open = None;
                    for document in app.workspace.documents.values_mut() {
                        if document.meta.resource.environment().is_some() {
                            document.provider = None;
                        }
                    }
                }
            })
        });
        inner.service.lock().unwrap().set_trust(inner.trust.clone());
        let watch_inner = Arc::downgrade(&inner);
        let watch_context = context.clone();
        let watch_application = application.clone();
        let _ = inner.watches.set(watches::Runner::new(move |event| {
            let inner = watch_inner.clone();
            let context = watch_context.clone();
            watch_application.post(move || {
                let Some(inner) = inner.upgrade() else {
                    return;
                };
                context.update(|app| {
                    let effects = inner.service.lock().unwrap().handle_watch_event(event);
                    inner.apply(app, effects);
                })
            });
        }));
        let process_inner = Arc::downgrade(&inner);
        let process_context = context.clone();
        let process_application = application.clone();
        let _ = inner.processes.set(processes::Runner::new(move |event| {
            let inner = process_inner.clone();
            let context = process_context.clone();
            process_application.post(move || {
                let Some(inner) = inner.upgrade() else {
                    return;
                };
                context.update(|app| {
                    let effects = inner.service.lock().unwrap().handle_process_event(event);
                    inner.apply(app, effects);
                })
            });
        }));
        // Output and exits of tasks reach their guest through the UI thread, where
        // the service knows which extension and generation a task belongs to.
        let task_inner = Arc::downgrade(&inner);
        let task_context = context.clone();
        let task_application = application.clone();
        let _ = inner.runner.set(tasks::Runner::new(move |event| {
            let inner = task_inner.clone();
            let context = task_context.clone();
            task_application.post(move || {
                let Some(inner) = inner.upgrade() else {
                    return;
                };
                context.update(|app| {
                    let effects = inner.service.lock().unwrap().handle_task_event(event);
                    inner.apply(app, effects);
                });
            });
        }));
        // The subsystem owns these sinks. Weak captures let unloading reclaim
        // the executor, processes and watches even with queued UI callbacks.
        let sink_inner = Arc::downgrade(&inner);
        let executor = Executor::spawn(Box::new(move |event: Event| {
            let inner = sink_inner.clone();
            let context = context.clone();
            application.post(move || {
                let Some(inner) = inner.upgrade() else {
                    return;
                };
                let mut language_changed = matches!(
                    &event,
                    Event::Provided { .. }
                        | Event::ProviderExpired { .. }
                        | Event::ProvidersConfigured { .. }
                        | Event::DiagnosticsPublished { .. }
                        | Event::Failed { .. }
                        | Event::Loaded { .. }
                );
                language_changed |= matches!(&event, Event::WorkspaceSubmissions { submissions, .. }
                    if submissions.iter().any(|item| matches!(item.command, loog_workspace_api::session::Command::ConfigureLanguageProviders { .. })));
                context.update(|app| {
                    let effects = inner.service.lock().unwrap().handle_event(app, event);
                    // Results may arrive through any guest interface. Observe
                    // the effects rather than assuming the base provider event
                    // is the only delivery route that can complete a UI request.
                    language_changed |= effects.iter().any(|effect| {
                        matches!(
                            effect,
                            Effect::LanguageAnswer { .. }
                                | Effect::LanguageFailure { .. }
                                | Effect::PublishDiagnostics { .. }
                                | Effect::PublishHighlights { .. }
                                | Effect::ClearLocalDiagnostics { .. }
                                | Effect::ClearResourceDiagnostics { .. }
                        )
                    });
                    inner.apply(app, effects);
                });
                if language_changed {
                    crate::app::language_ui::tick(&context);
                }
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
        self.add_management(&mut frame);
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
            .and_then(|root| read_json(&root.join(".loog/extension-settings.json")));
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

    /// The installed extensions that answer for languages, for the language services panel
    /// and the status bar. What a running extension reports about `document`
    /// decides how it is doing; the lifecycle tells the rest.
    pub(crate) fn language_services(
        &self,
        document: Option<&loog_workspace_api::ResourceRef>,
    ) -> Vec<crate::features::language_services_menu::Service> {
        use crate::app::language_services_menu::{report_state, short, state_of};
        use crate::features::language_services_menu::{Service, State};
        let service = self.inner.service.lock().unwrap();
        service
            .packages()
            .filter(|package| {
                !package
                    .manifest
                    .contributes
                    .language_service
                    .providers
                    .is_empty()
            })
            .map(|package| {
                let declared = &package.manifest.contributes.language_service;
                let mut languages: Vec<String> = declared
                    .providers
                    .iter()
                    .flat_map(|provider| provider.selector.languages.iter().cloned())
                    .collect();
                languages.sort();
                languages.dedup();
                let mut state = state_of(package.lifecycle.state());
                let mut note = String::new();
                if state == State::Failed {
                    note = package.last_error.as_deref().map(short).unwrap_or_default();
                } else if state == State::Ready {
                    // Running: what it says about itself, the most urgent first.
                    let reports = service.service_reports(&package.manifest.id, document);
                    let reported =
                        State::most_urgent(reports.iter().map(|report| report_state(report.0)));
                    if let Some(reported) = reported {
                        state = reported;
                        note = reports
                            .iter()
                            .find(|report| report_state(report.0) == reported)
                            .and_then(|report| report.1)
                            .map(short)
                            .unwrap_or_default();
                    }
                }
                Service {
                    extension: package.manifest.id.clone(),
                    name: declared
                        .name
                        .clone()
                        .or_else(|| package.manifest.name.clone())
                        .unwrap_or_else(|| package.manifest.id.clone()),
                    languages,
                    state,
                    note,
                }
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn service(&self) -> std::sync::MutexGuard<'_, ExtensionService> {
        self.inner.service.lock().unwrap()
    }

    pub(crate) fn ui_event(
        &self,
        app: &AppState,
        origin: crate::features::extensions::ui::Origin,
        payload: crate::features::extensions::ui::Payload,
    ) {
        let effects = self
            .inner
            .service
            .lock()
            .unwrap()
            .ui_event(app, origin, payload);
        self.submit(effects);
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

    pub(crate) fn execute_provider_command(&self, source: u64, raw: &serde_json::Value) {
        let effects = self
            .inner
            .service
            .lock()
            .unwrap()
            .execute_provider_command(source, raw);
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
        let effects = self.inner.service.lock().unwrap().observe(app);
        self.submit(effects);
    }

    pub(super) fn start_language_resources(&self, app: &mut AppState) {
        self.inner.language_sources.get_or_init(Mutex::default);
        self.inner.resources_seen.store(u64::MAX, Ordering::Relaxed);
        self.publish_resources(app);
    }

    /// Publish the enabled contribution set before syncing open documents and
    /// providers. Every operation that changes the package set advances its
    /// resource revision, including replacement at the same disk path.
    pub(crate) fn publish_resources(&self, app: &mut AppState) -> bool {
        let (themes, language_sources) = {
            let service = self.inner.service.lock().unwrap();
            let revision = service.resources_revision();
            if self.inner.resources_seen.swap(revision, Ordering::Relaxed) == revision {
                return false;
            }
            let sources = self.inner.language_sources.get().and_then(|published| {
                let packages = service.contributing_packages().filter(|package| {
                    let contributes = &package.manifest.contributes;
                    !contributes.languages.is_empty()
                        || !contributes.highlight.is_empty()
                        || !contributes.highlight_queries.is_empty()
                });
                let stamps = packages
                    .clone()
                    .map(|p| (p.manifest.id.clone(), p.resource_revision))
                    .collect();
                let mut published = published.lock().unwrap();
                if published.as_ref() == Some(&stamps) {
                    return None;
                }
                *published = Some(stamps);
                Some(ExtensionSources::new(
                    packages.map(|p| (p.dir.as_path(), &p.manifest)),
                ))
            });
            (service.themes(), sources)
        };
        self.publish_themes(themes);
        if let Some(sources) = language_sources {
            self.publish_language_resources(app, sources);
        }
        true
    }

    fn publish_language_resources(&self, app: &mut AppState, sources: ExtensionSources) {
        let packages = sources.load();
        match packages.registry() {
            Ok(registry) => {
                let errors = packages.activity().errors;
                Registry::install(registry);
                language_packs::resources().replace(packages);
                if app.refresh_language_rules() {
                    crate::app::editing::configure_all(app);
                }
                if !errors.is_empty() {
                    app.show_error(format!(
                        "Could not load some language resources. {}",
                        errors.into_iter().take(4).collect::<Vec<_>>().join("\n")
                    ));
                }
            }
            Err(error) => app.show_error(format!("Could not load language resources: {error}")),
        }
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

    /// Whether an extension declares a language server for the language `name`.
    pub(crate) fn serves_language(&self, name: &str) -> bool {
        self.inner.service.lock().unwrap().serves_language(name)
    }

    /// Retry process-capable language owners only for documents inside a newly
    /// trusted folder. Every process launch still checks the persisted trust store.
    pub(crate) fn resume_tools_in(&self, context: &AppContext, root: &Path) {
        context.update(|app| {
            let mut service = self.inner.service.lock().unwrap();
            let languages = app
                .workspace
                .documents
                .values()
                .filter(|d| {
                    d.is_text()
                        && d.meta
                            .local_path()
                            .is_some_and(|path| path.starts_with(root))
                })
                .map(|d| d.meta.lang.definition().name.as_ref())
                .collect::<Vec<_>>();
            let ids = service
                .packages()
                .filter(|p| {
                    !matches!(
                        p.lifecycle.state(),
                        crate::features::extensions::lifecycle::State::Stopped
                            | crate::features::extensions::lifecycle::State::Stopping
                    ) && p
                        .manifest
                        .capabilities
                        .iter()
                        .any(|c| c == "process.execute-local")
                        && p.manifest
                            .contributes
                            .language_service
                            .providers
                            .iter()
                            .any(|provider| {
                                provider
                                    .selector
                                    .languages
                                    .iter()
                                    .any(|lang| languages.contains(&lang.as_str()))
                            })
                })
                .map(|p| p.manifest.id.clone())
                .collect::<Vec<_>>();
            for id in ids {
                match service.reload(&id) {
                    Ok(effects) => self.inner.apply(app, effects),
                    Err(error) => app.show_error(format!("{id}: {error}")),
                }
            }
        });
    }

    /// A folder lost its trust: what runs there stops.
    pub(crate) fn stop_tools_in(&self, root: &Path) {
        let effects = self.inner.service.lock().unwrap().stop_tools_in(root);
        for effect in effects {
            match effect {
                Effect::CancelTask { task } => {
                    if let Some(runner) = self.inner.runner.get() {
                        runner.cancel(task);
                    }
                }
                Effect::KillProcess { process } => {
                    if let Some(runner) = self.inner.processes.get() {
                        runner.kill(process);
                    }
                }
                _ => {}
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
            let inner = Arc::downgrade(&self.inner);
            let context = self.inner.context.clone();
            let application = self.inner.application.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(
                    super::save::SAVE_DEADLINE_MS + 100,
                ));
                application.post(move || {
                    let Some(inner) = inner.upgrade() else {
                        return;
                    };
                    context.update(|app| {
                        let effects = inner.service.lock().unwrap().expire_save(chain);
                        inner.apply(app, effects);
                    });
                });
            });
        });
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
                            "{} {} {}",
                            if done.replaced.is_some() {
                                "Updated"
                            } else {
                                "Imported"
                            },
                            done.name,
                            done.version,
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

    /// Turn an extension off or on now and keep the choice for the next run.
    pub(crate) fn set_enabled(&self, context: &AppContext, id: &str, enabled: bool) {
        context.update(|app| {
            let result = {
                let mut service = self.inner.service.lock().unwrap();
                let Some(package) = service.package(id) else {
                    return app.show_error(format!("{id}: unknown extension"));
                };
                if let Some(directory) = self.directory()
                    && let Err(error) = crate::infrastructure::extensions::state::set_enabled(
                        directory, id, enabled,
                    )
                {
                    let name = package.manifest.name.as_deref().unwrap_or(id);
                    return app
                        .show_error(format!("Could not keep the choice for {name}: {error}"));
                }
                if enabled {
                    service.enable(id).map(|()| Vec::new())
                } else {
                    service.disable(id)
                }
            };
            let effects = match result {
                Ok(effects) => effects,
                Err(error) => return app.show_error(format!("{id}: {error}")),
            };
            self.inner.apply(app, effects);
        });
    }

    /// Stop an installed extension and delete its folder. A package loaded from
    /// anywhere else is left alone: only what Loog installed is Loog's to delete.
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
                    "{name} was not installed by Loog; delete {place} yourself"
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
            // A user-installed version can shadow an extension that ships with Loog.
            // Removing the override exposes the bundled version again.
            if let Some(bundled) =
                language_packs::bundled_extensions_directory().map(|root| root.join(id))
                && bundled.join("manifest.json").is_file()
                && let Err(error) = self.load_package(&bundled)
            {
                return app.show_error(format!(
                    "Could not restore the bundled version of {name}. {error}"
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

    /// Stop an extension and make it ready again: it starts when something it serves
    /// next needs it. Unlike `set_enabled`, nothing is kept for the next run.
    pub(crate) fn restart(&self, context: &AppContext, id: &str) {
        context.update(|app| {
            let result = {
                let mut service = self.inner.service.lock().unwrap();
                service
                    .disable(id)
                    .and_then(|effects| service.enable(id).map(|()| effects))
            };
            match result {
                Ok(effects) => self.inner.apply(app, effects),
                Err(error) => app.show_error(format!("{id}: {error}")),
            }
        });
    }

    /// Turn an extension on again. One that failed has only to be retried; one that was
    /// turned off is enabled, which is kept for the next run.
    pub(crate) fn turn_on(&self, context: &AppContext, id: &str) {
        let failed = self
            .inner
            .service
            .lock()
            .unwrap()
            .package(id)
            .is_some_and(|package| package.lifecycle.state() == super::State::Failed);
        if failed {
            if let Err(error) = self.retry(id) {
                context.update(|app| app.show_error(format!("{id}: {error}")));
            }
        } else {
            self.set_enabled(context, id, true);
        }
    }

    pub(crate) fn report(&self) -> String {
        self.inner.service.lock().unwrap().report()
    }

    pub(crate) fn retry(&self, id: &str) -> Result<(), String> {
        self.inner.service.lock().unwrap().retry(id)
    }
}

#[cfg(test)]
mod lifetime_tests {
    use super::*;
    #[test]
    fn an_extension_completion_and_its_resolve_reach_the_native_ui_without_a_host_server() {
        let cache = super::super::tests::precompiled_cache().clone();
        let mut app = AppState::new();
        let path =
            std::env::temp_dir().join(format!("loog-provider-native-{}.txt", std::process::id()));
        let file = app.workspace.open_path(path.clone(), "lo".into());
        app.workspace.active_editor_mut().unwrap().set_cursor(2);
        app.preferences.values_mut().file_associations.push(
            crate::features::settings::rules::FileAssociation {
                pattern: path.file_name().unwrap().to_string_lossy().into_owned(),
                language: "rust".into(),
            },
        );
        app.refresh_language_rules();
        assert_eq!(
            app.workspace.documents[&file]
                .meta
                .lang
                .definition()
                .name
                .as_ref(),
            "rust"
        );
        let language = app
            .workspace
            .documents
            .values()
            .next()
            .unwrap()
            .meta
            .lang
            .definition()
            .name
            .to_string();
        let ui = crate::app::test_support::SavingUi::new(app);
        ui.runtime
            .install_language_services(crate::app::language_services::LanguageServices::new());
        let host = ExtensionHost::start(
            ui.state.clone(),
            ui.tasks.application(),
            cache,
            Arc::new(TrustStore::default()),
            None,
        )
        .unwrap();
        let manifest = serde_json::json!({"schema":1,"id":"example.tools","version":"0.1.0","api":"loog:extension@0.1.0","component":"extension.wasm","activation":[],"capabilities":["documents.read","languages"],"contributes":{"language_service":{"providers":[{"id":"example.tools.complete","kind":"completion","selector":{"languages":[language]}}]}}}).to_string();
        host.load_package(&super::super::tests::package_dir_with(
            "native-completion",
            &manifest,
        ))
        .unwrap();
        host.inner.service.lock().unwrap().set_clock(|| 0);
        ui.runtime.install_extensions(host);
        ui.state
            .update(|app| crate::app::language_ui::completion::request(&ui.state, app, true, None));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while ui
            .state
            .get()
            .editor
            .language
            .completion
            .as_ref()
            .is_none_or(|s| s.data.entries.is_empty())
        {
            ui.tasks.run_all();
            assert!(
                std::time::Instant::now() < deadline,
                "completion did not reach the UI"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        ui.state.update(|app| {
            let session = app.editor.language.completion.as_mut().unwrap();
            let index = session
                .shown
                .iter()
                .position(|shown| session.data.entries[shown.entry].item.label == "loog_world")
                .unwrap();
            session.set_selected(index);
            crate::app::language_ui::completion::resolve_selected(&ui.state, app);
        });
        while !ui
            .state
            .get()
            .editor
            .language
            .completion
            .as_ref()
            .unwrap()
            .data
            .entries
            .iter()
            .any(|e| {
                e.item
                    .documentation
                    .as_ref()
                    .is_some_and(|doc| doc.contains("world"))
            })
        {
            ui.tasks.run_all();
            assert!(
                std::time::Instant::now() < deadline,
                "resolve did not return to its originating provider"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(
            !ui.runtime
                .language_services()
                .unwrap()
                .capability_sources(None)
                .is_empty()
        );
        ui.runtime.clear_extensions();
        ui.runtime.clear_language_services();
    }
    #[test]
    fn a_replaced_resource_session_rejects_old_results_and_old_drop_keeps_new_roots() {
        use loog_workspace_api::{
            Answer, Delivery, EnvironmentKey, FailureKind, PresentedRoot, ResourceRef,
        };
        let ui = crate::app::test_support::SavingUi::new(AppState::new());
        let make = || {
            ExtensionHost::start(
                ui.state.clone(),
                ui.tasks.application(),
                std::env::temp_dir().join("loog-resource-session-cache"),
                Arc::new(TrustStore::default()),
                None,
            )
            .unwrap()
        };
        let old = make();
        ui.tasks.run_all();
        let result = Arc::new(Mutex::new(None));
        let received = result.clone();
        old.inner.resource_callbacks.lock().unwrap().insert(
            1,
            (
                false,
                Box::new(move |_, value| {
                    *received.lock().unwrap() = Some(value);
                }),
            ),
        );
        let new = make();
        ui.tasks.run_all();
        let root = PresentedRoot {
            scope: 1,
            resource: ResourceRef::provided(
                EnvironmentKey::new("fixture", "new").unwrap(),
                "opaque:root",
            )
            .unwrap(),
            name: "New session".into(),
            capabilities: vec![],
        };
        ui.state.update(|app| {
            app.explorer.resources.sync(vec![root.clone()]);
            old.inner.apply(
                app,
                vec![
                    Effect::ResourceRoots(Vec::new()),
                    Effect::ResourceDelivery(Delivery::Complete {
                        request: 1,
                        result: Ok(Answer::Ok),
                    }),
                ],
            );
        });
        ui.tasks.run_all();
        assert_eq!(
            result.lock().unwrap().take().unwrap().unwrap_err().kind,
            FailureKind::Unavailable
        );
        drop(old);
        ui.tasks.run_all();
        assert_eq!(ui.state.get().explorer.resources.roots[0].descriptor, root);
        drop(new);
        ui.tasks.run_all();
        assert!(ui.state.get().explorer.resources.roots.is_empty());
    }

    #[test]
    fn unloading_the_host_releases_its_runners_and_executor() {
        let queue = crate::app::test_support::UiTasks::default();
        let state = crate::app::test_support::live(AppState::new());
        let host = ExtensionHost::start(
            state,
            queue.application(),
            std::env::temp_dir().join("loog-host-lifetime-cache"),
            Arc::new(TrustStore::default()),
            None,
        )
        .unwrap();
        let observed = Arc::downgrade(&host.inner);
        drop(host);
        assert!(
            observed.upgrade().is_none(),
            "background sinks must not retain the owning host"
        );
        queue.run_all();
    }
}
