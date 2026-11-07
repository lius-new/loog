use super::*;
use crate::features::git::diff::UnifiedDiff;
use crate::features::git::{DiffContent, DiffTarget};
use loog_editor_core::text::EditKind;

#[test]
fn preview_reuse_and_pane_moves_flush_retired_data_at_the_application_boundary() {
    let mut workspace = Workspace::new();
    let shared = workspace.test_open_view("shared.txt".into(), "shared".into());
    let left = workspace.active_pane();
    let right = workspace.split_empty(left, Direction::Right).unwrap();
    workspace.activate_pane(right);
    let old_preview = workspace.test_preview_view("preview.txt".into(), "preview".into());
    let right_shared = workspace.test_preview_view("shared.txt".into(), "ignored".into());
    assert_ne!(right_shared, shared);
    assert_eq!(
        workspace.test_document(right_shared),
        workspace.test_document(shared)
    );
    assert!(workspace.view_meta(old_preview).is_none());
    assert!(workspace.core.take_retired().is_empty());
    workspace.activate_pane(left);
    let moving = workspace.test_preview_view("moving.txt".into(), "moving".into());
    workspace.activate_pane(right);
    let replaced = workspace.test_preview_view("replaced.txt".into(), "replaced".into());
    workspace.activate_pane(left);
    assert!(workspace.move_active_item(Direction::Right));
    assert_eq!(workspace.pane(right).unwrap().active(), Some(moving));
    assert!(workspace.view_meta(replaced).is_none());
    assert!(workspace.core.take_retired().is_empty());
}
use std::sync::Arc;

#[test]
fn settings_tab_is_unique_and_never_treated_as_a_file() {
    let mut workspace = Workspace::new();
    let file = workspace.test_open_view(PathBuf::from("src/main.rs"), "fn main() {}".into());
    let settings = workspace.open_settings();
    workspace.set_active(file);
    assert_eq!(workspace.open_settings(), settings);

    assert_eq!(workspace.active(), Some(settings));
    assert!(workspace.active_is_settings());
    assert_eq!(workspace.active_path(), None);
    assert!(workspace.active_editor().is_none());
    assert!(workspace.view_document(settings).is_none());
    assert!(!workspace.is_view_dirty(settings));
    assert!(!workspace.is_file(settings));
    assert_eq!(workspace.open_paths(), vec![PathBuf::from("src/main.rs")]);

    workspace.test_close_document(settings);
    assert_eq!(workspace.settings_id(), None);
    assert_eq!(workspace.active(), Some(file));
}

#[test]
fn keymap_page_is_its_own_unique_tab() {
    let mut workspace = Workspace::new();
    let settings = workspace.open_settings();
    let keymap = workspace.open_keymap();
    assert_ne!(keymap, settings);
    assert_eq!(workspace.open_keymap(), keymap);
    assert_eq!(workspace.active_page(), Some(AppPage::Keymap));
    assert!(!workspace.active_is_settings());
    assert!(!workspace.is_file(keymap));
    assert_eq!(workspace.active_path(), None);
    assert!(workspace.open_paths().is_empty());
}

#[test]
fn opens_disk_files_once_and_reactivates_existing_document() {
    let mut workspace = Workspace::new();
    let first = workspace.test_open_view(PathBuf::from("src/main.rs"), "fn main() {}".into());
    let second = workspace.test_open_view(PathBuf::from("README.md"), "hello".into());
    let reopened = workspace.test_open_view(PathBuf::from("src/main.rs"), "ignored".into());

    assert_eq!(reopened, first);
    assert_eq!(workspace.active(), Some(first));
    assert_eq!(workspace.active_items(), &[first, second]);
    assert_eq!(
        workspace.open_paths(),
        vec![PathBuf::from("src/main.rs"), PathBuf::from("README.md")]
    );
    assert_eq!(workspace.active_editor().unwrap().text(), "fn main() {}");
}

#[test]
fn a_new_preview_replaces_the_clean_preview_at_the_same_index() {
    let mut workspace = Workspace::new();
    let permanent = workspace.test_open_view(PathBuf::from("permanent.rs"), String::new());
    let first = workspace.test_preview_view(PathBuf::from("first.rs"), "first".into());
    let trailing = workspace.test_open_view(PathBuf::from("trailing.rs"), String::new());
    workspace.set_active(first);

    let second = workspace.test_preview_view(PathBuf::from("second.rs"), "second".into());

    assert_eq!(workspace.active_items(), &[permanent, second, trailing]);
    assert_eq!(workspace.preview(), Some(second));
    assert_eq!(workspace.active(), Some(second));
    assert_eq!(workspace.test_view_for_path(Path::new("first.rs")), None);
}

