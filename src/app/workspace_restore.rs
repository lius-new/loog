//! Session tabs exist from the first frame; their file contents are prepared
//! serially off the event loop and installed without changing user selection.
use crate::app::context::AppContext;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

use super::state::AppState;
use super::workspace::PreparedTextFile;
use crate::domain::identity::FileId;
use lgui::ApplicationHandle;

pub struct SessionRestore {
    alive: Arc<AtomicBool>,
    stop_delay: mpsc::Sender<()>,
    work: mpsc::Sender<Option<(FileId, PathBuf)>>,
}

impl SessionRestore {
    pub fn new(state: AppContext, application: ApplicationHandle) -> Self {
        let reader = state.clone();
        Self::with_prepare(state, application, move |path| {
            let app = reader.get();
            let explicit = app
                .workspace
                .file_id_for_path(path)
                .and_then(|id| app.workspace.documents.get(&id))
                .and_then(|doc| doc.recovery.as_ref())
                .and_then(|saved| {
                    if saved.unsaved_text.is_some() {
                        Some(saved.saved)
                    } else {
                        saved.read_override
                    }
                });
            if crate::domain::media::is_raster(path) {
                return crate::app::encoding::prepare(path);
            }
            let read = crate::infrastructure::document_filesystem::read_encoded(
                path,
                explicit,
                crate::infrastructure::document_filesystem::MAX_EDITABLE_FILE_BYTES,
            );
            read.map(|(decoded, disk)| {
                PreparedTextFile::with_encoding(path.to_path_buf(), decoded, disk)
            })
            .map_err(|e| format!("Could not restore {}: {e}", path.display()))
        })
    }

    fn with_prepare(
        state: AppContext,
        application: ApplicationHandle,
        mut prepare: impl FnMut(&Path) -> Result<PreparedTextFile, String> + Send + 'static,
    ) -> Self {
        let files = state.get().workspace.loading_files();
        let alive = Arc::new(AtomicBool::new(true));
        let (stop_delay, delay) = mpsc::channel();
        let (work, receiver) = mpsc::channel();
        for file in &files {
            let _ = work.send(Some(file.clone()));
        }
        if !files.is_empty() {
            let worker_alive = alive.clone();
            let worker_state = state.clone();
            let worker_application = application.clone();
            let retry = work.clone();
            std::thread::Builder::new()
                .name("loog-session-restore".into())
                .spawn(move || {
                    while let Ok(Some((id, path))) = receiver.recv() {
                        if !worker_alive.load(Ordering::Acquire) {
                            break;
                        }
                        let prepared = prepare(&path);
                        if !worker_alive.load(Ordering::Acquire) {
                            break;
                        }
                        let state = worker_state.clone();
                        let alive = worker_alive.clone();
                        let retry = retry.clone();
                        worker_application.post(move || {
                            if alive.load(Ordering::Acquire) {
                                // Explorer rename can retarget a placeholder while
                                // its old path is being read. Retry the current path
                                // without changing the tab's identity or focus.
                                let app = state.get();
                                let current_path = app
                                    .workspace
                                    .meta(id)
                                    .filter(|_| app.workspace.is_loading(id))
                                    .map(|m| m.path.clone());
                                if let Some(current) = current_path.filter(|p| *p != path) {
                                    crate::infrastructure::ui_dispatch::retire(prepared);
                                    let _ = retry.send(Some((id, current)));
                                    return;
                                }
                                state.try_update(|app| finish(app, id, path, prepared));
                            } else {
                                crate::infrastructure::ui_dispatch::retire(prepared);
                            }
                        });
                    }
                })
                .expect("could not start session restore");
            let timer_alive = alive.clone();
            std::thread::Builder::new()
                .name("loog-session-restore-delay".into())
                .spawn(move || {
                    if matches!(
                        delay.recv_timeout(crate::app::file_loading::INDICATOR_DELAY),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    ) {
                        application.post(move || {
                            if timer_alive.load(Ordering::Acquire) {
                                state.try_update(|app| app.workspace.show_restore_indicators());
                            }
                        });
                    }
                })
                .expect("could not start session restore delay");
        }
        Self {
            alive,
            stop_delay,
            work,
        }
    }

