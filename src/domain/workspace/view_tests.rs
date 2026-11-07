//! P1 invariants run against the production workspace, without a GUI adapter.
use crate::domain::document::{DiskRead, DiskState, FileMeta};
use crate::domain::identity::{FileId, ViewId};
use crate::domain::pane_layout::Direction;
use crate::domain::view::{Presentation, ReaderPosition, ViewTarget};
use crate::domain::workspace::{AppPage, OpenMode, PreparedTextFile, Workspace};
use loog_editor_core::normal;
use std::collections::HashSet;
use std::sync::Arc;

type Model = Workspace<(), ()>;
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
        assert_eq!(w.paths.get(&d.meta.path), Some(&id));
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
