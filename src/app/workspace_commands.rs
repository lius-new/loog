//! Shared workspace commands used by the welcome screen and editor chrome.
use crate::app::context::AppContext;

#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

#[cfg(test)]
use crate::infrastructure::document_filesystem::MAX_EDITABLE_FILE_BYTES;

use crate::app::state::{AppState, CloseContinuation, CloseRequest};
#[cfg(test)]
use crate::app::workspace_session::hydrate_editor_layout_with;
#[cfg(test)]
use crate::app::workspace_session::saved_editor_layout;
pub(crate) use crate::app::workspace_session::{
    hydrate_editor_layout, save_editor_session, saved_app_editor_layout,
};
use crate::domain::identity::{PaneId, ViewId};
#[cfg(test)]
use crate::domain::workspace_session::{EditorLayout, SavedAxis, SavedChild, SavedPaneNode};
use crate::model::document::FileId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabCloseScope {
    Tab,
    Others,
    Right,
    Saved,
    All,
}

pub fn save_active_document(state: &AppContext) -> bool {
    let Some(id) = state.get().workspace.active_document() else {
        return false;
    };
    save_document(state, id)
}

/// Save the active file without running save participants (the way out when
/// an extension keeps a file from being saved).
pub fn save_active_document_without_participants(state: &AppContext) -> bool {
    let Some(id) = state.get().workspace.active_document() else {
        return false;
    };
    crate::app::saving::document_without_participants(state, id, |_, _| {})
}

/// Returns whether the request was queued; completion is reported on the UI thread.
pub fn save_document(state: &AppContext, id: FileId) -> bool {
    crate::app::saving::document(state, id, |_, _| {})
}

pub fn request_close_tab(state: &AppContext, pane: PaneId, id: ViewId) {
    state.update(move |app| request_close_tab_in(app, pane, id));
}

pub(crate) fn request_close_tab_in(app: &mut AppState, pane: PaneId, id: ViewId) {
    request_close_tabs_in(app, vec![(pane, id)]);
}

/// Close every tab of a pane (asking about dirty files), which then closes
/// the pane itself; an already empty pane closes directly.
pub(crate) fn request_close_pane_in(app: &mut AppState, pane: PaneId) {
    let Some(items) = app.workspace.pane(pane).map(|p| p.items().to_vec()) else {
        return;
    };
    if items.is_empty() {
        app.workspace.close_pane(pane);
    } else {
        request_close_tabs_in(app, items.into_iter().map(|id| (pane, id)).collect());
    }
}

pub fn tab_close_targets(
    app: &AppState,
    pane: PaneId,
    target: ViewId,
    scope: TabCloseScope,
) -> Vec<(PaneId, ViewId)> {
    let Some(open) = app.workspace.pane(pane).map(|pane| pane.items()) else {
        return Vec::new();
    };
    let Some(target_index) = open.iter().position(|id| *id == target) else {
        return Vec::new();
    };
    let ids: Vec<ViewId> = match scope {
        TabCloseScope::Tab => vec![target],
        TabCloseScope::Others => open.iter().copied().filter(|id| *id != target).collect(),
        TabCloseScope::Right => open.iter().copied().skip(target_index + 1).collect(),
        TabCloseScope::Saved => open
            .iter()
            .copied()
            .filter(|id| !app.workspace.is_view_dirty(*id))
            .collect(),
        TabCloseScope::All => open.to_vec(),
    };
    ids.into_iter().map(|id| (pane, id)).collect()
}

pub fn request_close_tabs(state: &AppContext, pane: PaneId, target: ViewId, scope: TabCloseScope) {
    let targets = tab_close_targets(&state.get(), pane, target, scope);
    state.update(move |app| request_close_tabs_in(app, targets));
}