    pub fn shutdown(&self) {
        self.alive.store(false, Ordering::Release);
        let _ = self.stop_delay.send(());
        let _ = self.work.send(None);
    }
}

fn finish(
    app: &mut AppState,
    id: FileId,
    path: PathBuf,
    prepared: Result<PreparedTextFile, String>,
) -> bool {
    match prepared {
        Ok(prepared) => app.workspace.install_restored(id, &path, prepared),
        Err(error) => {
            if !app.workspace.is_loading(id)
                || app.workspace.meta(id).is_none_or(|m| m.path != path)
            {
                return false;
            }
            if app
                .workspace
                .documents
                .get(&id)
                .and_then(|d| d.recovery.as_ref())
                .is_some_and(|r| r.unsaved_text.is_some())
            {
                let prepared = PreparedTextFile::new(path.clone(), String::new());
                app.workspace.install_restored(id, &path, prepared);
                if let Some(doc) = app.workspace.documents.get_mut(&id) {
                    doc.missing_on_disk = !path.exists();
                    doc.disk_conflict = path.exists();
                }
            } else {
                app.workspace.fail_open(id, &path, error);
            }
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::MainSurface;
    use crate::app::workspace_session::{EditorLayout, SavedPaneNode};
    use lgui::{
        application::AppView,
        core::{UiId, UiScale},
        prelude::{UiRect, VisualStyle, group, panel},
        session::UiSession,
    };
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    type Task = Box<dyn FnOnce() + Send>;
    struct Harness {
        state: AppContext,
        tasks: Arc<Mutex<Vec<Task>>>,
        view: AppView,
        session: UiSession,
    }
    fn layout() -> EditorLayout {
        EditorLayout {
            root: SavedPaneNode::Pane {
                items: vec!["first.txt".into(), "large.json".into(), "last.txt".into()],
                active: Some(1),
            },
            active_pane: 0,
        }
    }
    impl Harness {
        fn new() -> Self {
            let exposed = Arc::new(Mutex::new(None));
            let output = exposed.clone();
            let rect = UiRect::new(0.0, 0.0, 800.0, 400.0);
            let view: AppView = Arc::new(move |cx| {
                let state = AppContext::detached(cx.state_with(|| {
                    let mut app = AppState::new();
                    crate::app::workspace_commands::hydrate_editor_layout(&mut app, Some(layout()));
                    app
                }));
                *output.lock().unwrap() = Some(state.clone());
                let focus = cx.focus_handle(UiId::new("restore-surface"));
                let pane = state.get().workspace.active_pane();
                group(rect)
                    .child(panel(rect, VisualStyle::filled(crate::ui::theme::c().bg)))
                    .child(crate::app::tabs::render(
                        pane,
                        UiRect::new(0.0, 0.0, 800.0, 36.0),
                        state,
                        focus,
                    ))
            });
            let mut session = UiSession::new();
            session.render_view(&view, rect, UiScale::ONE);
            let state = exposed.lock().unwrap().clone().unwrap();
            Self {
                state,
                tasks: Arc::new(Mutex::new(Vec::new())),
                view,
                session,
            }
        }
        fn application(&self) -> ApplicationHandle {
            let queue = self.tasks.clone();
            ApplicationHandle::new(move |task| queue.lock().unwrap().push(task), || {})
        }
        fn pump_until(&mut self, done: impl Fn(&AppState) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let tasks = std::mem::take(&mut *self.tasks.lock().unwrap());
                for task in tasks {
                    task();
                }
                if done(&self.state.get()) {
                    return;
                }
                assert!(Instant::now() < deadline, "restore did not finish");
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    #[test]
    fn first_frame_and_interaction_do_not_wait_for_large_file_preparation() {
        let mut h = Harness::new();
        assert_eq!(
            crate::app::workspace_commands::saved_app_editor_layout(&h.state.get()),
            Some(layout())
        );
        assert_eq!(h.state.get().main_surface(), MainSurface::Empty);
        assert!(h.state.get().workspace.active_editor_mut().is_none());
        assert!(h.state.get().workspace.active_save_snapshot().is_none());
        let main_thread = std::thread::current().id();
        let (entered_tx, entered) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let mut first = true;
        let restore = SessionRestore::with_prepare(h.state.clone(), h.application(), move |path| {
            assert_ne!(std::thread::current().id(), main_thread);
            if first {
                entered_tx.send(path.to_path_buf()).unwrap();
                gate.recv().unwrap();
                first = false;
            }
            Ok(PreparedTextFile::new(
                path.to_path_buf(),
                if path == Path::new("large.json") {
                    "x".repeat(8 * 1024 * 1024)
                } else {
                    "small".into()
                },
            ))
        });
        assert_eq!(
            entered.recv_timeout(Duration::from_secs(2)).unwrap(),
            PathBuf::from("large.json")
        );
        let before = Instant::now();
        h.state.update(|app| {
            let id = app
                .workspace
                .file_id_for_path(Path::new("last.txt"))
                .unwrap();
            app.workspace.activate_document(id);
        });
        h.session
            .render_view(&h.view, UiRect::new(0.0, 0.0, 800.0, 400.0), UiScale::ONE);
        assert!(
            before.elapsed() < Duration::from_secs(1),
            "frame waited for the blocked worker"
        );
        assert_eq!(h.state.get().workspace.loading_files().len(), 3);
        release.send(()).unwrap();
        h.pump_until(|app| app.workspace.loading_files().is_empty());
        let app = h.state.get();
        assert_eq!(
            app.workspace
                .meta(app.workspace.active_document().unwrap())
                .unwrap()
                .name,
            "last.txt"
        );
        assert_eq!(app.workspace.active_editor().unwrap().text(), "small");
        let id = app
            .workspace
            .file_id_for_path(Path::new("large.json"))
            .unwrap();
        assert!(app.workspace.documents.contains_key(&id));
        assert!(!app.workspace.is_dirty(id));
        restore.shutdown();
    }

    #[test]
    fn closing_restoring_tab_prevents_resurrection_and_updates_session() {
        let mut app = AppState::new();
        crate::app::workspace_commands::hydrate_editor_layout(&mut app, Some(layout()));
        let id = app.workspace.active_document().unwrap();
        let path = PathBuf::from("large.json");
        app.workspace.close(id);
        assert!(app.workspace.file_id_for_path(&path).is_none());
        assert!(!finish(
            &mut app,
            id,
            path.clone(),
            Ok(PreparedTextFile::new(path.clone(), "late".into()))
        ));
        let saved = crate::app::workspace_commands::saved_app_editor_layout(&app).unwrap();
        let SavedPaneNode::Pane { items, .. } = saved.root else {
            panic!()
        };
        assert_eq!(
            items
                .iter()
                .map(|v| v.path().to_path_buf())
                .collect::<Vec<_>>(),
            vec![PathBuf::from("first.txt"), PathBuf::from("last.txt")]
        );
        let new = app
            .workspace
            .open_path(path.clone(), "new edited buffer".into());
        assert_ne!(new, id);
        assert!(!finish(
            &mut app,
            id,
            path.clone(),
            Ok(PreparedTextFile::new(path, "old request".into()))
        ));
        assert_eq!(
            app.workspace.active_editor().unwrap().text(),
            "new edited buffer"
        );
    }

    #[test]
    fn moved_shared_tab_is_filled_in_place_without_changing_focus() {
        use crate::model::pane_layout::Direction;
        let mut app = AppState::new();
        crate::app::workspace_commands::hydrate_editor_layout(&mut app, Some(layout()));
        let id = app.workspace.active_document().unwrap();
        let left = app.workspace.active_pane();
        let right = app.workspace.split_empty(left, Direction::Right).unwrap();
        let source = app.workspace.active().unwrap();
        assert!(app.workspace.copy_item(left, source, right, 0));
        let copied = app.workspace.active().unwrap();
        let other = app.workspace.open_path("other.txt".into(), "other".into());
        let before = crate::app::workspace_commands::saved_app_editor_layout(&app);
        assert!(finish(
            &mut app,
            id,
            "large.json".into(),
            Ok(PreparedTextFile::new(
                "large.json".into(),
                "restored".into()
            ))
        ));
        assert_eq!(app.workspace.active_document(), Some(other));
        assert_eq!(
            crate::app::workspace_commands::saved_app_editor_layout(&app),
            before
        );
        assert_eq!(app.workspace.editor(left).unwrap().text(), "restored");
        app.workspace.activate(right, copied);
        assert_eq!(app.workspace.active_editor().unwrap().text(), "restored");
    }

    #[test]
    fn failed_restore_keeps_the_tab_and_describes_the_failure_without_a_toast() {
        let mut app = AppState::new();
        crate::app::workspace_commands::hydrate_editor_layout(&mut app, Some(layout()));
        let id = app.workspace.active_document().unwrap();
        assert!(finish(
            &mut app,
            id,
            "large.json".into(),
            Err("Could not read large.json".into())
        ));
        assert!(app.chrome.toast.is_none());
        assert_eq!(
            app.workspace.file_id_for_path(Path::new("large.json")),
            Some(id)
        );
        assert_eq!(
            app.workspace.open_error(id),
            Some("Could not read large.json")
        );
        assert!(app.workspace.active_editor().is_none());
        assert!(app.workspace.save_snapshot(id).is_none());
        assert_eq!(app.workspace.loading_files().len(), 2);
        assert!(!finish(
            &mut app,
            id,
            "large.json".into(),
            Err("stale error".into())
        ));
    }

    #[test]
    fn rename_during_restore_retries_current_path_without_losing_tab() {
        let mut h = Harness::new();
        let (entered_tx, entered) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let mut first = true;
        let restore = SessionRestore::with_prepare(h.state.clone(), h.application(), move |path| {
            if first {
                entered_tx.send(()).unwrap();
                gate.recv().unwrap();
                first = false;
                return Err("old path was renamed".into());
            }
            Ok(PreparedTextFile::new(
                path.to_path_buf(),
                path.to_string_lossy().into_owned(),
            ))
        });
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        let id = h.state.get().workspace.active_document().unwrap();
        h.state.update(|app| {
            assert!(
                app.workspace
                    .move_open_path(Path::new("large.json"), "renamed.json".into())
            );
        });
        release.send(()).unwrap();
        h.pump_until(|app| app.workspace.loading_files().is_empty());
        let app = h.state.get();
        assert_eq!(app.workspace.active_document(), Some(id));
        assert_eq!(
            app.workspace.active_editor().unwrap().text(),
            "renamed.json"
        );
        assert!(
            app.workspace
                .file_id_for_path(Path::new("large.json"))
                .is_none()
        );
        assert_eq!(
            app.workspace.file_id_for_path(Path::new("renamed.json")),
            Some(id)
        );
        assert!(app.chrome.toast.is_none());
        restore.shutdown();
    }

    #[test]
    fn shutdown_ignores_already_posted_results() {
        let mut h = Harness::new();
        let restore = SessionRestore::with_prepare(h.state.clone(), h.application(), |path| {
            Ok(PreparedTextFile::new(path.to_path_buf(), "ready".into()))
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while h.tasks.lock().unwrap().is_empty() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        restore.shutdown();
        h.pump_until(|_| true);
        assert_eq!(h.state.get().workspace.loading_files().len(), 3);
        assert_eq!(
            crate::app::workspace_commands::saved_app_editor_layout(&h.state.get()),
            Some(layout())
        );
    }

    #[test]
    fn restore_indicator_delay_does_not_show_on_fast_ready_tabs() {
        let mut h = Harness::new();
        let (entered_tx, entered) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let restore = SessionRestore::with_prepare(h.state.clone(), h.application(), move |path| {
            entered_tx.send(()).unwrap();
            gate.recv().unwrap();
            Ok(PreparedTextFile::new(path.to_path_buf(), "ready".into()))
        });
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        let id = h.state.get().workspace.active_document().unwrap();
        assert!(!h.state.get().workspace.loading_indicator(id));
        h.pump_until(|app| app.workspace.loading_indicator(id));
        restore.shutdown();
        release.send(()).unwrap();
        // The completed document itself always clears the indicator; queued
        // timer callbacks only touch documents still marked as loading.
        h.state.update(|app| {
            finish(
                app,
                id,
                "large.json".into(),
                Ok(PreparedTextFile::new("large.json".into(), "ready".into())),
            );
        });
        assert!(!h.state.get().workspace.loading_indicator(id));
    }
}
