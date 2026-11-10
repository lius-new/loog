//! Insert and Replace mode: typing, the keys Vim handles while typing, and
//! finishing an insert (counts, visual blocks, `.`).

use unicode_segmentation::UnicodeSegmentation;

use crate::normal;
use crate::text::{EditKind, EditorMut, Selection};

use super::command::{Command, InsertKind};
use super::exec::{Region, visual_region};
use super::motion::first_non_blank;
use super::{BlockInsert, Change, Host, InsertEvent, Key, Mode, Vim, VisualKind};

impl Vim {
    /// A key reaching Vim in Insert or Replace mode. Printable keys are not
    /// consumed: they arrive again as text input.
    pub(super) fn insert_key(
        &mut self,
        editor: &mut EditorMut<'_>,
        key: Key,
        host: &mut dyn Host,
    ) -> bool {
        if self.keys.first() == Some(&Key::Ctrl('r')) {
            // `<C-r>{register}` inserts a register's text.
            self.keys.clear();
            if let Key::Char(name) = key
                && let Some(register) = self.read_register(Some(name), host)
            {
                let text = register.text.replace('\n', editor.line_ending());
                self.insert_event(editor, InsertEvent::Paste(text));
            }
            return true;
        }
        let event = match key {
            Key::Esc | Key::Ctrl('c') => {
                self.finish_insert(editor, true);
                return true;
            }
            Key::Backspace | Key::Ctrl('h') => InsertEvent::Backspace,
            Key::Delete => InsertEvent::Delete,
            Key::Enter | Key::Ctrl('m' | 'j') => InsertEvent::Newline,
            Key::Tab | Key::Ctrl('i') => InsertEvent::Tab,
            Key::Ctrl('w') => InsertEvent::DeleteWordBack,
            Key::Ctrl('u') => InsertEvent::DeleteToLineStart,
            Key::Ctrl('r') => {
                self.keys.push(key);
                return true;
            }
            Key::Ctrl('o') if self.mode == Mode::Insert => {
                // One Normal mode command, then back to typing.
                self.one_command = true;
                self.mode = Mode::Normal;
                return true;
            }
            Key::Left | Key::Right | Key::Up | Key::Down | Key::Home | Key::End => {
                let movement = match key {
                    Key::Left => normal::Movement::Left,
                    Key::Right => normal::Movement::Right,
                    Key::Up => normal::Movement::Up,
                    Key::Down => normal::Movement::Down,
                    Key::Home => normal::Movement::Home,
                    _ => normal::Movement::End,
                };
                if let Some(map) = host.display_map(editor.buffer()) {
                    editor.map_selections(|buffer, state| {
                        map.navigate(buffer, state, movement, false)
                    });
                } else {
                    normal::navigate(editor, movement, false);
                }
                self.record_insert(None);
                return true;
            }
            _ => return false,
        };
        self.insert_event(editor, event);
        true
    }

    /// Apply a typing event and remember it for repeating.
    fn sync_insert_group(&mut self, editor: &mut EditorMut<'_>) {
        if self.group_open && !editor.in_undo_group() {
            self.group_open = false;
            self.record_insert(None);
        }
    }
    pub(super) fn insert_event(&mut self, editor: &mut EditorMut<'_>, event: InsertEvent) {
        self.sync_insert_group(editor);
        if !self.group_open {
            editor.begin_group();
            self.group_open = true;
        }
        self.apply_event(editor, &event);
        if let Some(session) = self.insert.as_mut() {
            session.events.push(event);
        }
    }

