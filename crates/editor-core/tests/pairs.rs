use loog_editor_core::{
    normal,
    text::{EditKind, EditorMut, Selection, TextBuffer},
};

#[test]
fn pair_tracking_survives_history_nested_edits_and_peer_positions() {
    let mut text = TextBuffer::new("");
    let mut selection = Selection::default();
    let mut peer = Selection::default();
    let mut ed = EditorMut::new(&mut text, &mut selection, vec![&mut peer]);
    normal::type_text(&mut ed, "{", "rust");
    normal::type_text(&mut ed, "(", "rust");
    normal::insert(&mut ed, "中e\u{301}😀");
    assert_eq!(ed.text(), "{(中e\u{301}😀)}");
    normal::type_text(&mut ed, ")", "rust");
    normal::type_text(&mut ed, "}", "rust");
    assert_eq!(ed.cursor(), ed.len());
    ed.undo();
    assert_eq!(ed.text(), "{()}");
    assert!(ed.is_empty_auto_pair(ed.cursor()));
    normal::backspace(&mut ed);
    assert_eq!(ed.text(), "{}");
    ed.undo();
    ed.redo();
    normal::backspace(&mut ed);
    assert_eq!(ed.text(), "");
    ed.undo();
    ed.set_cursor(1);
    normal::type_text(&mut ed, "}", "rust");
    assert_eq!(ed.text(), "{}");

    assert!(peer.cursor <= text.len());
    assert_eq!(TextBuffer::new("{}").text(), "{}");
}

#[test]
fn replacing_a_delimiter_invalidates_it_and_undo_restores_its_provenance() {
    let mut text = TextBuffer::new("");
    let mut selection = Selection::default();
    let mut ed = EditorMut::new(&mut text, &mut selection, vec![]);
    normal::type_text(&mut ed, "(", "rust");
    ed.replace(1..2, "]", EditKind::Other).unwrap();
    assert!(!ed.is_auto_close(1));
    ed.undo();
    assert!(ed.is_auto_close(1));
    ed.set_cursor(0);
    normal::insert(&mut ed, "文");
    ed.set_cursor(4);
    normal::type_text(&mut ed, ")", "rust");
    assert_eq!(ed.text(), "文()");
    assert_eq!(ed.cursor(), 5);
}

#[test]
fn wrapping_preserves_selection_direction_unicode_and_one_step_undo() {
    for reversed in [false, true] {
        let mut text = TextBuffer::new("中e\u{301}😀\r\n尾");
        let end = text.len();
        let original = if reversed {
            Selection::spanning(end, 0)
        } else {
            Selection::spanning(0, end)
        };
        let mut selection = original;
        let mut ed = EditorMut::new(&mut text, &mut selection, vec![]);
        normal::type_text(&mut ed, "[", "rust");
        assert_eq!(ed.selected_text(), Some("中e\u{301}😀\r\n尾"));
        assert_eq!(
            ed.selection_state().cursor < ed.selection_state().anchor.unwrap(),
            reversed
        );
        ed.undo();
        assert_eq!(ed.selection_state(), original);
        ed.redo();
        assert_eq!(ed.text(), "[中e\u{301}😀\r\n尾]");
    }
}

#[test]
fn literal_closers_ime_text_strings_comments_and_lifetimes_stay_literal() {
    for (prefix, input, lang) in [
        ("", ")", "rust"),
        ("(", ")", "rust"),
        ("// ", "{", "rust"),
        ("/* ", "[", "rust"),
        ("\"text ", "(", "javascript"),
        ("# ", "{", "python"),
        ("-- ", "(", "sql"),
        ("can", "'", "javascript"),
        ("'", "{", "rust"),
        ("&", "'", "rust"),
        ("", "'", "text"),
        ("", "中文()", "rust"),
        ("\"escape\\\"", "{", "rust"),
    ] {
        let mut text = TextBuffer::new(prefix);
        let mut selection = Selection::caret(prefix.len());
        let mut ed = EditorMut::new(&mut text, &mut selection, vec![]);
        normal::type_text(&mut ed, input, lang);
        assert_eq!(ed.text(), format!("{prefix}{input}"), "{lang} {prefix:?}");
    }
    for (prefix, input, expected, language) in [
        ("// comment\n", "{", "// comment\n{}", "rust"),
        (
            "/* outer /* nested */ */",
            "[",
            "/* outer /* nested */ */[]",
            "rust",
        ),
        ("", "'", "''", "python"),
        ("", "\"", "\"\"", "rust"),
    ] {
        let mut text = TextBuffer::new(prefix);
        let mut selection = Selection::caret(prefix.len());
        let mut ed = EditorMut::new(&mut text, &mut selection, vec![]);
        normal::type_text(&mut ed, input, language);
        assert_eq!(ed.text(), expected);
    }
}
