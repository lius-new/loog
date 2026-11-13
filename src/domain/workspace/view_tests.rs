//! P1 invariants run against the production workspace, without a GUI adapter.
use crate::domain::document::{DiskRead, DiskState, FileMeta};
use crate::domain::identity::{FileId, ViewId};
use crate::domain::pane_layout::Direction;
use crate::domain::view::{Presentation, ReaderPosition, ViewTarget};
use crate::domain::workspace::{
    AppPage, OpenMode, PreparedTextFile, ResourceResolution, Workspace,
};
use loog_editor_core::normal;
use std::collections::HashSet;
use std::sync::Arc;

type Model = Workspace<(), ()>;

fn provided(text: &str, scope: u64, revision: &str) -> PreparedTextFile {
    let resource = loog_workspace_api::ResourceRef::provided(
        loog_workspace_api::EnvironmentKey::new("fixture", "saved").unwrap(),
        "/work/a.txt",
    )
    .unwrap();
    PreparedTextFile::provided(
        resource,
        "a.txt".into(),
        scope,
        Some(revision.into()),
        crate::domain::encoding::DecodedText {
            text: text.into(),
            format: Default::default(),
            source: Default::default(),
            bytes: Arc::new(text.as_bytes().to_vec()),
        },
    )
}

#[test]
fn resource_reconciliation_updates_options_even_with_identical_or_dirty_text() {
    let mut w = Model::new();
    let id = w.open_prepared_text(provided("before", 7, "r1"), OpenMode::Permanent);
    for (text, indent) in [("before", 2), ("external", 4)] {
        let options = loog_editor_core::text::FileOptions {
            indent_size: Some(indent),
            ..Default::default()
        };
        let result = w.resolve_resource(
            id,
            provided(text, 7, "r2").with_file_options(options),
            ResourceResolution::Check,
        );
        if text == "before" {
            result.unwrap();
            normal::insert(&mut w.active_editor_mut().unwrap(), "edited ");
        } else {
            assert!(result.is_err());
        }
        assert_eq!(
            w.documents[&id].buffer.file_options().indent_size,
            Some(indent)
        );
    }
    assert_eq!(w.documents[&id].buffer.text(), "edited before");
    w.active_editor_mut().unwrap().undo();
    assert_eq!(w.documents[&id].buffer.text(), "before");
}

#[test]
fn resource_watch_echo_keeps_undo_and_external_changes_preserve_unsaved_text() {
    let mut w = Model::new();
    let id = w.open_prepared_text(provided("before", 7, "r1"), OpenMode::Permanent);
    normal::insert(&mut w.active_editor_mut().unwrap(), "edited ");
    let text = w.documents[&id].buffer.text().to_owned();
    let saved = w.resource_save_snapshot(id).unwrap();
    assert!(w.mark_resource_saved(&saved, Some("r2".into())));
    let incarnation = w.documents[&id].incarnation;
    w.documents.get_mut(&id).unwrap().disk_conflict = true;
    w.resolve_resource(id, provided(&text, 7, "r2"), ResourceResolution::Check)
        .unwrap();
    assert_eq!(w.documents[&id].incarnation, incarnation);
    w.active_editor_mut().unwrap().undo();
    assert_eq!(w.documents[&id].buffer.text(), "before");
    assert!(w.is_dirty(id));
    assert!(
        w.resolve_resource(id, provided("external", 7, "r3"), ResourceResolution::Check)
            .is_err()
    );
    assert_eq!(w.documents[&id].buffer.text(), "before");
    assert!(w.documents[&id].disk_conflict);
}

#[test]
fn resource_save_preserves_later_edits_and_rejects_old_lifetimes_and_scopes() {
    let mut w = Model::new();
    let id = w.open_prepared_text(provided("before", 7, "r1"), OpenMode::Permanent);
    w.document_editor_mut(id)
        .unwrap()
        .replace(0..6, "saved", loog_editor_core::text::EditKind::Other)
        .unwrap();
    let saved = w.resource_save_snapshot(id).unwrap();
    w.document_editor_mut(id)
        .unwrap()
        .replace(0..5, "new edit", loog_editor_core::text::EditKind::Other)
        .unwrap();
    assert!(w.mark_resource_saved(&saved, Some("r2".into())));
    assert!(w.is_dirty(id));
    assert_eq!(w.documents[&id].buffer.text(), "new edit");
    assert_eq!(w.documents[&id].saved_text.as_str(), "saved");
    assert!(!w.mark_resource_saved(&saved, Some("late".into())));
    w.close(id);
    let reopened = w.open_prepared_text(provided("disk", 8, "r3"), OpenMode::Permanent);
    assert!(!w.mark_resource_saved(&saved, Some("late".into())));
    assert_eq!(w.documents[&reopened].saved_text.as_str(), "disk");
}