    fn apply_event(&mut self, editor: &mut EditorMut<'_>, event: &InsertEvent) {
        let replacing = self.mode == Mode::Replace;
        match event {
            InsertEvent::Text(text) if replacing => self.overwrite(editor, text),
            InsertEvent::Literal(text) if replacing => self.overwrite(editor, text),
            InsertEvent::Literal(text) => normal::insert(editor, text),
            InsertEvent::Text(text) => {
                let language = editor.options().language.clone();
                normal::type_text(editor, text, &language);
            }
            InsertEvent::Backspace if replacing => self.restore(editor),
            InsertEvent::Backspace => normal::backspace(editor),
            InsertEvent::Delete => normal::delete_forward(editor),
            InsertEvent::DeleteWordBack => normal::erase(editor, false, true),
            InsertEvent::DeleteWordForward => normal::erase(editor, true, true),
            InsertEvent::DeleteToLineStart if editor.has_secondary() => {
                crate::text::multiple::erase_prefix(editor)
            }
            InsertEvent::DeleteToLineStart => {
                let cursor = editor.cursor();
                let line = editor.line_of(cursor);
                let indent = first_non_blank(editor, line);
                let start = if cursor > indent {
                    indent
                } else {
                    editor.line_start(line)
                };
                let _ = editor.replace(start..cursor, "", EditKind::Other);
            }
            InsertEvent::Newline => normal::enter(editor),
            InsertEvent::Tab => normal::indent(editor, false),
            InsertEvent::Backtab => normal::indent(editor, true),
            InsertEvent::Paste(text) => normal::paste(editor, text),
        }
    }

    /// Replace mode: overwrite characters, appending past the line's end.
    fn overwrite(&mut self, editor: &mut EditorMut<'_>, text: &str) {
        for grapheme in text.graphemes(true) {
            let cursor = editor.cursor();
            let end = editor.line_end(editor.line_of(cursor));
            if cursor < end {
                let next = editor.next_grapheme(cursor);
                self.replaced
                    .push(Some(editor.text()[cursor..next].to_owned()));
                let _ = editor.replace(cursor..next, grapheme, EditKind::Typing);
            } else {
                self.replaced.push(None);
                let _ = editor.replace(cursor..cursor, grapheme, EditKind::Typing);
            }
        }
    }

    /// Backspace in Replace mode brings back the overwritten character.
    fn restore(&mut self, editor: &mut EditorMut<'_>) {
        let cursor = editor.cursor();
        if cursor == 0 {
            return;
        }
        let previous = editor.prev_grapheme(cursor);
        match self.replaced.pop() {
            Some(Some(original)) => {
                let _ = editor.replace(previous..cursor, &original, EditKind::Other);
                editor.set_cursor(previous);
            }
            Some(None) => {
                let _ = editor.replace(previous..cursor, "", EditKind::Other);
            }
            None => editor.set_cursor(previous),
        }
    }