#[test]
fn editing_a_preview_defensively_preserves_it_before_the_next_preview() {
    let mut workspace = Workspace::new();
    let first = workspace.test_preview_view(PathBuf::from("first.rs"), String::new());
    workspace
        .active_editor_mut()
        .unwrap()
        .replace_selection("changed", EditKind::Typing)
        .unwrap();

    let second = workspace.test_preview_view(PathBuf::from("second.rs"), String::new());

    assert_eq!(workspace.active_items(), &[first, second]);
    assert_ne!(workspace.preview(), Some(first));
    assert_eq!(workspace.preview(), Some(second));
    assert!(workspace.is_view_dirty(first));
}

#[test]
fn permanently_reopening_a_preview_promotes_without_duplication() {
    let mut workspace = Workspace::new();
    let path = PathBuf::from("main.rs");
    let preview = workspace.test_preview_view(path.clone(), "original".into());

    let reopened = workspace.test_open_view(path, "ignored".into());

    assert_eq!(reopened, preview);
    assert_eq!(workspace.active_items(), &[preview]);
    assert_eq!(workspace.preview(), None);
    assert_eq!(workspace.active_editor().unwrap().text(), "original");
}

#[test]
fn activating_an_existing_permanent_tab_keeps_the_preview_slot() {
    let mut workspace = Workspace::new();
    let permanent = workspace.test_open_view(PathBuf::from("permanent.rs"), String::new());
    let preview = workspace.test_preview_view(PathBuf::from("preview.rs"), String::new());

    let reopened = workspace.test_preview_view(PathBuf::from("permanent.rs"), "ignored".into());

    assert_eq!(reopened, permanent);
    assert_eq!(workspace.active(), Some(permanent));
    assert_eq!(workspace.preview(), Some(preview));
    assert_eq!(workspace.active_items(), &[permanent, preview]);
}

#[test]
fn closing_a_preview_clears_the_preview_slot() {
    let mut workspace = Workspace::new();
    let preview = workspace.test_preview_view(PathBuf::from("preview.rs"), String::new());

    workspace.test_close_document(preview);

    assert_eq!(workspace.preview(), None);
    assert!(workspace.active_items().is_empty());
}

#[test]
fn closing_document_removes_its_path_and_activates_a_neighbor() {
    let mut workspace = Workspace::new();
    let first = workspace.test_open_view(PathBuf::from("first.txt"), "first".into());
    let second = workspace.test_open_view(PathBuf::from("second.txt"), "second".into());

    workspace.test_close_document(second);

    assert_eq!(workspace.active(), Some(first));
    assert_eq!(workspace.test_view_for_path(Path::new("second.txt")), None);
}

#[test]
fn diff_tabs_are_reused_and_are_not_persisted_as_editable_files() {
    let mut workspace = Workspace::new();
    let file_path = PathBuf::from("repo/src/main.rs");
    let file = workspace.test_open_view(file_path.clone(), "fn main() {}".into());
    let make_diff = || {
        DiffDocument::from_unified(
            PathBuf::from("repo"),
            PathBuf::from("src/main.rs"),
            DiffTarget::IndexToWorktree,
            UnifiedDiff::default(),
        )
    };

    let first = workspace.open_diff(make_diff());
    let reopened = workspace.open_diff(make_diff());

    assert_eq!(first, reopened);
    assert_eq!(workspace.active(), Some(first));
    assert!(workspace.is_diff(first));
    assert!(workspace.active_editor().is_none());
    assert_eq!(workspace.active_items(), &[file, first]);
    assert_eq!(workspace.open_paths(), vec![file_path]);
}

#[test]
fn background_diff_refresh_preserves_tabs_focus_and_views() {
    use crate::features::git::error::{GitError, GitErrorKind};
    use std::collections::BTreeMap;
    let mut workspace = Workspace::new();
    let root = PathBuf::from("repo");
    let path = PathBuf::from("new.txt");
    let diff_id = workspace.open_diff(DiffDocument::added(
        root.clone(),
        path.clone(),
        DiffTarget::IndexToWorktree,
        "old\n",
    ));
    workspace.set_active_scroll(12.0, 48.0);
    let target = workspace.git_poll_target();
    let request = target.diffs[0].clone();
    let text_id = workspace.test_open_view(root.join("other.txt"), "editor text".into());
    let tabs = workspace.active_items().to_vec();
    let updated = BTreeMap::from([(
        request.clone(),
        Ok(DiffContent::Untracked("updated\n".into())),
    )]);
    assert!(workspace.apply_git_diffs(&updated));
    assert!(!workspace.apply_git_diffs(&updated));
    assert_eq!(workspace.active(), Some(text_id));
    assert_eq!(workspace.active_items(), tabs);
    assert_eq!(
        workspace.diff_id_for(&root, &path, DiffTarget::IndexToWorktree),
        Some(diff_id)
    );
    workspace.set_active(diff_id);
    assert_eq!(workspace.active_scroll(), (12.0, 48.0));
    assert_eq!(workspace.active_diff().unwrap().rows[0].text, "updated");
    assert!(!workspace.apply_git_diffs(&BTreeMap::from([(
        request.clone(),
        Err(GitError::new(GitErrorKind::Other, "read failed"))
    )])));
    assert_eq!(workspace.active_diff().unwrap().rows[0].text, "updated");
    // A newly empty diff clears old rows after stage/commit or deletion.
    assert!(workspace.apply_git_diffs(&BTreeMap::from([(
        request,
        Ok(DiffContent::Unified(UnifiedDiff::default()))
    )])));
    assert!(workspace.active_diff().unwrap().rows.is_empty());
    workspace.test_close_document(diff_id);
    assert!(workspace.git_poll_target().diffs.is_empty());
    assert!(!workspace.apply_git_diffs(&updated));
    assert_eq!(workspace.active_items(), &[text_id]);
}

