//! The text of one document, the queries editing behaviours build on, and
//! the one place where the text changes.

use std::sync::Arc;
use std::{collections::VecDeque, ops::Range};

use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

use super::change::{Change, ChangeSet, Edit, EditError, validate};
use super::history::History;
use super::lines::{LineIndex, grapheme_width_with_tab_width};
use super::selection::Selection;

/// The line break ending the first line, if it has one.
fn first_line_ending(text: &str, lines: &LineIndex) -> Option<&'static str> {
    let next = lines.next_start(0)?;
    Some(if next >= 2 && text.as_bytes()[next - 2] == b'\r' {
        "\r\n"
    } else {
        "\n"
    })
}

/// Apply validated, sorted edits to `text` in one pass.
pub(super) fn apply(text: &mut String, edits: Vec<Edit>) -> ChangeSet {
    let mut changes = Vec::with_capacity(edits.len());
    if let [single] = edits.as_slice() {
        let range = single.range.clone();
        let removed = text[range.clone()].to_owned();
        text.replace_range(range.clone(), &single.text);
        changes.push(Change {
            new_start: range.start,
            old: range,
            removed,
            inserted: single.text.clone(),
        });
        return ChangeSet::new(changes);
    }
    let mut tail_delta = 0isize;
    let mut moved = 0usize;
    let mut peak_len = text.len();
    for edit in edits.iter().rev() {
        let delta = edit.text.len() as isize - edit.range.len() as isize;
        if delta != 0 {
            moved = moved
                .saturating_add((text.len() - edit.range.end).saturating_add_signed(tail_delta));
        }
        tail_delta += delta;
        peak_len = peak_len.max(text.len().saturating_add_signed(tail_delta));
    }
    if peak_len <= text.capacity() && moved <= text.len().saturating_add(4096) {
        let mut delta = 0isize;
        for edit in edits {
            let new_start = (edit.range.start as isize + delta) as usize;
            delta += edit.text.len() as isize - edit.range.len() as isize;
            changes.push(Change {
                removed: text[edit.range.clone()].to_owned(),
                old: edit.range,
                new_start,
                inserted: edit.text,
            });
        }
        // Descending old offsets remain valid. The estimate above bounds total
        // suffix movement; equal-length edits move no untouched bytes at all.
        for change in changes.iter().rev() {
            text.replace_range(change.old.clone(), &change.inserted);
        }
        return ChangeSet::new(changes);
    }
    let (out, changes) = rebuild(text, edits);
    *text = out;
    changes
}

/// Build a batch directly from a shared source without first cloning it.
fn rebuild(text: &str, edits: Vec<Edit>) -> (String, ChangeSet) {
    let inserted: usize = edits.iter().map(|edit| edit.text.len()).sum();
    let removed: usize = edits.iter().map(|edit| edit.range.len()).sum();
    let capacity = (text.len() - removed + inserted).saturating_add(4096);
    let mut out = String::with_capacity(capacity);
    let mut changes = Vec::with_capacity(edits.len());
    let mut copied = 0;
    for edit in edits {
        out.push_str(&text[copied..edit.range.start]);
        let new_start = out.len();
        out.push_str(&edit.text);
        changes.push(Change {
            removed: text[edit.range.clone()].to_owned(),
            old: edit.range.clone(),
            new_start,
            inserted: edit.text,
        });
        copied = edit.range.end;
    }
    out.push_str(&text[copied..]);
    (out, ChangeSet::new(changes))
}

/// The text of one document with its line index and undo history.
///
/// Cloning is cheap: the text, lines and history are shared until the next
/// change, which copies them only while an older clone is still alive.
#[derive(Clone, Debug)]
pub struct TextBuffer {
    pub(super) pairs: Arc<Vec<super::pairs::Pair>>,
    text: Arc<String>,
    lines: Arc<LineIndex>,
    history: Arc<History>,
    version: u64,
    metadata: u32,
    revisions: Arc<VecDeque<super::revision::TextRevision>>,
    /// The line break of the first line, kept for when no line break is left.
    pub(super) ending: &'static str,
    observed_ending: bool,
    bracket_index: Arc<std::sync::OnceLock<Vec<(usize, usize)>>>,
    options: super::options::EditingOptions,
    file_options: super::options::FileOptions,
}