#[test]
fn resource_relocation_preserves_dirty_text_and_undo_and_rejects_identity_collisions() {
    use loog_workspace_api::{ResourceMove, ResourceRef};
    let mut w = Model::new();
    let id = w.open_prepared_text(provided("before", 7, "r1"), OpenMode::Permanent);
    let old = w.documents[&id].meta.resource.clone();
    let environment = old.environment().unwrap().clone();
    w.document_editor_mut(id)
        .unwrap()
        .replace(0..6, "my edits", loog_editor_core::text::EditKind::Other)
        .unwrap();
    let saved = w.resource_save_snapshot(id).unwrap();
    let new = ResourceRef::provided(environment.clone(), "/work/new.rs").unwrap();
    let mapping = ResourceMove {
        source: loog_workspace_api::ProviderPath::new("/work/a.txt").unwrap(),
        target: loog_workspace_api::ProviderPath::new("/work/new.rs").unwrap(),
        name: "new.rs".into(),
    };
    let collision = w.open_prepared_text(
        PreparedTextFile::offline(new.clone(), "new.rs".into(), None),
        OpenMode::Permanent,
    );
    assert!(
        w.relocate_resources(&environment, std::slice::from_ref(&mapping))
            .is_err()
    );
    assert_eq!(w.file_id_for_resource(&old), Some(id));
    w.close(collision);
    w.relocate_resources(&environment, &[mapping]).unwrap();
    assert_eq!(w.file_id_for_resource(&old), None);
    assert_eq!(w.file_id_for_resource(&new), Some(id));
    assert_eq!(w.documents[&id].buffer.text(), "my edits");
    assert!(w.documents[&id].disk_conflict);
    assert!(w.is_dirty(id));
    assert!(!w.mark_resource_saved(&saved, Some("late".into())));
    w.document_editor_mut(id).unwrap().undo();
    assert_eq!(w.documents[&id].buffer.text(), "before");
    invariants(&w);
}

#[test]
fn resource_session_retains_identity_and_unsaved_text_and_rechecks_on_reconnect() {
    use crate::domain::workspace_session::{self, SavedPaneNode, SavedView};
    let mut w = Model::new();
    let id = w.open_prepared_text(provided("baseline", 7, "r1"), OpenMode::Permanent);
    w.document_editor_mut(id)
        .unwrap()
        .replace(0..8, "my edits", loog_editor_core::text::EditKind::Other)
        .unwrap();
    let layout = workspace_session::capture(&w, None).unwrap();
    let json = serde_json::to_value(&layout).unwrap();
    assert_eq!(json["schema"], 3);
    let restored: workspace_session::EditorLayout = serde_json::from_value(json).unwrap();
    let SavedPaneNode::Pane { items, .. } = restored.root else {
        panic!("pane")
    };
    let SavedView::Resource {
        resource,
        name,
        encoding,
        ..
    } = items[0].clone()
    else {
        panic!("resource")
    };
    let mut restored = Model::new();
    let id = restored.open_prepared_text(
        PreparedTextFile::offline(resource, name, encoding),
        OpenMode::Permanent,
    );
    assert_eq!(restored.documents[&id].buffer.text(), "my edits");
    assert!(restored.is_dirty(id));
    assert!(restored.shared_save_snapshot(id).is_none());
    assert!(restored.resource_save_snapshot(id).is_none());
    assert!(
        restored
            .reconnect_resource(id, provided("baseline", 8, "r2"))
            .is_ok()
    );
    assert_eq!(restored.resource_save_snapshot(id).unwrap().scope, 8);
    assert_eq!(restored.documents[&id].buffer.text(), "my edits");
    assert!(
        restored
            .reconnect_resource(id, provided("external", 9, "r3"))
            .is_err()
    );
    assert!(restored.documents[&id].disk_conflict);
    assert_eq!(restored.documents[&id].buffer.text(), "my edits");
}