#[test]
fn keeps_an_independent_scroll_position_for_each_document() {
    let mut workspace = Workspace::new();
    let first = workspace.test_open_view(PathBuf::from("first.txt"), "first".into());
    workspace.set_active_scroll(24.0, 120.0);
    let second = workspace.test_open_view(PathBuf::from("second.txt"), "second".into());
    workspace.set_active_scroll(8.0, 40.0);

    workspace.set_active(first);
    assert_eq!(workspace.active_scroll(), (24.0, 120.0));
    workspace.set_active(second);
    assert_eq!(workspace.active_scroll(), (8.0, 40.0));
}

#[test]
fn dirty_state_tracks_content_against_the_opened_file() {
    let mut workspace = Workspace::new();
    let file = workspace.test_open_view(PathBuf::from("notes.txt"), "hello".into());

    assert!(!workspace.is_view_dirty(file));
    workspace
        .active_editor_mut()
        .unwrap()
        .select_to(usize::MAX, false);
    assert!(!workspace.is_view_dirty(file));

    workspace
        .active_editor_mut()
        .unwrap()
        .replace_selection("!", EditKind::Typing)
        .unwrap();
    assert!(workspace.is_view_dirty(file));

    let (snapshot_id, path, contents) = workspace.active_save_snapshot().unwrap();
    assert_eq!(snapshot_id, workspace.test_document(file));
    assert_eq!(path, PathBuf::from("notes.txt"));
    assert_eq!(contents, "hello!");
    assert!(workspace.mark_saved(workspace.test_document(file)));
    assert!(!workspace.is_view_dirty(file));

    workspace
        .active_editor_mut()
        .unwrap()
        .replace(5..6, "", EditKind::Backspace)
        .unwrap();
    assert!(workspace.is_view_dirty(file));
}

#[test]
fn dirty_file_ids_follow_tab_order_and_exclude_clean_files() {
    let mut workspace = Workspace::new();
    let first = workspace.test_open_view(PathBuf::from("first.rs"), String::new());
    workspace
        .active_editor_mut()
        .unwrap()
        .replace_selection("first change", EditKind::Typing)
        .unwrap();
    let clean = workspace.test_open_view(PathBuf::from("clean.rs"), String::new());
    let second = workspace.test_open_view(PathBuf::from("second.rs"), String::new());
    workspace
        .active_editor_mut()
        .unwrap()
        .replace_selection("second change", EditKind::Typing)
        .unwrap();

    assert_eq!(
        workspace.dirty_file_ids(),
        vec![
            workspace.test_document(first),
            workspace.test_document(second)
        ]
    );
    assert!(
        !workspace
            .dirty_file_ids()
            .contains(&workspace.test_document(clean))
    );
}

#[test]
fn moving_tabs_uses_the_requested_final_index_and_keeps_the_active_document() {
    let mut workspace = Workspace::new();
    let first = workspace.test_open_view(PathBuf::from("first.rs"), String::new());
    let second = workspace.test_open_view(PathBuf::from("second.rs"), String::new());
    let third = workspace.test_open_view(PathBuf::from("third.rs"), String::new());

    assert!(workspace.move_tab(workspace.active_pane(), first, 2));
    assert_eq!(workspace.active_items(), &[second, third, first]);
    assert_eq!(workspace.active(), Some(third));

    assert!(workspace.move_tab(workspace.active_pane(), first, 0));
    assert_eq!(workspace.active_items(), &[first, second, third]);
    assert!(!workspace.move_tab(workspace.active_pane(), first, 0));
}

