//! File preparation stays off the event loop. Failures keep an inert file tab.
use crate::app::context::AppContext;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::app::state::{AppState, TabDrop};
use crate::app::workspace::{OpenMode, PreparedTextFile};
use crate::domain::identity::ViewId;
use crate::model::pane_layout::PaneId;
use lgui::ApplicationHandle;

pub const INDICATOR_DELAY: Duration = Duration::from_millis(300);

#[derive(Clone)]
pub struct PendingFile {
    pub generation: u64,
    pub path: PathBuf,
    pub pane: PaneId,
    pub mode: OpenMode,
    pub drop: Option<TabDrop>,
    pub markdown: bool,
    pub markdown_fragment: Option<String>,
    pub toggle_svg: bool,
    pub indicator: bool,
    started: Instant,
    active: Option<ViewId>,
    input_generation: Option<u64>,
    origin_pane: PaneId,
    folders: Vec<PathBuf>,
    remaining: Vec<PathBuf>,
}

#[derive(Clone)]
pub struct FileLoader {
    worker: crate::infrastructure::file_loader::Worker<PendingFile>,
}
impl crate::infrastructure::file_loader::Request for PendingFile {
    fn generation(&self) -> u64 {
        self.generation
    }
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}
impl FileLoader {
    pub fn new(state: AppContext, application: ApplicationHandle) -> Self {
        Self::with_prepare(state, application, |request| {
            crate::app::encoding::prepare(&request.path)
        })
    }
    fn with_prepare(
        state: AppContext,
        application: ApplicationHandle,
        prepare: impl Fn(&PendingFile) -> Result<PreparedTextFile, String> + Send + 'static,
    ) -> Self {
        let ready_state = state.clone();
        let ready_application = application.clone();
        let worker = crate::infrastructure::file_loader::Worker::new(
            prepare,
            move |generation, prepared, lease| {
                let state = ready_state.clone();
                ready_application.post(move || {
                    if lease.is_current(generation) {
                        let loader = state.services().and_then(|runtime| runtime.file_loader());
                        state.try_update(|app| {
                            let next = app
                                .editor
                                .pending_file
                                .as_ref()
                                .filter(|pending| {
                                    pending.generation == generation && valid(app, pending)
                                })
                                .map(|pending| (pending.pane, pending.remaining.clone()));
                            let changed = finish(app, generation, prepared);
                            if let Some((pane, paths)) = next {
                                request_files(app, pane, paths, loader.as_ref());
                            }
                            changed
                        });
                    } else {
                        crate::infrastructure::ui_dispatch::retire(prepared);
                    }
                });
            },
            move |generation, lease| {
                let state = state.clone();
                application.post(move || {
                    if lease.is_current(generation) {
                        state.try_update(|app| show_indicator(app, generation, Instant::now()));
                    }
                });
            },
            INDICATOR_DELAY,
        );
        Self { worker }
    }
    pub(crate) fn invalidate(&self) {
        self.worker.invalidate();
    }
    pub fn shutdown(&self) {
        self.worker.shutdown();
    }
}

/// Returns false only before the application has installed its worker.
pub fn request(
    app: &mut AppState,
    path: PathBuf,
    mode: OpenMode,
    drop: Option<TabDrop>,
    loader: Option<&FileLoader>,
) -> bool {
    let Some(loader) = loader else {
        return false;
    };
    let origin_pane = app.workspace.active_pane();
    if let Some(pending) = app
        .editor
        .pending_file
        .as_mut()
        .filter(|p| p.path == path && p.drop == drop && p.origin_pane == origin_pane)
    {
        if mode == OpenMode::Permanent {
            pending.mode = mode;
        }
        pending.toggle_svg = false;
        return true;
    }
    let generation = loader.worker.reserve_generation();
    let pane = match drop {
        Some(TabDrop::Insert { pane, .. } | TabDrop::Pane { pane, .. }) => pane,
        None => app.workspace.active_pane(),
    };
    let pending = PendingFile {
        generation,
        path,
        pane,
        mode,
        drop,
        markdown: false,
        markdown_fragment: None,
        toggle_svg: false,
        indicator: false,
        started: Instant::now(),
        active: app.workspace.pane(pane).and_then(|p| p.active()),
        input_generation: app
            .workspace
            .pane(pane)
            .and_then(|p| p.active())
            .and_then(|id| app.workspace.view(id))
            .map(|v| v.input_generation),
        origin_pane,
        folders: app.explorer.model.roots().to_vec(),
        remaining: Vec::new(),
    };
    app.editor.pending_file = Some(pending.clone());
    if !loader.worker.send_work(pending.clone()) {
        finish(app, generation, Err("Could not start file loading.".into()));
        return true;
    }
    loader.worker.send_timer(pending);
    true
}

