//! Line starts and display widths, kept up to date across edits.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::change::{Change, ChangeSet};
use super::line_tree::{self, Line, LineTree};
use super::newline;
use std::sync::Arc;

/// Display columns a tab advances to the next multiple of.
pub const TAB_WIDTH: usize = 4;

/// Display width of one grapheme starting at display column `column`.
pub fn grapheme_width(grapheme: &str, column: usize) -> usize {
    grapheme_width_with_tab_width(grapheme, column, TAB_WIDTH)
}
pub fn grapheme_width_with_tab_width(grapheme: &str, column: usize, width: usize) -> usize {
    if grapheme == "\t" {
        width - column % width
    } else {
        UnicodeWidthStr::width(grapheme)
    }
}

/// Display columns `s` occupies when it starts at column 0.
pub fn display_width(s: &str) -> usize {
    // These ASCII graphemes have the same width as their byte length.
    // Keep tabs and CR on the grapheme path for tab stops and CRLF pairs.
    if s.is_ascii() && !s.as_bytes().contains(&b'\t') && !s.as_bytes().contains(&b'\r') {
        return s.len();
    }
    s.graphemes(true).fold(0, |column, grapheme| {
        column + grapheme_width(grapheme, column)
    })
}

/// The offset after a line's content, before its CR LF or LF.
fn content_end(text: &str, start: usize, next_start: Option<usize>) -> usize {
    match next_start {
        None => text.len(),
        Some(next) => {
            let end = next - 1;
            if end > start && text.as_bytes()[end - 1] == b'\r' {
                end - 1
            } else {
                end
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Position {
    pub byte: usize,
    pub chars: usize,
    pub columns: usize,
}

#[derive(Clone, Debug)]
pub(super) struct LineIndex {
    tree: LineTree,
    tab_width: usize,
}

/// The lines starting in `text[from..to]`. `to_end` says whether `to` is the end of the text,
/// so a final line break opens a last line.
fn scan_lines(text: &str, from: usize, to: usize, to_end: bool, tab_width: usize) -> Vec<Line> {
    let mut starts = vec![from];
    let bytes = &text.as_bytes()[from..to];
    let mut at = 0;
    while let Some(found) = newline::position(&bytes[at..]) {
        let start = from + at + found + 1;
        if to_end || start < to {
            starts.push(start);
        }
        at += found + 1;
    }
    let next_after_region = (!to_end).then_some(to);
    let mut lines = Vec::with_capacity(starts.len());
    for (i, &start) in starts.iter().enumerate() {
        let next = starts.get(i + 1).copied().or(next_after_region);
        let end = content_end(text, start, next);
        let crlf = next.is_some_and(|n| n >= 2 && text.as_bytes()[n - 2] == b'\r');
        let line = &text[start..end];
        let mut positions: Vec<Position> = Vec::new();
        let mut columns = 0;
        let mut chars = 0;
        if line.len() <= 256 {
            columns = columns_of(line, tab_width);
        } else {
            // Long lines have a sparse index for viewport and cursor lookups.
            // Never split a grapheme, including combining marks and emoji.
            for (byte, grapheme) in line.grapheme_indices(true) {
                if positions.is_empty() || byte - positions.last().unwrap().byte >= 256 {
                    positions.push(Position {
                        byte,
                        chars,
                        columns,
                    });
                }
                columns += grapheme_width_with_tab_width(grapheme, columns, tab_width);
                chars += grapheme.chars().count();
            }
            positions.push(Position {
                byte: line.len(),
                chars,
                columns,
            });
        }
        lines.push(Line {
            bytes: next.unwrap_or(text.len()) - start,
            width: columns as u32,
            crlf,
            positions: (!positions.is_empty()).then(|| Arc::new(positions)),
        });
    }
    lines
}

/// Display columns of one line's content.
fn columns_of(line: &str, tab_width: usize) -> usize {
    if line.is_ascii() && !line.as_bytes().contains(&b'\t') {
        return line.len();
    }
    line.graphemes(true).fold(0, |columns, grapheme| {
        columns + grapheme_width_with_tab_width(grapheme, columns, tab_width)
    })
}

const PACKED_NEEDS_TEXT: &str = "a packed block needs the text it describes";

impl LineIndex {
    pub fn new(text: &str) -> Self {
        Self::with_tab_width(text, TAB_WIDTH)
    }
    pub fn with_tab_width(text: &str, tab_width: usize) -> Self {
        // Blocks are scanned and packed one at a time, so even a very large text never holds
        // per-line data for more than one block. Sizes are spread as `from_lines` would.
        let total = newline::count(text.as_bytes()) + 1;
        let blocks = total.div_ceil(line_tree::BLOCK);
        let (mut remaining, mut from) = (total, 0);
        let tree = LineTree::from_packed_blocks(blocks, |i| {
            let size = remaining.div_ceil(blocks - i);
            remaining -= size;
            let last = i + 1 == blocks;
            let to = if last {
                text.len()
            } else {
                // The block ends where the next one starts: after its `size`th line break.
                from + newline::nth(&text.as_bytes()[from..], size - 1).unwrap() + 1
            };
            let lines = scan_lines(text, from, to, last, tab_width);
            from = to;
            lines
        });
        Self { tab_width, tree }
    }

    /// Append the lines starting in `text[from..to]`. `to_end` says whether
    /// `to` is the end of the text, so a final line break opens a last line.
    fn scan(&mut self, text: &str, from: usize, to: usize, to_end: bool) {
        self.tree
            .extend(scan_lines(text, from, to, to_end, self.tab_width));
    }

    pub fn mixed_endings(&self) -> bool {
        self.tree.crlf_count() > 0 && self.tree.crlf_count() < self.len().saturating_sub(1)
    }
    pub fn len(&self) -> usize {
        self.tree.len()
    }

    /// Where a line starts. `text` may be left out only while rows of unpacked blocks are read,
    /// which is what an update does after `prepare`.
    fn start_in(&self, text: Option<&str>, line: usize) -> usize {
        let found = self.tree.locate_row(line);
        let skip = line - found.base_row;
        match found.data {
            Some((_, starts)) => found.base_byte + starts[skip],
            None if skip == 0 => found.base_byte,
            None => {
                let text = text.expect(PACKED_NEEDS_TEXT);
                let block = &text.as_bytes()[found.base_byte..found.base_byte + found.bytes];
                found.base_byte + newline::nth(block, skip - 1).unwrap() + 1
            }
        }
    }
    pub fn start(&self, text: &str, line: usize) -> usize {
        self.start_in(Some(text), line)
    }

    fn next_start_in(&self, text: Option<&str>, line: usize) -> Option<usize> {
        (line + 1 < self.len()).then(|| self.start_in(text, line + 1))
    }
    /// The start of the following line, if there is one.
    pub fn next_start(&self, text: &str, line: usize) -> Option<usize> {
        self.next_start_in(Some(text), line)
    }

    pub fn width(&self, text: &str, line: usize) -> usize {
        let found = self.tree.locate_row(line);
        match found.data {
            Some((lines, _)) => lines[line - found.base_row].width as usize,
            None => {
                let start = self.start(text, line);
                let end = self.content_end(text, line);
                columns_of(&text[start..end], self.tab_width)
            }
        }
    }

    pub fn max_width(&self) -> usize {
        self.tree.max_width()
    }

    pub fn position_by(
        &self,
        line: usize,
        key: usize,
        value: impl Fn(&Position) -> usize,
    ) -> Position {
        let found = self.tree.locate_row(line);
        // Packed blocks hold only lines short enough to need no checkpoints.
        let positions = found.data.map_or(&[][..], |(lines, _)| {
            lines[line - found.base_row].positions()
        });
        let index = positions.partition_point(|p| value(p) <= key);
        index
            .checked_sub(1)
            .map(|i| positions[i].clone())
            .unwrap_or(Position {
                byte: 0,
                chars: 0,
                columns: 0,
            })
    }

    fn line_of_in(&self, text: Option<&str>, offset: usize) -> usize {
        let found = self.tree.locate_byte(offset);
        let within = offset - found.base_byte;
        match found.data {
            Some((_, starts)) => {
                found.base_row + starts.partition_point(|&s| s <= within).saturating_sub(1)
            }
            None => {
                let text = text.expect(PACKED_NEEDS_TEXT);
                let block = &text.as_bytes()[found.base_byte..found.base_byte + within];
                found.base_row + newline::count(block)
            }
        }
    }
    /// The line containing `offset`; a line break belongs to the line it ends.
    pub fn line_of(&self, text: &str, offset: usize) -> usize {
        self.line_of_in(Some(text), offset)
    }

    pub fn content_end(&self, text: &str, line: usize) -> usize {
        content_end(text, self.start(text, line), self.next_start(text, line))
    }

    /// Bring back the lines of every block an edit to `ranges` will rebuild: the blocks the
    /// edits touch and their neighbours. Call before the text changes, since the lines are read
    /// from it; `update` then needs no text for the old index.
    pub fn prepare(&mut self, text: &str, ranges: impl Iterator<Item = std::ops::Range<usize>>) {
        for range in ranges {
            for offset in [range.start, range.end] {
                let row = self.line_of(text, offset);
                let found = self.tree.locate_row(row);
                let (before, after) = (found.base_row, found.base_row + found.rows);
                if before > 0 {
                    self.unpack(text, before - 1);
                }
                self.unpack(text, row);
                if after < self.len() {
                    self.unpack(text, after);
                }
            }
        }
    }
    fn unpack(&mut self, text: &str, row: usize) {
        let tab_width = self.tab_width;
        self.tree.unpack(row, |from, bytes, last| {
            scan_lines(text, from, from + bytes, last, tab_width)
        });
    }

    /// Rebuild only overlapping groups of touched lines in the edited text.
    pub fn update(&mut self, text: &str, changes: &ChangeSet) {
        let edits = changes.changes();
        let (Some(first), Some(last)) = (edits.first(), edits.last()) else {
            return;
        };
        let first = self.line_of_in(None, first.old.start);
        let last = self.line_of_in(None, last.old.end);
        let before = self.len();
        self.update_unpacked(text, changes);
        // Blocks rebuilt for the edit were packed again; this catches the neighbours that were
        // unpacked for it but not replaced.
        let last = (last + self.len()).saturating_sub(before);
        self.tree.pack_range(
            first.saturating_sub(line_tree::BLOCK + 1)
                ..(last + line_tree::BLOCK + 1).min(self.len()),
        );
    }

    fn update_unpacked(&mut self, text: &str, changes: &ChangeSet) {
        if self.update_long_line(text, changes) {
            return;
        }
        let edits = changes.changes();
        if let [change] = edits {
            self.update_single(text, change);
            return;
        }
        if edits.is_empty() {
            return;
        }
        // Merge only overlapping old line ranges. Offsets of later groups are
        // mapped by the accumulated byte delta, so insertions and deletions are
        // handled just like equal-length replacements.
        let mut groups: Vec<(usize, usize, usize, usize)> = Vec::new();
        for (i, edit) in edits.iter().enumerate() {
            let first = self.line_of_in(None, edit.old.start);
            let last = self.line_of_in(None, edit.old.end);
            if let Some(group) = groups.last_mut()
                && first <= group.1
            {
                group.1 = group.1.max(last);
                group.3 = i + 1;
            } else {
                groups.push((first, last, i, i + 1));
            }
        }
        let mut rebuilt = Vec::with_capacity(groups.len());
        let mut delta = 0isize;
        for (first, last, from, to) in groups {
            let start = (self.start_in(None, first) as isize + delta) as usize;
            delta += edits[from..to]
                .iter()
                .map(|c| c.inserted.len() as isize - c.removed.len() as isize)
                .sum::<isize>();
            let following = self.next_start_in(None, last);
            let end = following.map_or(text.len(), |n| (n as isize + delta) as usize);
            let mut region = Self {
                tree: LineTree::default(),
                tab_width: self.tab_width,
            };
            region.scan_changed(
                text,
                self,
                first..last + 1,
                &edits[from..to],
                start..end,
                following.is_none(),
            );
            rebuilt.push((first..last + 1, region.tree));
        }
        for (range, region) in rebuilt.into_iter().rev() {
            self.tree.replace(range, region);
        }
    }

    fn update_single(&mut self, text: &str, change: &Change) {
        let first = self.line_of_in(None, change.old.start);
        let last = self.line_of_in(None, change.old.end);
        let delta = change.inserted.len() as isize - change.removed.len() as isize;
        let following = self.next_start_in(None, last);
        let end = following.map_or(text.len(), |n| (n as isize + delta) as usize);
        let mut rebuilt = Self {
            tab_width: self.tab_width,
            tree: LineTree::default(),
        };
        rebuilt.scan_changed(
            text,
            self,
            first..last + 1,
            std::slice::from_ref(change),
            self.start_in(None, first)..end,
            following.is_none(),
        );
        self.tree.replace(first..last + 1, rebuilt.tree);
    }

    /// Reuse the unchanged sparse prefix and suffix even when a line splits or
    /// several lines join. Only the middle needs grapheme segmentation.
    fn scan_changed(
        &mut self,
        text: &str,
        old: &Self,
        lines: std::ops::Range<usize>,
        changes: &[Change],
        region: std::ops::Range<usize>,
        to_end: bool,
    ) {
        let first = lines.start;
        let last = lines.end - 1;
        let first_positions = old.tree.get(first).positions();
        let last_positions = old.tree.get(last).positions();
        if first_positions.is_empty() && last_positions.is_empty() {
            self.scan(text, region.start, region.end, to_end);
            return;
        }
        let prefix_index = first_positions
            .partition_point(|p| p.byte < changes[0].old.start - old.start_in(None, first))
            .saturating_sub(1);
        let prefix = first_positions
            .get(prefix_index)
            .cloned()
            .unwrap_or(Position {
                byte: 0,
                chars: 0,
                columns: 0,
            });
        let tail = changes.last().unwrap();
        let suffix_index =
            last_positions.partition_point(|p| p.byte <= tail.old.end - old.start_in(None, last));
        let suffix = last_positions.get(suffix_index).and_then(|p| {
            let offset = tail.new_start
                + tail.inserted.len()
                + (old.start_in(None, last) + p.byte - tail.old.end);
            (offset <= region.end
                && text.is_char_boundary(offset)
                && unicode_segmentation::GraphemeCursor::new(offset, text.len(), true)
                    .is_boundary(text, 0)
                    .unwrap_or(false))
            .then_some((offset, p))
        });
        let scan_from = region.start + prefix.byte;
        let scan_to = suffix.map_or(region.end, |(offset, _)| offset);
        let mut starts = vec![region.start];
        for (i, byte) in text.as_bytes()[scan_from..scan_to].iter().enumerate() {
            if *byte == b'\n' && (to_end || scan_from + i + 1 < region.end) {
                starts.push(scan_from + i + 1);
            }
        }
        let mut rebuilt = Vec::with_capacity(starts.len());
        for (i, &start) in starts.iter().enumerate() {
            let next = starts
                .get(i + 1)
                .copied()
                .or((!to_end).then_some(region.end));
            let end = content_end(text, start, next);
            let long = end - start > 256;
            let (mut chars, mut columns, from) = if i == 0 {
                (prefix.chars, prefix.columns, scan_from)
            } else {
                (0, 0, start)
            };
            let last_line = i + 1 == starts.len();
            let until = if last_line {
                suffix.map_or(end, |(offset, _)| offset)
            } else {
                end
            };
            let prefix_count = if i == 0 { prefix_index } else { 0 };
            let suffix_count = if last_line && suffix.is_some() {
                last_positions.len() - suffix_index
            } else {
                0
            };
            let mut positions = Vec::with_capacity(if long {
                prefix_count + suffix_count + (until - from) / 256 + 2
            } else {
                0
            });
            if long && i == 0 {
                positions.extend_from_slice(&first_positions[..prefix_index]);
            }
            for (at, grapheme) in text[from..until].grapheme_indices(true) {
                let byte = from + at - start;
                if long && positions.last().is_none_or(|p| byte - p.byte >= 256) {
                    positions.push(Position {
                        byte,
                        chars,
                        columns,
                    });
                }
                columns += grapheme_width_with_tab_width(grapheme, columns, self.tab_width);
                chars += grapheme.chars().count();
            }
            if let Some((offset, after)) = suffix.filter(|_| last_line) {
                let byte_delta = (offset - start) as isize - after.byte as isize;
                let char_delta = chars as isize - after.chars as isize;
                let column_delta = columns as isize - after.columns as isize;
                // A changed tab phase converges at the first unchanged tab.
                // Locate it by bytes, then measure only its checkpoint fragment.
                let tab = (column_delta % self.tab_width as isize != 0)
                    .then(|| text[offset..end].find('\t'))
                    .flatten()
                    .map(|at| {
                        let old_byte = after.byte + at;
                        let p = &last_positions[last_positions
                            .partition_point(|p| p.byte <= old_byte)
                            .saturating_sub(1)];
                        let from = (start as isize + p.byte as isize + byte_delta) as usize;
                        let column = text[from..offset + at]
                            .graphemes(true)
                            .fold(p.columns, |col, g| {
                                col + grapheme_width_with_tab_width(g, col, self.tab_width)
                            });
                        let new_column = (column as isize + column_delta) as usize;
                        let after_tab = new_column + self.tab_width - new_column % self.tab_width;
                        let old_after_tab = column + self.tab_width - column % self.tab_width;
                        (old_byte, after_tab as isize - old_after_tab as isize)
                    });
                for p in &last_positions[suffix_index..] {
                    let shift = tab
                        .filter(|(at, _)| p.byte > *at)
                        .map_or(column_delta, |(_, d)| d);
                    if long {
                        positions.push(Position {
                            byte: (p.byte as isize + byte_delta) as usize,
                            chars: (p.chars as isize + char_delta) as usize,
                            columns: (p.columns as isize + shift) as usize,
                        });
                    }
                }
                let terminal = last_positions.last().unwrap();
                columns =
                    (terminal.columns as isize + tab.map_or(column_delta, |(_, d)| d)) as usize;
            } else if long {
                positions.push(Position {
                    byte: end - start,
                    chars,
                    columns,
                });
            }
            let crlf = next.is_some_and(|n| n >= 2 && text.as_bytes()[n - 2] == b'\r');
            rebuilt.push(Line {
                bytes: next.unwrap_or(text.len()) - start,
                width: columns as u32,
                crlf,
                positions: (!positions.is_empty()).then(|| Arc::new(positions)),
            });
        }
        self.tree.extend(rebuilt);
    }

    /// Re-index the edit's surrounding graphemes, shifting sparse tail entries.
    /// A tab-phase or grapheme-context change falls back to the full scanner.
    fn update_long_line(&mut self, text: &str, changes: &ChangeSet) -> bool {
        let [change] = changes.changes() else {
            return false;
        };
        if change.inserted.contains(['\r', '\n']) || change.removed.contains(['\r', '\n']) {
            return false;
        }
        let line = self.line_of_in(None, change.old.start);
        if self.line_of_in(None, change.old.end) != line {
            return false;
        }
        let start = self.start_in(None, line);
        let delta = change.inserted.len() as isize - change.removed.len() as isize;
        let next = self
            .next_start_in(None, line)
            .map(|offset| (offset as isize + delta) as usize);
        let end = content_end(text, start, next);
        if end - start <= 256 || self.tree.get(line).positions().len() < 3 {
            return false;
        }
        let positions = self.tree.get(line).positions();
        let from = positions
            .partition_point(|p| p.byte < change.old.start - start)
            .saturating_sub(1);
        let to = (positions.partition_point(|p| p.byte <= change.old.end - start) + 1)
            .min(positions.len() - 1);
        let before = positions[from].clone();
        let after = positions[to].clone();
        let tail = (after.byte as isize + delta) as usize;
        let tail_offset = start + tail;
        if tail_offset > end
            || !text.is_char_boundary(tail_offset)
            || !unicode_segmentation::GraphemeCursor::new(tail_offset, text.len(), true)
                .is_boundary(text, 0)
                .unwrap_or(false)
        {
            return false;
        }
        let mut columns = before.columns;
        let mut chars = before.chars;
        let mut last = before.byte;
        let mut rebuilt = Vec::new();
        for (byte, grapheme) in text[start + before.byte..tail_offset].grapheme_indices(true) {
            let byte = before.byte + byte;
            if byte - last >= 256 {
                rebuilt.push(Position {
                    byte,
                    chars,
                    columns,
                });
                last = byte;
            }
            columns += grapheme_width_with_tab_width(grapheme, columns, self.tab_width);
            chars += grapheme.chars().count();
        }
        let column_delta = columns as isize - after.columns as isize;
        if column_delta % self.tab_width as isize != 0 && text[tail_offset..end].contains('\t') {
            return false;
        }
        let char_delta = chars as isize - after.chars as isize;
        let crlf = next.is_some_and(|n| n >= 2 && text.as_bytes()[n - 2] == b'\r');
        self.tree.update_line(line, |info| {
            let positions = Arc::make_mut(info.positions.as_mut().unwrap());
            for position in &mut positions[to..] {
                position.byte = (position.byte as isize + delta) as usize;
                position.chars = (position.chars as isize + char_delta) as usize;
                position.columns = (position.columns as isize + column_delta) as usize;
            }
            positions.splice(from + 1..to, rebuilt);
            info.bytes = (info.bytes as isize + delta) as usize;
            info.width = (info.width as isize + column_delta) as u32;
            info.crlf = crlf;
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::buffer::apply;
    use super::super::change::{Edit, validate};
    use super::*;

    #[test]
    fn display_width_preserves_ascii_controls_and_unicode_sequences() {
        let reference = |text: &str| {
            text.graphemes(true).fold(0, |column, grapheme| {
                column + grapheme_width(grapheme, column)
            })
        };
        for left in 0..=127u8 {
            for right in 0..=127u8 {
                let text = String::from_utf8(vec![left, right]).unwrap();
                assert_eq!(display_width(&text), reference(&text), "{text:?}");
            }
        }
        for text in [
            "",
            "ordinary ASCII text",
            "a\tbb\tccc",
            "a\r\nb\n",
            "中文\tend",
            "e\u{301}x",
            "👩\u{200d}💻\t🇨🇳",
            "a\u{fe0f}\u{20e3}",
        ] {
            assert_eq!(display_width(text), reference(text), "{text:?}");
        }
    }

    #[test]
    fn lines_split_on_lf_and_crlf_and_measure_tabs_and_wide_characters() {
        let text = "a\tb\r\n中文\n\nend\n";
        let index = LineIndex::new(text);
        assert_eq!(
            (0..index.len())
                .map(|n| index.start(text, n))
                .collect::<Vec<_>>(),
            [0, 5, 12, 13, 17]
        );
        assert_eq!(
            (0..index.len())
                .map(|n| index.width(text, n))
                .collect::<Vec<_>>(),
            [5, 4, 0, 3, 0]
        );
        assert_eq!(index.content_end(text, 0), 3);
        assert_eq!(
            index.line_of(text, 4),
            0,
            "the LF belongs to the line it ends"
        );
        assert_eq!(index.line_of(text, 5), 1);
        assert_eq!(index.line_of(text, 17), 4);
    }

    /// Every incremental update must equal a fresh scan of the result.
    #[test]
    fn incremental_updates_match_a_full_rebuild() {
        let mut text = String::from("one\ntwo\r\nthree\n\tfour\nfive");
        let mut index = LineIndex::new(&text);
        let mut seed = 7u64;
        let mut next = |bound: usize| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) as usize) % bound.max(1)
        };
        let pieces = ["", "x", "\n", "\r\n", "a\nb", "\t", "中\n\n", "end"];
        for _ in 0..500 {
            let mut edits = Vec::new();
            let mut cursor = 0;
            while cursor < text.len() && edits.len() < 3 {
                let start = cursor + next(text.len() - cursor + 1);
                let end = (start + next(6)).min(text.len());
                if start > text.len() {
                    break;
                }
                let (start, end) = (floor(&text, start), floor(&text, end));
                edits.push(Edit::replace(start..end, pieces[next(pieces.len())]));
                cursor = end + 1;
            }
            let Ok(edits) = validate(&text, edits) else {
                continue;
            };
            edit_text(&mut index, &mut text, edits);
            assert_same(&text, &index);
        }
    }

    /// Edit the text the way a buffer does: unpack what the edit rebuilds, change the text,
    /// then update the index from the new text.
    fn edit_text(index: &mut LineIndex, text: &mut String, edits: Vec<Edit>) {
        index.prepare(text, edits.iter().map(|edit| edit.range.clone()));
        let changes = apply(text, edits);
        index.update(text, &changes);
    }

    /// Many blocks, edits spanning several of them, and a few long lines: every update must
    /// match a fresh scan, and blocks of plain lines must go back to holding no lines.
    #[test]
    fn edits_across_many_packed_blocks_match_a_full_rebuild_and_stay_packed() {
        let mut text: String = (0..4000)
            .map(|i| match i % 41 {
                0 => format!("{}\r\n", "long中line ".repeat(60)),
                1 => format!("\t{i} tab\r\n"),
                _ => format!("line {i} 中\n"),
            })
            .collect();
        let mut index = LineIndex::new(&text);
        let long_lines = (0..index.len())
            .filter(|&row| index.content_end(&text, row) - index.start(&text, row) > 256);
        let long_blocks = long_lines
            .map(|row| index.tree.locate_row(row).base_row)
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        assert_eq!(index.tree.unpacked_leaves(), long_blocks);
        let pieces = ["", "x", "\n", "\r\n", "a\nb\nc", "\t", "中\n\n", "end\n"];
        let mut seed = 11u64;
        let mut next = |bound: usize| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) as usize) % bound.max(1)
        };
        for _ in 0..150 {
            let mut edits = Vec::new();
            let mut cursor = 0;
            while cursor < text.len() && edits.len() < 4 {
                // Edits may be far apart, so a batch reaches several blocks.
                let start = floor(&text, cursor + next((text.len() - cursor) / 2 + 1));
                let end = floor(&text, (start + next(300)).min(text.len()));
                edits.push(Edit::replace(start..end, pieces[next(pieces.len())]));
                cursor = end + 1;
            }
            let Ok(edits) = validate(&text, edits) else {
                continue;
            };
            edit_text(&mut index, &mut text, edits);
            assert_same(&text, &index);
        }
        let long_blocks = (0..index.len())
            .filter(|&row| index.content_end(&text, row) - index.start(&text, row) > 256)
            .map(|row| index.tree.locate_row(row).base_row)
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        assert!(
            index.tree.unpacked_leaves() <= long_blocks + 2,
            "{} blocks hold their lines, {long_blocks} contain a long line",
            index.tree.unpacked_leaves()
        );
    }

    #[test]
    fn disjoint_edits_with_different_lengths_preserve_untouched_lines() {
        let original = format!("head\r\n{}tail\n", "中\tstay\r\n".repeat(4096));
        for inserted in ["longer", "", "new\nlines\r\n", "e\u{301}"] {
            let mut text = original.clone();
            let mut index = LineIndex::new(&text);
            let at = text.len() - 5;
            let edits = validate(
                &original,
                vec![
                    Edit::replace(0..4, inserted),
                    Edit::replace(at..at + 4, "尾"),
                ],
            )
            .unwrap();
            edit_text(&mut index, &mut text, edits);
            assert_same(&text, &index);
        }
    }

    /// Every line as the index describes it, packed blocks read back from the text.
    fn rows(index: &LineIndex, text: &str) -> Vec<Line> {
        (0..index.len())
            .map(|row| {
                let start = index.start(text, row);
                let next = index.next_start(text, row);
                let found = index.tree.locate_row(row);
                Line {
                    bytes: next.unwrap_or(text.len()) - start,
                    width: index.width(text, row) as u32,
                    crlf: next.is_some_and(|n| n >= 2 && text.as_bytes()[n - 2] == b'\r'),
                    positions: found
                        .data
                        .and_then(|(lines, _)| lines[row - found.base_row].positions.clone()),
                }
            })
            .collect()
    }

    /// An updated index must describe the text as a fresh scan does. Checkpoints inside long lines
    /// may differ in where they fall; `assert_sparse_index` checks that they are valid.
    fn assert_same(text: &str, index: &LineIndex) {
        let fresh = LineIndex::with_tab_width(text, index.tab_width);
        let (actual, expected) = (rows(index, text), rows(&fresh, text));
        assert_eq!(actual.len(), expected.len());
        for (row, (a, b)) in actual.iter().zip(&expected).enumerate() {
            let summary = |l: &Line| (l.bytes, l.width, l.crlf, l.positions.is_some());
            assert_eq!(summary(a), summary(b), "line {row}");
        }
        assert_eq!(index.max_width(), fresh.max_width());
        assert_eq!(index.tree.crlf_count(), fresh.tree.crlf_count());
        assert_eq!(index.mixed_endings(), fresh.mixed_endings());
    }

    fn assert_sparse_index(text: &str, index: &LineIndex) {
        assert_same(text, index);
        for line in 0..index.len() {
            let start = index.start(text, line);
            let end = index.content_end(text, line);
            let positions = index.tree.locate_row(line);
            let positions = positions.data.map_or(&[][..], |(lines, _)| {
                lines[line - positions.base_row].positions()
            });
            let mut checkpoints = positions.iter().peekable();
            let (mut chars, mut columns) = (0, 0);
            for (byte, g) in text[start..end]
                .grapheme_indices(true)
                .chain(std::iter::once((end - start, "")))
            {
                if checkpoints.peek().is_some_and(|p| p.byte == byte) {
                    assert_eq!(
                        checkpoints.next().unwrap(),
                        &Position {
                            byte,
                            chars,
                            columns
                        }
                    );
                }
                chars += g.chars().count();
                columns += grapheme_width_with_tab_width(g, columns, index.tab_width);
            }
            assert!(checkpoints.next().is_none(), "checkpoint split a grapheme");
        }
    }

    #[test]
    fn long_line_splits_joins_and_sparse_tabs_reuse_correct_checkpoints() {
        for width in [2, 4, 8] {
            for pattern in ["a", "中e\u{301}😀", "🇨🇳", "👩\u{200d}💻"] {
                let mut text = format!("{}\tend\r\n{}", pattern.repeat(1500), pattern.repeat(1200));
                let mut index = LineIndex::with_tab_width(&text, width);
                for step in 0..4 {
                    let (range, insert) = match step {
                        0 => (0..0, "x"),
                        1 => (text.len()..text.len(), "\n"),
                        2 => (0..0, "\n"),
                        _ => (0..1, ""),
                    };
                    let edits = validate(&text, vec![Edit::replace(range, insert)]).unwrap();
                    edit_text(&mut index, &mut text, edits);
                    assert_sparse_index(&text, &index);
                }
                let boundary = text.find('\n').unwrap();
                let start =
                    boundary.saturating_sub(usize::from(text.as_bytes()[boundary - 1] == b'\r'));
                let edits =
                    validate(&text, vec![Edit::replace(start..boundary + 1, "e\u{301}")]).unwrap();
                edit_text(&mut index, &mut text, edits);
                assert_sparse_index(&text, &index);
            }
        }
    }

    #[test]
    fn randomized_long_line_edits_match_full_segmentation() {
        let mut text = format!(
            "{}\t{}\r\n{}",
            "中a".repeat(700),
            "🇨🇳".repeat(300),
            "abc".repeat(700)
        );
        let mut index = LineIndex::new(&text);
        let pieces = ["", "\n", "\r\n", "\t", "e\u{301}", "🇨🇳", "👩\u{200d}💻"];
        let mut seed = 17u64;
        for i in 0..200 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let at = floor(&text, seed as usize % (text.len() + 1));
            let end = floor(&text, (at + (seed >> 32) as usize % 50).min(text.len()));
            edit_text(
                &mut index,
                &mut text,
                vec![Edit::replace(at..end, pieces[i % pieces.len()])],
            );
            assert_sparse_index(&text, &index);
        }
    }

    fn floor(text: &str, mut offset: usize) -> usize {
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }
}
