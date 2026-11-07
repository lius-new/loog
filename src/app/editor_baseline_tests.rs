//! Behaviour the editor must keep through the editing-core refactor.
//!
//! These tests drive the editor through the same entry points the UI uses
//! (editor commands, committed text, the workspace's per-pane editors), so
//! they stay valid while the layers underneath change.

use std::sync::Mutex;

use lgui::prelude::UiRect;
use lgui::services::{Clipboard, ClipboardError};

use crate::app::editor::{apply_command, insert_text};
use crate::app::state::AppState;
use crate::model::pane_layout::Direction;
use loog_editor_core::commands::Command;
use loog_editor_core::normal::Movement;

const RECT: UiRect = UiRect {
    left: 0.0,
    top: 0.0,
    right: 600.0,
    bottom: 400.0,
};

#[derive(Default)]
struct TestClipboard(Mutex<Option<String>>);

impl Clipboard for TestClipboard {
    fn read_text(&self) -> Result<Option<String>, ClipboardError> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn write_text(&self, text: &str) -> Result<(), ClipboardError> {
        *self.0.lock().unwrap() = Some(text.to_owned());
        Ok(())
    }
}

fn document(contents: &str) -> AppState {
    let mut app = AppState::new();
    app.workspace
        .open_path("baseline.txt".into(), contents.into());
    app
}

fn run(app: &mut AppState, command: Command) {
    apply_command(app, command, &TestClipboard::default()).unwrap();
}

fn text(app: &AppState) -> String {
    app.workspace.active_editor().unwrap().text().to_owned()
}

fn cursor(app: &AppState) -> usize {
    app.workspace.active_editor().unwrap().cursor()
}

fn set_cursor(app: &mut AppState, offset: usize) {
    app.workspace
        .active_editor_mut()
        .unwrap()
        .set_cursor(offset);
}

#[test]
fn crlf_is_one_line_break_for_navigation_and_deletion() {
    let mut app = document("ab\r\ncd\r\nef");
    run(&mut app, Command::Move(Movement::End, false));
    assert_eq!(cursor(&app), 2, "End stops before the CR");
    run(&mut app, Command::Move(Movement::Right, false));
    assert_eq!(cursor(&app), 4, "Right steps over CRLF at once");
    run(&mut app, Command::Delete(false, false));
    assert_eq!(text(&app), "abcd\r\nef");
    run(&mut app, Command::Move(Movement::End, false));
    run(&mut app, Command::Delete(true, false));
    assert_eq!(text(&app), "abcdef");
    run(&mut app, Command::Undo);
    run(&mut app, Command::Undo);
    assert_eq!(text(&app), "ab\r\ncd\r\nef");

    set_cursor(&mut app, 1);
    run(&mut app, Command::Move(Movement::Down, false));
    assert_eq!(cursor(&app), 5, "Down keeps the column across CRLF");
    run(&mut app, Command::Enter);
    assert_eq!(text(&app), "ab\r\nc\r\nd\r\nef");
}

#[test]
fn committed_text_merges_into_one_undo_step_until_the_caret_moves() {
    let mut app = document("");
    for typed in ["a", "b", "c"] {
        insert_text(&mut app, typed, RECT);
    }
    run(&mut app, Command::Move(Movement::Left, false));
    insert_text(&mut app, "x", RECT);
    assert_eq!(text(&app), "abxc");
    run(&mut app, Command::Undo);
    assert_eq!(text(&app), "abc");
    assert_eq!(cursor(&app), 2);
    run(&mut app, Command::Undo);
    assert_eq!(text(&app), "");
    run(&mut app, Command::Redo);
    run(&mut app, Command::Redo);
    assert_eq!(text(&app), "abxc");
}

