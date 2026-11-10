use loog_editor_core::{
    normal,
    text::{EditingOptions, EditorMut, LineEnding, Selection, TextBuffer},
};

#[test]
fn ending_conversion_preserves_peer_selections_and_encoding_metadata_in_one_step() {
    let mut text = TextBuffer::new("中\ne\u{301}😀\r\n尾");
    text.initialize_metadata(7);
    let original = text.text().to_owned();
    let mut selection = Selection::spanning(original.len(), 0);
    let mut peer = Selection::caret(4);
    {
        let mut ed = EditorMut::new(&mut text, &mut selection, vec![&mut peer]);
        assert_eq!(ed.line_ending_label(), "Mixed");
        ed.set_line_ending(LineEnding::Crlf).unwrap();
        assert_eq!(ed.text(), "中\r\ne\u{301}😀\r\n尾");
        assert_eq!(ed.metadata() & 255, 7);
        assert_eq!(ed.line_ending_label(), "CRLF");
        assert_eq!(ed.selected_text(), Some("中\r\ne\u{301}😀\r\n尾"));
        ed.undo();
        assert_eq!(ed.text(), original);
        assert_eq!(ed.metadata(), 7);
        assert_eq!(ed.line_ending_label(), "Mixed");
        ed.redo();
        assert_eq!(ed.chosen_line_ending(), Some(LineEnding::Crlf));
        ed.undo();
    }
    assert_eq!(peer.cursor, 4);
    assert_eq!(selection, Selection::spanning(original.len(), 0));
}

#[test]
fn empty_single_line_and_no_final_newline_keep_policy_across_history() {
    for source in ["", "单行", "a\nb", "a\nb\n"] {
        let mut text = TextBuffer::new(source);
        let mut selection = Selection::caret(source.len());
        let mut ed = EditorMut::new(&mut text, &mut selection, vec![]);
        ed.set_line_ending(LineEnding::Crlf).unwrap();
        assert_eq!(ed.text(), source.replace('\n', "\r\n"));
        normal::enter(&mut ed);
        assert!(ed.text().ends_with("\r\n"));
        ed.undo();
        ed.undo();
        assert_eq!(ed.text(), source);
        assert_eq!(ed.line_ending(), "\n");
        ed.redo();
        assert_eq!(ed.line_ending(), "\r\n");
    }
}

#[test]
fn brace_enter_indent_outdent_and_tab_columns_use_independent_preferences() {
    for use_tabs in [false, true] {
        for indent_size in [2, 4] {
            for ending in [LineEnding::Lf, LineEnding::Crlf] {
                let mut text = TextBuffer::new("{}");
                text.configure(EditingOptions {
                    use_tabs,
                    indent_size,
                    tab_width: 8,
                    ..Default::default()
                });
                text.initialize_line_ending(ending);
                let indent = if use_tabs {
                    "\t".into()
                } else {
                    " ".repeat(indent_size)
                };
                let mut selection = Selection::caret(1);
                let mut ed = EditorMut::new(&mut text, &mut selection, vec![]);
                normal::enter(&mut ed);
                assert_eq!(
                    ed.text(),
                    format!("{{{}{indent}{}}}", ending.text(), ending.text())
                );
                assert_eq!(
                    ed.display_column(ed.cursor()),
                    if use_tabs { 8 } else { indent_size }
                );
                ed.undo();
                assert_eq!(ed.text(), "{}");
                assert_eq!(ed.cursor(), 1);
                ed.redo();
                ed.select_all();
                let direction = ed.selection_state();
                normal::indent(&mut ed, false);
                normal::indent(&mut ed, true);
                assert_eq!(ed.selection_state(), direction);
            }
        }
    }
}

#[test]
fn configurable_sparse_tab_indexes_and_mixed_endings_survive_edits() {
    let source = format!("{}\r\nlast\n", "文\txy".repeat(1000));
    let mut text = TextBuffer::new(&source);
    text.configure(EditingOptions {
        tab_width: 8,
        ..Default::default()
    });
    let mut selection = Selection::default();
    let mut ed = EditorMut::new(&mut text, &mut selection, vec![]);
    for i in 0..20 {
        ed.set_cursor(i * 5);
        normal::insert(&mut ed, "a");
    }
    let mut rebuilt = TextBuffer::new(ed.text());
    rebuilt.configure(ed.options().clone());
    assert_eq!(
        ed.max_line_display_width(),
        rebuilt.max_line_display_width()
    );
    for col in [0, 8, 40, 400, 4000] {
        assert_eq!(
            ed.offset_at_display_column(0, col),
            rebuilt.offset_at_display_column(0, col)
        );
    }
    assert_eq!(ed.line_ending_label(), "Mixed");
    ed.set_line_ending(LineEnding::Lf).unwrap();
    assert_eq!(ed.line_ending_label(), "LF");
}