#[test]
fn duplicate_tab_names_use_the_shortest_distinguishing_parent_suffix() {
    let mut workspace = Workspace::new();
    let first = workspace.test_open_view(PathBuf::from("alpha/src/main.rs"), String::new());
    let second = workspace.test_open_view(PathBuf::from("beta/src/main.rs"), String::new());
    let unique = workspace.test_open_view(PathBuf::from("beta/src/lib.rs"), String::new());

    let labels = workspace.tab_labels(workspace.active_pane());
    assert_eq!(labels.get(&first).unwrap(), "main.rs — alpha/src");
    assert_eq!(labels.get(&second).unwrap(), "main.rs — beta/src");
    assert_eq!(labels.get(&unique).unwrap(), "lib.rs");
}

#[test]
fn positional_activation_selects_existing_indices_and_the_last_tab() {
    let mut workspace = Workspace::new();
    let first = workspace.test_open_view(PathBuf::from("first.rs"), String::new());
    let second = workspace.test_open_view(PathBuf::from("second.rs"), String::new());
    let third = workspace.test_open_view(PathBuf::from("third.rs"), String::new());

    assert!(workspace.activate_at(0));
    assert_eq!(workspace.active(), Some(first));
    assert!(!workspace.activate_at(99));
    assert_eq!(workspace.active(), Some(first));
    assert!(workspace.activate_last());
    assert_eq!(workspace.active(), Some(third));
    assert_ne!(workspace.active(), Some(second));
}

#[test]
fn directory_rename_retargets_open_files_without_reopening_them() {
    let mut workspace = Workspace::new();
    let old_root = PathBuf::from("project/src");
    let new_root = PathBuf::from("project/source");
    let old_file = old_root.join("nested/main.rs");
    let new_file = new_root.join("nested/main.rs");
    let id = workspace.test_open_view(old_file.clone(), "fn main() {}".into());

    assert_eq!(workspace.move_open_paths_under(&old_root, &new_root), 1);
    assert_eq!(workspace.test_view_for_path(&old_file), None);
    assert_eq!(workspace.test_view_for_path(&new_file), Some(id));
    assert_eq!(workspace.active_path(), Some(new_file.as_path()));
}

#[test]
fn directory_delete_closes_only_open_files_below_that_directory() {
    let mut workspace = Workspace::new();
    let removed = workspace.test_open_view(PathBuf::from("project/src/main.rs"), String::new());
    let kept_path = PathBuf::from("project/tests/main.rs");
    let kept = workspace.test_open_view(kept_path.clone(), String::new());

    assert_eq!(workspace.close_paths_under(Path::new("project/src")), 1);
    assert_eq!(
        workspace.test_view_for_path(Path::new("project/src/main.rs")),
        None
    );
    assert_eq!(workspace.test_view_for_path(&kept_path), Some(kept));
    assert_eq!(workspace.active(), Some(kept));
    assert_ne!(workspace.active(), Some(removed));
}

