//! A document seen through one view's selection.
//!
//! `EditorMut` is the only way to change a document's text. It applies the
//! edits, sets the editing view's selection, moves every other view's
//! selection across the change and records the change in the history.

use std::ops::{Deref, Range};

use super::buffer::TextBuffer;
use super::change::{Assoc, ChangeSet, Edit, EditError};
use super::history::EditKind;
use super::selection::Selection;

/// Read-only view of a buffer through one selection.
#[derive(Clone, Copy)]
pub struct Editor<'a> {
    buffer: &'a TextBuffer,
    selection: Selection,
    secondary: &'a [Selection],
}

impl Deref for Editor<'_> {
    type Target = TextBuffer;
    fn deref(&self) -> &TextBuffer {
        self.buffer
    }
}

impl<'a> Editor<'a> {
    pub fn new(buffer: &'a TextBuffer, selection: Selection) -> Self {
        Self {
            buffer,
            selection,
            secondary: &[],
        }
    }

    pub fn with_secondary(mut self, secondary: &'a [Selection]) -> Self {
        self.secondary = secondary;
        self
    }
    pub fn secondary(&self) -> &[Selection] {
        self.secondary
    }
    pub fn selections(&self) -> Vec<Selection> {
        let (primary, secondary) =
            super::multiple::normalize(self.buffer, self.selection, self.secondary.to_vec());
        std::iter::once(primary).chain(secondary).collect()
    }
    pub fn selected_texts(&self) -> Option<String> {
        let mut ranges: Vec<_> = self
            .selections()
            .into_iter()
            .filter_map(|s| s.range())
            .collect();
        ranges.sort_by_key(|r| r.start);
        (!ranges.is_empty()).then(|| {
            ranges
                .into_iter()
                .map(|r| self.buffer.slice(r))
                .collect::<Vec<_>>()
                .join(self.buffer.line_ending())
        })
    }
    pub fn buffer(&self) -> &'a TextBuffer {
        self.buffer
    }

    pub fn cursor(&self) -> usize {
        self.selection.cursor
    }

    pub fn selection_state(&self) -> Selection {
        self.selection
    }

    pub fn selection(&self) -> Option<Range<usize>> {
        self.selection.range()
    }

    pub fn selected_text(&self) -> Option<&'a str> {
        let buffer = self.buffer;
        self.selection().map(|range| &buffer.text()[range])
    }

    pub fn line_col(&self) -> (usize, usize) {
        self.buffer.line_col(self.selection.cursor)
    }
}

/// Editable view of a buffer through one selection. `peers` are the
/// selections of the document's other views.
pub struct EditorMut<'a> {
    buffer: &'a mut TextBuffer,
    selection: &'a mut Selection,
    peers: Vec<&'a mut Selection>,
    secondary: Option<&'a mut Vec<Selection>>,
    anchors: Vec<(&'a mut usize, Assoc)>,
}

impl Deref for EditorMut<'_> {
    type Target = TextBuffer;
    fn deref(&self) -> &TextBuffer {
        self.buffer
    }
}

impl<'a> EditorMut<'a> {
    pub fn new(
        buffer: &'a mut TextBuffer,
        selection: &'a mut Selection,
        peers: Vec<&'a mut Selection>,
    ) -> Self {
        Self {
            buffer,
            selection,
            peers,
            secondary: None,
            anchors: Vec::new(),
        }
    }