#[test]
fn provided_documents_share_views_only_with_the_same_environment_and_path() {
    use loog_workspace_api::{EnvironmentKey, ResourceRef};
    let mut w = Model::new();
    let a = ResourceRef::provided(
        EnvironmentKey::new("fixture", "a").unwrap(),
        "/work/main.rs",
    )
    .unwrap();
    let b = ResourceRef::provided(
        EnvironmentKey::new("fixture", "b").unwrap(),
        "/work/main.rs",
    )
    .unwrap();
    let first = w.open_prepared_text(
        PreparedTextFile::with_resource(
            a.clone(),
            "main.rs".into(),
            "one".into(),
            DiskState::from_contents("one"),
        ),
        OpenMode::Permanent,
    );
    let second = w.open_prepared_text(
        PreparedTextFile::with_resource(
            b.clone(),
            "main.rs".into(),
            "two".into(),
            DiskState::from_contents("two"),
        ),
        OpenMode::Permanent,
    );
    assert_ne!(first, second);
    assert_eq!(w.file_id_for_resource(&a), Some(first));
    assert_eq!(w.file_id_for_resource(&b), Some(second));
    assert!(
        w.file_id_for_path(std::path::Path::new("/work/main.rs"))
            .is_none()
    );
    assert!(w.active_path().is_none());
    assert!(
        w.shared_save_snapshot(first).is_none(),
        "local saver must not see provided files"
    );
    assert!(matches!(
        w.reconcile_with_disk(first, DiskRead::Missing),
        crate::domain::workspace::ReconcileResult::Provided(_)
    ));
    assert_eq!(w.close_paths_under(std::path::Path::new("/work")), 0);
    w.split(w.active_pane(), Direction::Right);
    let reopened = w.open_prepared_text(
        PreparedTextFile::with_resource(
            a,
            "main.rs".into(),
            "stale".into(),
            DiskState::from_contents("stale"),
        ),
        OpenMode::Permanent,
    );
    assert_eq!(first, reopened);
    assert_eq!(w.documents[&first].buffer.text(), "one");
    invariants(&w);
}

fn open(w: &mut Model, path: &str, text: &str) -> (FileId, ViewId) {
    let document = w.open_prepared_text(
        PreparedTextFile::with_disk_state(path.into(), text.into(), DiskState::from_contents(text)),
        OpenMode::Permanent,
    );
    (document, w.active().unwrap())
}
fn invariants(w: &Model) {
    let mut slots = HashSet::new();
    assert_eq!(w.layout().leaves().len(), w.panes.len());
    for pane in w.panes.values() {
        assert!(pane.active.is_none_or(|id| pane.contains(id)));
        assert!(pane.preview.is_none_or(|id| pane.contains(id)));
        for id in pane.items() {
            assert!(slots.insert(*id));
            assert!(w.views.contains_key(id));
        }
    }
    assert_eq!(slots.len(), w.views.len());
    for (&id, d) in &w.documents {
        assert!(!d.view_ids.is_empty());
        assert_eq!(w.paths.get(&d.meta.resource), Some(&id));
        assert_eq!(
            d.view_ids.iter().copied().collect::<HashSet<_>>(),
            w.views
                .values()
                .filter(|v| v.document() == Some(id))
                .map(|v| v.id)
                .collect()
        );
    }
    for v in w.views.values() {
        if let Some(id) = v.document() {
            assert!(w.documents[&id].view_ids.contains(&v.id));
        }
        if let ViewTarget::Diff(id) = v.target {
            assert_eq!(w.diffs[&id].view, v.id);
        }
    }
}

