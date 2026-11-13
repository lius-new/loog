//! Runtime ownership and version-checked invalidation for the common reader.
use crate::app::context::AppContext;
use crate::app::state::AppState;
use crate::domain::identity::{FileId, ViewId};
use crate::domain::view::Presentation;
use crate::features::markdown::model::{
    ContentFormat, ParseFailure, ParseLimits, ParseProgress, ParseStop, SyntaxOptions,
};
use crate::infrastructure::markdown_worker::{
    Consumer, ContentOrigin, Demand, MemoryBudget, ParseKey, ParseStatus, Worker,
};
use crate::infrastructure::ui_dispatch::LatestNotification;
use lgui::ApplicationHandle;
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

pub(crate) struct MarkdownService {
    pub(super) worker: Worker,
    pub(super) context: AppContext,
    pub(super) application: ApplicationHandle,
    pub(super) drag_timers: Mutex<HashMap<Consumer, Arc<super::ui_timer::Timer>>>,
    pub(super) clipboard: lgui::services::ClipboardHandle,
    pub(super) copy_errors: LatestNotification<String>,
    pub(super) files: Mutex<Vec<Demand>>,
    pub(super) cards: Mutex<HashMap<u64, super::markdown_cards::Card>>,
    pub(super) card_revision: std::sync::atomic::AtomicU64,
    pub(super) notify: LatestNotification<()>,
    pub render: super::markdown_render::RenderWorker,
    pub(super) alive: Arc<AtomicBool>,
}
impl MarkdownService {
    pub fn new(
        context: AppContext,
        application: ApplicationHandle,
        clipboard: lgui::services::ClipboardHandle,
    ) -> Self {
        let drag_context = context.clone();
        let drag_application = application.clone();
        let alive = Arc::new(AtomicBool::new(true));
        let error_alive = alive.clone();
        let error_state = context.clone();
        let copy_errors = LatestNotification::new(application.clone(), move |error: String| {
            if error_alive.load(Ordering::Acquire) {
                error_state.update(|app| app.show_error(format!("Clipboard: {error}")));
            }
        });
        let delivering = alive.clone();
        let completed: Arc<Mutex<HashMap<FileId, ParseKey>>> = Arc::new(Mutex::new(HashMap::new()));
        let drain = completed.clone();
        let render_context = context.clone();
        let notifications = LatestNotification::new(application.clone(), move |()| {
            if !delivering.load(Ordering::Acquire) {
                return;
            }
            let results = std::mem::take(&mut *drain.lock().unwrap());
            context.try_update(|app| {
                let changed = results.into_iter().any(|(_, key)| {
                    let Some(stamp) = key.document_stamp() else {
                        return false;
                    };
                    app.workspace
                        .documents
                        .get(&stamp.document)
                        .is_some_and(|document| {
                            document.is_text() && document.stamp(stamp.document) == stamp
                        })
                        && app.workspace.views.values().any(|view| {
                            view.document() == Some(stamp.document)
                                && view.presentation == Presentation::Markdown
                        })
                });
                if changed {
                    app.editor.markdown_revision = app.editor.markdown_revision.wrapping_add(1);
                }
                changed
            });
        });
        let render_state = render_context;
        let render_alive = alive.clone();
        let render_notification = LatestNotification::new(application.clone(), move |()| {
            if render_alive.load(Ordering::Acquire) {
                render_state.update(|app| {
                    app.editor.markdown_revision = app.editor.markdown_revision.wrapping_add(1)
                });
            }
        });
        let budget = MemoryBudget::default();
        let notify = render_notification.clone();
        let card_notification = render_notification.clone();
        let render = super::markdown_render::RenderWorker::new(
            budget.clone(),
            drag_context.clone(),
            move || render_notification.send(()),
        );
        let worker = Worker::new(budget, move |key| {
            if let Some(stamp) = key.document_stamp() {
                completed.lock().unwrap().insert(stamp.document, key);
                notifications.send(());
            } else {
                card_notification.send(());
            }
        });
        Self {
            worker,
            render,
            alive,
            context: drag_context,
            application: drag_application,
            drag_timers: Mutex::default(),
            clipboard,
            copy_errors,
            files: Mutex::default(),
            cards: Mutex::default(),
            card_revision: std::sync::atomic::AtomicU64::new(1),
            notify,
        }
    }
    pub fn sync(&self, app: &AppState) {
        let demands: Vec<Demand> = app
            .workspace
            .views
            .values()
            .filter(|view| view.presentation == Presentation::Markdown)
            .filter_map(|view| {
                let id = view.document()?;
                let document = app
                    .workspace
                    .documents
                    .get(&id)
                    .filter(|document| document.is_text())?;
                let limits = ParseLimits::automatic();
                if document.buffer.len() > limits.source_bytes {
                    return None;
                }
                let active = app
                    .workspace
                    .pane_of(view.id)
                    .and_then(|pane| app.workspace.pane(pane))
                    .is_some_and(|pane| pane.active() == Some(view.id));
                Some(Demand {
                    consumer: Consumer::Document(view.id),
                    key: Self::key(document.stamp(id), limits),
                    source: document.buffer.text_snapshot(),
                    active,
                })
            })
            .collect();
        *self.files.lock().unwrap() = demands;
        self.cards
            .lock()
            .unwrap()
            .retain(|owner, card| super::markdown_cards::current_host(app, *owner, card));
        self.refresh_demands();
    }
    fn key(stamp: crate::domain::document::DocumentStamp, limits: ParseLimits) -> ParseKey {
        ParseKey {
            origin: ContentOrigin::Document(stamp),
            generation: 0,
            format: ContentFormat::Markdown,
            syntax: SyntaxOptions::default(),
            limits,
        }
    }
    pub fn query(&self, app: &AppState, view: ViewId) -> Option<ParseStatus> {
        let view = app
            .workspace
            .view(view)
            .filter(|view| view.presentation == Presentation::Markdown)?;
        let id = view.document()?;
        let document = app
            .workspace
            .documents
            .get(&id)
            .filter(|document| document.is_text())?;
        let limits = ParseLimits::automatic();
        if document.buffer.len() > limits.source_bytes {
            return Some(ParseStatus::Declined(ParseFailure {
                reason: ParseStop::SourceBudget,
                progress: ParseProgress::default(),
            }));
        }
        Some(self.worker.query(
            Consumer::Document(view.id),
            Self::key(document.stamp(id), limits),
        ))
    }
    pub fn budget(&self) -> MemoryBudget {
        self.worker.budget()
    }
    pub fn shutdown(&self) {
        self.alive.store(false, Ordering::Release);
        self.drag_timers.lock().unwrap().clear();
        self.cards.lock().unwrap().clear();
        self.files.lock().unwrap().clear();
        self.worker.shutdown();
        self.render.shutdown();
    }
}
impl Drop for MarkdownService {
    fn drop(&mut self) {
        self.shutdown();
    }
}