    pub fn with_secondary(mut self, secondary: &'a mut Vec<Selection>) -> Self {
        self.secondary = Some(secondary);
        let extra = self.secondary.as_deref().cloned().unwrap_or_default();
        self.install_selections(*self.selection, extra);
        self
    }
    pub fn supports_secondary(&self) -> bool {
        self.secondary.is_some()
    }
    pub fn has_secondary(&self) -> bool {
        self.secondary.as_ref().is_some_and(|s| !s.is_empty())
    }
    pub fn selections(&self) -> Vec<Selection> {
        self.as_ref().selections()
    }
    pub fn set_selections(&mut self, primary: Selection, others: Vec<Selection>) {
        self.buffer.break_undo_group();
        self.install_selections(primary, others);
    }
    pub(super) fn install_selections(&mut self, primary: Selection, others: Vec<Selection>) {
        let (primary, others) = super::multiple::normalize(self.buffer, primary, others);
        *self.selection = primary;
        if let Some(secondary) = self.secondary.as_mut() {
            **secondary = others;
        }
    }
    pub fn map_selections(&mut self, mut map: impl FnMut(&TextBuffer, Selection) -> Selection) {
        let mut states = self.selections().into_iter().map(|s| map(self.buffer, s));
        let primary = states.next().unwrap();
        let rest = states.collect();
        self.set_selections(primary, rest);
    }
    pub fn cut_selections(&mut self) {
        let states = self.selections();
        let edits = states
            .iter()
            .filter_map(|s| s.range())
            .map(Edit::delete)
            .collect();
        let before = states[0];
        let _ = self.edit(EditKind::Other, edits, |changes, _| {
            Selection::caret(changes.map(
                before.range().map_or(before.cursor, |r| r.start),
                Assoc::Before,
            ))
        });
        self.map_selections(|_, s| Selection::caret(s.range().map_or(s.cursor, |r| r.start)));
        self.finish_multiple();
    }
    pub(crate) fn finish_multiple(&mut self) {
        let primary = *self.selection;
        let extra = self.secondary.as_deref().cloned().unwrap_or_default();
        self.install_selections(primary, extra);
        let extra = self.secondary.as_deref().cloned().unwrap_or_default();
        self.buffer
            .history_mut()
            .update_selections(*self.selection, extra);
    }
    /// Attach positions that follow every edit, including undo/redo and long
    /// grouped commands. This does not depend on the bounded revision journal.
    pub fn with_anchors(mut self, anchors: Vec<(&'a mut usize, Assoc)>) -> Self {
        self.anchors = anchors;
        self
    }

    pub fn as_ref(&self) -> Editor<'_> {
        Editor::new(self.buffer, *self.selection)
            .with_secondary(self.secondary.as_deref().map_or(&[], |s| s.as_slice()))
    }

    pub fn buffer(&self) -> &TextBuffer {
        self.buffer
    }

    pub fn cursor(&self) -> usize {
        self.selection.cursor
    }

    pub fn selection_state(&self) -> Selection {
        *self.selection
    }