#[test]
fn p1_same_pane_views_share_text_undo_and_services_but_keep_positions() {
    let mut w = Model::new();
    let pane = w.active_pane();
    let (document, source) = open(&mut w, "note.md", "a界b\r\n👩‍💻");
    w.active_editor_mut().unwrap().set_cursor(1);
    assert!(w.copy_item(pane, source, pane, 1));
    let reader = w.active().unwrap();
    assert_ne!(source, reader);
    assert!(w.set_presentation(reader, Presentation::Markdown));
    w.set_reader_position(
        reader,
        ReaderPosition {
            source_byte: 4,
            block_fraction: 0.4,
        },
    );
    assert!(w.active_editor().is_none());
    assert!(w.active_editor_mut().is_none());
    assert!(w.view_editor_mut(pane, reader).is_none());
    w.activate(pane, source);
    w.set_scroll(pane, 11.0, 40.0);
    normal::insert(&mut w.active_editor_mut().unwrap(), "你");
    assert_eq!(w.view(reader).unwrap().reader.source_byte, 7);
    assert_eq!(w.view(reader).unwrap().source.selection.cursor, 1);
    assert_eq!(w.view(reader).unwrap().source.scroll_y, 0.0);
    assert_eq!(w.documents.len(), 1);
    assert_eq!(w.text_snapshots().len(), 1);
    assert_eq!(w.dirty_file_ids(), vec![document]);
    w.active_editor_mut().unwrap().undo();
    assert_eq!(w.view(reader).unwrap().reader.source_byte, 4);
    assert!(!w.is_dirty(document));
    w.active_editor_mut().unwrap().redo();
    assert_eq!(w.view(reader).unwrap().reader.source_byte, 7);
    w.mark_saved_with_disk(document, DiskState::from_contents("a你界b\r\n👩‍💻"));
    assert!(!w.is_view_dirty(reader));
    invariants(&w);
}

#[test]
fn p1_toggle_keeps_identity_and_both_positions_without_changing_text_history() {
    let mut w = Model::new();
    let pane = w.active_pane();
    let (document, view) = open(&mut w, "note.md", "中文\r\ntext");
    w.active_editor_mut().unwrap().select_range(0..6);
    w.set_scroll(pane, 7.0, 30.0);
    w.set_reader_position(
        view,
        ReaderPosition {
            source_byte: 8,
            block_fraction: 0.6,
        },
    );
    let before = w.view(view).unwrap().clone();
    let stamp = w.documents[&document].stamp(document);
    assert!(w.set_presentation(view, Presentation::Markdown));
    w.set_scroll(pane, 100.0, 200.0);
    assert!(w.set_presentation(view, Presentation::Source));
    let after = w.view(view).unwrap().clone();
    assert_eq!(before.id, after.id);
    assert_eq!(before.source, after.source);
    assert_eq!(before.reader, after.reader);
    assert_eq!(after.input_generation, 2);
    assert_eq!(stamp, w.documents[&document].stamp(document));
    assert!(!w.is_dirty(document));
    assert!(!w.active_editor().unwrap().can_undo());
}

#[test]
fn p1_copy_and_move_into_another_holder_never_merge_views() {
    let mut w = Model::new();
    let left = w.active_pane();
    let (document, original) = open(&mut w, "same.md", "text");
    let right = w.split(left, Direction::Right).unwrap();
    let copy = w.active().unwrap();
    w.active_editor_mut().unwrap().set_cursor(3);
    w.set_scroll(right, 8.0, 90.0);
    assert_ne!(copy, original);
    assert!(w.move_item(right, copy, left, 0));
    assert_eq!(w.pane(left).unwrap().items(), &[copy, original]);
    assert_eq!(w.view(copy).unwrap().source.selection.cursor, 3);
    assert_eq!(w.scroll(left), (8.0, 90.0));
    assert_eq!(w.tabs_of(document).len(), 2);
    assert!(w.released_by(&[(left, copy)]).is_empty());
    assert_eq!(
        w.released_by(&[(left, copy), (left, original), (left, copy)]),
        vec![document]
    );
    w.close_item(left, copy);
    assert!(w.documents.contains_key(&document));
    w.close_item(left, original);
    assert!(w.documents.is_empty());
    invariants(&w);
}

#[test]
fn p1_builtin_and_diff_have_no_text_document_and_cannot_be_copied() {
    let mut w = Model::new();
    let pane = w.active_pane();
    let settings = w.open_page(AppPage::Settings);
    assert_eq!(settings, w.open_settings());
    let diff = w.insert_diff(pane, FileMeta::from_path("change.txt".into()), Arc::new(()));
    assert!(!w.copy_item(pane, settings, pane, 0));
    assert!(!w.copy_item(pane, diff, pane, 0));
    assert!(!w.set_presentation(settings, Presentation::Markdown));
    assert!(!w.set_presentation(diff, Presentation::Markdown));
    assert!(w.documents.is_empty());
    assert!(w.paths.is_empty());
    assert!(w.text_snapshots().is_empty());
    assert!(w.active_save_snapshot().is_none());
    assert_eq!(w.diffs.len(), 1);
    w.close_item(pane, diff);
    assert!(w.diffs.is_empty());
    invariants(&w);
}