#[test]
fn dirty_document_is_never_replaced_during_disk_reconciliation() {
    let root = std::env::temp_dir().join(format!(
        "loog-reconcile-{}-{}",
        std::process::id(),
        crate::domain::document::content_hash(module_path!())
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("note.txt");
    std::fs::write(&path, "disk one").unwrap();
    let mut workspace = Workspace::new();
    let id = workspace.test_open_view(path.clone(), "disk one".into());
    workspace
        .active_editor_mut()
        .unwrap()
        .select_to(usize::MAX, false);
    workspace
        .active_editor_mut()
        .unwrap()
        .replace_selection(" + editor", EditKind::Typing)
        .unwrap();
    std::fs::write(&path, "disk two with size").unwrap();

    assert_eq!(
        workspace.reconcile_document(workspace.test_document(id)),
        ReconcileResult::Conflict(path.clone())
    );
    assert_eq!(
        workspace.active_editor().unwrap().text(),
        "disk one + editor"
    );
    assert!(workspace.has_disk_conflict(workspace.test_document(id)));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn delayed_disk_notification_preserves_intervening_edits_and_saves() {
    let root = std::env::temp_dir().join(format!("loog-async-disk-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("file.txt");
    std::fs::write(&path, "base").unwrap();
    let mut workspace = Workspace::new();
    let id = workspace.test_open_view(path.clone(), "base".into());
    std::fs::write(&path, "disk changed").unwrap();
    let pending = workspace.prepare_disk_updates(&[], true);
    workspace
        .active_editor_mut()
        .unwrap()
        .select_to(usize::MAX, false);
    workspace
        .active_editor_mut()
        .unwrap()
        .replace_selection(" + editor", EditKind::Typing)
        .unwrap();
    assert_eq!(
        workspace.apply_disk_updates(pending),
        vec![ReconcileResult::Conflict(path.clone())]
    );
    assert_eq!(workspace.active_editor().unwrap().text(), "base + editor");
    let pending = workspace.prepare_disk_updates(&[], true);
    std::fs::write(&path, workspace.active_editor().unwrap().text()).unwrap();
    workspace.mark_saved(workspace.test_document(id));
    assert!(workspace.apply_disk_updates(pending).is_empty());
    assert!(!workspace.has_disk_conflict(workspace.test_document(id)));
    assert_eq!(workspace.active_editor().unwrap().text(), "base + editor");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn prepared_disk_and_diff_notifications_apply_without_rereading_files() {
    let root = std::env::temp_dir().join(format!("loog-prepared-disk-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("file.txt");
    std::fs::write(&path, "base").unwrap();
    let mut workspace = Workspace::new();
    workspace.test_open_view(path.clone(), "base".into());
    workspace.set_active_scroll(12.0, 24.0);
    std::fs::write(&path, "read by worker\nsecond line").unwrap();
    let updates = workspace.prepare_disk_updates(&[], true);
    std::fs::remove_dir_all(&root).unwrap();
    assert_eq!(
        workspace.apply_disk_updates(updates),
        vec![ReconcileResult::Reloaded(path)]
    );
    assert_eq!(
        workspace.active_editor().unwrap().text(),
        "read by worker\nsecond line"
    );
    assert_eq!(workspace.active_scroll(), (12.0, 24.0));
    let request = DiffRequest {
        repository_root: root,
        path: PathBuf::from("diff.txt"),
        target: DiffTarget::IndexToWorktree,
    };
    let document = Arc::new(DiffDocument::added(
        request.repository_root.clone(),
        request.path.clone(),
        request.target,
        "prepared diff\n",
    ));
    workspace.open_prepared_diff(document.clone());
    let clone = workspace.clone();
    assert!(std::ptr::eq(
        clone.active_diff().unwrap(),
        workspace.active_diff().unwrap()
    ));
    let diffs = std::collections::BTreeMap::from([(request, document)]);
    assert!(workspace.apply_prepared_git_diffs(&diffs, false).is_empty());
}

#[test]
fn changes_tab_reuses_identity_and_preserves_file_anchor_and_collapsed_sections() {
    use crate::features::git::{ChangeDiff, ChangeGroup};
    let root = PathBuf::from("repo");
    let request = DiffRequest {
        repository_root: root.clone(),
        path: PathBuf::new(),
        target: DiffTarget::Changes,
    };
    let make = |first_lines: usize| {
        Arc::new(DiffDocument::from_content(
            request.clone(),
            DiffContent::Changes(vec![
                ChangeDiff {
                    path: "first.txt".into(),
                    target: DiffTarget::HeadToIndex,
                    group: ChangeGroup::Staged,
                    content: Ok(DiffContent::Untracked("one\n".repeat(first_lines))),
                },
                ChangeDiff {
                    path: "second.txt".into(),
                    target: DiffTarget::IndexToWorktree,
                    group: ChangeGroup::Unstaged,
                    content: Ok(DiffContent::Untracked("two\n".repeat(20))),
                },
                ChangeDiff {
                    path: "third.txt".into(),
                    target: DiffTarget::IndexToWorktree,
                    group: ChangeGroup::Untracked,
                    content: Ok(DiffContent::Untracked("three\n".repeat(20))),
                },
            ]),
        ))
    };
    let second = DiffRequest {
        repository_root: root.clone(),
        path: "second.txt".into(),
        target: DiffTarget::IndexToWorktree,
    };
    let third = DiffRequest {
        path: "third.txt".into(),
        ..second.clone()
    };
    let mut workspace = Workspace::new();
    let id = workspace.open_prepared_diff(make(10));
    assert_eq!(workspace.open_prepared_diff(make(10)), id);
    workspace.focus_diff_section(&second, false);
    let (x, y) = workspace.active_scroll();
    workspace.set_active_scroll(x, y + 25.0);
    let pane = workspace.active_pane();
    workspace.toggle_diff_section(pane, third.clone());
    let refreshed = make(30);
    let diffs = std::collections::BTreeMap::from([(request.clone(), refreshed)]);
    assert_eq!(workspace.apply_prepared_git_diffs(&diffs, false).len(), 1);
    assert_eq!(
        workspace.active_scroll().1,
        y + 25.0 + 20.0 * crate::ui::diff_editor::row_h()
    );
    assert!(workspace.collapsed_diffs(pane).contains(&third));
    assert_eq!(workspace.git_poll_target().diffs, [request]);
    let single = workspace.open_diff(DiffDocument::added(
        root.clone(),
        "second.txt".into(),
        DiffTarget::IndexToWorktree,
        "two\n",
    ));
    assert_ne!(single, id);
    assert_eq!(workspace.active_pane().get(), pane.get());
    assert_eq!(workspace.pane(pane).unwrap().items().len(), 2);
    workspace.activate(pane, id);
    workspace.focus_diff_section(&third, false);
    assert!(!workspace.collapsed_diffs(pane).contains(&third));
    assert_eq!(workspace.active_diff_focus(), Some(&third));
}

#[test]
fn clean_document_reload_preserves_cursor_and_scroll() {
    let root = std::env::temp_dir().join(format!(
        "loog-reload-clean-{}-{}",
        std::process::id(),
        crate::domain::document::content_hash(module_path!())
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("note.txt");
    std::fs::write(&path, "before").unwrap();
    let mut workspace = Workspace::new();
    let id = workspace.test_open_view(path.clone(), "before".into());
    workspace.active_editor_mut().unwrap().select_range(2..4);
    workspace.set_active_scroll(12.0, 34.0);
    std::fs::write(&path, "after and longer").unwrap();

    assert_eq!(
        workspace.reconcile_document(workspace.test_document(id)),
        ReconcileResult::Reloaded(path.clone())
    );
    assert_eq!(
        workspace.active_editor().unwrap().text(),
        "after and longer"
    );
    assert_eq!(workspace.active_editor().unwrap().cursor(), 4);
    assert_eq!(workspace.active_editor().unwrap().selection(), Some(2..4));
    assert_eq!(workspace.active_scroll(), (12.0, 34.0));
    assert!(!workspace.is_view_dirty(id));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn deleted_document_remains_open_and_is_marked_missing() {
    let root = std::env::temp_dir().join(format!(
        "loog-reload-missing-{}-{}",
        std::process::id(),
        crate::domain::document::content_hash(module_path!())
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("note.txt");
    std::fs::write(&path, "contents").unwrap();
    let mut workspace = Workspace::new();
    let id = workspace.test_open_view(path.clone(), "contents".into());
    std::fs::remove_file(&path).unwrap();

    assert_eq!(
        workspace.reconcile_document(workspace.test_document(id)),
        ReconcileResult::Missing(path.clone())
    );
    assert_eq!(workspace.active_editor().unwrap().text(), "contents");
    assert!(workspace.is_missing_on_disk(workspace.test_document(id)));
    assert!(!workspace.has_disk_conflict(workspace.test_document(id)));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn recreating_a_missing_file_with_saved_contents_clears_missing_state() {
    let root = std::env::temp_dir().join(format!(
        "loog-reload-recreated-{}-{}",
        std::process::id(),
        crate::domain::document::content_hash(module_path!())
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("note.txt");
    std::fs::write(&path, "contents").unwrap();
    let mut workspace = Workspace::new();
    let id = workspace.test_open_view(path.clone(), "contents".into());
    std::fs::remove_file(&path).unwrap();
    workspace.reconcile_document(workspace.test_document(id));
    assert!(workspace.is_missing_on_disk(workspace.test_document(id)));

    std::fs::write(&path, "contents").unwrap();
    assert_eq!(
        workspace.reconcile_document(workspace.test_document(id)),
        ReconcileResult::Unchanged(path)
    );
    assert!(!workspace.is_missing_on_disk(workspace.test_document(id)));
    assert!(!workspace.has_disk_conflict(workspace.test_document(id)));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn save_echo_does_not_create_a_disk_conflict() {
    let root = std::env::temp_dir().join(format!(
        "loog-save-echo-{}-{}",
        std::process::id(),
        crate::domain::document::content_hash(module_path!())
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("note.txt");
    std::fs::write(&path, "before").unwrap();
    let mut workspace = Workspace::new();
    let id = workspace.test_open_view(path.clone(), "before".into());
    workspace
        .active_editor_mut()
        .unwrap()
        .select_to(usize::MAX, false);
    workspace
        .active_editor_mut()
        .unwrap()
        .replace_selection(" saved", EditKind::Typing)
        .unwrap();
    std::fs::write(&path, "before saved").unwrap();
    assert!(workspace.mark_saved(workspace.test_document(id)));

    assert_eq!(
        workspace.reconcile_document(workspace.test_document(id)),
        ReconcileResult::Unchanged(path)
    );
    assert!(!workspace.has_disk_conflict(workspace.test_document(id)));
    assert!(!workspace.is_missing_on_disk(workspace.test_document(id)));

    let _ = std::fs::remove_dir_all(root);
}

fn two_panes_with(path: &str, text: &str) -> (Workspace, PaneId, PaneId, ViewId) {
    let mut workspace = Workspace::new();
    let left = workspace.active_pane();
    let id = workspace.test_open_view(PathBuf::from(path), text.into());
    let right = workspace.split(left, Direction::Right).unwrap();
    (workspace, left, right, id)
}

#[test]
fn split_copies_the_active_tab_and_its_view_into_a_focused_new_pane() {
    let mut workspace = Workspace::new();
    let left = workspace.active_pane();
    let id = workspace.test_open_view(PathBuf::from("a.rs"), "hello world".into());
    workspace.active_editor_mut().unwrap().select_range(0..5);
    workspace.set_active_scroll(3.0, 40.0);

    let right = workspace.split(left, Direction::Right).unwrap();

    assert_eq!(workspace.active_pane(), right);
    assert_eq!(workspace.pane_ids(), vec![left, right]);
    let copied = workspace.active().unwrap();
    assert_ne!(copied, id);
    assert_eq!(workspace.test_document(copied), workspace.test_document(id));
    assert_eq!(workspace.pane(right).unwrap().items(), &[copied]);
    assert_eq!(workspace.active_editor().unwrap().selection(), Some(0..5));
    assert_eq!(workspace.scroll(right), (3.0, 40.0));
    assert_eq!(workspace.pane(left).unwrap().items(), &[id]);
}

#[test]
fn views_of_one_document_share_text_but_not_selection_or_scroll() {
    let (mut workspace, left, right, id) = two_panes_with("a.rs", "one two");
    workspace.editor_mut(left).unwrap().set_cursor(0);
    workspace.editor_mut(right).unwrap().set_cursor(7);
    workspace.set_scroll(right, 0.0, 99.0);

    workspace
        .editor_mut(left)
        .unwrap()
        .replace_selection(">> ", EditKind::Typing)
        .unwrap();

    assert_eq!(workspace.editor(right).unwrap().text(), ">> one two");
    assert_eq!(workspace.editor(right).unwrap().cursor(), 10);
    assert_eq!(workspace.editor(left).unwrap().cursor(), 3);
    assert_eq!(workspace.scroll(left), (0.0, 0.0));
    assert_eq!(workspace.scroll(right), (0.0, 99.0));
    assert!(workspace.is_view_dirty(id));
    assert_eq!(
        workspace.dirty_file_ids(),
        vec![workspace.test_document(id)]
    );
}

#[test]
fn closing_one_tab_keeps_the_document_until_its_last_tab_closes() {
    let (mut workspace, left, right, id) = two_panes_with("a.rs", "text");
    let right_view = workspace.pane(right).unwrap().active().unwrap();
    workspace
        .editor_mut(right)
        .unwrap()
        .replace_selection("x", EditKind::Typing)
        .unwrap();

    assert!(workspace.released_by(&[(right, right_view)]).is_empty());
    assert_eq!(
        workspace.released_by(&[(left, id), (right, right_view)]),
        vec![workspace.test_document(id)]
    );
    assert_eq!(
        workspace.tabs_of(workspace.test_document(id)),
        vec![(left, id), (right, right_view)]
    );

    workspace.close_item(right, right_view);
    // The emptied pane closes and focus returns to the remaining one.
    assert_eq!(workspace.pane_ids(), vec![left]);
    assert_eq!(workspace.active_pane(), left);
    assert!(workspace.is_view_dirty(id));
    assert_eq!(workspace.editor(left).unwrap().text(), "xtext");

    workspace.close_item(left, id);
    assert!(workspace.view_meta(id).is_none());
    assert_eq!(workspace.test_view_for_path(Path::new("a.rs")), None);
    assert_eq!(workspace.pane_count(), 1);
}

#[test]
fn opening_a_file_shown_elsewhere_adds_a_tab_sharing_its_buffer() {
    let mut workspace = Workspace::new();
    let left = workspace.active_pane();
    let id = workspace.test_open_view(PathBuf::from("a.rs"), "text".into());
    let right = workspace.split(left, Direction::Down).unwrap();
    let right_view = workspace.active().unwrap();
    let other = workspace.test_open_view(PathBuf::from("b.rs"), String::new());
    workspace.close_item(right, right_view);
    assert_eq!(workspace.pane(right).unwrap().items(), &[other]);

    let reopened = workspace.test_open_view(PathBuf::from("a.rs"), "ignored".into());

    assert_ne!(reopened, id);
    assert_eq!(
        workspace.test_document(reopened),
        workspace.test_document(id)
    );
    assert_eq!(workspace.pane(right).unwrap().items(), &[other, reopened]);
    assert_eq!(workspace.editor(right).unwrap().text(), "text");
}

#[test]
fn previews_are_tracked_per_pane() {
    let mut workspace = Workspace::new();
    let left = workspace.active_pane();
    let first = workspace.test_preview_view(PathBuf::from("first.rs"), String::new());
    let right = workspace.split(left, Direction::Right).unwrap();
    let copied = workspace.active().unwrap();
    assert_ne!(copied, first);
    assert_eq!(workspace.pane(right).unwrap().preview(), None);

    let second = workspace.test_preview_view(PathBuf::from("second.rs"), String::new());
    assert_eq!(workspace.pane(right).unwrap().items(), &[copied, second]);
    assert_eq!(workspace.pane(right).unwrap().preview(), Some(second));
    assert_eq!(workspace.pane(left).unwrap().preview(), Some(first));

    let third = workspace.test_preview_view(PathBuf::from("third.rs"), String::new());
    assert_eq!(workspace.pane(right).unwrap().items(), &[copied, third]);
    assert!(workspace.view_meta(second).is_none());
}

#[test]
fn single_instance_tabs_are_revealed_instead_of_duplicated() {
    let mut workspace = Workspace::new();
    let left = workspace.active_pane();
    let settings = workspace.open_settings();
    let right = workspace.split(left, Direction::Right).unwrap();
    assert!(workspace.pane(right).unwrap().is_empty());

    assert_eq!(workspace.open_settings(), settings);
    assert_eq!(workspace.active_pane(), left);
    assert!(!workspace.copy_item(left, settings, right, 0));
}

#[test]
fn moving_a_tab_between_panes_keeps_its_view_and_closes_an_emptied_source() {
    let mut workspace = Workspace::new();
    let left = workspace.active_pane();
    let keep = workspace.test_open_view(PathBuf::from("keep.rs"), String::new());
    let right = workspace.split(left, Direction::Right).unwrap();
    let copied_keep = workspace.active().unwrap();
    let moved = workspace.test_open_view(PathBuf::from("moved.rs"), "abc".into());
    workspace.close_item(right, copied_keep);
    workspace.editor_mut(right).unwrap().set_cursor(2);

    assert!(workspace.move_item(right, moved, left, 0));

    assert_eq!(workspace.pane_ids(), vec![left]);
    assert_eq!(workspace.pane(left).unwrap().items(), &[moved, keep]);
    assert_eq!(workspace.active(), Some(moved));
    assert_eq!(workspace.active_editor().unwrap().cursor(), 2);
}

#[test]
fn closing_a_pane_focuses_the_most_recent_remaining_pane() {
    let mut workspace = Workspace::new();
    let first = workspace.active_pane();
    workspace.test_open_view(PathBuf::from("a.rs"), String::new());
    let second = workspace.split(first, Direction::Right).unwrap();
    let third = workspace.split(second, Direction::Down).unwrap();
    workspace.activate_pane(first);
    workspace.activate_pane(third);

    workspace.close_pane(third);

    assert_eq!(workspace.active_pane(), first);
    assert_eq!(workspace.pane_ids(), vec![first, second]);
    // The last pane survives and only loses its tabs.
    workspace.close_pane(second);
    workspace.close_pane(first);
    assert_eq!(workspace.pane_ids(), vec![first]);
    assert!(workspace.active_items().is_empty());
}

#[test]
fn reload_from_disk_clamps_every_view() {
    let root = std::env::temp_dir().join(format!(
        "loog-reload-views-{}-{}",
        std::process::id(),
        crate::domain::document::content_hash(module_path!())
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("note.txt");
    std::fs::write(&path, "a long original line").unwrap();
    let mut workspace = Workspace::new();
    let left = workspace.active_pane();
    let id = workspace.test_open_view(path.clone(), "a long original line".into());
    let right = workspace.split(left, Direction::Right).unwrap();
    workspace.editor_mut(left).unwrap().set_cursor(20);
    workspace.editor_mut(right).unwrap().set_cursor(2);
    std::fs::write(&path, "short").unwrap();

    assert_eq!(
        workspace.reconcile_document(workspace.test_document(id)),
        ReconcileResult::Reloaded(path)
    );
    assert_eq!(workspace.editor(left).unwrap().cursor(), 5);
    assert_eq!(workspace.editor(right).unwrap().cursor(), 2);

    let _ = std::fs::remove_dir_all(root);
}

impl Workspace {
    pub(crate) fn test_open_view(&mut self, path: PathBuf, contents: String) -> ViewId {
        self.open_path(path, contents);
        self.active().unwrap()
    }
    pub(crate) fn test_preview_view(&mut self, path: PathBuf, contents: String) -> ViewId {
        self.preview_path(path, contents);
        self.active().unwrap()
    }
    pub(crate) fn test_document(&self, view: ViewId) -> FileId {
        self.view_document(view)
            .expect("test view has text document")
    }
    pub(crate) fn test_view_for_path(&self, path: &Path) -> Option<ViewId> {
        self.tabs_of(self.file_id_for_path(path)?)
            .first()
            .map(|(_, v)| *v)
    }
    pub(crate) fn test_close_document(&mut self, view: ViewId) {
        if let Some(id) = self.view_document(view) {
            self.close(id);
        } else if let Some(pane) = self.pane_of(view) {
            self.close_item(pane, view);
        }
    }
}

#[test]
fn verbatim_and_plain_spellings_open_one_document() {
    let mut workspace = Workspace::new();
    let plain = workspace.open_path(PathBuf::from(r"D:\proj\notes.md"), "text".into());
    let verbatim = workspace.open_path(PathBuf::from(r"\\?\D:\proj\notes.md"), "text".into());
    assert_eq!(plain, verbatim);
    assert_eq!(workspace.open_paths().len(), 1);
}
