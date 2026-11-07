//! Search application adapter: composes feature state, services and Workspace navigation.
use crate::app::context::AppContext;
use crate::app::state::AppState;
use crate::features::search::model::SearchKey as Key;
use crate::features::search::{FileMatches, Request, Results};
use crate::infrastructure::ui_dispatch::LatestNotification;
use lgui::ApplicationHandle;
use lgui::core::UiFocusHandle;
use lgui::prelude::UiRect;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub fn key(app: &AppState) -> Key {
    Key {
        spec: app.search.spec(),
        roots: app.explorer.model.roots().to_vec(),
        versions: app
            .workspace
            .dirty_text_versions()
            .into_iter()
            .filter(|(path, _)| {
                app.explorer
                    .model
                    .roots()
                    .iter()
                    .any(|root| path.starts_with(root))
            })
            .collect(),
        composing: app.search.composing(),
        refresh: app.search.ui.refresh,
    }
}

/// Effect cleanup cancels only its own ticket, never a later search.
pub fn start(state: AppContext, application: ApplicationHandle, expected: Key) -> impl FnOnce() {
    let cancel = Arc::new(AtomicBool::new(false));
    let mut request = None;
    let mut generation = 0;
    state.try_update(|app| {
        if key(app) != expected {
            return false;
        }
        let begin = app.search.model.begin(expected.clone());
        generation = begin.generation;
        if let Some(runtime) = state.services() {
            runtime.install_search_cancel(cancel.clone());
        }
        crate::infrastructure::ui_dispatch::retire(begin.retired);
        if !begin.retained {
            app.search.ui.selected = None;
            app.search.ui.scroll_y = 0.0;
        }
        if begin.running {
            request = Some(Request {
                spec: expected.spec.clone(),
                roots: expected.roots.clone(),
                buffers: app.workspace.text_snapshots(),
            });
        }
        true
    });
    let apply_state = state.clone();
    let apply_cancel = cancel.clone();
    let queue = LatestNotification::new(application, move |update: Result<Results, String>| {
        apply_state.try_update(|app| {
            let current = key(app);
            let applied = app.search.model.apply(
                generation,
                &expected,
                &current,
                apply_cancel.load(Ordering::Relaxed),
                update,
            );
            if applied.replaced {
                if let Some((path, index)) = &app.search.ui.selected
                    && !app
                        .search
                        .model
                        .status()
                        .results
                        .files
                        .iter()
                        .any(|file| &file.path == path && *index < file.matches.len())
                {
                    app.search.ui.selected = None;
                }
            }
            crate::infrastructure::ui_dispatch::retire(applied.retired);
            applied.changed
        });
    });
    let ticket = super::search_tasks::SearchTasks::launch(generation, request, cancel, queue);
    move || ticket.stop()
}

pub fn reset_filters(state: &AppContext) {
    state.update(|app| {
        app.search.ui.include.clear();
        app.search.ui.exclude.clear();
        app.search.ui.include_hidden = false;
        app.search.ui.include_ignored = false;
    });
}

pub fn clear(state: &AppContext) {
    state.update(|app| {
        if let Some(runtime) = state.services() {
            runtime.cancel_search();
        }
        app.search.ui.query.clear();
        app.search.ui.selected = None;
        app.search.ui.scroll_y = 0.0;
        app.search.ui.details_open = false;
        app.search.ui.collapsed.clear();
        let previous = app.search.model.clear();
        crate::infrastructure::ui_dispatch::retire(previous);
    });
}

pub fn refresh(state: &AppContext) {
    state.update(|app| app.search.ui.refresh += 1);
}

pub fn cancel(state: &AppContext) {
    state.try_update(|app| {
        if !app.search.model.status().running {
            return false;
        }
        if let Some(runtime) = state.services() {
            runtime.cancel_search();
        }
        app.search.model.cancel()
    });
}