#[test]
fn p1_reload_clamps_hidden_source_and_reader_and_keeps_view_identity() {
    let mut w = Model::new();
    let pane = w.active_pane();
    let (document, source) = open(&mut w, "reload.md", "long 中文 text");
    w.active_editor_mut().unwrap().set_cursor(100);
    w.copy_item(pane, source, pane, 1);
    let reader = w.active().unwrap();
    w.set_presentation(reader, Presentation::Markdown);
    w.set_reader_position(
        reader,
        ReaderPosition {
            source_byte: 100,
            block_fraction: 0.8,
        },
    );
    w.reconcile_with_disk(
        document,
        DiskRead::Found {
            contents: "界".into(),
            state: DiskState::from_contents("界"),
            bytes: None,
        },
    );
    for id in [source, reader] {
        assert_eq!(w.view(id).unwrap().source.selection.cursor, 3);
        assert!(w.view(id).unwrap().reader.source_byte <= 3);
    }
    assert_eq!(w.active(), Some(reader));
    assert_eq!(w.view(reader).unwrap().reader.block_fraction, 0.8);
    invariants(&w);
}

#[test]
fn p1_deferred_install_keeps_document_lifetime_views_and_focus() {
    let mut w = Model::new();
    let pane = w.active_pane();
    let document = w.open_deferred("restore.md".into());
    let source = w.active().unwrap();
    let incarnation = w.documents[&document].incarnation;
    w.copy_item(pane, source, pane, 1);
    let reader = w.active().unwrap();
    w.set_presentation(reader, Presentation::Markdown);
    assert_eq!(w.loading_files(), vec![(document, "restore.md".into())]);
    assert!(w.install_restored(
        document,
        std::path::Path::new("restore.md"),
        PreparedTextFile::with_disk_state(
            "restore.md".into(),
            "ready".into(),
            DiskState::from_contents("ready")
        )
    ));
    assert_eq!(incarnation, w.documents[&document].incarnation);
    assert_eq!(w.active(), Some(reader));
    assert!(w.loading_files().is_empty());
    assert_eq!(w.documents[&document].buffer.text(), "ready");
    invariants(&w);
}

#[test]
fn p1_long_mixed_tab_lifecycle_keeps_membership_indexes_consistent() {
    let mut w = Model::new();
    for step in 0..160 {
        let pane = w.active_pane();
        let (_, view) = open(&mut w, &format!("{}.md", step % 7), "text");
        if step % 3 == 0 {
            w.copy_item(pane, view, pane, 0);
        }
        if step % 5 == 0 {
            w.split(pane, Direction::Right);
        }
        if step % 4 == 0 {
            w.move_active_item(Direction::Left);
        }
        if step % 2 == 0
            && let Some(id) = w.active()
        {
            let pane = w.active_pane();
            w.close_item(pane, id);
        }
        if w.pane_count() > 4 {
            let pane = w.pane_ids()[0];
            w.close_pane(pane);
        }
        invariants(&w);
    }
    for pane in w.pane_ids() {
        w.close_pane(pane);
    }
    assert!(w.views.is_empty());
    assert!(w.documents.is_empty());
    assert!(w.paths.is_empty());
    invariants(&w);
}

#[test]
fn p1_reader_anchor_follows_long_grouped_commands_beyond_revision_retention() {
    let mut w = Model::new();
    let pane = w.active_pane();
    let (_, source) = open(&mut w, "macro.md", "界");
    w.copy_item(pane, source, pane, 1);
    let reader = w.active().unwrap();
    w.set_reader_position(
        reader,
        ReaderPosition {
            source_byte: 3,
            block_fraction: 0.2,
        },
    );
    w.set_presentation(reader, Presentation::Markdown);
    w.activate(pane, source);
    {
        let mut editor = w.active_editor_mut().unwrap();
        editor.begin_group();
        for _ in 0..160 {
            editor.set_cursor(0);
            normal::insert(&mut editor, "你");
        }
        editor.end_group();
    }
    assert_eq!(w.view(reader).unwrap().reader.source_byte, 483);
    w.active_editor_mut().unwrap().undo();
    assert_eq!(w.view(reader).unwrap().reader.source_byte, 3);
    w.active_editor_mut().unwrap().redo();
    assert_eq!(w.view(reader).unwrap().reader.source_byte, 483);
}