pub fn cancel(app: &mut AppState) {
    app.editor.pending_file = None;
}

/// Keep all files in a native drop; starting separate requests would supersede
/// every file except the last. Each item still uses the interactive loader.
pub(crate) fn request_files(
    app: &mut AppState,
    pane: PaneId,
    paths: Vec<PathBuf>,
    loader: Option<&FileLoader>,
) -> bool {
    if app.workspace.pane(pane).is_none() || paths.is_empty() {
        return false;
    }
    app.workspace.activate_pane(pane);
    let mut paths = paths.into_iter();
    while let Some(path) = paths.next() {
        if app
            .workspace
            .file_id_for_path(&path)
            .is_some_and(|id| app.workspace.open_error(id).is_none())
            || loader.is_none()
        {
            crate::app::explorer::open_tree_file(app, path, 2, loader);
        } else {
            request(app, path, OpenMode::Permanent, None, loader);
            if let Some(pending) = &mut app.editor.pending_file {
                pending.remaining = paths.collect();
            }
            break;
        }
    }
    true
}

fn valid(app: &AppState, pending: &PendingFile) -> bool {
    app.explorer.model.roots() == pending.folders
        && pending
            .active
            .and_then(|id| app.workspace.view(id))
            .map(|v| v.input_generation)
            == pending.input_generation
        && app.workspace.active_pane() == pending.origin_pane
        && app
            .workspace
            .pane(pending.pane)
            .is_some_and(|pane| pane.active() == pending.active)
}

fn show_indicator(app: &mut AppState, generation: u64, now: Instant) -> bool {
    let Some(pending) = &mut app.editor.pending_file else {
        return false;
    };
    if pending.generation != generation
        || now.saturating_duration_since(pending.started) < INDICATOR_DELAY
    {
        return false;
    }
    if pending.indicator {
        return false;
    }
    pending.indicator = true;
    true
}