    /// Leave Insert or Replace mode. `exiting` (Esc) repeats the typed text
    /// for a count or a visual block and steps the cursor back onto the last
    /// typed character; leaving a view only closes the session.
    pub(super) fn finish_insert(&mut self, editor: &mut EditorMut<'_>, exiting: bool) {
        self.sync_insert_group(editor);
        let Some(session) = self.insert.take() else {
            self.mode = Mode::Normal;
            return;
        };
        if exiting {
            for _ in 1..session.count {
                if let Some(kind @ (InsertKind::LineBelow | InsertKind::LineAbove)) = session.kind {
                    let line = editor.line_of(editor.cursor());
                    let caret = self.open_line(editor, line, kind == InsertKind::LineBelow);
                    editor.set_cursor(caret);
                }
                for event in &session.events {
                    self.apply_event(editor, event);
                }
            }
            if let Some(block) = session.block {
                self.replicate(editor, block, &session.events);
            }
        }
        let typed: String = session
            .events
            .iter()
            .filter_map(|event| match event {
                InsertEvent::Text(text) | InsertEvent::Literal(text) | InsertEvent::Paste(text) => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect();
        if !typed.is_empty() {
            self.registers.last_inserted = Some(typed);
        }
        if self.group_open {
            editor.end_group();
            self.group_open = false;
        }
        self.mode = Mode::Normal;
        self.replaced.clear();
        self.one_command = false;
        if exiting {
            editor.set_selection(editor.selection_state());
            let cursor = editor.cursor();
            let line_start = editor.line_start(editor.line_of(cursor));
            if cursor > line_start {
                editor.set_cursor(editor.prev_grapheme(cursor));
            }
        }
        if exiting {
            self.normalize(editor);
        }
        if let Some(command) = session.repeat {
            self.last_change = Some(Change {
                command,
                insert: Some(session.events),
                visual: session.visual,
            });
        }
    }

    /// Type the text inserted on a block's first line onto its other lines.
    fn replicate(
        &mut self,
        editor: &mut EditorMut<'_>,
        block: BlockInsert,
        events: &[InsertEvent],
    ) {
        let mut text = String::new();
        for event in events {
            match event {
                InsertEvent::Text(typed) | InsertEvent::Literal(typed) => text.push_str(typed),
                // Line breaks or deletions make the insert unsuitable for a block.
                _ => return,
            }
        }
        if text.is_empty() {
            return;
        }
        let cursor = editor.cursor();
        let edits: Vec<_> = (block.lines.0..=block.lines.1.min(editor.line_count() - 1))
            .filter_map(|line| {
                if block.to_line_end {
                    return Some(crate::text::Edit::insert(
                        editor.line_end(line),
                        text.clone(),
                    ));
                }
                let width = editor.line_display_width(line);
                if width < block.column {
                    return None;
                }
                let at = editor.offset_at_display_column(line, block.column);
                Some(crate::text::Edit::insert(at, text.clone()))
            })
            .collect();
        let _ = editor.edit(EditKind::Other, edits, |_, _| Selection::caret(cursor));
    }

    /// `I` and `A` on a visual selection: insert before or after a block on
    /// every line, or at the start or end of a character or line selection.
    pub(super) fn visual_insert(
        &mut self,
        editor: &mut EditorMut<'_>,
        command: &Command,
        append: bool,
    ) {
        let Mode::Visual(kind) = self.mode else {
            return;
        };
        let selection = editor.selection_state();
        let region = visual_region(editor, selection, kind);
        self.exit_visual(editor);
        let (offset, block) = match region {
            Region::Block {
                first,
                last,
                left,
                right,
            } => {
                let to_line_end = right == usize::MAX;
                let column = if append && !to_line_end { right } else { left };
                let offset = if append && to_line_end {
                    editor.line_end(first)
                } else {
                    let width = editor.line_display_width(first);
                    if width < column {
                        // Pad a short first line so the block column exists.
                        let end = editor.line_end(first);
                        let padding = " ".repeat(column - width);
                        let _ = editor.edit(
                            EditKind::Other,
                            vec![crate::text::Edit::insert(end, padding.clone())],
                            |_, _| Selection::caret(end + padding.len()),
                        );
                        end + padding.len()
                    } else {
                        editor.offset_at_display_column(first, column)
                    }
                };
                let block = (last > first).then_some(BlockInsert {
                    lines: (first + 1, last),
                    column,
                    to_line_end: append && to_line_end,
                });
                (offset, block)
            }
            Region::Chars(range) => (if append { range.end } else { range.start }, None),
            Region::Lines(first, last) => (
                if append {
                    editor.line_end(last)
                } else {
                    first_non_blank(editor, first)
                },
                None,
            ),
        };
        self.start_insert(editor, offset, Some(command.clone()), 1, None, false);
        let mut block = block;
        if editor.supports_secondary()
            && let Some(region) = block.take()
        {
            let primary = editor.selection_state();
            let others = (region.lines.0..=region.lines.1.min(editor.line_count() - 1))
                .filter_map(|row| {
                    let at = if region.to_line_end {
                        editor.line_end(row)
                    } else {
                        if editor.line_display_width(row) < region.column {
                            return None;
                        }
                        editor.offset_at_display_column(row, region.column)
                    };
                    Some(Selection::caret(at))
                })
                .collect();
            editor.set_selections(primary, others);
        }
        if let Some(session) = self.insert.as_mut() {
            session.block = block;
            session.visual = None;
            if kind == VisualKind::Block {
                // `.` repeats a block insert as a plain insert at the cursor.
                session.repeat = Some(super::insert_command(InsertKind::Before));
            }
        }
    }

    /// Feed one key as if typed, for macros: keys Vim does not consume while
    /// typing are typed as text.
    pub(super) fn feed(&mut self, editor: &mut EditorMut<'_>, host: &mut dyn Host, key: Key) {
        if let Key::Literal(c) = key {
            self.handle_commit(editor, &c.to_string());
            return;
        }
        if self.handle_key(editor, key, host) {
            return;
        }
        if let Key::Char(character) = key {
            self.handle_text(editor, &character.to_string());
        }
    }
}