#[test]
fn ime_commit_is_its_own_undo_step() {
    let mut app = document("");
    insert_text(&mut app, "a", RECT);
    let pane = app.workspace.active_pane();
    let document = app.workspace.active().unwrap();
    app.editor.ime = Some(crate::app::state::editor::ImeSession {
        input_generation: 0,
        pane,
        document,
        text: String::new(),
        cursor: None,
    });
    insert_text(&mut app, "你好", RECT);
    insert_text(&mut app, "b", RECT);
    assert_eq!(text(&app), "a你好b");
    run(&mut app, Command::Undo);
    assert_eq!(text(&app), "a你好");
    run(&mut app, Command::Undo);
    assert_eq!(text(&app), "a");
}

#[test]
fn cut_and_paste_are_separate_undo_steps_from_typing() {
    let mut app = document("");
    let clipboard = TestClipboard::default();
    insert_text(&mut app, "hello", RECT);
    app.workspace
        .active_editor_mut()
        .unwrap()
        .select_range(1..3);
    apply_command(&mut app, Command::Cut, &clipboard).unwrap();
    assert_eq!(text(&app), "hlo");
    apply_command(&mut app, Command::Paste, &clipboard).unwrap();
    apply_command(&mut app, Command::Paste, &clipboard).unwrap();
    assert_eq!(text(&app), "helello");
    run(&mut app, Command::Undo);
    assert_eq!(text(&app), "hello");
    run(&mut app, Command::Undo);
    assert_eq!(text(&app), "hlo");
    run(&mut app, Command::Undo);
    assert_eq!(text(&app), "hello");
    assert_eq!(
        app.workspace.active_editor().unwrap().selected_text(),
        Some("el")
    );
}

#[test]
fn edits_through_one_pane_keep_the_other_panes_selection_on_the_same_text() {
    let mut app = document("alpha beta gamma");
    let left = app.workspace.active_pane();
    app.workspace.editor_mut(left).unwrap().select_range(6..10);
    let right = app.workspace.split(left, Direction::Right).unwrap();
    app.workspace.editor_mut(right).unwrap().set_cursor(0);

    insert_text(&mut app, ">> ", RECT);
    assert_eq!(
        app.workspace.editor(left).unwrap().selected_text(),
        Some("beta")
    );

    app.workspace.editor_mut(right).unwrap().select_range(9..19);
    insert_text(&mut app, "X", RECT);
    assert_eq!(text(&app), ">> alpha X");
    assert_eq!(app.workspace.editor(left).unwrap().cursor(), 9);
    assert_eq!(app.workspace.editor(left).unwrap().selection(), None);

    app.workspace.activate_pane(left);
    app.workspace.editor_mut(left).unwrap().select_range(0..1);
    run(&mut app, Command::Indent(false));
    assert_eq!(text(&app), "  >> alpha X");
    assert_eq!(app.workspace.editor(right).unwrap().cursor(), 12);
}

#[test]
fn undo_restores_the_undoing_pane_and_shifts_the_other_panes() {
    let mut app = document("one two");
    let left = app.workspace.active_pane();
    let right = app.workspace.split(left, Direction::Right).unwrap();
    app.workspace.editor_mut(left).unwrap().set_cursor(3);
    app.workspace.editor_mut(right).unwrap().set_cursor(7);

    app.workspace.activate_pane(left);
    insert_text(&mut app, " big", RECT);
    assert_eq!(app.workspace.editor(right).unwrap().cursor(), 11);

    // Undo from the other pane: the history belongs to the document, and the
    // undoing pane takes the selection recorded before the change.
    app.workspace.activate_pane(right);
    run(&mut app, Command::Undo);
    assert_eq!(text(&app), "one two");
    assert_eq!(app.workspace.editor(right).unwrap().cursor(), 3);
    assert_eq!(app.workspace.editor(left).unwrap().cursor(), 3);

    run(&mut app, Command::Redo);
    assert_eq!(text(&app), "one big two");
    app.workspace.activate_pane(left);
    run(&mut app, Command::Undo);
    assert_eq!(text(&app), "one two");
}

