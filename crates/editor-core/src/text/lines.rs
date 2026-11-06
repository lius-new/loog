//! Line starts and display widths, kept up to date across edits.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::change::{Change, ChangeSet};
use super::line_tree::{Line, LineTree};
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LineIndex {
    tree: LineTree,
    tab_width: usize,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        Self::with_tab_width(text, TAB_WIDTH)
    }
    pub fn with_tab_width(text: &str, tab_width: usize) -> Self {
        let mut index = Self {
            tab_width,
            tree: LineTree::default(),
        };
        index.scan(text, 0, text.len(), true);
        index
    }

    /// Append the lines starting in `text[from..to]`. `to_end` says whether
    /// `to` is the end of the text, so a final line break opens a last line.
    fn scan(&mut self, text: &str, from: usize, to: usize, to_end: bool) {
        let mut starts = vec![from];
        for (i, byte) in text.as_bytes()[from..to].iter().enumerate() {
            if *byte == b'\n' && (to_end || from + i + 1 < to) {
                starts.push(from + i + 1);
            }
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
            // Long lines have a sparse index for viewport and cursor lookups.
            // Never split a grapheme, including combining marks and emoji.
            for (byte, grapheme) in line.grapheme_indices(true) {
                if line.len() > 256
                    && (positions.is_empty() || byte - positions.last().unwrap().byte >= 256)
                {
                    positions.push(Position {
                        byte,
                        chars,
                        columns,
                    });
                }
                columns += grapheme_width_with_tab_width(grapheme, columns, self.tab_width);
                chars += grapheme.chars().count();
            }
            if !positions.is_empty() {
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
        self.tree.extend(lines);
    }

    pub fn mixed_endings(&self) -> bool {
        self.tree.crlf_count() > 0 && self.tree.crlf_count() < self.len().saturating_sub(1)
    }
    pub fn len(&self) -> usize {
        self.tree.len()
    }

    pub fn start(&self, line: usize) -> usize {
        self.tree.start(line)
    }

    /// The start of the following line, if there is one.
    pub fn next_start(&self, line: usize) -> Option<usize> {
        (line + 1 < self.len()).then(|| self.start(line + 1))
    }

    pub fn width(&self, line: usize) -> usize {
        self.tree.get(line).width as usize
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
        let positions = self.tree.get(line).positions();
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

    /// The line containing `offset`; a line break belongs to the line it ends.
    pub fn line_of(&self, offset: usize) -> usize {
        self.tree.line_of(offset)
    }

    pub fn content_end(&self, text: &str, line: usize) -> usize {
        content_end(text, self.start(line), self.next_start(line))
    }

    /// Rebuild only overlapping groups of touched lines in the edited text.
    pub fn update(&mut self, text: &str, changes: &ChangeSet) {
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
            let first = self.line_of(edit.old.start);
            let last = self.line_of(edit.old.end);
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
            let start = (self.start(first) as isize + delta) as usize;
            delta += edits[from..to]
                .iter()
                .map(|c| c.inserted.len() as isize - c.removed.len() as isize)
                .sum::<isize>();
            let following = self.next_start(last);
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
        let first = self.line_of(change.old.start);
        let last = self.line_of(change.old.end);
        let delta = change.inserted.len() as isize - change.removed.len() as isize;
        let following = self.next_start(last);
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
            self.start(first)..end,
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
            .partition_point(|p| p.byte < changes[0].old.start - old.start(first))
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
            last_positions.partition_point(|p| p.byte <= tail.old.end - old.start(last));
        let suffix = last_positions.get(suffix_index).and_then(|p| {
            let offset =
                tail.new_start + tail.inserted.len() + (old.start(last) + p.byte - tail.old.end);
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
        let line = self.line_of(change.old.start);
        if self.line_of(change.old.end) != line {
            return false;
        }
        let start = self.start(line);
        let delta = change.inserted.len() as isize - change.removed.len() as isize;
        let next = self
            .next_start(line)
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
            (0..index.len()).map(|n| index.start(n)).collect::<Vec<_>>(),
            [0, 5, 12, 13, 17]
        );
        assert_eq!(
            (0..index.len()).map(|n| index.width(n)).collect::<Vec<_>>(),
            [5, 4, 0, 3, 0]
        );
        assert_eq!(index.content_end(text, 0), 3);
        assert_eq!(index.line_of(4), 0, "the LF belongs to the line it ends");
        assert_eq!(index.line_of(5), 1);
        assert_eq!(index.line_of(17), 4);
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
            let changes = apply(&mut text, edits);
            index.update(&text, &changes);
            assert_eq!(index, LineIndex::new(&text), "after edits to {text:?}");
        }
    }

    #[test]
    fn disjoint_edits_with_different_lengths_preserve_untouched_lines() {
        let original = format!("head\r\n{}tail\n", "中\tstay\r\n".repeat(4096));
        for inserted in ["longer", "", "new\nlines\r\n", "e\u{301}"] {
            let mut text = original.clone();
            let mut index = LineIndex::new(&text);
            let at = text.len() - 5;
            let changes = apply(
                &mut text,
                validate(
                    &original,
                    vec![
                        Edit::replace(0..4, inserted),
                        Edit::replace(at..at + 4, "尾"),
                    ],
                )
                .unwrap(),
            );
            index.update(&text, &changes);
            assert_eq!(index, LineIndex::new(&text));
        }
    }

    fn assert_sparse_index(text: &str, index: &LineIndex) {
        let fresh = LineIndex::with_tab_width(text, index.tab_width);
        assert_eq!(index.len(), fresh.len());
        for n in 0..index.len() {
            assert_eq!(index.start(n), fresh.start(n));
            assert_eq!(index.width(n), fresh.width(n));
            assert_eq!(index.tree.get(n).crlf, fresh.tree.get(n).crlf);
        }
        assert_eq!(index.tree.crlf_count(), fresh.tree.crlf_count());
        assert_eq!(index.max_width(), fresh.max_width());
        for line in 0..index.len() {
            let start = index.start(line);
            let end = index.content_end(text, line);
            let mut checkpoints = index.tree.get(line).positions().iter().peekable();
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
                    let changes = apply(&mut text, edits);
                    index.update(&text, &changes);
                    assert_sparse_index(&text, &index);
                }
                let boundary = text.find('\n').unwrap();
                let start =
                    boundary.saturating_sub(usize::from(text.as_bytes()[boundary - 1] == b'\r'));
                let edits =
                    validate(&text, vec![Edit::replace(start..boundary + 1, "e\u{301}")]).unwrap();
                let changes = apply(&mut text, edits);
                index.update(&text, &changes);
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
            let changes = apply(
                &mut text,
                vec![Edit::replace(at..end, pieces[i % pieces.len()])],
            );
            index.update(&text, &changes);
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
