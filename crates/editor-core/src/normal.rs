//! Default editing behaviour: what typing, deleting, moving and indenting
//! mean in an ordinary editor, expressed as edits on the editing core.

mod lines;
pub use self::lines::{LineOperation, comment, lines};

use crate::text::{Assoc, Edit, EditKind, EditorMut, Selection};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Movement {
    Left,
    Right,
    WordLeft,
    WordRight,
    Up,
    Down,
    Home,
    End,
    Start,
    Finish,
    PageUp(usize),
    PageDown(usize),
}

/// Spaces one indentation step inserts.
pub const INDENT: &str = "  ";

/// Insert typed or committed text over the selection. Plain characters join
/// the current typing run; line breaks and tabs are undo steps of their own.
pub fn insert(editor: &mut EditorMut<'_>, text: &str) {
    if editor.has_secondary() {
        crate::text::multiple::insert(
            editor,
            text,
            if text.contains(['\n', '\r', '\t']) {
                EditKind::Other
            } else {
                EditKind::Typing
            },
        );
        return;
    }
    let kind = if text.contains(['\n', '\r', '\t']) {
        EditKind::Other
    } else {
        EditKind::Typing
    };
    // Selection-relative edits on a validated buffer cannot fail.
    let _ = editor.replace_selection(text, kind);
}

/// Type one key with delimiter pairing. IME commits and paste use literal insertion.
pub fn type_text(editor: &mut EditorMut<'_>, text: &str, language: &str) {
    if editor.has_secondary() {
        crate::text::multiple::type_text(editor, text, language);
        return;
    }
    let mut chars = text.chars();
    let Some(ch) = chars.next() else {
        return;
    };
    if chars.next().is_some() {
        insert(editor, text);
        return;
    }
    let cursor = editor.cursor();
    if editor.selection().is_none()
        && editor.is_auto_close(cursor)
        && editor.text()[cursor..].starts_with(ch)
        && matches!(ch, ')' | ']' | '}' | '\'' | '"')
    {
        editor.set_cursor(cursor + ch.len_utf8());
        return;
    }
    if editor.selection().is_none() && matches!(ch, '}' | ']' | ')') {
        let start = editor.line_start(editor.line_of(cursor));
        let prefix = &editor.text()[start..cursor];
        if !prefix.is_empty()
            && prefix.chars().all(|c| c == ' ' || c == '\t')
            && code_context(editor.text(), cursor, language)
        {
            let removed = if prefix.ends_with('\t') {
                1
            } else {
                prefix.len().min(editor.options().indent_size)
            };
            let value = format!("{}{ch}", &prefix[..prefix.len() - removed]);
            let _ = editor.replace(start..cursor, &value, EditKind::Other);
            editor.break_undo_group();
            return;
        }
    }
    if !editor.options().auto_pairs {
        insert(editor, text);
        return;
    }
    let close = match ch {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '"' if quotes(language) => '"',
        '\'' if quotes(language) && (language != "rust" || editor.selection().is_some()) => '\'',
        _ => {
            insert(editor, text);
            return;
        }
    };
    let range = editor.selection().unwrap_or(cursor..cursor);
    let before = editor.selection_state();
    let previous = editor.text()[..range.start].chars().next_back();
    let next = editor.text()[range.end..].chars().next();
    let quote = ch == '\'' || ch == '"';
    if (language == "rust" && previous == Some('\''))
        || !code_context(editor.text(), range.start, language)
        || (quote && previous.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '\\'))
        || (range.is_empty()
            && next.is_some_and(|c| {
                !c.is_whitespace() && !matches!(c, ')' | ']' | '}' | ',' | ';' | ':')
            }))
    {
        insert(editor, text);
        return;
    }
    let content = &editor.text()[range.clone()];
    let value = format!("{ch}{content}{close}");
    let open = range.start;
    let end = open + 1 + content.len();
    let selected = !range.is_empty();
    let _ = editor.edit(
        EditKind::Other,
        vec![Edit::replace(range.clone(), value)],
        |_, _| {
            if selected {
                if before.cursor < before.anchor.unwrap_or(before.cursor) {
                    Selection::spanning(end, open + 1)
                } else {
                    Selection::spanning(open + 1, end)
                }
            } else {
                Selection::caret(open + 1)
            }
        },
    );
    editor.track_pair(open, end);
    editor.break_undo_group();
}

pub(crate) fn quotes(language: &str) -> bool {
    !matches!(language, "text" | "plain" | "plaintext" | "markdown" | "")
}