#[test]
fn markdown_preview_associations_do_not_outlive_their_source_surface() {
    let mut workspace = Model::new();
    open(&mut workspace, "association.md", "# title");
    let source = workspace.active().unwrap();
    let pane = workspace.active_pane();
    assert!(workspace.copy_item(pane, source, pane, 1));
    let preview = workspace.active().unwrap();
    workspace.set_presentation(preview, Presentation::Markdown);
    workspace.views.get_mut(&preview).unwrap().associated_source = Some(source);
    workspace.set_presentation(source, Presentation::Markdown);
    assert_eq!(workspace.view(preview).unwrap().associated_source, None);
    workspace.set_presentation(source, Presentation::Source);
    workspace.views.get_mut(&preview).unwrap().associated_source = Some(source);
    workspace.close_item(pane, source);
    assert_eq!(workspace.view(preview).unwrap().associated_source, None);
    assert_eq!(workspace.documents.len(), 1);
}

#[test]
fn multicursor_split_selections_follow_shared_edits_and_hidden_reload_clamps_them() {
    use loog_editor_core::text::Selection;
    let mut w = Model::new();
    let left = w.active_pane();
    let (document, original) = open(&mut w, "multi.md", "abc\r\nxyz");
    w.active_editor_mut()
        .unwrap()
        .set_selections(Selection::caret(1), vec![Selection::caret(6)]);
    let right = w.split(left, Direction::Right).unwrap();
    let copy = w.active().unwrap();
    w.active_editor_mut()
        .unwrap()
        .set_selections(Selection::caret(0), vec![Selection::caret(5)]);
    normal::insert(&mut w.active_editor_mut().unwrap(), "中");
    assert_eq!(
        w.editor(left)
            .unwrap()
            .selections()
            .iter()
            .map(|s| s.cursor)
            .collect::<Vec<_>>(),
        [4, 12]
    );
    assert_eq!(
        w.editor(right)
            .unwrap()
            .selections()
            .iter()
            .map(|s| s.cursor)
            .collect::<Vec<_>>(),
        [3, 11]
    );
    w.active_editor_mut().unwrap().undo();
    assert_eq!(
        w.editor(left)
            .unwrap()
            .selections()
            .iter()
            .map(|s| s.cursor)
            .collect::<Vec<_>>(),
        [1, 6]
    );
    w.set_presentation(original, Presentation::Markdown);
    w.reconcile_with_disk(
        document,
        DiskRead::Found {
            contents: "界".into(),
            state: DiskState::from_contents("界"),
            bytes: None,
        },
    );
    assert!(
        w.view(original)
            .unwrap()
            .source
            .secondary
            .iter()
            .all(|s| s.cursor <= 3)
    );
    assert_eq!(w.view(copy).unwrap().document(), Some(document));
    invariants(&w);
}

#[test]
fn multicursor_document_edits_record_the_active_source_view_selection() {
    use loog_editor_core::text::{Assoc, Edit, EditKind, Selection};
    let mut w = Model::new();
    let left = w.active_pane();
    let (document, _) = open(&mut w, "active.md", "abc\nxyz");
    w.split(left, Direction::Right).unwrap();
    w.active_editor_mut()
        .unwrap()
        .set_selections(Selection::caret(1), vec![Selection::caret(5)]);
    let before = w.active_editor().unwrap().selections();
    let mut editor = w.document_editor_mut(document).unwrap();
    let primary = editor.selection_state();
    editor
        .edit(
            EditKind::Other,
            vec![Edit::insert(0, "// prefix\n")],
            |c, _| Selection::caret(c.map(primary.cursor, Assoc::After)),
        )
        .unwrap();
    drop(editor);
    w.active_editor_mut().unwrap().undo();
    assert_eq!(w.active_editor().unwrap().selections(), before);
}