#[test]
fn pasting_follows_the_documents_line_endings() {
    let mut app = document("a\r\nb");
    let clipboard = TestClipboard::default();
    clipboard.write_text("1\n2\r3").unwrap();
    set_cursor(&mut app, 1);
    apply_command(&mut app, Command::Paste, &clipboard).unwrap();
    assert_eq!(text(&app), "a1\r\n2\r\n3\r\nb");

    let mut app = document("a\nb");
    clipboard.write_text("1\r\n2").unwrap();
    set_cursor(&mut app, 1);
    apply_command(&mut app, Command::Paste, &clipboard).unwrap();
    assert_eq!(text(&app), "a1\n2\nb");
}

#[test]
fn outdent_is_one_undo_step_and_each_word_deletion_is_another() {
    let mut app = document("    one\n\ttwo\nthree");
    run(&mut app, Command::SelectAll);
    run(&mut app, Command::Indent(true));
    assert_eq!(text(&app), "  one\ntwo\nthree");
    run(&mut app, Command::Undo);
    assert_eq!(text(&app), "    one\n\ttwo\nthree");

    run(&mut app, Command::Move(Movement::Finish, false));
    run(&mut app, Command::Delete(false, true));
    run(&mut app, Command::Delete(false, true));
    assert_eq!(text(&app), "    one\n\t");
    run(&mut app, Command::Undo);
    assert_eq!(text(&app), "    one\n\ttwo\n");
    run(&mut app, Command::Undo);
    assert_eq!(text(&app), "    one\n\ttwo\nthree");
}

#[test]
fn saving_and_undoing_back_to_saved_text_tracks_dirty_state() {
    let mut app = document("saved");
    let id = app.workspace.active().unwrap();
    insert_text(&mut app, "x", RECT);
    assert!(app.workspace.is_view_dirty(id));
    run(&mut app, Command::Undo);
    assert!(!app.workspace.is_view_dirty(id));
    run(&mut app, Command::Redo);
    assert!(app.workspace.is_view_dirty(id));
}

#[test]
fn grapheme_clusters_move_and_delete_as_one_character() {
    let mut app = document("a👍🏽e\u{301}z");
    run(&mut app, Command::Move(Movement::Right, false));
    run(&mut app, Command::Move(Movement::Right, false));
    assert_eq!(cursor(&app), 1 + "👍🏽".len());
    run(&mut app, Command::Delete(true, false));
    assert_eq!(text(&app), "a👍🏽z");
    set_cursor(&mut app, 3);
    assert_eq!(
        cursor(&app),
        1,
        "a caret inside a cluster snaps to its start"
    );
    set_cursor(&mut app, 1 + "👍🏽".len());
    run(&mut app, Command::Delete(false, false));
    assert_eq!(text(&app), "az");
}

/// Timing and history-memory baseline on large documents. Run with
/// `cargo test --release perf_baseline -- --ignored --nocapture`.
#[test]
#[ignore]
fn perf_baseline() {
    use std::time::Instant;

    for megabytes in [1, 10] {
        let line = "let value = compute(alpha, beta, gamma); // baseline text\n";
        let contents = line.repeat(megabytes * 1024 * 1024 / line.len());
        let middle = contents.len() / 2 / line.len() * line.len();
        let mut app = document(&contents);
        let clipboard = TestClipboard::default();

        set_cursor(&mut app, middle);
        let start = Instant::now();
        for _ in 0..200 {
            insert_text(&mut app, "x", RECT);
        }
        let typing = start.elapsed() / 200;

        clipboard
            .write_text(&line.repeat(1024 * 1024 / line.len()))
            .unwrap();
        let start = Instant::now();
        apply_command(&mut app, Command::Paste, &clipboard).unwrap();
        let paste = start.elapsed();

        let start = Instant::now();
        run(&mut app, Command::Undo);
        let undo = start.elapsed();

        let start = Instant::now();
        run(&mut app, Command::SelectAll);
        run(&mut app, Command::Indent(false));
        let indent = start.elapsed();

        let history = app
            .workspace
            .active_editor()
            .unwrap()
            .buffer()
            .history_bytes();
        println!(
            "{megabytes} MB: typing {typing:?}/char, paste 1 MB {paste:?}, undo {undo:?}, \
             select-all indent {indent:?}, history {:.1} MB",
            history as f64 / 1024.0 / 1024.0
        );
    }
}