/// Bounded lexical fallback: uncertainty in long prefixes disables pairing.
/// Comments and strings deliberately suppress pairs until a reliable boundary.
pub(crate) fn code_context(text: &str, end: usize, language: &str) -> bool {
    if end > 64 * 1024 {
        return false;
    }
    let hash = matches!(
        language,
        "python" | "ruby" | "shell" | "bash" | "toml" | "yaml"
    );
    let dash = matches!(language, "sql" | "lua" | "haskell");
    let slash = !hash && !matches!(language, "text" | "plain" | "plaintext" | "markdown" | "");
    let bytes = &text.as_bytes()[..end];
    let (mut i, mut quote, mut block, mut line, mut escaped) = (0, 0, 0usize, false, false);
    while i < bytes.len() {
        let c = bytes[i];
        let n = bytes.get(i + 1).copied().unwrap_or(0);
        if line {
            if c == b'\n' {
                line = false;
            }
            i += 1;
            continue;
        }
        if block > 0 {
            if c == b'/' && n == b'*' {
                block += 1;
                i += 2;
            } else if c == b'*' && n == b'/' {
                block -= 1;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if quote != 0 {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == quote {
                quote = 0;
            }
            i += 1;
            continue;
        }
        if (hash && c == b'#')
            || (dash && c == b'-' && n == b'-')
            || (slash && c == b'/' && n == b'/')
        {
            line = true;
        } else if slash && c == b'/' && n == b'*' {
            block = 1;
            i += 1;
        } else if quotes(language) && (c == b'"' || c == b'\'' && language != "rust" || c == b'`') {
            quote = c;
        }
        i += 1;
    }
    !line && block == 0 && quote == 0
}

/// Paste text, converting its line breaks to the document's convention.
pub fn paste(editor: &mut EditorMut<'_>, text: &str) {
    if editor.has_secondary() {
        crate::text::multiple::paste(editor, text);
        return;
    }
    let newline = editor.line_ending();
    let normalized = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', newline);
    let _ = editor.replace_selection(&normalized, EditKind::Other);
    editor.break_undo_group();
}

pub fn backspace(editor: &mut EditorMut<'_>) {
    erase(editor, false, false);
}

pub fn delete_forward(editor: &mut EditorMut<'_>) {
    erase(editor, true, false);
}

/// Delete the selection, or one character or word before or after the caret.
pub fn erase(editor: &mut EditorMut<'_>, forward: bool, word: bool) {
    if editor.has_secondary() {
        crate::text::multiple::erase(editor, forward, word);
        return;
    }
    let cursor = editor.cursor();
    if !forward && !word && editor.selection().is_none() && editor.is_empty_auto_pair(cursor) {
        let _ = editor.replace(cursor - 1..cursor + 1, "", EditKind::Other);
        editor.break_undo_group();
        return;
    }
    let range = editor.selection().unwrap_or_else(|| match (forward, word) {
        (true, true) => cursor..editor.word_right(cursor),
        (true, false) => cursor..editor.next_grapheme(cursor),
        (false, true) => editor.word_left(cursor)..cursor,
        (false, false) => editor.prev_grapheme(cursor)..cursor,
    });
    let kind = match (forward, word) {
        (_, true) => EditKind::Other,
        (true, false) => EditKind::Delete,
        (false, false) => EditKind::Backspace,
    };
    let _ = editor.replace(range, "", kind);
}

/// Break the line, carrying over the current line's indentation.
pub fn enter(editor: &mut EditorMut<'_>) {
    if editor.has_secondary() {
        crate::text::multiple::enter(editor);
        return;
    }
    let start = editor
        .selection()
        .map_or(editor.cursor(), |range| range.start);
    let line_start = editor.line_start(editor.line_of(start));
    let indent: String = editor.text()[line_start..start]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    let newline = editor.line_ending();
    let range = editor.selection().unwrap_or(start..start);
    let before = editor.text()[line_start..start].trim_end();
    let after = editor.text()[range.end..].trim_start_matches([' ', '\t']);
    let opening = before.chars().next_back();
    let closing = after.chars().next();
    let pair = matches!(
        (opening, closing),
        (Some('{'), Some('}')) | (Some('['), Some(']')) | (Some('('), Some(')'))
    );
    let extra = if code_context(editor.text(), start, &editor.options().language)
        && (matches!(opening, Some('{' | '[' | '('))
            || opening == Some(':')
                && matches!(editor.options().language.as_ref(), "python" | "yaml"))
    {
        editor.options().indent()
    } else {
        String::new()
    };
    let value = if pair {
        format!("{newline}{indent}{extra}{newline}{indent}")
    } else {
        format!("{newline}{indent}{extra}")
    };
    let cursor = start + newline.len() + indent.len() + extra.len();
    let _ = editor.edit(
        EditKind::Other,
        vec![Edit::replace(range, value)],
        |_, _| Selection::caret(cursor),
    );
    editor.break_undo_group();
}

/// Indent or outdent every line the selection touches, as one undo step.
/// Without a selection, Tab inserts spaces at the caret instead.
pub fn indent(editor: &mut EditorMut<'_>, outdent: bool) {
    if editor.has_secondary() {
        crate::text::multiple::indent(editor, outdent);
        return;
    }
    let indent = editor.options().indent();
    let size = editor.options().indent_size;
    if !outdent && editor.selection().is_none() {
        insert(editor, &indent);
        editor.break_undo_group();
        return;
    }
    let cursor = editor.cursor();
    let range = editor.selection().unwrap_or(cursor..cursor);
    let first = editor.line_of(range.start);
    // A selection ending at a line start does not include that line.
    let last =
        if range.end > range.start && editor.line_start(editor.line_of(range.end)) == range.end {
            editor.line_of(range.end) - 1
        } else {
            editor.line_of(range.end)
        };
    let edits: Vec<Edit> = (first..=last)
        .filter_map(|line| {
            let start = editor.line_start(line);
            if !outdent {
                return Some(Edit::insert(start, &indent));
            }
            let rest = &editor.text()[start..editor.line_end(line)];
            let remove = if rest.starts_with('\t') {
                1
            } else {
                rest.chars().take(size).take_while(|c| *c == ' ').count()
            };
            (remove > 0).then(|| Edit::delete(start..start + remove))
        })
        .collect();
    let selection = editor.selection_state();
    let _ = editor.edit(EditKind::Other, edits, |changes, _| Selection {
        cursor: changes.map(selection.cursor, Assoc::After),
        anchor: selection
            .anchor
            .map(|anchor| changes.map(anchor, Assoc::After)),
        preferred_column: None,
        wrap_affinity: Default::default(),
    });
    editor.break_undo_group();
}

/// Move the caret, extending the selection when `extend` is set.
pub fn navigate(editor: &mut EditorMut<'_>, movement: Movement, extend: bool) {
    editor.map_selections(|buffer, state| navigated(buffer, state, movement, extend));
}
pub fn navigated(
    buffer: &crate::text::TextBuffer,
    state: Selection,
    movement: Movement,
    extend: bool,
) -> Selection {
    if !extend
        && matches!(movement, Movement::Left | Movement::Right)
        && let Some(r) = state.range()
    {
        return Selection::caret(if movement == Movement::Left {
            r.start
        } else {
            r.end
        });
    }
    let cursor = state.cursor;
    let row = buffer.line_of(cursor);
    let last = buffer.line_count() - 1;
    let vertical = match movement {
        Movement::Up => Some(row.saturating_sub(1)),
        Movement::Down => Some((row + 1).min(last)),
        Movement::PageUp(n) => Some(row.saturating_sub(n)),
        Movement::PageDown(n) => Some((row + n).min(last)),
        _ => None,
    };
    let column = state
        .preferred_column
        .unwrap_or_else(|| buffer.display_column(cursor));
    let target = if let Some(row) = vertical {
        buffer.offset_at_display_column(row, column)
    } else {
        match movement {
            Movement::Left => buffer.prev_grapheme(cursor),
            Movement::Right => buffer.next_grapheme(cursor),
            Movement::WordLeft => buffer.word_left(cursor),
            Movement::WordRight => buffer.word_right(cursor),
            Movement::Home => buffer.line_start(row),
            Movement::End => buffer.line_end(row),
            Movement::Start => 0,
            Movement::Finish => buffer.len(),
            _ => unreachable!(),
        }
    };
    Selection {
        cursor: target,
        anchor: extend.then(|| state.anchor.unwrap_or(cursor)),
        preferred_column: vertical.map(|_| column),
        wrap_affinity: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::TextBuffer;

    /// A buffer with one view, the shape most tests need.
    struct Single {
        buffer: TextBuffer,
        selection: Selection,
    }

    impl Single {
        fn new(text: &str) -> Self {
            Self {
                buffer: TextBuffer::new(text),
                selection: Selection::default(),
            }
        }

        fn ed(&mut self) -> EditorMut<'_> {
            EditorMut::new(&mut self.buffer, &mut self.selection, Vec::new())
        }
    }

    #[test]
    fn unicode_navigation_and_delete_are_grapheme_safe() {
        let mut s = Single::new("中e\u{301}👩‍💻\r\n尾");
        let mut b = s.ed();
        for expected in [3, 6, 17, 19, 22] {
            navigate(&mut b, Movement::Right, false);
            assert_eq!(b.cursor(), expected);
        }
        backspace(&mut b);
        backspace(&mut b);
        assert_eq!(b.text(), "中e\u{301}👩‍💻");
        backspace(&mut b);
        assert_eq!(b.text(), "中e\u{301}");
        b.set_cursor(4);
        assert_eq!(b.cursor(), 3);
    }

    #[test]
    fn selection_replace_and_history_restore_both_endpoints() {
        let mut s = Single::new("hello world");
        let mut b = s.ed();
        b.select_range(0..5);
        insert(&mut b, "你好");
        assert_eq!(b.text(), "你好 world");
        b.undo();
        assert_eq!(b.selected_text(), Some("hello"));
        b.redo();
        assert_eq!(b.text(), "你好 world");
        b.undo();
        insert(&mut b, "other");
        assert!(!b.can_redo());
    }

    #[test]
    fn typing_groups_but_navigation_and_paste_break_groups() {
        let mut s = Single::new("");
        let mut b = s.ed();
        insert(&mut b, "a");
        insert(&mut b, "b");
        insert(&mut b, "c");
        b.undo();
        assert_eq!(b.text(), "");
        b.redo();
        navigate(&mut b, Movement::Left, false);
        insert(&mut b, "x");
        paste(&mut b, "yz");
        b.undo();
        assert_eq!(b.text(), "abxc");
        b.undo();
        assert_eq!(b.text(), "abc");
    }

    #[test]
    fn vertical_navigation_keeps_visual_column_and_selection_anchor() {
        let mut s = Single::new("abcdef\nx\nabcdef");
        let mut b = s.ed();
        b.set_cursor(5);
        navigate(&mut b, Movement::Down, true);
        navigate(&mut b, Movement::Down, true);
        assert_eq!(b.line_col(), (2, 5));
        assert_eq!(b.selection(), Some(5..14));
        navigate(&mut b, Movement::Left, false);
        assert_eq!(b.cursor(), 5);
    }

    #[test]
    fn crlf_enter_paste_and_indent_preserve_document_conventions() {
        let mut s = Single::new("  one\r\n  two");
        let mut b = s.ed();
        b.set_cursor(5);
        enter(&mut b);
        assert_eq!(b.text(), "  one\r\n  \r\n  two");
        b.undo();
        b.select_all();
        indent(&mut b, false);
        assert_eq!(b.text(), "    one\r\n    two");
        indent(&mut b, true);
        assert_eq!(b.text(), "  one\r\n  two");
        b.select_all();
        paste(&mut b, "a\nb\r\nc");
        assert_eq!(b.text(), "a\r\nb\r\nc");
    }

    #[test]
    fn indent_excludes_line_at_selection_end() {
        let mut s = Single::new("a\nb\nc");
        let mut b = s.ed();
        b.select_range(0..2);
        indent(&mut b, false);
        assert_eq!(b.text(), "  a\nb\nc");
        assert_eq!(b.selection(), Some(2..4));
        b.undo();
        assert_eq!(b.selection(), Some(0..2));
    }

    #[test]
    fn words_deletion_and_document_boundaries() {
        let mut s = Single::new("hello  world!");
        let mut b = s.ed();
        navigate(&mut b, Movement::WordRight, false);
        assert_eq!(b.cursor(), 7);
        erase(&mut b, true, true);
        assert_eq!(b.text(), "hello  !");
        erase(&mut b, false, true);
        assert_eq!(b.text(), "!");
        navigate(&mut b, Movement::Start, true);
        backspace(&mut b);
        navigate(&mut b, Movement::Finish, false);
        delete_forward(&mut b);
        assert_eq!(b.text(), "!");
    }

    #[test]
    fn edits_in_one_view_move_the_other_views() {
        let mut buffer = TextBuffer::new("one two three");
        let mut mine = Selection::caret(4);
        let mut theirs = Selection::spanning(8, 13);
        let mut ed = EditorMut::new(&mut buffer, &mut mine, vec![&mut theirs]);
        insert(&mut ed, "big ");
        assert_eq!(ed.text(), "one big two three");
        drop(ed);
        assert_eq!(theirs.range(), Some(12..17));
        assert_eq!(mine.cursor, 8);

        let mut ed = EditorMut::new(&mut buffer, &mut mine, vec![&mut theirs]);
        ed.select_all();
        indent(&mut ed, false);
        drop(ed);
        assert_eq!(buffer.text(), "  one big two three");
        assert_eq!(theirs.range(), Some(14..19));
    }

    #[test]
    fn typing_in_another_view_starts_a_new_undo_group() {
        let mut buffer = TextBuffer::new("ab");
        let mut left = Selection::caret(0);
        let mut right = Selection::caret(2);
        insert(
            &mut EditorMut::new(&mut buffer, &mut left, vec![&mut right]),
            "x",
        );
        insert(
            &mut EditorMut::new(&mut buffer, &mut right, vec![&mut left]),
            "y",
        );
        assert_eq!(buffer.text(), "xaby");
        EditorMut::new(&mut buffer, &mut right, vec![&mut left]).undo();
        assert_eq!(buffer.text(), "xab");
    }
}