impl TextBuffer {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let lines = LineIndex::new(&text);
        let observed_ending = lines.len() > 1;
        let ending = first_line_ending(&text, &lines).unwrap_or("\n");
        Self {
            pairs: Arc::default(),
            lines: Arc::new(lines),
            text: Arc::new(text),
            history: Arc::default(),
            version: 0,
            metadata: 0,
            revisions: Arc::default(),
            ending,
            observed_ending,
            bracket_index: Arc::default(),
            options: Default::default(),
            file_options: Default::default(),
        }
    }

    /// Opaque document metadata, with undo steps interleaved with text edits.
    pub fn metadata(&self) -> u32 {
        self.metadata
    }
    pub fn initialize_metadata(&mut self, value: u32) {
        self.metadata = value;
    }
    pub fn set_metadata(&mut self, value: u32) {
        if self.metadata != value {
            let before = self.metadata;
            self.history_mut().record_metadata(before, value);
            self.change_metadata(value);
        }
    }
    pub(super) fn replay_metadata(&mut self, value: u32) {
        self.change_metadata(value);
    }
    fn change_metadata(&mut self, value: u32) {
        let before = self.version;
        let identity = self.snapshot_identity();
        self.metadata = value;
        self.version += 1;
        let revision = super::revision::TextRevision {
            before_version: before,
            after_version: self.version,
            before_snapshot: identity,
            after_snapshot: identity,
            edits: Arc::from([]),
        };
        let journal = Arc::make_mut(&mut self.revisions);
        journal.push_back(revision);
        while journal.len() > 64 {
            journal.pop_front();
        }
    }
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Immutable text only: background readers do not retain undo history.
    pub fn text_snapshot(&self) -> Arc<String> {
        self.text.clone()
    }

    pub fn matches_snapshot(&self, saved: &Arc<String>) -> bool {
        Arc::ptr_eq(&self.text, saved) || self.text.as_str() == saved.as_str()
    }

    pub fn snapshot_identity(&self) -> usize {
        Arc::as_ptr(&self.text) as usize
    }

    pub fn len(&self) -> usize {
        self.text.len()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Increases with every change to the text.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Returns exact edit provenance, or None after reload/history eviction.
    /// No source strings or undo payloads are retained by this journal.
    pub fn revisions_since(
        &self,
        version: u64,
        snapshot: usize,
    ) -> Option<Vec<super::revision::TextRevision>> {
        let mut current = (version, snapshot);
        let mut result = Vec::new();
        for revision in self.revisions.iter().filter(|r| r.after_version > version) {
            if (revision.before_version, revision.before_snapshot) != current {
                return None;
            }
            current = (revision.after_version, revision.after_snapshot);
            result.push(revision.clone());
        }
        (current == (self.version, self.snapshot_identity())).then_some(result)
    }

    pub fn revision_log(&self) -> Arc<VecDeque<super::revision::TextRevision>> {
        self.revisions.clone()
    }

    /// Replace the whole text with a version loaded from disk. History is
    /// reset because it describes a different file revision.
    pub fn reload(&mut self, contents: String) {
        let version = self.version + 1;
        let options = self.options.clone();
        let file_options = self.file_options;
        *self = Self::new(contents);
        self.configure(options);
        self.file_options = file_options;
        self.version = version;
    }

    pub fn install_prepared(&mut self, mut prepared: Self) {
        prepared.configure(self.options.clone());
        if prepared.file_options == Default::default() {
            prepared.file_options = self.file_options;
        }
        prepared.version = self.version + 1;
        *self = prepared;
    }

    /// Validate and apply `edits`, all given in the current text's offsets.
    /// Nothing changes when any edit is invalid.
    pub(super) fn apply_edits(&mut self, edits: Vec<Edit>) -> Result<ChangeSet, EditError> {
        let edits = validate(&self.text, edits)?;
        if edits.is_empty() {
            return Ok(ChangeSet::default());
        }
        let before_snapshot = self.snapshot_identity();
        let before_version = self.version;
        // Points describe the text before the transaction, so take them while
        // the old text and line index still exist. `validate` sorted the edits
        // and `apply` yields one change per edit in that order.
        let points: Vec<_> = edits
            .iter()
            .map(|edit| {
                (
                    self.byte_point(edit.range.start),
                    self.byte_point(edit.range.end),
                )
            })
            .collect();
        self.bracket_index = Arc::default();
        let changes = if Arc::strong_count(&self.text) > 1 {
            // Copy directly into the final layout with insertion headroom.
            // Cloning String first would allocate again when it grows, and a
            // batch would then build another full output from that clone.
            let (text, changes) = rebuild(&self.text, edits);
            self.text = Arc::new(text);
            changes
        } else {
            apply(Arc::make_mut(&mut self.text), edits)
        };
        Arc::make_mut(&mut self.pairs).retain_mut(|pair| {
            if let Some(mapped) = pair.transform(&changes) {
                *pair = mapped;
                true
            } else {
                false
            }
        });
        Arc::make_mut(&mut self.lines).update(&self.text, &changes);
        self.version += 1;
        let revision = super::revision::TextRevision {
            before_version,
            after_version: self.version,
            before_snapshot,
            after_snapshot: self.snapshot_identity(),
            edits: changes
                .changes()
                .iter()
                .zip(points)
                .map(
                    |(change, (old_start_point, old_end_point))| super::revision::RevisionEdit {
                        old: change.old.clone(),
                        new: change.new_range(),
                        old_start_point,
                        old_end_point,
                        inserted_extent: super::revision::TextExtent::of(&change.inserted),
                        line_breaks: change.removed.contains(['\r', '\n'])
                            || change.inserted.contains(['\r', '\n']),
                    },
                )
                .collect::<Vec<_>>()
                .into(),
        };
        let journal = Arc::make_mut(&mut self.revisions);
        journal.push_back(revision);
        while journal.len() > 64 || journal.iter().map(|r| r.edits.len()).sum::<usize>() > 2048 {
            journal.pop_front();
        }
        if let Some(ending) = first_line_ending(&self.text, &self.lines) {
            self.ending = ending;
            self.observed_ending = true;
        }
        Ok(changes)
    }

    pub(super) fn history_mut(&mut self) -> &mut History {
        Arc::make_mut(&mut self.history)
    }

    /// Whether `edits` could be applied to the text now, without applying them.
    pub fn check_edits(&self, edits: Vec<Edit>) -> Result<(), EditError> {
        super::change::validate(self.text(), edits).map(|_| ())
    }

    /// Edits made until `set_transaction(None)` form one undo step tagged `id`,
    /// for a change that spans several documents.
    pub fn set_transaction(&mut self, id: Option<u64>) {
        self.history_mut().set_transaction(id);
    }

    /// The transaction the next undo would undo, if it belongs to one.
    pub fn undo_transaction(&self) -> Option<u64> {
        self.history.top_undo_transaction()
    }

    /// The transaction the next redo would redo, if it belongs to one.
    pub fn redo_transaction(&self) -> Option<u64> {
        self.history.top_redo_transaction()
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    pub fn in_undo_group(&self) -> bool {
        self.history.in_group()
    }

    /// Start a new undo step with the next edit, unless an explicit group
    /// is open.
    pub fn break_undo_group(&mut self) {
        if !self.history.in_group() {
            self.history_mut().break_typing();
        }
    }

    /// Bytes held by the undo and redo history.
    #[cfg(any(test, feature = "diagnostics"))]
    pub fn history_bytes(&self) -> usize {
        self.history.bytes()
    }

    // ---- Characters ----------------------------------------------------------

    fn char_floor(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    pub fn is_grapheme_boundary(&self, offset: usize) -> bool {
        offset <= self.text.len()
            && self.text.is_char_boundary(offset)
            && GraphemeCursor::new(offset, self.text.len(), true)
                .is_boundary(&self.text, 0)
                .unwrap_or(true)
    }

    /// The nearest grapheme boundary at or before `offset`.
    pub fn grapheme_floor(&self, offset: usize) -> usize {
        let offset = self.char_floor(offset);
        if self.is_grapheme_boundary(offset) {
            return offset;
        }
        GraphemeCursor::new(offset, self.text.len(), true)
            .prev_boundary(&self.text, 0)
            .ok()
            .flatten()
            .unwrap_or(0)
    }

    /// The grapheme boundary after `offset`, or the end of the text.
    pub fn next_grapheme(&self, offset: usize) -> usize {
        let offset = self.grapheme_floor(offset);
        GraphemeCursor::new(offset, self.text.len(), true)
            .next_boundary(&self.text, 0)
            .ok()
            .flatten()
            .unwrap_or(self.text.len())
    }

    /// The grapheme boundary before `offset`, or the start of the text.
    pub fn prev_grapheme(&self, offset: usize) -> usize {
        let offset = self.char_floor(offset);
        GraphemeCursor::new(offset, self.text.len(), true)
            .prev_boundary(&self.text, 0)
            .ok()
            .flatten()
            .unwrap_or(0)
    }

    /// The grapheme starting at `offset`, if any.
    pub fn grapheme_at(&self, offset: usize) -> Option<&str> {
        let start = self.grapheme_floor(offset);
        (start < self.text.len()).then(|| &self.text[start..self.next_grapheme(start)])
    }

    /// Move both ends of a selection onto grapheme boundaries.
    pub fn clamp(&self, selection: Selection) -> Selection {
        Selection {
            cursor: self.grapheme_floor(selection.cursor),
            anchor: selection.anchor.map(|anchor| self.grapheme_floor(anchor)),
            preferred_column: selection.preferred_column,
            wrap_affinity: Default::default(),
        }
    }

    // ---- Lines ---------------------------------------------------------------

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// The line containing `offset`. A line break belongs to the line it ends.
    pub fn line_of(&self, offset: usize) -> usize {
        self.lines.line_of(offset.min(self.text.len()))
    }

    /// `offset` as a row and a byte column. Rows count LF bytes, matching
    /// parsers such as Tree-sitter; a CRLF pair is one break.
    pub fn byte_point(&self, offset: usize) -> super::revision::BytePoint {
        let offset = offset.min(self.text.len());
        let row = self.lines.line_of(offset);
        super::revision::BytePoint {
            row,
            column: offset - self.lines.start(row),
        }
    }

    pub fn line_start(&self, line: usize) -> usize {
        self.lines.start(self.last_line_at_most(line))
    }

    /// Where a line's content ends, before its line break.
    pub fn line_end(&self, line: usize) -> usize {
        self.lines
            .content_end(&self.text, self.last_line_at_most(line))
    }

    /// A line's content, without its line break.
    pub fn line_range(&self, line: usize) -> Range<usize> {
        self.line_start(line)..self.line_end(line)
    }

    /// A line including its line break.
    pub fn line_range_with_break(&self, line: usize) -> Range<usize> {
        let line = self.last_line_at_most(line);
        self.lines.start(line)..self.lines.next_start(line).unwrap_or(self.text.len())
    }

    /// A line's content, without its line break.
    pub fn line(&self, line: usize) -> &str {
        &self.text[self.line_range(line)]
    }

    pub fn lines(&self) -> impl Iterator<Item = &str> + '_ {
        (0..self.line_count()).map(|line| self.line(line))
    }

    fn last_line_at_most(&self, line: usize) -> usize {
        line.min(self.lines.len() - 1)
    }

    /// Zero-based line and character column of an offset.
    pub fn line_col(&self, offset: usize) -> (usize, usize) {
        let offset = self.char_floor(offset);
        let line = self.line_of(offset);
        let position = self
            .lines
            .position_by(line, offset - self.lines.start(line), |p| p.byte);
        (
            line,
            position.chars
                + self.text[self.lines.start(line) + position.byte..offset]
                    .chars()
                    .count(),
        )
    }

    /// The offset of a character column, clamped to the line's content.
    pub fn offset_at_char_column(&self, line: usize, column: usize) -> usize {
        let range = self.line_range(line);
        let position = self
            .lines
            .position_by(self.last_line_at_most(line), column, |p| p.chars);
        self.text[range.start + position.byte..range.end]
            .char_indices()
            .nth(column.saturating_sub(position.chars))
            .map_or(range.end, |(offset, _)| {
                range.start + position.byte + offset
            })
    }

    /// The line break the document uses, judged by its first line.
    pub fn line_ending(&self) -> &'static str {
        self.chosen_line_ending()
            .map_or(self.ending, |ending| ending.text())
    }
    pub fn needs_default_line_ending(&self) -> bool {
        !self.observed_ending && self.chosen_line_ending().is_none()
    }
    pub fn chosen_line_ending(&self) -> Option<super::options::LineEnding> {
        super::options::LineEnding::from_code(self.metadata)
    }
    pub fn file_options(&self) -> super::options::FileOptions {
        self.file_options
    }
    pub fn initialize_file_options(&mut self, options: super::options::FileOptions) {
        self.file_options = options;
    }
    pub fn matching_brackets(&self, cursor: usize) -> Option<(usize, usize)> {
        let pairs = self
            .bracket_index
            .get_or_init(|| super::brackets::index(self.text(), &self.options.language));
        let cursor = self.grapheme_floor(cursor);
        for at in [cursor, self.prev_grapheme(cursor)] {
            if let Ok(index) = pairs.binary_search_by_key(&at, |&(open, _)| open) {
                return Some(pairs[index]);
            }
        }
        None
    }
    pub fn options(&self) -> &super::options::EditingOptions {
        &self.options
    }
    pub fn configure(&mut self, options: super::options::EditingOptions) {
        let options = options.clamped();
        if options.tab_width != self.options.tab_width {
            self.lines = Arc::new(LineIndex::with_tab_width(self.text(), options.tab_width));
        }
        if self.options.language != options.language {
            self.bracket_index = Arc::default();
        }
        self.options = options;
    }
    pub fn initialize_line_ending(&mut self, ending: super::options::LineEnding) {
        self.metadata = (self.metadata & !super::options::ENDING_MASK) | ending.code();
    }
    pub fn line_ending_label(&self) -> &'static str {
        if self.lines.mixed_endings() {
            "Mixed"
        } else if self.line_ending() == "\r\n" {
            "CRLF"
        } else {
            "LF"
        }
    }

    // ---- Display columns -----------------------------------------------------

    /// Display column of `offset` within its line (tabs and wide characters
    /// expanded).
    pub fn display_column(&self, offset: usize) -> usize {
        let offset = self.char_floor(offset);
        let line = self.line_of(offset);
        let start = self.lines.start(line);
        let position = self.lines.position_by(line, offset - start, |p| p.byte);
        self.text[start + position.byte..offset]
            .graphemes(true)
            .fold(position.columns, |x, g| {
                x + grapheme_width_with_tab_width(g, x, self.options.tab_width)
            })
    }

    /// The last grapheme boundary of `line` whose display column does not pass
    /// `column`, so a column inside a wide character or tab rounds down.
    pub fn offset_at_display_column(&self, line: usize, column: usize) -> usize {
        let range = self.line_range(line);
        let position = self
            .lines
            .position_by(self.last_line_at_most(line), column, |p| p.columns);
        let mut x = position.columns;
        let mut offset = range.start + position.byte;
        for (index, grapheme) in
            self.text[range.start + position.byte..range.end].grapheme_indices(true)
        {
            let width = grapheme_width_with_tab_width(grapheme, x, self.options.tab_width);
            if x + width > column {
                break;
            }
            x += width;
            offset = range.start + position.byte + index + grapheme.len();
        }
        offset
    }

    /// The text of `line` covering display columns `left..right`. A wide
    /// character or tab that straddles either edge is included whole; a line
    /// that ends before `left` has no range.
    pub fn display_column_range(
        &self,
        line: usize,
        left: usize,
        right: usize,
    ) -> Option<Range<usize>> {
        let range = self.line_range(line);
        let position = self
            .lines
            .position_by(self.last_line_at_most(line), left, |p| p.columns);
        let mut x = position.columns;
        let mut start = None;
        let mut end = range.start + position.byte;
        for (index, grapheme) in
            self.text[range.start + position.byte..range.end].grapheme_indices(true)
        {
            if x >= right {
                break;
            }
            let width = grapheme_width_with_tab_width(grapheme, x, self.options.tab_width);
            if x + width.max(1) > left && start.is_none() {
                start = Some(range.start + position.byte + index);
            }
            x += width;
            end = range.start + position.byte + index + grapheme.len();
        }
        start.map(|start| start..end)
    }

    pub fn line_display_width(&self, line: usize) -> usize {
        self.lines.width(self.last_line_at_most(line))
    }

    pub fn max_line_display_width(&self) -> usize {
        self.lines.max_width()
    }

    // ---- Words (Unicode word boundaries) -----------------------------------

    /// The word at `offset`: the identifier-like run around it, a run of
    /// whitespace, or a single punctuation mark. Unicode word boundaries
    /// join across `.` and `'`, so `a.rs` is cut at the dot into three.
    pub fn word_range(&self, offset: usize) -> Range<usize> {
        let offset = self.grapheme_floor(offset);
        let line = self.line_range_with_break(self.line_of(offset));
        let Some(segment) = self.text[line.clone()]
            .split_word_bound_indices()
            .map(|(index, segment)| line.start + index..line.start + index + segment.len())
            .find(|range| range.contains(&offset))
        else {
            return offset..offset;
        };
        let text = &self.text[segment.clone()];
        let word = |ch: char| ch.is_alphanumeric() || ch == '_';
        let at = offset - segment.start;
        let Some(here) = text[at..].chars().next() else {
            return segment;
        };
        if word(here) {
            let start = text[..at]
                .char_indices()
                .rev()
                .take_while(|(_, ch)| word(*ch))
                .last()
                .map_or(at, |(index, _)| index);
            let end = text[at..]
                .char_indices()
                .find(|(_, ch)| !word(*ch))
                .map_or(text.len(), |(index, _)| at + index);
            segment.start + start..segment.start + end
        } else if text.chars().all(char::is_whitespace) {
            segment
        } else {
            offset..self.next_grapheme(offset).min(segment.end)
        }
    }

    /// Where else `selected` appears within `lines`, for tinting after a
    /// selection. A selected whole word matches whole words only; single
    /// lines, blank text and very long text match nothing.
    pub fn other_occurrences(
        &self,
        selected: Range<usize>,
        lines: Range<usize>,
    ) -> Vec<Range<usize>> {
        const MAX_LEN: usize = 200;
        const MAX_FOUND: usize = 2000;
        if lines.is_empty() || selected.is_empty() || selected.len() > MAX_LEN {
            return Vec::new();
        }
        let Some(needle) = self.text.get(selected.clone()) else {
            return Vec::new();
        };
        if needle.contains(['\n', '\r']) || needle.chars().all(char::is_whitespace) {
            return Vec::new();
        }
        let whole_word = self.word_range(selected.start) == selected;
        let word = |ch: char| ch.is_alphanumeric() || ch == '_';
        let start = self.line_start(lines.start);
        let end = self.line_range_with_break(lines.end - 1).end;
        self.text[start..end]
            .match_indices(needle)
            .map(|(index, found)| start + index..start + index + found.len())
            .filter(|found| *found != selected)
            .filter(|found| {
                !whole_word
                    || (!self.text[..found.start]
                        .chars()
                        .next_back()
                        .is_some_and(word)
                        && !self.text[found.end..].chars().next().is_some_and(word))
            })
            .take(MAX_FOUND)
            .collect()
    }

    /// The start of the word before `from`, skipping whitespace.
    pub fn word_left(&self, from: usize) -> usize {
        self.text[..self.char_floor(from)]
            .split_word_bound_indices()
            .rev()
            .find(|(_, segment)| !segment.chars().all(char::is_whitespace))
            .map_or(0, |(index, _)| index)
    }

    /// The end of the word after `from`, skipping leading whitespace.
    pub fn word_right(&self, from: usize) -> usize {
        let from = self.char_floor(from);
        let rest = &self.text[from..];
        let mut end = 0;
        let mut found = false;
        let leading_space = rest.chars().next().is_some_and(char::is_whitespace);
        for (index, segment) in rest.split_word_bound_indices() {
            let space = segment.chars().all(char::is_whitespace);
            if (found || leading_space) && !space {
                break;
            }
            end = index + segment.len();
            if !space {
                found = true;
            }
        }
        from + end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::grapheme_width;

    #[test]
    fn medium_line_indexes_survive_edits_across_old_size_thresholds() {
        for count in [80, 130, 260, 820, 1030] {
            let mut buffer = TextBuffer::new("ab中e\u{301}\t".repeat(count));
            for insertion in ["x", "中", "\t", "e\u{301}", ""] {
                let start = buffer.offset_at_display_column(0, 95);
                let end = buffer.next_grapheme(start);
                buffer
                    .apply_edits(vec![Edit::replace(start..end, insertion)])
                    .unwrap();
                let fresh = TextBuffer::new(buffer.text().to_owned());
                assert_eq!(
                    buffer.max_line_display_width(),
                    fresh.max_line_display_width()
                );
                for column in (0..buffer.line_display_width(0)).step_by(37) {
                    let offset = buffer.offset_at_display_column(0, column);
                    assert_eq!(offset, fresh.offset_at_display_column(0, column));
                    assert_eq!(buffer.display_column(offset), fresh.display_column(offset));
                    assert_eq!(buffer.line_col(offset), fresh.line_col(offset));
                }
            }
        }
    }

    #[test]
    fn grapheme_queries_step_over_clusters_and_crlf() {
        let buffer = TextBuffer::new("中e\u{301}👩‍💻\r\n尾");
        let mut offset = 0;
        let mut stops = Vec::new();
        while offset < buffer.len() {
            offset = buffer.next_grapheme(offset);
            stops.push(offset);
        }
        assert_eq!(stops, [3, 6, 17, 19, 22]);
        assert_eq!(buffer.prev_grapheme(19), 6 + 11);
        assert_eq!(buffer.prev_grapheme(17), 6);
        assert_eq!(buffer.grapheme_floor(4), 3);
        assert_eq!(buffer.grapheme_floor(18), 17, "between CR and LF");
        assert_eq!(buffer.grapheme_at(3), Some("e\u{301}"));
        assert_eq!(buffer.grapheme_at(22), None);
    }

    #[test]
    fn line_queries_separate_content_from_line_breaks() {
        let buffer = TextBuffer::new("a\tb\r\n中文\nend");
        assert_eq!(buffer.line_count(), 3);
        assert_eq!(buffer.line_range(0), 0..3);
        assert_eq!(buffer.line_range_with_break(0), 0..5);
        assert_eq!(buffer.line(1), "中文");
        assert_eq!(buffer.line_of(4), 0);
        assert_eq!(buffer.line_col(8), (1, 1));
        assert_eq!(buffer.offset_at_char_column(1, 9), 11);
        assert_eq!(buffer.line_ending(), "\r\n");
        assert_eq!(TextBuffer::new("a\nb\r\n").line_ending(), "\n");
        assert_eq!(buffer.display_column(2), 4);
        assert_eq!(buffer.offset_at_display_column(0, 3), 1, "inside the tab");
        assert_eq!(buffer.offset_at_display_column(1, 3), 8, "inside 文");
        assert_eq!(buffer.max_line_display_width(), 5);
        assert_eq!(buffer.line(7), "end", "lines clamp to the last one");
        assert_eq!(
            buffer.display_column_range(0, 1, 3),
            Some(1..2),
            "the tab spans 1..4"
        );
        assert_eq!(
            buffer.display_column_range(1, 1, 3),
            Some(5..11),
            "both wide characters"
        );
        assert_eq!(buffer.display_column_range(1, 5, 9), None);
        assert_eq!(buffer.display_column_range(2, 1, 99), Some(13..15));
    }

    #[test]
    fn long_line_index_preserves_unicode_tabs_and_character_offsets() {
        let line = "a\t中文e\u{301}👩‍💻😀Z".repeat(1800);
        let text = format!("prefix\r\n{line}\r\nlast");
        let buffer = TextBuffer::new(text);
        let base = buffer.line_start(1);
        let mut column = 0;
        let mut chars = 0;
        for (i, (byte, grapheme)) in line.grapheme_indices(true).enumerate() {
            if i % 97 == 0 {
                assert_eq!(buffer.line_col(base + byte), (1, chars));
                assert_eq!(buffer.offset_at_char_column(1, chars), base + byte);
                assert_eq!(buffer.display_column(base + byte), column);
                assert_eq!(buffer.offset_at_display_column(1, column), base + byte);
                let range = buffer.display_column_range(1, column, column + 12).unwrap();
                assert_eq!(range.start, base + byte);
                assert!(buffer.is_grapheme_boundary(range.end));
                assert!(range.end - range.start < 128);
            }
            column += grapheme_width(grapheme, column);
            chars += grapheme.chars().count();
        }
        assert_eq!(buffer.line_display_width(1), column);
        assert_eq!(buffer.line_col(buffer.line_end(1)), (1, chars));
    }

    #[test]
    fn editing_long_lines_keeps_position_indexes_and_max_width_correct() {
        let mut buffer = TextBuffer::new(format!(
            "{}\nshort\n{}",
            "a中\t".repeat(3000),
            "z".repeat(5000)
        ));
        for step in 0..3 {
            let at = buffer.grapheme_floor(256);
            let edit = match step {
                0 => Edit::replace(at..buffer.next_grapheme(at), "😀\n"),
                1 => Edit::replace(0..buffer.grapheme_floor(10), "head"),
                _ => {
                    let at = buffer.grapheme_floor(100);
                    Edit::replace(at..at, "\t中文")
                }
            };
            buffer.apply_edits(vec![edit]).unwrap();
            let rebuilt = TextBuffer::new(buffer.text().to_owned());
            assert_eq!(buffer.line_count(), rebuilt.line_count());
            assert_eq!(
                buffer.max_line_display_width(),
                rebuilt.max_line_display_width()
            );
            for line in 0..buffer.line_count() {
                assert_eq!(buffer.line(line), rebuilt.line(line));
                assert_eq!(
                    buffer.line_display_width(line),
                    rebuilt.line_display_width(line)
                );
                for column in [0, 128, 257, 1000, 4999, 10000] {
                    assert_eq!(
                        buffer.offset_at_display_column(line, column),
                        rebuilt.offset_at_display_column(line, column)
                    );
                    assert_eq!(
                        buffer.offset_at_char_column(line, column),
                        rebuilt.offset_at_char_column(line, column)
                    );
                }
            }
        }
    }

    #[test]
    fn incremental_long_line_edits_match_full_unicode_queries() {
        let mut buffer = TextBuffer::new("a中文e\u{301}😀XYZ".repeat(3000));
        let pieces = ["x", "中文", "\t", "😀", "\u{301}", "", "e\u{301}", "👩‍💻"];
        let mut seed = 42u64;
        for i in 0..60 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let at = if i % 10 == 0 {
                buffer.len()
            } else {
                buffer.grapheme_floor((seed as usize) % buffer.len())
            };
            let end = if i % 3 == 0 {
                buffer.next_grapheme(at)
            } else {
                at
            };
            buffer
                .apply_edits(vec![Edit::replace(at..end, pieces[i % pieces.len()])])
                .unwrap();
            let rebuilt = TextBuffer::new(buffer.text().to_owned());
            assert_eq!(
                buffer.max_line_display_width(),
                rebuilt.max_line_display_width()
            );
            for offset in [at, end.min(buffer.len()), buffer.len() / 2, buffer.len()] {
                let offset = buffer.grapheme_floor(offset);
                assert_eq!(buffer.line_col(offset), rebuilt.line_col(offset));
                assert_eq!(
                    buffer.display_column(offset),
                    rebuilt.display_column(offset)
                );
            }
            for column in [
                0,
                255,
                256,
                257,
                1000,
                10000,
                buffer.line_display_width(0).saturating_sub(1),
            ] {
                assert_eq!(
                    buffer.offset_at_display_column(0, column),
                    rebuilt.offset_at_display_column(0, column)
                );
                assert_eq!(
                    buffer.display_column_range(0, column, column + 64),
                    rebuilt.display_column_range(0, column, column + 64)
                );
            }
        }
    }

    #[test]
    fn invalid_edits_change_nothing_and_valid_batches_apply_once() {
        let mut buffer = TextBuffer::new("one two three");
        assert!(
            buffer
                .apply_edits(vec![Edit::insert(0, "x"), Edit::delete(2..20)])
                .is_err()
        );
        assert_eq!((buffer.text(), buffer.version()), ("one two three", 0));
        let changes = buffer
            .apply_edits(vec![Edit::replace(8..13, "3"), Edit::replace(0..3, "1")])
            .unwrap();
        assert_eq!(buffer.text(), "1 two 3");
        assert_eq!(buffer.version(), 1);
        assert_eq!(changes.changes().len(), 2);
        assert_eq!(buffer.line_count(), 1);
    }

    #[test]
    fn word_queries_stay_within_unicode_word_boundaries() {
        let buffer = TextBuffer::new("hello  world!\nnext");
        assert_eq!(buffer.word_range(2), 0..5);
        assert_eq!(buffer.word_range(6), 5..7);
        assert_eq!(buffer.word_range(15), 14..18);
        let repeated = TextBuffer::new("foo foobar foo\nfoo");
        assert_eq!(
            repeated.other_occurrences(0..3, 0..2),
            [11..14, 15..18],
            "a whole word skips foobar"
        );
        assert_eq!(
            repeated.other_occurrences(0..2, 0..2),
            [4..6, 11..13, 15..17],
            "a fragment matches inside words"
        );
        assert!(repeated.other_occurrences(3..4, 0..2).is_empty(), "blank");
        assert!(repeated.other_occurrences(0..3, 0..0).is_empty());
        let path = TextBuffer::new("a_b.rs(x)");
        assert_eq!(path.word_range(1), 0..3, "underscores stay in the word");
        assert_eq!(path.word_range(3), 3..4, "the dot alone");
        assert_eq!(path.word_range(5), 4..6, "the extension alone");
        assert_eq!(path.word_range(6), 6..7, "a bracket alone");
        assert_eq!(path.word_range(7), 7..8);
        assert_eq!(buffer.word_right(0), 7, "a word and the spaces after it");
        assert_eq!(buffer.word_right(5), 7, "leading spaces only");
        assert_eq!(buffer.word_left(13), 12);
        assert_eq!(buffer.word_left(14), 12);
    }

    #[test]
    fn clones_share_text_until_one_of_them_changes() {
        let mut buffer = TextBuffer::new("shared");
        let snapshot = buffer.clone();
        assert!(Arc::ptr_eq(&buffer.text, &snapshot.text));
        buffer.apply_edits(vec![Edit::insert(0, ">")]).unwrap();
        assert_eq!((buffer.text(), snapshot.text()), (">shared", "shared"));
    }

    #[test]
    fn shared_single_and_batch_edits_preserve_source_and_revision_provenance() {
        for edits in [
            vec![Edit::insert(0, "中")],
            vec![Edit::replace(0..3, "begin"), Edit::replace(8..13, "尾\n")],
            vec![Edit::delete(0..3), Edit::delete(8..13)],
        ] {
            let mut buffer = TextBuffer::new("one two three");
            let saved = buffer.text_snapshot();
            let identity = buffer.snapshot_identity();
            let mut expected = saved.as_str().to_owned();
            let changes = apply(&mut expected, edits.clone());
            let applied = buffer.apply_edits(edits).unwrap();
            assert_eq!(buffer.text(), expected);
            assert_eq!(saved.as_str(), "one two three");
            assert_eq!(applied, changes);
            let revisions = buffer.revisions_since(0, identity).unwrap();
            assert_eq!(revisions.len(), 1);
            assert_eq!(revisions[0].after_snapshot, buffer.snapshot_identity());
            assert_eq!(buffer.line_count(), TextBuffer::new(expected).line_count());
        }
    }

    #[test]
    fn history_forks_preserve_independent_undo_and_redo() {
        let mut original = TextBuffer::new("base");
        let mut selection = Selection::caret(0);
        super::super::editor::EditorMut::new(&mut original, &mut selection, vec![])
            .replace(0..4, "first", super::super::history::EditKind::Other)
            .unwrap();
        let mut fork = original.clone();
        let mut fork_selection = selection;
        let mut editor =
            super::super::editor::EditorMut::new(&mut fork, &mut fork_selection, vec![]);
        editor
            .replace(0..5, "branch", super::super::history::EditKind::Other)
            .unwrap();
        editor.undo();
        assert_eq!(editor.text(), "first");
        editor.undo();
        assert_eq!(editor.text(), "base");
        editor.redo();
        assert_eq!(editor.text(), "first");
        assert_eq!(original.text(), "first");
        let mut editor =
            super::super::editor::EditorMut::new(&mut original, &mut selection, vec![]);
        editor.undo();
        assert_eq!(editor.text(), "base");
        editor.redo();
        assert_eq!(editor.text(), "first");
    }

    #[test]
    fn bounded_batch_edits_keep_storage_and_replay_with_exact_provenance() {
        let mut text = String::from("ab中文\r\ncdtail");
        text.reserve(64);
        let mut buffer = TextBuffer::new(text);
        let mut selection = Selection::caret(0);
        let storage = buffer.text().as_ptr();
        let mut editor = super::super::editor::EditorMut::new(&mut buffer, &mut selection, vec![]);
        let changes = editor
            .edit(
                super::super::history::EditKind::Other,
                vec![
                    Edit::replace(0..2, "AB"),
                    Edit::replace(2..8, "世界"),
                    Edit::replace(10..12, "CD"),
                ],
                |_, _| Selection::caret(0),
            )
            .unwrap();
        assert_eq!(editor.text(), "AB世界\r\nCDtail");
        assert_eq!(editor.text().as_ptr(), storage);
        assert_eq!(
            changes
                .changes()
                .iter()
                .map(|c| c.new_start)
                .collect::<Vec<_>>(),
            [0, 2, 10]
        );
        editor.undo();
        assert_eq!(editor.text(), "ab中文\r\ncdtail");
        editor.redo();
        assert_eq!(editor.text(), "AB世界\r\nCDtail");
        let changes = editor
            .edit(
                super::super::history::EditKind::Other,
                vec![Edit::insert(0, "前"), Edit::insert(16, "后")],
                |_, _| Selection::caret(0),
            )
            .unwrap();
        assert_eq!(editor.text(), "前AB世界\r\nCDtail后");
        assert_eq!(editor.text().as_ptr(), storage);
        assert_eq!(changes.changes()[1].new_start, 19);
        editor.undo();
        assert_eq!(editor.text(), "AB世界\r\nCDtail");
    }
}