fn request_close_tabs_in(app: &mut AppState, targets: Vec<(PaneId, ViewId)>) {
    if app.editor.saving.closing {
        return;
    }
    let mut existing = Vec::with_capacity(targets.len());
    for (pane, id) in targets {
        if app.workspace.pane(pane).is_some_and(|p| p.contains(id))
            && !existing.contains(&(pane, id))
        {
            existing.push((pane, id));
        }
    }
    if existing.is_empty() {
        return;
    }
    app.editor.tabs.tab_context_menu = None;
    app.editor.tabs.tab_drag = None;
    app.editor.menu = None;
    // A dirty document only needs a decision when its last tab closes.
    if app
        .workspace
        .released_by(&existing)
        .into_iter()
        .any(|id| app.workspace.is_dirty(id))
    {
        app.editor.close_request = Some(CloseRequest {
            targets: existing,
            continuation: CloseContinuation::CloseTabs,
        });
    } else {
        for (pane, id) in existing {
            app.workspace.close_item(pane, id);
        }
    }
}

pub fn cancel_close_request(state: &AppContext) {
    if state.get().editor.saving.closing {
        return;
    }
    state.update(cancel_close_request_in);
}

fn cancel_close_request_in(app: &mut AppState) {
    app.editor.close_request = None;
}

pub fn discard_close_request(state: &AppContext) -> Option<CloseContinuation> {
    let continuation = state.get().editor.close_request.as_ref()?.continuation;
    state.update(|app| {
        let _ = discard_close_request_in(app);
    });
    Some(continuation)
}

pub(super) fn discard_close_request_in(app: &mut AppState) -> Option<CloseContinuation> {
    let request = app.editor.close_request.take()?;
    if request.continuation == CloseContinuation::ExitApplication {
        for id in app.workspace.released_by(&request.targets) {
            if let Some(doc) = app.workspace.documents.get_mut(&id) {
                crate::domain::workspace::replace_buffer_from_disk(
                    doc,
                    doc.saved_text.as_ref().clone(),
                );
                doc.buffer.initialize_metadata(doc.saved_format.code());
                doc.recovery = None;
            }
        }
    }
    if request.continuation == CloseContinuation::CloseTabs {
        for (pane, id) in request.targets {
            app.workspace.close_item(pane, id);
        }
    }
    Some(request.continuation)
}

pub fn save_close_request(
    state: &AppContext,
    done: impl FnOnce(&AppContext, CloseContinuation) + Send + 'static,
) -> bool {
    crate::app::saving::close(state, true, done)
}

pub(crate) fn discard_close_request_after_saves(
    state: &AppContext,
    done: impl FnOnce(&AppContext, CloseContinuation) + Send + 'static,
) -> bool {
    crate::app::saving::close(state, false, done)
}

#[cfg(test)]
pub(crate) fn dirty_test_file(
    app: &mut AppState,
    path: PathBuf,
    original: &str,
    change: &str,
) -> ViewId {
    fs::write(&path, original).unwrap();
    let id = app.workspace.test_open_view(path, original.to_owned());
    app.workspace
        .active_editor_mut()
        .unwrap()
        .select_to(usize::MAX, false);
    app.workspace
        .active_editor_mut()
        .unwrap()
        .replace_selection(change, crate::model::text::EditKind::Typing)
        .unwrap();
    id
}