pub fn selected(app: &AppState) -> Option<(Arc<FileMatches>, usize)> {
    crate::ui::search::commands::selected(&app.search)
}
pub fn move_selection(app: &mut AppState, delta: isize) {
    crate::ui::search::commands::move_selection(&mut app.search, delta);
}
pub fn reveal_selection(app: &mut AppState) {
    crate::ui::search::commands::reveal_selection(&mut app.search);
}
pub fn toggle_filters(search: &mut crate::ui::search::state::SearchPanelState) {
    crate::ui::search::commands::toggle_filters(&mut search.ui);
}

pub fn navigate_selected(
    state: &AppContext,
    application: ApplicationHandle,
    rect: UiRect,
    editor_focus: UiFocusHandle,
    permanent: bool,
    focus_editor: bool,
) {
    let app = state.get();
    let Some((file, index)) = selected(&app) else {
        return;
    };
    navigate(
        state,
        application,
        rect,
        editor_focus,
        file,
        index,
        permanent,
        focus_editor,
    );
}

pub fn navigate(
    state: &AppContext,
    application: ApplicationHandle,
    rect: UiRect,
    editor_focus: UiFocusHandle,
    file: Arc<FileMatches>,
    index: usize,
    permanent: bool,
    focus_editor: bool,
) {
    let app = state.get();
    if index >= file.matches.len() {
        return;
    }
    let Some(intent) =
        app.search
            .model
            .navigation(&key(&app), file.clone(), index, permanent, focus_editor)
    else {
        return;
    };
    let generation = intent.generation;
    let buffer = app
        .workspace
        .text_snapshots()
        .into_iter()
        .find(|buffer| buffer.path == file.path);
    state.update(|app| app.search.ui.selected = Some((file.path.clone(), index)));
    let state = state.clone();
    let worker_state = state.clone();
    let failed_path = file.path.clone();
    let worker = std::thread::Builder::new()
        .name("loog-search-open".into())
        .spawn(move || {
            let opened = crate::features::search::navigation::prepare(
                &crate::infrastructure::search_filesystem::SearchFilesystem,
                &intent,
                buffer.as_ref(),
            );
            application.post(move || {
                let mut focused = false;
                worker_state.try_update(|app| {
                    if app.search.model.status().generation != generation
                        || app.search.model.status().applied_key.as_ref() != Some(&key(app))
                        || app.search.ui.selected != Some((file.path.clone(), index))
                    {
                        return false;
                    }
                    match opened {
                        Ok((text, range)) => {
                            let current = app
                                .workspace
                                .text_snapshots()
                                .into_iter()
                                .find(|snapshot| snapshot.path == file.path);
                            let unchanged = match (&buffer, &current) {
                                (Some(before), Some(current)) => {
                                    before.version == current.version
                                        && Arc::ptr_eq(&before.text, &current.text)
                                }
                                (None, None) => true,
                                _ => false,
                            };
                            if !unchanged {
                                app.search.ui.refresh += 1;
                                return true;
                            }
                            let contents = if current.is_some() {
                                String::new()
                            } else {
                                text.as_ref().clone()
                            };
                            if permanent {
                                app.workspace.open_path(file.path.clone(), contents);
                            } else {
                                app.workspace.preview_path(file.path.clone(), contents);
                            }
                            if let Some(mut editor) = app.workspace.active_editor_mut() {
                                editor.select_range(range);
                            }
                            crate::app::editor::reveal_cursor(app, rect);
                            app.editor.drag = None;
                            app.editor.ime = None;
                            focused = focus_editor;
                        }
                        Err(crate::features::search::navigation::OpenError::Read(error)) => {
                            let mode = if permanent {
                                crate::app::workspace::OpenMode::Permanent
                            } else {
                                crate::app::workspace::OpenMode::Preview
                            };
                            app.workspace.open_failed(file.path.clone(), error, mode);
                            app.chrome.toast = None;
                            focused = focus_editor;
                            app.search.ui.refresh += 1;
                        }
                        Err(crate::features::search::navigation::OpenError::Changed(_)) => {
                            app.search.ui.refresh += 1;
                        }
                    }
                    true
                });
                if focused {
                    editor_focus.focus();
                }
            });
        });
    if let Err(error) = worker {
        state.update(|app| {
            let mode = if permanent {
                crate::app::workspace::OpenMode::Permanent
            } else {
                crate::app::workspace::OpenMode::Preview
            };
            app.workspace.open_failed(
                failed_path,
                format!("Could not open search result: {error}"),
                mode,
            );
            app.chrome.toast = None;
        });
    }
}

