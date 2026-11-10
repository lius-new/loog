//! Own the syntax worker and deliver coalesced, version-checked UI invalidations.
use super::context::AppContext;
use crate::{
    domain::{document::DocumentStamp, identity::FileId},
    infrastructure::{syntax_worker::Worker, ui_dispatch::LatestNotification},
};
use lgui::ApplicationHandle;
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone)]
pub(crate) struct Highlighter {
    pub worker: Worker,
    alive: Arc<AtomicBool>,
}
impl Highlighter {
    pub fn new(state: AppContext, application: ApplicationHandle) -> Self {
        let alive = Arc::new(AtomicBool::new(true));
        let deliver_alive = alive.clone();
        let completed: Arc<Mutex<HashMap<FileId, DocumentStamp>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let delivery = completed.clone();
        let notifications = LatestNotification::new(application, move |()| {
            let completed = std::mem::take(&mut *delivery.lock().unwrap());
            if !deliver_alive.load(Ordering::Acquire) {
                return;
            }
            state.try_update(|app| {
                let worker = state
                    .services()
                    .and_then(|r| r.highlighter())
                    .map(|h| h.worker);
                let mut changed = false;
                for (document, stamp) in completed {
                    if app
                        .workspace
                        .documents
                        .get(&document)
                        .is_some_and(|doc| doc.is_text() && doc.stamp(document) == stamp)
                    {
                        if let Some(worker) = &worker {
                            super::folding::publish_syntax(app, stamp, worker.folding(stamp));
                        }
                        changed = true;
                    }
                }
                if !changed {
                    return false;
                }
                app.editor.syntax_revision = app.editor.syntax_revision.wrapping_add(1);
                true
            });
        });
        Self {
            worker: Worker::new(move |stamp| {
                completed.lock().unwrap().insert(stamp.document, stamp);
                notifications.send(())
            }),
            alive,
        }
    }
    pub fn shutdown(&self) {
        self.alive.store(false, Ordering::Release);
        self.worker.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{
            runtime::AppRuntime,
            state::AppState,
            test_support::{UiTasks, live},
        },
        features::syntax::{Source, Window},
    };
    use std::time::{Duration, Instant};

    fn wait_for_ui(queue: &UiTasks) {
        let limit = Instant::now() + Duration::from_secs(5);
        while queue.pending_ids().is_empty() {
            assert!(Instant::now() < limit, "background notification");
            std::thread::yield_now();
        }
    }

    #[test]
    fn queued_notifications_validate_versions_and_runtime_teardown_is_safe() {
        for shutdown in [false, true] {
            let mut app = AppState::new();
            app.workspace
                .open_path("example.js".into(), "const value=123;".into());
            let queue = UiTasks::default();
            let runtime = Arc::new(AppRuntime::default());
            let state = live(app).with_runtime(&runtime);
            let highlighter = Highlighter::new(state.clone(), queue.application());
            runtime.install_highlighter(highlighter.clone());
            let snapshot = state.get();
            let (id, doc) = snapshot
                .workspace
                .documents
                .iter()
                .find(|(_, d)| d.is_text())
                .unwrap();
            highlighter.worker.query(
                crate::domain::identity::PaneId::new(1),
                doc.stamp(*id),
                Source::new(&doc.buffer),
                doc.meta.lang.clone(),
                vec![Window {
                    line: doc.buffer.line_range(0),
                    range: doc.buffer.line_range(0),
                }],
            );
            wait_for_ui(&queue);
            if shutdown {
                drop(runtime);
                assert!(state.services().is_none());
            } else {
                state.update(|app| {
                    app.workspace
                        .documents
                        .get_mut(id)
                        .unwrap()
                        .buffer
                        .reload("new contents".into());
                });
            }
            let revision = state.get().editor.syntax_revision;
            queue.run_all();
            assert_eq!(state.get().editor.syntax_revision, revision);
        }
    }
}