#[cfg(test)]
pub(crate) fn close_test_directory(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "loog-close-{name}-{}-{}",
        std::process::id(),
        crate::domain::document::content_hash(name)
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

pub fn choose_file(state: &AppContext) -> bool {
    let Some(path) = crate::infrastructure::file_dialogs::pick_file(
        state.get().explorer.model.roots().first().cloned(),
    ) else {
        return false;
    };

    let mut opened = false;
    state.update(|app| {
        opened = crate::app::explorer::open_tree_file(
            app,
            path,
            2,
            state.services().and_then(|r| r.file_loader()).as_ref(),
        );
        app.chrome.welcome_hover = None;
        app.chrome.workspace_home_hover = None;
    });
    opened
}

pub(crate) use crate::infrastructure::document_filesystem::read_text_file;

#[cfg(test)]
mod tests {
    use super::*;
    fn read_all_but(missing: &'static str) -> impl FnMut(&Path) -> Result<String, String> {
        move |path| {
            if path == Path::new(missing) {
                Err("missing".to_owned())
            } else {
                Ok(format!("// {}", path.display()))
            }
        }
    }

    use crate::model::text::EditKind;

    #[test]
    fn text_file_size_boundary_is_inclusive() {
        let root = close_test_directory("text-file-size-boundary");
        let path = root.join("large.txt");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_EDITABLE_FILE_BYTES).unwrap();
        drop(file);
        fs::write(&path, vec![b'a'; MAX_EDITABLE_FILE_BYTES as usize]).unwrap();
        assert_eq!(
            read_text_file(&path).unwrap().len() as u64,
            MAX_EDITABLE_FILE_BYTES
        );
        let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(MAX_EDITABLE_FILE_BYTES + 1).unwrap();
        drop(file);
        assert_eq!(
            read_text_file(&path).unwrap_err(),
            format!(
                "Could not open {}: file is larger than 64 MiB",
                path.display()
            )
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn text_file_displays_malformed_encoding_and_rejects_directories() {
        let root = close_test_directory("invalid-text-file");
        let path = root.join("invalid.txt");
        fs::write(&path, [0xff, 0xfe, 0x00, 0xd8]).unwrap();
        assert_eq!(read_text_file(&path).unwrap(), "\u{fffd}");
        assert!(read_text_file(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    fn tabs(app: &AppState, ids: &[ViewId]) -> Vec<(PaneId, ViewId)> {
        let pane = app.workspace.active_pane();
        ids.iter().map(|id| (pane, *id)).collect()
    }

    fn close_tab(app: &mut AppState, id: ViewId) {
        let pane = app.workspace.active_pane();
        request_close_tab_in(app, pane, id);
    }

    #[test]
    fn clean_tab_closes_without_confirmation() {
        let mut app = AppState::new();
        let id = app
            .workspace
            .test_open_view(PathBuf::from("workspace/clean.rs"), String::new());

        close_tab(&mut app, id);

        assert!(app.workspace.view_meta(id).is_none());
        assert!(app.editor.close_request.is_none());
    }

    #[test]
    fn closing_one_of_two_tabs_of_a_dirty_document_needs_no_confirmation() {
        let mut app = AppState::new();
        let left = app.workspace.active_pane();
        let id = app
            .workspace
            .test_open_view(PathBuf::from("workspace/shared.rs"), String::new());
        let right = app
            .workspace
            .split(left, crate::model::pane_layout::Direction::Right)
            .unwrap();
        app.workspace
            .active_editor_mut()
            .unwrap()
            .replace_selection("changed", EditKind::Typing)
            .unwrap();

        let copy = app.workspace.pane(right).unwrap().active().unwrap();
        request_close_tab_in(&mut app, right, copy);
        assert!(app.editor.close_request.is_none());
        assert!(app.workspace.is_view_dirty(id));

        request_close_tab_in(&mut app, left, id);
        assert_eq!(
            app.editor.close_request.map(|request| request.targets),
            Some(vec![(left, id)])
        );
    }

    #[test]
    fn dirty_tab_waits_for_confirmation() {
        let mut app = AppState::new();
        let id = app
            .workspace
            .test_open_view(PathBuf::from("workspace/dirty.rs"), String::new());
        app.workspace
            .active_editor_mut()
            .unwrap()
            .replace_selection("changed", EditKind::Typing)
            .unwrap();

        close_tab(&mut app, id);

        assert!(app.workspace.view_meta(id).is_some());
        assert_eq!(
            app.editor.close_request,
            Some(CloseRequest {
                targets: tabs(&app, &[id]),
                continuation: CloseContinuation::CloseTabs,
            })
        );
    }

    #[test]
    fn keyboard_close_uses_the_guarded_request_for_the_active_tab() {
        use crate::features::keymap::action::Action;

        let mut app = AppState::new();
        let clean = app
            .workspace
            .test_open_view(PathBuf::from("workspace/clean.rs"), String::new());
        let dirty = app
            .workspace
            .test_open_view(PathBuf::from("workspace/dirty.rs"), String::new());
        app.workspace
            .active_editor_mut()
            .unwrap()
            .replace_selection("changed", EditKind::Typing)
            .unwrap();

        app.apply(Action::CloseActiveItem);
        assert!(app.workspace.view_meta(dirty).is_some());
        assert_eq!(
            app.editor
                .close_request
                .as_ref()
                .map(|request| request.targets.clone()),
            Some(tabs(&app, &[dirty]))
        );

        // A pending request is never replaced by another keyboard close.
        app.workspace.set_active(clean);
        app.apply(Action::CloseActiveItem);
        assert!(app.workspace.view_meta(clean).is_some());
        assert_eq!(
            app.editor
                .close_request
                .as_ref()
                .map(|request| request.targets.clone()),
            Some(tabs(&app, &[dirty]))
        );

        app.editor.close_request = None;
        app.apply(Action::CloseActiveItem);
        assert!(app.workspace.view_meta(clean).is_none());
        assert!(app.editor.close_request.is_none());
    }

    #[test]
    fn tab_close_scopes_follow_visible_order_and_dirty_state() {
        let mut app = AppState::new();
        let first = app
            .workspace
            .test_open_view(PathBuf::from("first.rs"), String::new());
        let second = app
            .workspace
            .test_open_view(PathBuf::from("second.rs"), String::new());
        app.workspace
            .active_editor_mut()
            .unwrap()
            .replace_selection("dirty", EditKind::Typing)
            .unwrap();
        let third = app
            .workspace
            .test_open_view(PathBuf::from("third.rs"), String::new());
        let fourth = app
            .workspace
            .test_open_view(PathBuf::from("fourth.rs"), String::new());

        assert_eq!(
            tab_close_targets(
                &app,
                app.workspace.active_pane(),
                second,
                TabCloseScope::Tab
            ),
            tabs(&app, &[second])
        );
        assert_eq!(
            tab_close_targets(
                &app,
                app.workspace.active_pane(),
                second,
                TabCloseScope::Others
            ),
            tabs(&app, &[first, third, fourth])
        );
        assert_eq!(
            tab_close_targets(
                &app,
                app.workspace.active_pane(),
                second,
                TabCloseScope::Right
            ),
            tabs(&app, &[third, fourth])
        );
        assert_eq!(
            tab_close_targets(
                &app,
                app.workspace.active_pane(),
                second,
                TabCloseScope::Saved
            ),
            tabs(&app, &[first, third, fourth])
        );
        assert_eq!(
            tab_close_targets(
                &app,
                app.workspace.active_pane(),
                second,
                TabCloseScope::All
            ),
            tabs(&app, &[first, second, third, fourth])
        );
    }

    #[test]
    fn mixed_batch_waits_for_dirty_review_before_closing_clean_tabs() {
        let mut app = AppState::new();
        let clean = app
            .workspace
            .test_open_view(PathBuf::from("clean.rs"), String::new());
        let dirty = app
            .workspace
            .test_open_view(PathBuf::from("dirty.rs"), String::new());
        app.workspace
            .active_editor_mut()
            .unwrap()
            .replace_selection("dirty", EditKind::Typing)
            .unwrap();

        let targets = tabs(&app, &[clean, dirty]);
        request_close_tabs_in(&mut app, targets);

        assert!(app.workspace.view_meta(clean).is_some());
        assert!(app.workspace.view_meta(dirty).is_some());
        assert_eq!(
            app.editor.close_request,
            Some(CloseRequest {
                targets: tabs(&app, &[clean, dirty]),
                continuation: CloseContinuation::CloseTabs,
            })
        );
    }

    #[test]
    fn saving_for_application_exit_preserves_tabs_for_the_final_snapshot() {
        let root = close_test_directory("save-all");
        let mut app = AppState::new();
        let first_path = root.join("first.txt");
        let second_path = root.join("second.txt");
        let first = dirty_test_file(&mut app, first_path.clone(), "first", " updated");
        let second = dirty_test_file(&mut app, second_path.clone(), "second", " updated");
        app.editor.close_request = Some(CloseRequest {
            targets: tabs(&app, &[first, second]),
            continuation: CloseContinuation::ExitApplication,
        });

        let saving = super::super::test_support::SavingUi::new(app);
        let completed = std::sync::Arc::new(std::sync::Mutex::new(None));
        let result = completed.clone();
        assert!(save_close_request(&saving.state, move |_, next| *result
            .lock()
            .unwrap() =
            Some(next)));
        saving.wait();
        let app = saving.state.get();

        assert_eq!(
            *completed.lock().unwrap(),
            Some(CloseContinuation::ExitApplication)
        );
        assert!(app.workspace.view_meta(first).is_some());
        assert!(app.workspace.view_meta(second).is_some());
        assert!(!app.workspace.is_view_dirty(first));
        assert!(!app.workspace.is_view_dirty(second));
        let saved = saved_app_editor_layout(&app).unwrap();
        let SavedPaneNode::Pane { items, active } = saved.root else {
            panic!("one pane")
        };
        assert_eq!(
            items
                .iter()
                .map(|v| v.path().to_path_buf())
                .collect::<Vec<_>>(),
            vec![first_path.clone(), second_path.clone()]
        );
        assert_eq!(active, Some(1));
        let cursors = items
            .iter()
            .map(|item| match item {
                crate::domain::workspace_session::SavedView::Text { source, .. } => source.cursor,
                crate::domain::workspace_session::SavedView::Resource { source, .. } => {
                    source.cursor
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(cursors, vec![13, 14]);
        assert!(app.editor.close_request.is_none());
        assert_eq!(fs::read_to_string(first_path).unwrap(), "first updated");
        assert_eq!(fs::read_to_string(second_path).unwrap(), "second updated");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_batch_save_keeps_every_tab_open() {
        let root = close_test_directory("partial-failure");
        let mut app = AppState::new();
        let first_path = root.join("first.txt");
        let first = dirty_test_file(&mut app, first_path.clone(), "first", " updated");
        let missing_path = root.join("missing-parent").join("second.txt");
        let second = app
            .workspace
            .test_open_view(missing_path, "second".to_owned());
        app.workspace
            .active_editor_mut()
            .unwrap()
            .select_to(usize::MAX, false);
        app.workspace
            .active_editor_mut()
            .unwrap()
            .replace_selection(" updated", EditKind::Typing)
            .unwrap();
        app.editor.close_request = Some(CloseRequest {
            targets: tabs(&app, &[first, second]),
            continuation: CloseContinuation::CloseTabs,
        });

        let saving = super::super::test_support::SavingUi::new(app);
        assert!(save_close_request(&saving.state, |_, _| panic!(
            "failed save must not close"
        )));
        saving.wait();
        let app = saving.state.get();

        assert!(
            app.chrome
                .toast
                .as_ref()
                .unwrap()
                .message
                .contains("Could not save")
        );
        assert!(app.workspace.view_meta(first).is_some());
        assert!(app.workspace.view_meta(second).is_some());
        assert!(!app.workspace.is_view_dirty(first));
        assert!(app.workspace.is_view_dirty(second));
        assert!(app.editor.close_request.is_some());
        assert_eq!(fs::read_to_string(first_path).unwrap(), "first updated");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn discarding_multiple_dirty_tabs_closes_every_target() {
        let mut app = AppState::new();
        let first = app
            .workspace
            .test_open_view(PathBuf::from("first.txt"), String::new());
        app.workspace
            .active_editor_mut()
            .unwrap()
            .replace_selection("first", EditKind::Typing)
            .unwrap();
        let second = app
            .workspace
            .test_open_view(PathBuf::from("second.txt"), String::new());
        app.workspace
            .active_editor_mut()
            .unwrap()
            .replace_selection("second", EditKind::Typing)
            .unwrap();
        app.editor.close_request = Some(CloseRequest {
            targets: tabs(&app, &[first, second]),
            continuation: CloseContinuation::CloseTabs,
        });

        let result = discard_close_request_in(&mut app);

        assert_eq!(result, Some(CloseContinuation::CloseTabs));
        assert!(app.workspace.view_meta(first).is_none());
        assert!(app.workspace.view_meta(second).is_none());
        assert!(app.editor.close_request.is_none());
    }

    #[test]
    fn cancelling_batch_close_preserves_dirty_tabs() {
        let mut app = AppState::new();
        let id = app
            .workspace
            .test_open_view(PathBuf::from("dirty.txt"), String::new());
        app.workspace
            .active_editor_mut()
            .unwrap()
            .replace_selection("changed", EditKind::Typing)
            .unwrap();
        app.editor.close_request = Some(CloseRequest {
            targets: tabs(&app, &[id]),
            continuation: CloseContinuation::ExitApplication,
        });

        cancel_close_request_in(&mut app);

        assert!(app.workspace.view_meta(id).is_some());
        assert!(app.workspace.is_view_dirty(id));
        assert!(app.editor.close_request.is_none());
    }

    #[test]
    fn restores_file_tabs_in_order_and_reactivates_the_saved_file() {
        let mut app = AppState::new();
        let first = PathBuf::from("first.rs");
        let second = PathBuf::from("second.rs");
        let layout = EditorLayout {
            root: SavedPaneNode::Pane {
                items: vec![
                    first.clone().into(),
                    PathBuf::from("missing.rs").into(),
                    second.clone().into(),
                ],
                active: Some(0),
            },
            active_pane: 0,
        };

        hydrate_editor_layout_with(&mut app, Some(layout), read_all_but("missing.rs"));

        assert_eq!(app.workspace.open_paths(), vec![first.clone(), second]);
        assert_eq!(app.workspace.active_items().len(), 3);
        let failed = app
            .workspace
            .file_id_for_path(Path::new("missing.rs"))
            .unwrap();
        assert!(app.workspace.open_error(failed).is_some());
        assert_eq!(app.workspace.active_path(), Some(first.as_path()));
    }

    #[test]
    fn unclosed_single_click_preview_restores_in_order_and_closed_tabs_stay_closed() {
        let mut app = AppState::new();
        app.workspace
            .test_open_view("first.rs".into(), String::new());
        let closed = app
            .workspace
            .test_open_view("closed.rs".into(), String::new());
        app.workspace.test_close_document(closed);
        app.workspace
            .test_preview_view("preview.json".into(), String::new());
        let saved = saved_app_editor_layout(&app).unwrap();
        assert_eq!(
            saved.root,
            SavedPaneNode::Pane {
                items: vec!["first.rs".into(), "preview.json".into()],
                active: Some(1)
            }
        );
        let serialized = serde_json::to_string(&saved).unwrap();
        let mut restored = AppState::new();
        hydrate_editor_layout_with(
            &mut restored,
            Some(serde_json::from_str(&serialized).unwrap()),
            read_all_but(""),
        );
        assert_eq!(
            restored.workspace.active_path(),
            Some(Path::new("preview.json"))
        );
        assert!(
            restored
                .workspace
                .test_view_for_path(Path::new("closed.rs"))
                .is_none()
        );
        assert_eq!(saved_app_editor_layout(&restored), Some(saved));
    }

    #[test]
    fn dont_save_on_application_exit_keeps_tabs_and_reopens_disk_contents() {
        let root = close_test_directory("discard-on-exit");
        let file = root.join("dirty.txt");
        let mut app = AppState::new();
        let id = dirty_test_file(&mut app, file.clone(), "disk", " modified");
        app.editor.close_request = Some(CloseRequest {
            targets: tabs(&app, &[id]),
            continuation: CloseContinuation::ExitApplication,
        });
        assert_eq!(
            discard_close_request_in(&mut app),
            Some(CloseContinuation::ExitApplication)
        );
        assert!(app.workspace.view_meta(id).is_some());
        let layout = saved_app_editor_layout(&app);
        let mut restored = AppState::new();
        hydrate_editor_layout_with(&mut restored, layout, read_text_file);
        assert_eq!(restored.workspace.active_path(), Some(file.as_path()));
        assert_eq!(restored.workspace.active_editor().unwrap().text(), "disk");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn split_layout_round_trips_through_the_session_format() {
        let mut app = AppState::new();
        let left = app.workspace.active_pane();
        app.workspace
            .test_open_view(PathBuf::from("a.rs"), "a".into());
        app.workspace
            .test_open_view(PathBuf::from("b.rs"), "b".into());
        let right = app
            .workspace
            .split(left, crate::model::pane_layout::Direction::Right)
            .unwrap();
        app.workspace
            .test_preview_view(PathBuf::from("preview.rs"), String::new());
        app.workspace.open_settings();
        app.workspace
            .split(right, crate::model::pane_layout::Direction::Down)
            .unwrap();
        app.workspace
            .test_open_view(PathBuf::from("c.rs"), "c".into());
        app.workspace.activate_pane(right);

        let saved = saved_editor_layout(&app.workspace).unwrap();
        // The settings page is excluded; the open preview is restored too.
        assert_eq!(
            saved.root,
            SavedPaneNode::Split {
                axis: SavedAxis::Horizontal,
                children: vec![
                    SavedChild {
                        size: 0.5,
                        node: SavedPaneNode::Pane {
                            items: vec![PathBuf::from("a.rs").into(), PathBuf::from("b.rs").into()],
                            active: Some(1),
                        },
                    },
                    SavedChild {
                        size: 0.5,
                        node: SavedPaneNode::Split {
                            axis: SavedAxis::Vertical,
                            children: vec![
                                SavedChild {
                                    size: 0.5,
                                    node: SavedPaneNode::Pane {
                                        items: vec![
                                            PathBuf::from("b.rs").into(),
                                            PathBuf::from("preview.rs").into()
                                        ],
                                        active: None,
                                    },
                                },
                                SavedChild {
                                    size: 0.5,
                                    node: SavedPaneNode::Pane {
                                        items: vec![PathBuf::from("c.rs").into()],
                                        active: Some(0),
                                    },
                                },
                            ],
                        },
                    },
                ],
            }
        );
        assert_eq!(saved.active_pane, 1);

        let mut restored = AppState::new();
        hydrate_editor_layout_with(&mut restored, Some(saved.clone()), read_all_but(""));
        assert_eq!(restored.workspace.pane_count(), 3);
        let resaved = saved_editor_layout(&restored.workspace).unwrap();
        assert_eq!(resaved.active_pane, 1);
        assert_eq!(
            restored.workspace.open_paths(),
            vec![
                PathBuf::from("a.rs"),
                PathBuf::from("b.rs"),
                PathBuf::from("preview.rs"),
                PathBuf::from("c.rs")
            ]
        );
        // Without a saved active tab, the restored pane activates its first tab.
        let SavedPaneNode::Split { children, .. } = resaved.root else {
            panic!("expected a split")
        };
        assert_eq!(children.len(), 2);
    }

    #[test]
    fn panes_whose_files_are_gone_keep_their_failure_tabs_on_restore() {
        let mut app = AppState::new();
        let layout = EditorLayout {
            root: SavedPaneNode::Split {
                axis: SavedAxis::Horizontal,
                children: vec![
                    SavedChild {
                        size: 0.3,
                        node: SavedPaneNode::Pane {
                            items: vec![PathBuf::from("gone.rs").into()],
                            active: None,
                        },
                    },
                    SavedChild {
                        size: 0.7,
                        node: SavedPaneNode::Pane {
                            items: vec![PathBuf::from("kept.rs").into()],
                            active: None,
                        },
                    },
                ],
            },
            active_pane: 0,
        };

        hydrate_editor_layout_with(&mut app, Some(layout), read_all_but("gone.rs"));

        assert_eq!(app.workspace.pane_count(), 2);
        assert_eq!(app.workspace.open_paths(), vec![PathBuf::from("kept.rs")]);
        assert_eq!(app.workspace.active_path(), Some(Path::new("gone.rs")));
        assert!(
            app.workspace
                .pane_open_error(app.workspace.active_pane())
                .is_some()
        );
        assert!(app.chrome.toast.is_none());
    }
}