/// Composition stays here; Search receives no editor, Workspace or application handle.
pub fn render(
    rect: UiRect,
    state: AppContext,
    application: ApplicationHandle,
    editor_focus: UiFocusHandle,
    editor_rect: UiRect,
    inputs: crate::ui::search::view::Inputs,
) -> lgui::prelude::Element {
    use crate::ui::components::state_access::StateAccess;
    use crate::ui::search::{
        commands::{Command, Commands},
        view,
    };
    let root_count = state.get().explorer.model.roots().len();
    let has_roots = root_count > 0;
    let header = crate::app::explorer_view::drawer_header(
        UiRect::new(
            rect.left,
            rect.top,
            rect.right,
            rect.top + crate::ui::theme::TABS_H,
        ),
        &state,
    );
    let projected = StateAccess::project(state.model(), |app| &app.search, |app| &mut app.search);
    let commands = Commands::new(move |command| match command {
        Command::Refresh => refresh(&state),
        Command::Cancel => cancel(&state),
        Command::Clear => clear(&state),
        Command::ResetFilters => reset_filters(&state),
        Command::Select { file, index, point } => {
            let mut clicks = 1;
            state.update(|app| clicks = app.editor.tree_click_count(&file.path, point.0, point.1));
            navigate(
                &state,
                application.clone(),
                editor_rect,
                editor_focus.clone(),
                file,
                index,
                clicks > 1,
                clicks > 1,
            );
        }
    });
    view::render(
        rect,
        projected,
        commands,
        has_roots,
        root_count > 1,
        inputs,
        header,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::keymap::action::Action;
    use crate::features::search;
    use crate::features::search::{Matcher, SearchSpec};
    use std::time::Duration;
    #[test]
    fn selection_scroll_uses_file_and_match_heights_with_collapsed_groups() {
        let mut app = AppState::new();
        let matcher = Matcher::new(&SearchSpec {
            query: "x".into(),
            include: String::new(),
            exclude: String::new(),
            case_sensitive: false,
            whole_word: false,
            regex: false,
            include_hidden: false,
            include_ignored: false,
        })
        .unwrap();
        let files = ["a.rs", "b.rs", "c.rs"]
            .into_iter()
            .map(|name| {
                Arc::new(FileMatches {
                    path: name.into(),
                    root: PathBuf::new(),
                    content_hash: search::hash("x\nx\n"),
                    matches: matcher.matches("x\nx\n", &AtomicBool::new(false), 10),
                })
            })
            .collect::<Vec<_>>();
        app.search.model.fixture_status().results = Arc::new(Results {
            files,
            matches: 6,
            complete: true,
            ..Results::default()
        });
        app.search.ui.collapsed.insert("a.rs".into());
        move_selection(&mut app, 1);
        assert_eq!(app.search.ui.selected, Some(("b.rs".into(), 0)));
        assert_eq!(app.search.ui.scroll_y, 0.0);
        app.search.ui.viewport_height = 40.0;
        reveal_selection(&mut app);
        assert_eq!(app.search.ui.scroll_y, 28.0);
        move_selection(&mut app, 1);
        assert_eq!(app.search.ui.selected, Some(("b.rs".into(), 1)));
        assert_eq!(app.search.ui.scroll_y, 48.0);
        move_selection(&mut app, 1);
        assert_eq!(app.search.ui.selected, Some(("c.rs".into(), 0)));
        assert_eq!(app.search.ui.scroll_y, 92.0);
        move_selection(&mut app, -1);
        move_selection(&mut app, -1);
        assert_eq!(app.search.ui.scroll_y, 48.0);
    }
    #[test]
    fn query_key_tracks_search_inputs_and_workspace_edits_but_not_view_state() {
        let mut app = AppState::new();
        app.explorer
            .model
            .restore(vec![PathBuf::from("project")], Vec::new());
        let original = key(&app);
        app.apply(Action::OpenSearch);
        assert!(key(&app) == original);
        app.search.ui.query.set_text("needle");
        let query = key(&app);
        app.workspace
            .open_path(PathBuf::from("project/test.rs"), "needle".into());
        assert!(key(&app) == query);
        let before = key(&app);
        app.workspace
            .active_editor_mut()
            .unwrap()
            .replace_selection("x", crate::model::text::EditKind::Other)
            .unwrap();
        assert!(key(&app) != before);
        app.search.ui.query.preedit = "中".into();
        assert!(key(&app).composing);
        app.apply(Action::OpenExplorer);
        assert!(
            !app.search.composing(),
            "Hidden inputs must not retain IME ownership"
        );
        let hidden = key(&app);
        app.workspace
            .active_editor_mut()
            .unwrap()
            .replace_selection("y", crate::model::text::EditKind::Other)
            .unwrap();
        assert!(
            key(&app) != hidden,
            "Content changes must invalidate cached results even with the drawer hidden"
        );
        let edited = key(&app);
        app.workspace
            .open_path(PathBuf::from("outside.rs"), "needle".into());
        app.workspace
            .active_editor_mut()
            .unwrap()
            .replace_selection("z", crate::model::text::EditKind::Other)
            .unwrap();
        assert!(
            key(&app) == edited,
            "Files outside the workspace do not affect searching"
        );
        app.search.ui.case_sensitive = true;
        assert!(key(&app) != edited);
    }

    #[test]
    fn search_effect_preserves_results_across_unrelated_ui_and_drawer_changes() {
        use lgui::application::AppView;
        use lgui::core::UiScale;
        use lgui::prelude::{VisualStyle, panel};
        use lgui::session::UiSession;
        use std::sync::Mutex;
        use std::time::Instant;
        type Task = Box<dyn FnOnce() + Send>;
        let root = std::env::temp_dir().join(format!("loog-search-cache-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "needle").unwrap();
        let fixture_root = root.clone();
        let exposed = Arc::new(Mutex::new(None::<AppContext>));
        let output = exposed.clone();
        let tasks = Arc::new(Mutex::new(Vec::<Task>::new()));
        let posted = tasks.clone();
        let application =
            ApplicationHandle::new(move |task| posted.lock().unwrap().push(task), || {});
        let viewport = UiRect::new(0.0, 0.0, 500.0, 400.0);
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.explorer
                    .model
                    .restore(vec![fixture_root.clone()], Vec::new());
                app.apply(Action::OpenSearch);
                app.search.ui.query.set_text("needle");
                app
            }));
            *output.lock().unwrap() = Some(state.clone());
            let expected = key(&state.get());
            let application = application.clone();
            cx.use_effect(expected.clone(), move || {
                start(state, application, expected)
            });
            panel(viewport, VisualStyle::default())
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        session.runtime().run_effects();
        let state = exposed.lock().unwrap().clone().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while state.get().search.model.status().running && Instant::now() < deadline {
            for task in std::mem::take(&mut *tasks.lock().unwrap()) {
                task();
            }
            session.render_view(&view, viewport, UiScale::ONE);
            session.runtime().run_effects();
            std::thread::sleep(Duration::from_millis(10));
        }
        let result = state.get().search.model.status().results.clone();
        assert!(result.complete);
        assert_eq!(result.matches, 1);
        let generation = state.get().search.model.status().generation;
        for action in [Action::OpenExplorer, Action::OpenSearch] {
            state.update(|app| app.apply(action));
            session.render_view(&view, viewport, UiScale::ONE);
            session.runtime().run_effects();
            assert_eq!(state.get().search.model.status().generation, generation);
            assert!(Arc::ptr_eq(
                &state.get().search.model.status().results,
                &result
            ));
        }
        state.update(|app| {
            app.search.ui.filters_open = true;
            app.search.ui.details_open = true;
            app.search.ui.scroll_y = 20.0;
            app.source_control.visible = !app.source_control.visible;
            app.drawer.visible = false;
        });
        session.render_view(&view, viewport, UiScale::ONE);
        session.runtime().run_effects();
        state.update(|app| app.drawer.visible = true);
        session.render_view(&view, viewport, UiScale::ONE);
        session.runtime().run_effects();
        assert_eq!(state.get().search.model.status().generation, generation);
        assert!(Arc::ptr_eq(
            &state.get().search.model.status().results,
            &result
        ));
        refresh(&state);
        session.render_view(&view, viewport, UiScale::ONE);
        session.runtime().run_effects();
        assert_eq!(state.get().search.model.status().generation, generation + 1);
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn older_search_notifications_cannot_replace_new_query_and_navigation_uses_buffer() {
        use lgui::application::AppView;
        use lgui::core::UiScale;
        use lgui::prelude::{VisualStyle, panel};
        use lgui::session::UiSession;
        use std::sync::Mutex;
        use std::time::Instant;
        type Task = Box<dyn FnOnce() + Send>;
        let root = std::env::temp_dir().join(format!("loog-search-actions-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("document.txt");
        std::fs::write(&path, "old disk\n").unwrap();
        let fixture_root = root.clone();
        let fixture_path = path.clone();
        let exposed = Arc::new(Mutex::new(None::<(AppContext, UiFocusHandle)>));
        let output = exposed.clone();
        let viewport = UiRect::new(0.0, 0.0, 500.0, 400.0);
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.apply(Action::OpenSearch);
                app.explorer
                    .model
                    .restore(vec![fixture_root.clone()], Vec::new());
                app.workspace
                    .open_path(fixture_path.clone(), "old disk\n".into());
                app.workspace.active_editor_mut().unwrap().select_all();
                app.workspace
                    .active_editor_mut()
                    .unwrap()
                    .replace_selection("new 中文\r\n", crate::model::text::EditKind::Other)
                    .unwrap();
                app.search.ui.query.set_text("old");
                app
            }));
            *output.lock().unwrap() =
                Some((state, cx.focus_handle(lgui::core::UiId::new("test-editor"))));
            panel(viewport, VisualStyle::default())
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let (state, focus) = exposed.lock().unwrap().clone().unwrap();
        let tasks = Arc::new(Mutex::new(Vec::<Task>::new()));
        let posted = tasks.clone();
        let application =
            ApplicationHandle::new(move |task| posted.lock().unwrap().push(task), || {});
        let stop_old = start(state.clone(), application.clone(), key(&state.get()));
        let deadline = Instant::now() + Duration::from_secs(5);
        while tasks.lock().unwrap().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!tasks.lock().unwrap().is_empty());
        state.update(|app| app.search.ui.query.set_text("中文"));
        let stop_new = start(state.clone(), application.clone(), key(&state.get()));
        stop_old();
        let stale = std::mem::take(&mut *tasks.lock().unwrap());
        for task in stale {
            task();
        }
        assert_eq!(state.get().search.model.status().results.matches, 0);
        while state.get().search.model.status().running && Instant::now() < deadline {
            let pending = std::mem::take(&mut *tasks.lock().unwrap());
            for task in pending {
                task();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(state.get().search.model.status().results.matches, 1);
        state.update(|app| move_selection(app, 1));
        navigate_selected(&state, application, viewport, focus, false, false);
        while tasks.lock().unwrap().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let pending = std::mem::take(&mut *tasks.lock().unwrap());
        for task in pending {
            task();
        }
        let app = state.get();
        assert_eq!(
            app.workspace.active_editor().unwrap().selected_text(),
            Some("中文")
        );
        assert!(app.workspace.is_view_dirty(app.workspace.active().unwrap()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old disk\n");
        stop_new();
        let _ = std::fs::remove_dir_all(root);
    }
}