fn finish(app: &mut AppState, generation: u64, prepared: Result<PreparedTextFile, String>) -> bool {
    let Some(pending) = app
        .editor
        .pending_file
        .as_ref()
        .filter(|p| p.generation == generation)
        .cloned()
    else {
        crate::infrastructure::ui_dispatch::retire(prepared);
        return false;
    };
    let valid = valid(app, &pending);
    cancel(app);
    if !valid {
        crate::infrastructure::ui_dispatch::retire(prepared);
        return true;
    }
    let succeeded = prepared.is_ok();
    let prepared =
        prepared.unwrap_or_else(|error| PreparedTextFile::failed(pending.path.clone(), error));
    let pane = if let Some(TabDrop::Pane {
        pane,
        split: Some(direction),
    }) = pending.drop
    {
        let Some(new) = app.workspace.split_empty(pane, direction) else {
            crate::infrastructure::ui_dispatch::retire(prepared);
            return true;
        };
        new
    } else {
        pending.pane
    };
    app.workspace.activate_pane(pane);
    app.workspace.open_prepared_text(prepared, pending.mode);
    let id = app.workspace.active().expect("opened view");
    if let Some(TabDrop::Insert { index, .. }) = pending.drop {
        if let Some(position) = app
            .workspace
            .pane(pane)
            .and_then(|p| p.items().iter().position(|item| *item == id))
        {
            app.workspace.move_tab(
                pane,
                id,
                if position < index {
                    index.saturating_sub(1)
                } else {
                    index
                },
            );
        }
    }
    app.chrome.toast = None;
    app.chrome.welcome_hover = None;
    app.chrome.workspace_home_hover = None;
    app.editor.drag = None;
    app.editor.ime = None;
    if pending.markdown && succeeded {
        crate::app::markdown_view::finish_navigation(app, id, pending.markdown_fragment.clone());
    }
    if pending.toggle_svg && succeeded {
        crate::app::image_preview::run(app, id, crate::ui::image_preview::Command::Toggle);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn svg_tree_menu_request_switches_the_completed_tab_to_source() {
        let mut app = AppState::new();
        app.workspace.open_path("seed.txt".into(), "keep".into());
        let pane = app.workspace.active_pane();
        let active = app.workspace.active();
        app.editor.pending_file = Some(PendingFile {
            generation: 42,
            path: "logo.svg".into(),
            pane,
            mode: OpenMode::Permanent,
            drop: None,
            markdown: false,
            markdown_fragment: None,
            toggle_svg: true,
            indicator: false,
            started: Instant::now(),
            active,
            input_generation: active
                .and_then(|id| app.workspace.view(id))
                .map(|view| view.input_generation),
            origin_pane: pane,
            folders: app.explorer.model.roots().to_vec(),
            remaining: Vec::new(),
        });
        let text = "<svg xmlns=\"http://www.w3.org/2000/svg\"/>";
        assert!(finish(
            &mut app,
            42,
            Ok(PreparedTextFile::new("logo.svg".into(), text.into()))
        ));
        assert_eq!(app.main_surface(), crate::app::state::MainSurface::Editor);
        assert_eq!(app.workspace.active_editor().unwrap().text(), text);
        assert_eq!(app.workspace.active_items().len(), 2);
    }
    use crate::app::test_support::UiTasks;
    use lgui::{
        application::AppView,
        core::UiScale,
        prelude::{UiRect, VisualStyle, panel},
        session::UiSession,
    };
    use std::sync::{Arc, Mutex, mpsc};

    #[test]
    fn p1_pending_open_rejects_a_changed_presentation_on_the_same_view() {
        use crate::domain::view::Presentation;
        let mut app = AppState::new();
        app.workspace.open_path("current.md".into(), "keep".into());
        let pane = app.workspace.active_pane();
        let active = app.workspace.active();
        let pending = PendingFile {
            generation: 17,
            path: "next.md".into(),
            pane,
            mode: OpenMode::Permanent,
            drop: None,
            markdown: false,
            markdown_fragment: None,
            toggle_svg: false,
            indicator: false,
            started: Instant::now(),
            active,
            input_generation: Some(0),
            origin_pane: pane,
            folders: Vec::new(),
            remaining: Vec::new(),
        };
        assert!(valid(&app, &pending));
        app.editor.pending_file = Some(pending);
        app.workspace
            .set_presentation(active.unwrap(), Presentation::Markdown);
        app.workspace
            .set_presentation(active.unwrap(), Presentation::Source);
        assert!(finish(
            &mut app,
            17,
            Ok(PreparedTextFile::new("next.md".into(), "late".into()))
        ));
        assert_eq!(app.workspace.active(), active);
        assert_eq!(app.workspace.active_editor().unwrap().text(), "keep");
        assert!(
            app.workspace
                .file_id_for_path(std::path::Path::new("next.md"))
                .is_none()
        );
    }

    struct Harness {
        state: AppContext,
        tasks: UiTasks,
        runtime: Arc<crate::app::runtime::AppRuntime>,
        _session: UiSession,
    }
    impl Harness {
        fn new(
            prepare: impl Fn(&PendingFile) -> Result<PreparedTextFile, String> + Send + 'static,
        ) -> Self {
            let exposed = Arc::new(Mutex::new(None));
            let output = exposed.clone();
            let rect = UiRect::new(0.0, 0.0, 600.0, 400.0);
            let view: AppView = Arc::new(move |cx| {
                let state = AppContext::detached(cx.state_with(|| {
                    let mut app = AppState::new();
                    app.workspace
                        .open_path("current.txt".into(), "Keep this screen".into());
                    app
                }));
                *output.lock().unwrap() = Some(state);
                panel(rect, VisualStyle::default())
            });
            let mut session = UiSession::new();
            session.render_view(&view, rect, UiScale::ONE);
            let state = exposed.lock().unwrap().clone().unwrap();
            let tasks = UiTasks::default();
            let application = tasks.application();
            let runtime = Arc::new(crate::app::runtime::AppRuntime::default());
            let state = state.with_runtime(&runtime);
            let loader = FileLoader::with_prepare(state.clone(), application, prepare);
            runtime.install_file_loader(loader);
            Self {
                state,
                tasks,
                runtime,
                _session: session,
            }
        }
        fn open(&self, path: &str) {
            self.state.update(|app| {
                assert!(crate::app::explorer::open_tree_file(
                    app,
                    path.into(),
                    1,
                    self.runtime.file_loader().as_ref()
                ));
            });
        }
        fn pump_until(&self, predicate: impl Fn(&AppState) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                self.tasks.run_all();
                if predicate(&self.state.get()) {
                    break;
                }
                assert!(Instant::now() < deadline, "file loader did not respond");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    impl Drop for Harness {
        fn drop(&mut self) {
            self.runtime.clear_file_loader();
        }
    }

    #[test]
    fn external_drop_loads_every_file_in_order_and_keeps_tabs_permanent() {
        let h = Harness::new(|request| {
            if request.path == std::path::Path::new("bad.bin") {
                return Err("Unsupported encoding".into());
            }
            Ok(PreparedTextFile::new(request.path.clone(), "loaded".into()))
        });
        h.state.update(|app| {
            let pane = app.workspace.active_pane();
            assert!(request_files(
                app,
                pane,
                ["first.txt", "bad.bin", "second.txt", "first.txt"]
                    .map(PathBuf::from)
                    .to_vec(),
                h.runtime.file_loader().as_ref()
            ));
        });
        h.pump_until(|app| app.editor.pending_file.is_none());
        let app = h.state.get();
        let paths: Vec<_> = app
            .workspace
            .active_items()
            .iter()
            .map(|id| app.workspace.view_meta(*id).unwrap().path.clone())
            .collect();
        assert_eq!(
            paths,
            ["current.txt", "first.txt", "bad.bin", "second.txt"].map(PathBuf::from)
        );
        assert_eq!(
            app.workspace.active_path(),
            Some(std::path::Path::new("first.txt"))
        );
        assert!(app.workspace.preview().is_none());
    }

    #[test]
    fn external_drop_opens_in_target_pane_and_preserves_unsaved_document() {
        let h =
            Harness::new(|request| Ok(PreparedTextFile::new(request.path.clone(), "new".into())));
        let original = h.state.get().workspace.active_pane();
        let mut target = original;
        h.state.update(|app| {
            app.workspace
                .active_editor_mut()
                .unwrap()
                .replace_selection("unsaved ", loog_editor_core::text::EditKind::Typing)
                .unwrap();
            target = app
                .workspace
                .split_empty(original, crate::model::pane_layout::Direction::Right)
                .unwrap();
            request_files(
                app,
                target,
                vec!["dropped.txt".into()],
                h.runtime.file_loader().as_ref(),
            );
        });
        h.pump_until(|app| app.editor.pending_file.is_none());
        let app = h.state.get();
        assert_eq!(app.workspace.active_pane(), target);
        let original_file = app.workspace.pane(original).unwrap().active().unwrap();
        assert_eq!(
            app.workspace.view_meta(original_file).unwrap().path,
            PathBuf::from("current.txt")
        );
        assert!(app.workspace.is_view_dirty(original_file));
        assert_eq!(
            app.workspace.editor(original).unwrap().text(),
            "unsaved Keep this screen"
        );
        assert_eq!(
            app.workspace.active_path(),
            Some(std::path::Path::new("dropped.txt"))
        );
    }

    #[test]
    fn superseding_external_drop_cancels_its_remaining_files() {
        let (release, gate) = mpsc::channel();
        let h = Harness::new(move |request| {
            if request.path == std::path::Path::new("first.txt") {
                gate.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            Ok(PreparedTextFile::new(request.path.clone(), "loaded".into()))
        });
        h.state.update(|app| {
            request_files(
                app,
                app.workspace.active_pane(),
                vec!["first.txt".into(), "second.txt".into()],
                h.runtime.file_loader().as_ref(),
            );
        });
        h.open("replacement.txt");
        release.send(()).unwrap();
        h.pump_until(|app| app.editor.pending_file.is_none());
        let app = h.state.get();
        assert!(
            app.workspace
                .file_id_for_path(std::path::Path::new("first.txt"))
                .is_none()
        );
        assert!(
            app.workspace
                .file_id_for_path(std::path::Path::new("second.txt"))
                .is_none()
        );
        assert_eq!(
            app.workspace.active_path(),
            Some(std::path::Path::new("replacement.txt"))
        );
    }

    #[test]
    fn slow_open_keeps_current_screen_interactive_and_delays_only_the_icon() {
        let (release, gate) = mpsc::channel();
        let h = Harness::new(move |request| {
            gate.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(PreparedTextFile::new(
                request.path.clone(),
                "new contents".into(),
            ))
        });
        h.open("slow.json");
        let before = h.state.get().workspace.active();
        h.state.update(|app| {
            let pending = app.editor.pending_file.as_ref().unwrap();
            let generation = pending.generation;
            let started = pending.started;
            let mut clock = crate::app::test_support::ManualClock::default();
            clock.advance(Duration::from_millis(299));
            assert!(!show_indicator(app, generation, started + clock.elapsed()));
            assert!(!app.editor.pending_file.as_ref().unwrap().indicator);
            // An unrelated UI click and edit complete while the worker is held.
            app.drawer.visible = !app.drawer.visible;
            app.workspace.active_editor_mut().unwrap().set_cursor(4);
            assert_eq!(app.workspace.active(), before);
            assert_eq!(
                app.workspace.active_editor().unwrap().text(),
                "Keep this screen"
            );
        });
        h.pump_until(|app| app.editor.pending_file.as_ref().unwrap().indicator);
        assert_eq!(h.state.get().workspace.active(), before);
        // Double-click promotion does not restart the timer or disk operation.
        h.state.update(|app| {
            crate::app::explorer::open_tree_file(
                app,
                "slow.json".into(),
                2,
                h.runtime.file_loader().as_ref(),
            );
        });
        release.send(()).unwrap();
        h.pump_until(|app| app.editor.pending_file.is_none());
        let app = h.state.get();
        assert_eq!(
            app.workspace.active_path(),
            Some(std::path::Path::new("slow.json"))
        );
        assert!(
            !app.workspace
                .pane(app.workspace.active_pane())
                .unwrap()
                .is_preview(app.workspace.active().unwrap())
        );
        assert_eq!(
            app.workspace.active_editor().unwrap().text(),
            "new contents"
        );
    }

    #[test]
    fn last_selection_wins_and_a_cancelled_result_cannot_replace_the_screen() {
        let (entered, received) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let h = Harness::new(move |request| {
            if request.path == std::path::Path::new("first.json") {
                entered.send(()).unwrap();
                gate.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            Ok(PreparedTextFile::new(
                request.path.clone(),
                request.path.to_string_lossy().into_owned(),
            ))
        });
        h.open("first.json");
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        h.open("last.json");
        release.send(()).unwrap();
        h.pump_until(|app| app.editor.pending_file.is_none());
        let app = h.state.get();
        assert_eq!(
            app.workspace.active_path(),
            Some(std::path::Path::new("last.json"))
        );
        assert!(
            app.workspace
                .file_id_for_path(std::path::Path::new("first.json"))
                .is_none()
        );
        assert!(app.editor.pending_file.is_none());
    }

    #[test]
    fn returning_to_an_existing_tab_cancels_a_load_and_its_late_error() {
        let (entered, received) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let h = Harness::new(move |_| {
            entered.send(()).unwrap();
            gate.recv_timeout(Duration::from_secs(5)).unwrap();
            finished.send(()).unwrap();
            Err("old error".into())
        });
        h.open("cancelled.json");
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        h.open("current.txt");
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(5)).unwrap();
        h.pump_until(|app| app.editor.pending_file.is_none());
        assert_eq!(
            h.state.get().workspace.active_path(),
            Some(std::path::Path::new("current.txt"))
        );
        assert!(h.state.get().chrome.toast.is_none());
    }

    #[test]
    fn failures_keep_an_error_tab_and_ignore_delayed_indicators() {
        let h = Harness::new(|_| Err("Could not open missing.json".into()));
        h.open("missing.json");
        let generation = h
            .state
            .get()
            .editor
            .pending_file
            .as_ref()
            .unwrap()
            .generation;
        h.pump_until(|app| app.editor.pending_file.is_none());
        h.state.update(|app| {
            assert!(!show_indicator(
                app,
                generation,
                Instant::now() + INDICATOR_DELAY
            ));
            assert!(app.workspace.active_editor().is_none());
            assert_eq!(
                app.workspace.pane_open_error(app.workspace.active_pane()),
                Some("Could not open missing.json")
            );
            let previous = app
                .workspace
                .file_id_for_path(std::path::Path::new("current.txt"))
                .unwrap();
            assert_eq!(
                app.workspace.documents[&previous].buffer.text(),
                "Keep this screen"
            );
            assert!(app.chrome.toast.is_none());
        });
    }

    #[test]
    fn retrying_a_failed_file_reuses_its_tab_and_restores_editing() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let attempts = AtomicUsize::new(0);
        let h = Harness::new(move |request| {
            if attempts.fetch_add(1, Ordering::Relaxed) == 0 {
                Err("Permission denied".into())
            } else {
                Ok(PreparedTextFile::new(request.path.clone(), "ready".into()))
            }
        });
        h.open("retry.txt");
        h.pump_until(|app| app.editor.pending_file.is_none());
        let view = h.state.get().workspace.active().unwrap();
        let document = h.state.get().workspace.active_document().unwrap();
        assert_eq!(
            h.state.get().workspace.open_error(document),
            Some("Permission denied")
        );
        assert!(h.state.get().workspace.save_snapshot(document).is_none());
        h.open("retry.txt");
        h.pump_until(|app| app.editor.pending_file.is_none());
        let app = h.state.get();
        assert_eq!(app.workspace.active(), Some(view));
        assert_eq!(app.workspace.active_document(), Some(document));
        assert_eq!(app.workspace.active_editor().unwrap().text(), "ready");
        assert_eq!(app.workspace.active_items().len(), 2);
        assert!(app.chrome.toast.is_none());
    }

    #[test]
    fn final_session_snapshot_includes_pending_and_just_loaded_files_without_a_render() {
        let (release, gate) = mpsc::channel();
        let h = Harness::new(move |request| {
            gate.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(PreparedTextFile::new(request.path.clone(), "ready".into()))
        });
        h.open("single-click.json");
        let expected = crate::app::workspace_session::EditorLayout {
            root: crate::app::workspace_session::SavedPaneNode::Pane {
                items: vec!["current.txt".into(), "single-click.json".into()],
                active: Some(1),
            },
            active_pane: 0,
        };
        assert_eq!(
            crate::app::workspace_commands::saved_app_editor_layout(&h.state.get()),
            Some(expected.clone())
        );
        release.send(()).unwrap();
        h.pump_until(|app| app.editor.pending_file.is_none());
        // No session/render effect has run since the background result was installed.
        assert_eq!(
            crate::app::workspace_commands::saved_app_editor_layout(&h.state.get()),
            Some(expected)
        );
        assert!(
            h.state
                .get()
                .workspace
                .pane(PaneId::new(1))
                .unwrap()
                .is_preview(h.state.get().workspace.active().unwrap())
        );
    }

    #[test]
    fn changing_workspaces_discards_a_prepared_result_without_reopening_it() {
        let (release, gate) = mpsc::channel();
        let h = Harness::new(move |request| {
            gate.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(PreparedTextFile::new(request.path.clone(), "late".into()))
        });
        h.open("old-workspace.txt");
        h.state.update(|app| {
            app.explorer
                .model
                .restore(vec!["different-workspace".into()], Vec::new())
        });
        release.send(()).unwrap();
        h.pump_until(|app| app.editor.pending_file.is_none());
        assert_eq!(
            h.state.get().workspace.active_path(),
            Some(std::path::Path::new("current.txt"))
        );
    }

    #[test]
    #[ignore = "manual SVG tab appearance verification"]
    fn render_vector_tab_preview() {
        use lgui::{
            application::ApplicationContext,
            core::{PhysicalRect, UiId},
            icons::IconRegistration,
            prelude::group,
            render_skia::SkiaSoftwareSurface,
            renderer::{FrameInfo, FrameReason},
        };
        let rect = UiRect::new(0.0, 0.0, 300.0, 108.0);
        let view: AppView = Arc::new(move |cx| {
            let mut root = group(rect);
            for row in 0..3 {
                let state = AppContext::detached(cx.state_with(|| {
                    let mut app = AppState::new();
                    app.drawer.visible = true;
                    for _ in 0..row {
                        let pane = app.workspace.active_pane();
                        let new = app
                            .workspace
                            .split_empty(pane, crate::model::pane_layout::Direction::Right)
                            .unwrap();
                        app.workspace.activate_pane(new);
                    }
                    let pane = app.workspace.active_pane();
                    if row < 2 {
                        app.editor.pending_file = Some(PendingFile {
                            generation: 1,
                            path: "seed.database.json".into(),
                            pane,
                            mode: OpenMode::Permanent,
                            drop: None,
                            markdown: false,
                            markdown_fragment: None,
                            toggle_svg: false,
                            indicator: row == 1,
                            started: Instant::now(),
                            active: None,
                            input_generation: None,
                            origin_pane: pane,
                            folders: Vec::new(),
                            remaining: Vec::new(),
                        });
                    } else {
                        app.workspace
                            .open_path("seed.database.json".into(), String::new());
                    }
                    app
                }));
                let id = UiId::owned(format!("preview-focus-{row}"));
                let focus = cx.focus_handle(id);
                let pane = state.get().workspace.active_pane();
                root = root.child(crate::app::tabs::render(
                    pane,
                    UiRect::new(0.0, row as f32 * 36.0, 300.0, (row + 1) as f32 * 36.0),
                    state,
                    focus,
                ));
            }
            root
        });
        let mut session = UiSession::new();
        session.render_view(&view, rect, UiScale::new(2.0));
        let context = ApplicationContext::empty(Default::default());
        context
            .resources()
            .provide(IconRegistration(Arc::new(crate::app::svg_icons())));
        let mut surface = SkiaSoftwareSurface::new(1024 * 1024);
        let scale = UiScale::new(2.0);
        let frame = FrameInfo::new(
            PhysicalRect::new(0, 0, 600, 216),
            &[],
            scale,
            FrameReason::SceneChange,
            true,
        );
        lgui::assets::backend::with_render_resources(
            &context,
            lgui::assets::backend::render_resources(),
            || {
                surface
                    .draw(&session.tree().scene().project_to_physical(scale), &frame)
                    .unwrap();
            },
        );
        let pixels = surface.pixels().to_vec();
        let image = image::RgbaImage::from_raw(600, 216, pixels).unwrap();
        image
            .save(std::env::var("LOOG_TAB_PREVIEW").expect("preview destination"))
            .unwrap();
    }
    #[test]
    fn queued_completion_cannot_publish_after_loader_shutdown_with_snapshot_retained() {
        let harness = Harness::new(|request| {
            Ok(PreparedTextFile::new(
                request.path.clone(),
                "late payload".into(),
            ))
        });
        harness.open("queued-after-shutdown.txt");
        let deadline = Instant::now() + Duration::from_secs(5);
        while harness.tasks.pending_ids().is_empty() {
            assert!(Instant::now() < deadline, "worker did not queue completion");
            std::thread::yield_now();
        }
        let retained = harness.state.get();
        harness.runtime.clear_file_loader();
        harness.tasks.run_all();
        assert!(
            harness
                .state
                .get()
                .workspace
                .file_id_for_path(std::path::Path::new("queued-after-shutdown.txt"))
                .is_none()
        );
        assert_eq!(
            harness.state.get().workspace.active_path().unwrap(),
            std::path::Path::new("current.txt")
        );
        assert!(retained.editor.pending_file.is_some());
    }
}
