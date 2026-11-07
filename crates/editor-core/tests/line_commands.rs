use loog_editor_core::{
    normal::{LineOperation as Op, comment, lines},
    text::{EditorMut, Selection, TextBuffer},
};

#[test]
fn line_moves_preserve_unicode_selection_direction_end_boundaries_and_no_final_newline() {
    for eol in ["\n", "\r\n"] {
        for reverse in [false, true] {
            let source = format!("头{eol}中😀{eol}尾");
            let mut text = TextBuffer::new(&source);
            let start = "头".len() + eol.len();
            let end = start + "中😀".len() + eol.len();
            let initial = if reverse {
                Selection::spanning(end, start)
            } else {
                Selection::spanning(start, end)
            };
            let mut selection = initial;
            let mut ed = EditorMut::new(&mut text, &mut selection, vec![]);
            lines(&mut ed, Op::MoveDown);
            assert_eq!(ed.text(), format!("头{eol}尾{eol}中😀"));
            assert_eq!(ed.selected_text(), Some("中😀"));
            assert_eq!(
                ed.selection_state().cursor < ed.selection_state().anchor.unwrap(),
                reverse
            );
            ed.undo();
            assert_eq!(ed.text(), source);
            assert_eq!(ed.selection_state(), initial);
            lines(&mut ed, Op::MoveUp);
            assert_eq!(ed.text(), format!("中😀{eol}头{eol}尾"));
            ed.undo();
            assert_eq!(ed.text(), source);
        }
    }
}
#[test]
fn copying_and_deleting_eof_empty_files_and_trailing_empty_lines_undo_once() {
    for source in ["", "中", "a\nb", "a\r\nb", "a\n"] {
        for op in [Op::CopyUp, Op::CopyDown, Op::Delete] {
            let mut text = TextBuffer::new(source);
            let mut selection = Selection::caret(source.len());
            let mut ed = EditorMut::new(&mut text, &mut selection, vec![]);
            lines(&mut ed, op);
            assert!(ed.cursor() <= ed.len());
            if ed.can_undo() {
                ed.undo();
                assert_eq!(ed.text(), source);
                assert_eq!(ed.cursor(), source.len());
                ed.redo();
            }
        }
    }
    let mut text = TextBuffer::new("a\nb");
    let mut selection = Selection::caret(2);
    let mut ed = EditorMut::new(&mut text, &mut selection, vec![]);
    lines(&mut ed, Op::CopyDown);
    assert_eq!(ed.text(), "a\nb\nb");
    assert_eq!(ed.cursor(), 4);
    lines(&mut ed, Op::Delete);
    assert_eq!(ed.text(), "a\nb");
}
#[test]
fn multi_line_copy_move_and_comments_keep_selection_and_history() {
    let mut text = TextBuffer::new("first\n  a\n  // b\n\nlast");
    let mut selection = Selection::spanning(18, 6);
    let mut ed = EditorMut::new(&mut text, &mut selection, vec![]);
    let source = ed.text().to_owned();
    comment(&mut ed, "//");
    assert_eq!(ed.text(), "first\n  // a\n  // // b\n\nlast");
    comment(&mut ed, "//");
    assert_eq!(ed.text(), source);
    ed.undo();
    ed.undo();
    assert_eq!(ed.text(), source);
    ed.select_range(6..17);
    lines(&mut ed, Op::CopyUp);
    assert_eq!(ed.text(), "first\n  a\n  // b\n  a\n  // b\n\nlast");
    ed.undo();
    assert_eq!(ed.text(), source);
    ed.set_cursor(0);
    lines(&mut ed, Op::MoveUp);
    assert_eq!(ed.text(), source);
    ed.set_cursor(ed.len());
    lines(&mut ed, Op::MoveDown);
    assert_eq!(ed.text(), source);
}
