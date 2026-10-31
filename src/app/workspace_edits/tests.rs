use super::*;
use crate::app::language_ui::edit::undo_redo;
use crate::app::state::AppState;
use std::path::PathBuf;

fn app_with(files: &[(&str, &str)]) -> (AppState, Vec<PathBuf>) {
    let mut app = AppState::new();
    let mut paths = Vec::new();
    for (name, text) in files {
        let path = std::env::temp_dir().join(format!("loom-wsedit-{}-{name}", std::process::id()));
        app.workspace.open_path(path.clone(), (*text).into());
        paths.push(path);
    }
    (app, paths)
}

fn id_of(app: &AppState, path: &std::path::Path) -> FileId {
    app.workspace.file_id_for_path(path).unwrap()
}

fn text_of(app: &AppState, path: &std::path::Path) -> String {
    app.workspace.documents[&id_of(app, path)]
        .buffer
        .text()
        .to_owned()
}

fn change(
    app: &AppState,
    path: &std::path::Path,
    edits: Vec<(Range<usize>, &str)>,
) -> DocumentChange {
    let file = id_of(app, path);
    DocumentChange {
        file,
        stamp: app.workspace.documents[&file].stamp(file),
        edits: edits.into_iter().map(|(r, t)| (r, t.to_owned())).collect(),
    }
}

fn transaction(documents: Vec<DocumentChange>) -> TextTransaction {
    TextTransaction {
        title: "Test".into(),
        documents,
    }
}

#[test]
fn several_documents_commit_together_and_undo_together() {
    let (mut app, paths) = app_with(&[("a.txt", "one two"), ("b.txt", "three four")]);
    let tx = transaction(vec![
        change(&app, &paths[0], vec![(0..3, "ONE")]),
        change(&app, &paths[1], vec![(6..10, "FOUR")]),
    ]);
    let receipt = commit(&mut app, &tx, &EditLimits::default()).unwrap();
    assert_eq!(receipt.versions.len(), 2);
    assert_eq!(text_of(&app, &paths[0]), "ONE two");
    assert_eq!(text_of(&app, &paths[1]), "three FOUR");
    assert_eq!(undo_redo(&mut app, true), Some(Ok(())));
    assert_eq!(text_of(&app, &paths[0]), "one two");
    assert_eq!(text_of(&app, &paths[1]), "three four");
}

#[test]
fn a_stale_stamp_changes_nothing() {
    let (mut app, paths) = app_with(&[("c.txt", "text"), ("d.txt", "more")]);
    let tx = transaction(vec![
        change(&app, &paths[0], vec![(0..1, "T")]),
        change(&app, &paths[1], vec![(0..1, "M")]),
    ]);
    // The person types into the second document after the snapshot was taken.
    let id = id_of(&app, &paths[1]);
    app.workspace.document_editor_mut(id).unwrap().set_cursor(4);
    loom_editor_core::normal::insert(&mut app.workspace.document_editor_mut(id).unwrap(), "!");
    let error = commit(&mut app, &tx, &EditLimits::default()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::StaleSnapshot);
    assert_eq!(text_of(&app, &paths[0]), "text");
    assert_eq!(text_of(&app, &paths[1]), "more!");
}

#[test]
fn an_invalid_edit_in_a_later_document_leaves_every_document_untouched() {
    let (mut app, paths) = app_with(&[("e.txt", "alpha"), ("f.txt", "beta")]);
    let tx = transaction(vec![
        change(&app, &paths[0], vec![(0..2, "xx")]),
        change(&app, &paths[1], vec![(0..3, "a"), (2..4, "b")]),
    ]);
    let error = commit(&mut app, &tx, &EditLimits::default()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidEdit);
    assert_eq!(text_of(&app, &paths[0]), "alpha");
    assert_eq!(text_of(&app, &paths[1]), "beta");
    assert_eq!(
        app.editor.language.next_transaction, 0,
        "no transaction was consumed"
    );
}

#[test]
fn edits_inside_a_character_are_rejected() {
    let (mut app, paths) = app_with(&[("g.txt", "中文")]);
    let tx = transaction(vec![change(&app, &paths[0], vec![(1..2, "x")])]);
    assert_eq!(
        commit(&mut app, &tx, &EditLimits::default())
            .unwrap_err()
            .kind,
        ErrorKind::InvalidEdit
    );
}

#[test]
fn admission_limits_reject_before_anything_changes() {
    let (mut app, paths) = app_with(&[("h.txt", "abc"), ("i.txt", "def")]);
    let tx = transaction(vec![
        change(&app, &paths[0], vec![(0..1, "A")]),
        change(&app, &paths[1], vec![(0..1, "D")]),
    ]);
    let limits = EditLimits {
        documents: 1,
        ..EditLimits::default()
    };
    let error = commit(&mut app, &tx, &limits).unwrap_err();
    assert_eq!(error.kind, ErrorKind::LimitExceeded);
    assert!(error.message.contains("documents"));
    assert_eq!(text_of(&app, &paths[0]), "abc");
}

#[test]
fn the_same_document_twice_is_refused() {
    let (mut app, paths) = app_with(&[("j.txt", "abc")]);
    let one = change(&app, &paths[0], vec![(0..1, "A")]);
    let tx = transaction(vec![one.clone(), one]);
    assert_eq!(
        commit(&mut app, &tx, &EditLimits::default())
            .unwrap_err()
            .kind,
        ErrorKind::InvalidEdit
    );
}

#[test]
fn inserted_text_takes_the_documents_line_ending() {
    let (mut app, paths) = app_with(&[("k.txt", "a\r\nb")]);
    let tx = transaction(vec![change(&app, &paths[0], vec![(1..1, "\nx")])]);
    commit(&mut app, &tx, &EditLimits::default()).unwrap();
    assert_eq!(text_of(&app, &paths[0]), "a\r\nx\r\nb");
}

#[test]
fn the_edit_is_a_single_undo_step_and_keeps_the_document_unsaved() {
    let (mut app, paths) = app_with(&[("l.txt", "hello")]);
    let tx = transaction(vec![change(&app, &paths[0], vec![(0..5, "HELLO")])]);
    commit(&mut app, &tx, &EditLimits::default()).unwrap();
    let id = id_of(&app, &paths[0]);
    assert!(app.workspace.is_dirty(id));
    app.workspace.document_editor_mut(id).unwrap().undo();
    assert_eq!(text_of(&app, &paths[0]), "hello");
}