    pub fn selection(&self) -> Option<Range<usize>> {
        self.selection.range()
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|range| &self.buffer.text()[range])
    }

    pub fn line_col(&self) -> (usize, usize) {
        self.buffer.line_col(self.selection.cursor)
    }

    // ---- Selection -----------------------------------------------------------

    /// Replace this view's selection. Ends snap to grapheme boundaries, and
    /// the next edit starts a new undo step.
    pub fn set_selection(&mut self, selection: Selection) {
        self.buffer.break_undo_group();
        *self.selection = self.buffer.clamp(selection);
        if let Some(secondary) = self.secondary.as_mut() {
            secondary.clear();
        }
    }

    pub fn set_cursor(&mut self, offset: usize) {
        self.select_to(offset, false);
    }

    /// Move the cursor to `offset`, keeping (or starting) the anchor when
    /// `extend` is set and dropping it otherwise.
    pub fn select_to(&mut self, offset: usize, extend: bool) {
        let anchor = extend.then(|| self.selection.anchor.unwrap_or(self.selection.cursor));
        self.set_selection(Selection {
            cursor: offset,
            anchor,
            preferred_column: None,
            wrap_affinity: Default::default(),
        });
    }

    pub fn select_range(&mut self, range: Range<usize>) {
        self.set_selection(Selection::spanning(range.start, range.end));
    }

    pub fn select_all(&mut self) {
        self.select_range(0..self.buffer.len());
    }

    pub fn clear_selection(&mut self) {
        self.buffer.break_undo_group();
        self.selection.anchor = None;
        if let Some(secondary) = self.secondary.as_mut() {
            secondary.clear();
        }
    }

    /// Keep a display column for the next vertical move.
    pub fn set_wrap_affinity(&mut self, affinity: Assoc) {
        self.selection.wrap_affinity = affinity;
    }
    pub fn set_preferred_column(&mut self, column: Option<usize>) {
        self.selection.preferred_column = column;
    }

    /// Register a pair created by the most recent edit, including redo state.
    pub fn track_pair(&mut self, open: usize, close: usize) {
        let before = self.buffer.pairs.clone();
        std::sync::Arc::make_mut(&mut self.buffer.pairs).push(super::pairs::Pair { open, close });
        let after = self.buffer.pairs.clone();
        self.buffer.history_mut().attach_pairs(before, after);
    }

    pub fn is_auto_close(&self, at: usize) -> bool {
        self.buffer.pairs.iter().any(|pair| pair.close == at)
    }

    pub fn is_empty_auto_pair(&self, at: usize) -> bool {
        self.buffer
            .pairs
            .iter()
            .any(|pair| pair.close == at && pair.open + 1 == at)
    }

    // ---- Editing -------------------------------------------------------------

    /// Apply `edits` (all in the current text's offsets) as one change and
    /// give this view the selection `select` computes from the result. Other
    /// views follow the change. Invalid edits change nothing.
    pub fn edit(
        &mut self,
        kind: EditKind,
        edits: Vec<Edit>,
        select: impl FnOnce(&ChangeSet, &TextBuffer) -> Selection,
    ) -> Result<ChangeSet, EditError> {
        let before = *self.selection;
        let secondary_before = self.secondary.as_deref().cloned().unwrap_or_default();
        let pairs = self.buffer.pairs.clone();
        let ending = self.buffer.ending;
        let changes = self.buffer.apply_edits(edits)?;
        if changes.is_empty() {
            return Ok(changes);
        }
        let after = self.buffer.clamp(select(&changes, self.buffer));
        *self.selection = after;
        self.move_peers(&changes);
        let extra = self.secondary.as_deref().cloned().unwrap_or_default();
        self.install_selections(*self.selection, extra);
        let after = *self.selection;
        self.buffer
            .history_mut()
            .record(changes.clone(), kind, before, after);
        let secondary_after = self.secondary.as_deref().cloned().unwrap_or_default();
        self.buffer
            .history_mut()
            .attach_selections(secondary_before, secondary_after);
        let after_pairs = self.buffer.pairs.clone();
        self.buffer.history_mut().attach_pairs(pairs, after_pairs);
        let after_ending = self.buffer.ending;
        self.buffer
            .history_mut()
            .attach_ending(ending, after_ending);
        Ok(changes)
    }

    /// Replace `range` with `text` and put the caret after the inserted text.
    pub fn replace(
        &mut self,
        range: Range<usize>,
        text: &str,
        kind: EditKind,
    ) -> Result<ChangeSet, EditError> {
        let start = range.start;
        self.edit(kind, vec![Edit::replace(range, text)], |_, buffer| {
            let end = start + text.len();
            // Combining input can join the preceding grapheme; keep the caret
            // after the whole cluster.
            Selection::caret(if buffer.is_grapheme_boundary(end) {
                end
            } else {
                buffer.next_grapheme(end)
            })
        })
    }

    /// Convert only newline spans, mapping all view positions through the edit.
    pub fn set_line_ending(&mut self, ending: super::options::LineEnding) -> Result<(), EditError> {
        self.buffer.history_mut().close_groups();
        let before = self.buffer.metadata();
        let after = (before & !super::options::ENDING_MASK) | ending.code();
        let bytes = self.buffer.text().as_bytes();
        let edits = bytes
            .iter()
            .enumerate()
            .filter_map(|(i, &byte)| {
                if byte != b'\n' {
                    return None;
                }
                let crlf = i > 0 && bytes[i - 1] == b'\r';
                match (crlf, ending) {
                    (true, super::options::LineEnding::Lf) => {
                        Some(Edit::replace(i - 1..i + 1, "\n"))
                    }
                    (false, super::options::LineEnding::Crlf) => {
                        Some(Edit::replace(i..i + 1, "\r\n"))
                    }
                    _ => None,
                }
            })
            .collect::<Vec<_>>();
        if edits.is_empty() {
            self.buffer.set_metadata(after);
        } else {
            let selection = *self.selection;
            self.edit(EditKind::Other, edits, |changes, _| Selection {
                cursor: changes.map(selection.cursor, Assoc::Before),
                anchor: selection.anchor.map(|a| changes.map(a, Assoc::Before)),
                preferred_column: None,
                wrap_affinity: Default::default(),
            })?;
            self.buffer.initialize_metadata(after);
            self.buffer.history_mut().attach_metadata(before, after);
        }
        self.break_undo_group();
        Ok(())
    }

    /// Replace a document interpretation and its metadata as one undo step.
    pub fn replace_with_metadata(&mut self, text: &str, metadata: u32) -> Result<(), EditError> {
        if self.buffer.text() == text {
            self.buffer.set_metadata(metadata);
            return Ok(());
        }
        self.buffer.history_mut().close_groups();
        let before = self.buffer.metadata();
        let selection = *self.selection;
        self.edit(
            EditKind::Other,
            vec![Edit::replace(0..self.buffer.len(), text)],
            |_, buffer| buffer.clamp(selection),
        )?;
        self.buffer.initialize_metadata(metadata);
        self.buffer.history_mut().attach_metadata(before, metadata);
        Ok(())
    }

    /// Replace the selection, or insert at the caret, and put the caret after
    /// the inserted text.
    pub fn replace_selection(
        &mut self,
        text: &str,
        kind: EditKind,
    ) -> Result<ChangeSet, EditError> {
        let cursor = self.selection.cursor;
        let range = self.selection().unwrap_or(cursor..cursor);
        self.replace(range, text, kind)
    }

    fn move_peers(&mut self, changes: &ChangeSet) {
        for (anchor, assoc) in &mut self.anchors {
            **anchor = self
                .buffer
                .clamp(Selection::caret(changes.map(**anchor, *assoc)))
                .cursor;
        }
        if let Some(secondary) = self.secondary.as_mut() {
            for s in secondary.iter_mut() {
                s.transform(changes);
                *s = self.buffer.clamp(*s);
            }
        }
        for peer in &mut self.peers {
            peer.transform(changes);
            **peer = self.buffer.clamp(**peer);
        }
    }

    // ---- History -------------------------------------------------------------

    /// Edits until `end_transaction` are one undo step tagged `id`.
    pub fn begin_transaction(&mut self, id: u64) {
        self.buffer.set_transaction(Some(id));
    }

    pub fn end_transaction(&mut self) {
        self.buffer.set_transaction(None);
    }

    /// Join every edit until the matching `end_group` into one undo step.
    /// Groups nest; the outermost one decides the step.
    pub fn begin_group(&mut self) {
        self.buffer.history_mut().begin_group();
    }

    pub fn end_group(&mut self) {
        self.buffer.history_mut().end_group();
    }

    /// Start a new undo step with the next edit (outside explicit groups).
    pub fn break_undo_group(&mut self) {
        self.buffer.break_undo_group();
    }

    /// Undo the last step. This view gets the selection from before the step;
    /// other views move across the reverse change. Returns the range of the
    /// restored text.
    #[inline]
    pub fn undo(&mut self) -> Option<Range<usize>> {
        let entry = self.buffer.history_mut().pop_undo()?;
        let reverse = entry.changes.iter().rev().map(ChangeSet::inverse).collect();
        let extent = self.replay(reverse)?;
        if let Some((before, _)) = entry.ending {
            self.buffer.ending = before;
        }
        if let Some((before, _)) = &entry.pairs {
            self.buffer.pairs = before.clone();
        }
        if let Some((before, _)) = entry.metadata {
            self.buffer.replay_metadata(before);
        }
        if !entry.changes.is_empty() {
            *self.selection = self.buffer.clamp(Selection {
                preferred_column: None,
                ..entry.before
            });
        }
        if let Some((before, _)) = &entry.selections {
            self.install_selections(*self.selection, before.clone());
        }
        self.buffer.history_mut().push_redo(entry);
        Some(extent)
    }

    /// Redo the last undone step. This view gets the selection from after it.
    pub fn redo(&mut self) -> Option<Range<usize>> {
        let entry = self.buffer.history_mut().pop_redo()?;
        let forward = entry.changes.iter().map(ChangeSet::forward).collect();
        let extent = self.replay(forward)?;
        if let Some((_, after)) = entry.ending {
            self.buffer.ending = after;
        }
        if let Some((_, after)) = &entry.pairs {
            self.buffer.pairs = after.clone();
        }
        if let Some((_, after)) = entry.metadata {
            self.buffer.replay_metadata(after);
        }
        if !entry.changes.is_empty() {
            *self.selection = self.buffer.clamp(Selection {
                preferred_column: None,
                ..entry.after
            });
        }
        if let Some((_, after)) = &entry.selections {
            self.install_selections(*self.selection, after.clone());
        }
        self.buffer.history_mut().push_undo(entry);
        Some(extent)
    }

    /// Apply recorded edit batches in order, moving the peers, and return the
    /// range they cover in the final text.
    fn replay(&mut self, batches: Vec<Vec<Edit>>) -> Option<Range<usize>> {
        let mut extent: Option<Range<usize>> = None;
        for edits in batches {
            let Ok(changes) = self.buffer.apply_edits(edits) else {
                // The history no longer describes this text; drop it rather
                // than apply edits at the wrong place.
                *self.buffer.history_mut() = Default::default();
                return None;
            };
            self.move_peers(&changes);
            let mapped = extent.map(|range| {
                changes.map(range.start, Assoc::Before)..changes.map(range.end, Assoc::After)
            });
            extent = match (mapped, changes.new_extent()) {
                (Some(a), Some(b)) => Some(a.start.min(b.start)..a.end.max(b.end)),
                (a, b) => a.or(b),
            };
        }
        Some(extent.unwrap_or(self.selection.cursor..self.selection.cursor))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn replacement_and_metadata_undo_as_one_step_between_normal_edits() {
        use super::*;
        let mut buffer = TextBuffer::new("中文");
        buffer.initialize_metadata(6);
        let mut selection = Selection::caret(0);
        let mut editor = EditorMut::new(&mut buffer, &mut selection, Vec::new());
        crate::normal::insert(&mut editor, "x");
        editor.replace_with_metadata("x乱码", 0).unwrap();
        assert_eq!(editor.text(), "x乱码");
        assert_eq!(editor.metadata(), 0);
        editor.undo();
        assert_eq!(editor.text(), "x中文");
        assert_eq!(editor.metadata(), 6);
        editor.redo();
        assert_eq!(editor.text(), "x乱码");
        assert_eq!(editor.metadata(), 0);
        editor.undo();
        editor.undo();
        assert_eq!(editor.text(), "中文");
        assert_eq!(editor.metadata(), 6);
        assert!(!editor.can_undo());
    }

    #[test]
    fn a_transaction_is_one_tagged_step_that_never_joins_typing() {
        use super::*;
        let mut buffer = TextBuffer::new("abc");
        let mut selection = Selection::caret(3);
        let mut editor = EditorMut::new(&mut buffer, &mut selection, Vec::new());
        crate::normal::insert(&mut editor, "x");
        editor.begin_transaction(7);
        editor.replace(0..1, "A", EditKind::Typing).unwrap();
        editor.replace(1..2, "B", EditKind::Typing).unwrap();
        editor.end_transaction();
        crate::normal::insert(&mut editor, "y");
        assert_eq!(editor.buffer().undo_transaction(), None);
        editor.undo();
        assert_eq!(editor.buffer().undo_transaction(), Some(7));
        editor.undo();
        assert_eq!(
            editor.text(),
            "abcx",
            "both edits of the transaction went at once"
        );
        assert_eq!(editor.buffer().redo_transaction(), Some(7));
        editor.redo();
        assert_eq!(editor.text(), "ABcx");
    }

    use super::*;

    fn caret_after(changes: &ChangeSet, _: &TextBuffer) -> Selection {
        Selection::caret(changes.new_extent().map_or(0, |range| range.end))
    }

    #[test]
    fn edits_move_other_views_and_undo_reverses_exactly() {
        let mut buffer = TextBuffer::new("abc abc");
        let mut mine = Selection::caret(3);
        let mut theirs = Selection::spanning(4, 7);
        let mut ed = EditorMut::new(&mut buffer, &mut mine, vec![&mut theirs]);
        ed.replace_selection(" abc", EditKind::Other).unwrap();
        drop(ed);
        assert_eq!(buffer.text(), "abc abc abc");
        assert_eq!((mine.cursor, theirs.range()), (7, Some(8..11)));

        // Undo from the other view: the removed " abc" is identical to its
        // neighbour, yet the reverse change is exact.
        let mut ed = EditorMut::new(&mut buffer, &mut theirs, vec![&mut mine]);
        assert_eq!(ed.undo(), Some(3..3));
        drop(ed);
        assert_eq!(buffer.text(), "abc abc");
        assert_eq!((theirs.cursor, mine.cursor), (3, 3));
    }

    #[test]
    fn explicit_groups_join_edits_and_nest() {
        let mut buffer = TextBuffer::new("");
        let mut selection = Selection::default();
        let mut ed = EditorMut::new(&mut buffer, &mut selection, Vec::new());
        ed.begin_group();
        ed.replace_selection("a", EditKind::Other).unwrap();
        ed.begin_group();
        ed.set_cursor(0);
        ed.replace_selection("b", EditKind::Other).unwrap();
        ed.end_group();
        ed.break_undo_group();
        ed.replace_selection("c", EditKind::Typing).unwrap();
        ed.end_group();
        ed.replace_selection("d", EditKind::Typing).unwrap();
        assert_eq!(ed.text(), "bcda");
        assert_eq!(ed.undo(), Some(2..2));
        assert_eq!(ed.text(), "bca");
        assert_eq!(ed.undo(), Some(0..0));
        assert_eq!(ed.text(), "");
        assert_eq!(ed.redo(), Some(0..3));
        assert_eq!((ed.text(), ed.cursor()), ("bca", 2));
    }

    #[test]
    fn batch_edits_validate_first_and_produce_one_undo_step() {
        let mut buffer = TextBuffer::new("a\nb\nc");
        let mut selection = Selection::caret(4);
        let mut ed = EditorMut::new(&mut buffer, &mut selection, Vec::new());
        let bad = ed.edit(
            EditKind::Other,
            vec![Edit::insert(0, "> "), Edit::insert(0, "> ")],
            caret_after,
        );
        assert!(bad.is_err());
        assert!(!ed.can_undo());
        ed.edit(
            EditKind::Other,
            vec![
                Edit::insert(0, "> "),
                Edit::insert(2, "> "),
                Edit::insert(4, "> "),
            ],
            |changes, _| Selection::caret(changes.map(4, Assoc::After)),
        )
        .unwrap();
        assert_eq!((ed.text(), ed.cursor()), ("> a\n> b\n> c", 10));
        assert_eq!(ed.undo(), Some(0..4));
        assert_eq!((ed.text(), ed.cursor()), ("a\nb\nc", 4));
    }

    #[test]
    fn typing_runs_join_until_the_selection_moves() {
        let mut buffer = TextBuffer::new("");
        let mut selection = Selection::default();
        let mut ed = EditorMut::new(&mut buffer, &mut selection, Vec::new());
        for typed in ["a", "b"] {
            ed.replace_selection(typed, EditKind::Typing).unwrap();
        }
        ed.set_cursor(1);
        ed.replace_selection("x", EditKind::Typing).unwrap();
        ed.undo();
        assert_eq!(ed.text(), "ab");
        ed.undo();
        assert_eq!(ed.text(), "");
    }
}
