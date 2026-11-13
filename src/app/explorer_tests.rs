//! Explorer command transactions across Workspace and the Explorer model.
use crate::app::context::AppContext;
use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::app::workspace_commands::{close_test_directory, dirty_test_file};

#[test]
fn text_file_above_four_mib_opens_in_explorer() {
    let root = close_test_directory("large-text-file");
    let path = root.join("seed.database.json");
    let contents = format!("[\n{}null\n]\n", "\"中文😀\",\n".repeat(400_000));
    assert!(contents.len() > 4 * 1024 * 1024);
    fs::write(&path, &contents).unwrap();
    assert_eq!(read_text_file(&path).unwrap(), contents);

    let mut app = AppState::new();
    assert!(open_tree_file(&mut app, path.clone(), 2, None));
    assert_eq!(app.workspace.active_path(), Some(path.as_path()));
    let id = app.workspace.active().unwrap();
    assert_eq!(
        app.workspace
            .save_snapshot(app.workspace.test_document(id))
            .unwrap()
            .2,
        contents
    );
    assert!(app.chrome.toast.is_none());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn launch_paths_open_the_first_folder_add_others_and_open_files() {
    let root = close_test_directory("launch-paths");
    let first = root.join("first");
    let second = root.join("second");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();
    let file = first.join("main.rs");
    let mut app = AppState::new();
    app.explorer
        .model
        .restore(vec![PathBuf::from("previous-workspace")], Vec::new());

    let changed = open_launch_paths_with(
        &mut app,
        vec![first.clone(), file.clone(), second.clone()],
        |_| Ok("fn main() {}".to_owned()),
    );

    assert!(changed);
    assert_eq!(
        app.explorer.model.roots(),
        vec![normalized_folder(first), normalized_folder(second)]
    );
    let active = app.workspace.active().unwrap();
    assert_eq!(
        app.workspace.view_meta(active).unwrap().local_path(),
        Some(file.as_path())
    );
    assert!(app.chrome.toast.is_none());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn unreadable_launch_files_keep_error_tabs_without_a_toast() {
    let mut app = AppState::new();
    app.explorer
        .model
        .restore(vec![PathBuf::from("previous-workspace")], Vec::new());

    let changed = open_launch_paths_with(
        &mut app,
        vec![PathBuf::from("missing-a.rs"), PathBuf::from("missing-b.rs")],
        |path| Err(format!("Could not open {}", path.display())),
    );

    assert!(!changed);
    assert_eq!(
        app.explorer.model.roots(),
        vec![PathBuf::from("previous-workspace")]
    );
    assert_eq!(app.workspace.active_items().len(), 2);
    let pane = app.workspace.active_pane();
    assert!(
        app.workspace
            .pane_open_error(pane)
            .unwrap()
            .contains("missing-b.rs")
    );
    assert!(app.workspace.active_editor().is_none());
    assert!(app.chrome.toast.is_none());
}

#[test]
fn adding_a_folder_preserves_existing_roots_and_ignores_duplicates() {
    let mut app = AppState::new();
    let first = PathBuf::from("first-workspace");
    let second = PathBuf::from("second-workspace");

    replace_folder_state(&mut app, first.clone());
    assert!(add_folder_state(&mut app, second.clone()));
    assert!(!add_folder_state(&mut app, first.clone()));

    assert_eq!(
        app.explorer.model.roots(),
        vec![first.clone(), second.clone()]
    );
    assert!(app.explorer.model.is_expanded(&first.to_string_lossy()));
    assert!(app.explorer.model.is_expanded(&second.to_string_lossy()));
    assert_eq!(app.explorer.model.recents(), vec![first.clone(), second]);
}

#[test]
fn opening_a_folder_replaces_the_current_project_roots() {
    let mut app = AppState::new();
    let first = PathBuf::from("first-workspace");
    let second = PathBuf::from("second-workspace");

    add_folder_state(&mut app, first);
    replace_folder_state(&mut app, second.clone());

    assert_eq!(app.explorer.model.roots(), vec![second]);
}

#[test]
fn removing_a_workspace_folder_keeps_disk_documents_and_clears_tree_state() {
    let mut app = AppState::new();
    let removed = PathBuf::from("first-workspace");
    let child = removed.join("src");
    let kept = PathBuf::from("second-workspace");
    let open_file = child.join("main.rs");
    app.explorer.model.restore(
        vec![removed.clone(), kept.clone()],
        vec![removed.clone(), kept.clone()],
    );
    app.explorer
        .model
        .insert_listing(removed.to_string_lossy().into_owned(), Vec::new().into());
    app.explorer
        .model
        .insert_listing(child.to_string_lossy().into_owned(), Vec::new().into());
    app.explorer
        .model
        .insert_listing(kept.to_string_lossy().into_owned(), Vec::new().into());
    for path in [&removed, &child, &kept] {
        app.explorer
            .model
            .set_expanded(&path.to_string_lossy(), true);
    }
    app.explorer.ui.hovered_path = Some(open_file.to_string_lossy().into_owned());
    app.explorer
        .model
        .begin_create(child.clone(), ExplorerCreateKind::File);
    app.workspace
        .test_open_view(open_file.clone(), String::new());

    assert!(remove_folder_state(&mut app, &removed));

    assert_eq!(app.explorer.model.roots(), vec![kept.clone()]);
    assert_eq!(
        app.explorer.model.recents(),
        vec![removed.clone(), kept.clone()]
    );
    assert!(
        app.explorer
            .model
            .listings()
            .all(|(path, _)| !Path::new(path).starts_with(&removed))
    );
    for path in [&removed, &child] {
        assert!(!app.explorer.model.is_expanded(&path.to_string_lossy()));
    }
    assert!(
        app.explorer
            .model
            .listing(&kept.to_string_lossy())
            .is_some()
    );
    assert!(app.explorer.model.is_expanded(&kept.to_string_lossy()));
    assert!(app.explorer.ui.hovered_path.is_none());
    assert!(app.explorer.model.create_request().is_none());
    assert_eq!(app.workspace.active_path(), Some(open_file.as_path()));
}

fn press_file(app: &mut AppState, path: &str, clicks: u8, drop: Option<TabDrop>) {
    app.editor.tabs.file_drag = Some(crate::app::state::FileDragState {
        path: PathBuf::from(path),
        source: crate::app::state::FileDragSource::Explorer { clicks },
        origin: (0.0, 0.0),
        point: (0.0, 0.0),
        active: drop.is_some(),
        drop,
    });
}

fn read_ok(path: &Path) -> Result<String, String> {
    Ok(format!("// {}", path.display()))
}

#[test]
fn releasing_an_undragged_file_opens_it_like_a_click() {
    let mut app = AppState::new();
    press_file(&mut app, "a.rs", 1, None);
    assert!(finish_file_drag_with(&mut app, read_ok));
    assert!(app.editor.tabs.file_drag.is_none());
    let id = app.workspace.active().unwrap();
    assert_eq!(app.workspace.preview(), Some(id));

    press_file(&mut app, "a.rs", 2, None);
    finish_file_drag_with(&mut app, read_ok);
    assert_eq!(app.workspace.preview(), None);
    assert_eq!(app.workspace.active_items(), &[id]);
}

#[test]
fn dropping_a_file_on_a_strip_inserts_it_at_the_marker() {
    let mut app = AppState::new();
    let pane = app.workspace.active_pane();
    let first = app
        .workspace
        .test_open_view(PathBuf::from("first.rs"), String::new());
    let second = app
        .workspace
        .test_open_view(PathBuf::from("second.rs"), String::new());
    press_file(
        &mut app,
        "new.rs",
        1,
        Some(TabDrop::Insert { pane, index: 1 }),
    );

    assert!(finish_file_drag_with(&mut app, read_ok));

    let new = app.workspace.active().unwrap();
    assert_eq!(app.workspace.active_items(), &[first, new, second]);
    // A dragged file opens as a permanent tab.
    assert_eq!(app.workspace.preview(), None);
}

#[test]
fn dropping_a_file_near_a_pane_edge_opens_it_in_a_new_split() {
    let mut app = AppState::new();
    let pane = app.workspace.active_pane();
    let first = app
        .workspace
        .test_open_view(PathBuf::from("first.rs"), String::new());
    press_file(
        &mut app,
        "new.rs",
        1,
        Some(TabDrop::Pane {
            pane,
            split: Some(crate::model::pane_layout::Direction::Down),
        }),
    );

    assert!(finish_file_drag_with(&mut app, read_ok));

    let panes = app.workspace.pane_ids();
    assert_eq!(panes.len(), 2);
    assert_eq!(app.workspace.pane(panes[0]).unwrap().items(), &[first]);
    assert_eq!(app.workspace.active_pane(), panes[1]);
    assert_eq!(app.workspace.active_path(), Some(Path::new("new.rs")));
}

#[test]
fn dropping_a_file_on_another_pane_keeps_failures_in_that_pane() {
    let mut app = AppState::new();
    let left = app.workspace.active_pane();
    app.workspace
        .test_open_view(PathBuf::from("first.rs"), String::new());
    let right = app
        .workspace
        .split(left, crate::model::pane_layout::Direction::Right)
        .unwrap();
    app.workspace.activate_pane(left);
    let drop = Some(TabDrop::Pane {
        pane: right,
        split: None,
    });

    press_file(&mut app, "gone.rs", 1, drop);
    assert!(finish_file_drag_with(&mut app, |_| Err("gone".to_owned())));
    assert_eq!(app.workspace.pane_count(), 2);
    assert_eq!(app.workspace.active_pane(), right);
    assert_eq!(app.workspace.pane_open_error(right), Some("gone"));
    assert!(app.chrome.toast.is_none());

    press_file(&mut app, "new.rs", 1, drop);
    assert!(finish_file_drag_with(&mut app, read_ok));
    assert_eq!(app.workspace.active_pane(), right);
    assert_eq!(app.workspace.active_path(), Some(Path::new("new.rs")));
}

#[test]
fn renamed_directory_retargets_tabs_and_rebuilds_tree_state() {
    let mut app = AppState::new();
    let root = PathBuf::from("workspace");
    let old_path = root.join("src");
    let new_path = root.join("source");
    let old_file = old_path.join("main.rs");
    let new_file = new_path.join("main.rs");
    app.explorer.model.restore(vec![root], Vec::new());
    app.explorer
        .model
        .insert_listing(old_path.to_string_lossy().into_owned(), Vec::new().into());
    app.explorer
        .model
        .set_expanded(&old_path.to_string_lossy(), true);
    app.explorer
        .model
        .begin_rename(old_path.clone(), ExplorerTargetKind::Directory);
    app.workspace.test_open_view(old_file, String::new());

    apply_renamed_path_state(&mut app, &old_path, &new_path);

    assert_eq!(app.workspace.active_path(), Some(new_file.as_path()));
    assert!(
        app.explorer
            .model
            .listings()
            .all(|(path, _)| !Path::new(path).starts_with(&old_path))
    );
    assert!(!app.explorer.model.is_expanded(&old_path.to_string_lossy()));
    assert!(app.explorer.model.is_expanded(&new_path.to_string_lossy()));
    assert!(app.explorer.model.rename_request().is_none());
}

#[test]
fn renamed_file_retargets_its_open_editor_tab() {
    let mut app = AppState::new();
    let root = PathBuf::from("workspace");
    let old_path = root.join("before.rs");
    let new_path = root.join("after.rs");
    app.explorer.model.restore(vec![root], Vec::new());
    app.workspace
        .test_open_view(old_path.clone(), String::new());

    apply_renamed_path_state(&mut app, &old_path, &new_path);

    assert_eq!(app.workspace.active_path(), Some(new_path.as_path()));
    assert_eq!(app.workspace.file_id_for_path(&old_path), None);
}

#[test]
fn deleted_file_closes_its_open_editor_tab() {
    let mut app = AppState::new();
    let root = PathBuf::from("workspace");
    let path = root.join("deleted.rs");
    app.explorer.model.restore(vec![root], Vec::new());
    app.workspace.test_open_view(path.clone(), String::new());

    apply_deleted_path_state(&mut app, &path);

    assert_eq!(app.workspace.file_id_for_path(&path), None);
    assert_eq!(app.workspace.active_path(), None);
}

/// A live root state, so commands that take `&AppContext` can run.
fn live(app: AppState) -> AppContext {
    use lgui::application::AppView;
    use lgui::core::UiScale;
    use lgui::prelude::{UiRect, VisualStyle, panel};
    use lgui::session::UiSession;
    use std::sync::{Arc, Mutex};

    let initial = Arc::new(Mutex::new(Some(app)));
    let exposed = Arc::new(Mutex::new(None));
    let output = exposed.clone();
    let view: AppView = Arc::new(move |cx| {
        let state = AppContext::detached(cx.state_with(|| initial.lock().unwrap().take().unwrap()));
        *output.lock().unwrap() = Some(state);
        panel(UiRect::new(0.0, 0.0, 10.0, 10.0), VisualStyle::default())
    });
    UiSession::new().render_view(&view, UiRect::new(0.0, 0.0, 10.0, 10.0), UiScale::ONE);
    exposed.lock().unwrap().take().unwrap()
}

fn key(path: &Path) -> String {
    crate::features::explorer::tree::key(path)
}

#[test]
fn deleting_a_folder_with_modified_documents_is_refused() {
    let root = close_test_directory("explorer-delete-dirty");
    let folder = root.join("src");
    fs::create_dir_all(&folder).unwrap();
    let mut app = AppState::new();
    add_folder_state(&mut app, root.clone());
    dirty_test_file(&mut app, folder.join("main.rs"), "disk", " edited");
    let state = live(app);

    delete_item(&state, folder.clone());

    assert!(folder.join("main.rs").is_file());
    let app = state.get();
    assert!(app.chrome.toast.is_some());
    assert_eq!(app.explorer.model.roots(), std::slice::from_ref(&root));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn renaming_keeps_unicode_names_open_tabs_and_listings_in_step() {
    let root = close_test_directory("explorer-rename-unicode");
    let old = root.join("旧名.rs");
    fs::write(&old, "fn main() {}").unwrap();
    let mut app = AppState::new();
    add_folder_state(&mut app, root.clone());
    app.workspace
        .test_open_view(old.clone(), "fn main() {}".into());
    let state = live(app);

    begin_rename(&state, old.clone(), ExplorerTargetKind::File);
    assert_eq!(state.get().explorer.ui.create_input.text(), "旧名.rs");
    state.update(|app| {
        app.explorer
            .ui
            .create_input
            .set_text("新名 🙂.rs".to_owned())
    });
    finish_rename(&state);

    let renamed = root.join("新名 🙂.rs");
    assert!(renamed.is_file() && !old.exists());
    let app = state.get();
    assert_eq!(app.workspace.active_path(), Some(renamed.as_path()));
    assert!(app.explorer.model.rename_request().is_none());
    assert!(app.explorer.ui.create_error.is_none());
    let names = app.explorer.model.listing(&key(&root)).unwrap();
    assert_eq!(names.len(), 1);
    assert_eq!(names[0].name, "新名 🙂.rs");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn invalid_rename_keeps_the_row_open_with_its_error() {
    let root = close_test_directory("explorer-rename-invalid");
    let path = root.join("keep.rs");
    fs::write(&path, "").unwrap();
    let mut app = AppState::new();
    add_folder_state(&mut app, root.clone());
    let state = live(app);

    begin_rename(&state, path.clone(), ExplorerTargetKind::File);
    state.update(|app| {
        app.explorer
            .ui
            .create_input
            .set_text("nested/name".to_owned())
    });
    finish_rename(&state);

    assert!(path.is_file());
    let app = state.get();
    assert!(app.explorer.model.rename_request().is_some());
    assert!(app.explorer.ui.create_error.is_some());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn creating_nested_items_expands_their_folders_and_opens_new_files() {
    let root = close_test_directory("explorer-create-nested");
    let mut app = AppState::new();
    add_folder_state(&mut app, root.clone());
    let state = live(app);

    begin_create(&state, root.clone(), ExplorerCreateKind::File);
    state.update(|app| {
        app.explorer
            .ui
            .create_input
            .set_text("笔记/日志.md".to_owned())
    });
    finish_create(&state);
    begin_create(&state, root.clone(), ExplorerCreateKind::File);
    state.update(|app| {
        app.explorer
            .ui
            .create_input
            .set_text("assets/icons/".to_owned())
    });
    finish_create(&state);

    let file = root.join("笔记").join("日志.md");
    assert!(file.is_file());
    assert!(root.join("assets").join("icons").is_dir());
    let app = state.get();
    assert_eq!(app.workspace.active_path(), Some(file.as_path()));
    for folder in [
        root.join("笔记"),
        root.join("assets"),
        root.join("assets").join("icons"),
    ] {
        assert!(app.explorer.model.is_expanded(&key(&folder)), "{folder:?}");
    }
    assert!(app.explorer.model.create_request().is_none());
    assert!(app.explorer.ui.create_input.text().is_empty());
    let _ = fs::remove_dir_all(root);
}

/// A workspace `root/a/b/file.rs` plus a sibling `root/z.rs`, opened in a tab.
fn nested_workspace(name: &str) -> (AppState, PathBuf, PathBuf) {
    let root = close_test_directory(name);
    let nested = root.join("a").join("b");
    fs::create_dir_all(&nested).unwrap();
    let file = nested.join("file.rs");
    fs::write(&file, "").unwrap();
    fs::write(root.join("z.rs"), "").unwrap();
    let mut app = AppState::new();
    app.explorer
        .model
        .open_root(&ExplorerFilesystem, root.clone());
    app.workspace.test_open_view(file.clone(), String::new());
    (app, root, file)
}

#[test]
fn auto_reveal_expands_folders_and_requests_the_row_without_opening_the_drawer() {
    let (mut app, root, file) = nested_workspace("auto-reveal-expands");
    let drawer_visible = app.drawer.visible;
    assert!(!app.explorer.model.is_expanded(&key(&root.join("a"))));

    auto_reveal_active(&mut app);

    assert!(app.explorer.model.is_expanded(&key(&root.join("a"))));
    assert!(
        app.explorer
            .model
            .is_expanded(&key(&root.join("a").join("b")))
    );
    // root, a, b, file.rs: the file is the fourth row; a/b/file.rs is 3 deep.
    assert_eq!(
        app.explorer.ui.reveal,
        Some(ExplorerReveal { row: 3, covered: 3 })
    );
    assert_eq!(app.drawer.visible, drawer_visible);
    assert!(app.chrome.toast.is_none());
    assert!(app.workspace.active_path() == Some(file.as_path()));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn auto_reveal_keeps_directories_the_user_already_opened() {
    let (mut app, root, _file) = nested_workspace("auto-reveal-keeps");
    app.explorer
        .model
        .toggle_directory(&ExplorerFilesystem, &key(&root.join("a")));
    let sibling = root.join("a").join("other");
    fs::create_dir_all(&sibling).unwrap();
    let a = key(&root.join("a"));
    app.explorer.model.toggle_directory(&ExplorerFilesystem, &a);
    app.explorer
        .model
        .toggle_directory(&ExplorerFilesystem, &key(&sibling));
    assert!(app.explorer.model.is_expanded(&key(&sibling)));

    auto_reveal_active(&mut app);

    // The path to the file opens; what the user opened elsewhere stays open.
    assert!(app.explorer.model.is_expanded(&a));
    assert!(app.explorer.model.is_expanded(&key(&sibling)));
    assert!(
        app.explorer
            .model
            .is_expanded(&key(&root.join("a").join("b")))
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn auto_reveal_is_skipped_when_off_editing_or_outside_the_workspace() {
    let (mut app, root, _file) = nested_workspace("auto-reveal-skipped");

    app.preferences.values_mut().explorer_auto_reveal = false;
    auto_reveal_active(&mut app);
    assert_eq!(app.explorer.ui.reveal, None);
    assert!(!app.explorer.model.is_expanded(&key(&root.join("a"))));

    app.preferences.values_mut().explorer_auto_reveal = true;
    app.explorer
        .model
        .begin_create(root.clone(), ExplorerCreateKind::File);
    auto_reveal_active(&mut app);
    assert_eq!(app.explorer.ui.reveal, None);
    app.explorer.model.cancel_edit();

    let outside = close_test_directory("auto-reveal-outside").join("x.rs");
    app.workspace.test_open_view(outside, String::new());
    auto_reveal_active(&mut app);
    assert_eq!(app.explorer.ui.reveal, None);
    assert!(app.chrome.toast.is_none(), "automatic reveal never toasts");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn toggling_a_directory_ends_a_pending_reveal() {
    let (mut app, root, _file) = nested_workspace("auto-reveal-toggle");
    auto_reveal_active(&mut app);
    assert!(app.explorer.ui.reveal.is_some());

    toggle_directory(&mut app, &key(&root.join("a").join("b")));

    assert_eq!(app.explorer.ui.reveal, None);
    fs::remove_dir_all(root).unwrap();
}
