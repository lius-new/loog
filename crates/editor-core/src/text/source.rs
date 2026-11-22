//! Where a text that stays outside memory comes from, and what is known about its lines.
//!
//! A `TextSource` is read by ranges. The editing core never opens files itself: an adapter that
//! can (a file, here a memory buffer) implements it, and `TextBuffer::from_source` and
//! `PieceText` work on top of it, so a text of any size costs a few bytes per thousand lines.

use std::io;
use std::ops::Range;
use std::sync::{Arc, Mutex};

use super::newline;

/// A line start is kept for every this many lines; a lookup scans at most this many lines.
const LINES_PER_CHECKPOINT: usize = 64;
/// How much is looked at to count or find line breaks near a place. Lines are far shorter than a
/// block, and a lookup that starts near where the last one ended needs only a few of them.
const LOOK: usize = 8 * 1024;

/// A text read by byte ranges. It is UTF-8 as a whole, without its byte order mark if it has one.
pub trait TextSource: Send + Sync + std::fmt::Debug {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Whether the text came with a UTF-8 byte order mark, to be written back.
    fn has_bom(&self) -> bool;
    fn line_count(&self) -> usize;
    /// The longest line's length in bytes: an upper bound of its width for sizing scroll ranges.
    fn longest_line(&self) -> usize;
    fn first_line_is_crlf(&self) -> bool;
    /// The bytes of `range`, which is cut to the text.
    fn read(&self, range: Range<usize>) -> io::Result<Vec<u8>>;
    /// Read `range` into `buffer`, past any cache, for copying the whole text.
    fn read_into(&self, range: Range<usize>, buffer: &mut [u8]) -> io::Result<()>;
    /// Byte offset where `line` starts; the end of the text for a line past the last.
    fn line_start(&self, line: usize) -> usize;
    /// The line containing `offset`; a line break belongs to the line it ends.
    fn line_of(&self, offset: usize) -> usize;
}

/// A source shared by the buffers and pieces that read it.
pub type SharedSource = Arc<dyn TextSource>;

/// What a pass over the bytes of a text learns about its lines. An adapter makes it while it
/// reads the text to check it; a writer can make it while the text is written, so the file need
/// not be read again to be opened.
pub struct IndexBuilder {
    per_checkpoint: usize,
    checkpoints: Vec<u64>,
    lines: usize,
    line_start: usize,
    longest_line: usize,
    crlf_first: bool,
    seen_first_break: bool,
    previous_last: u8,
    pos: usize,
}

/// The lines of a text: where every 64th starts, how many there are, and where the last lookup
/// ended, so that the next one, which drawing makes for the line after, starts there.
#[derive(Debug)]
pub struct FileIndex {
    len: usize,
    checkpoints: Vec<u64>,
    per_checkpoint: usize,
    lines: usize,
    longest_line: usize,
    crlf_first: bool,
    /// Where the last lookup ended, as (offset, line breaks before it).
    last: Mutex<(usize, usize)>,
}

impl Default for IndexBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl IndexBuilder {
    pub fn new() -> Self {
        Self::with_checkpoints(LINES_PER_CHECKPOINT)
    }
    pub(crate) fn with_checkpoints(per_checkpoint: usize) -> Self {
        Self {
            per_checkpoint,
            checkpoints: vec![0],
            lines: 1,
            line_start: 0,
            longest_line: 0,
            crlf_first: false,
            seen_first_break: false,
            previous_last: 0,
            pos: 0,
        }
    }
    /// The next bytes of the text, in order.
    pub fn feed(&mut self, bytes: &[u8]) {
        let base = self.pos;
        let mut from = 0;
        while let Some(found) = newline::position(&bytes[from..]) {
            let end = base + from + found;
            self.longest_line = self.longest_line.max(end - self.line_start);
            if !self.seen_first_break {
                self.seen_first_break = true;
                let before = if from + found > 0 {
                    bytes[from + found - 1]
                } else {
                    self.previous_last
                };
                self.crlf_first = before == b'\r';
            }
            self.line_start = end + 1;
            if self.lines.is_multiple_of(self.per_checkpoint) {
                self.checkpoints.push(self.line_start as u64);
            }
            self.lines += 1;
            from += found + 1;
        }
        if let Some(&last) = bytes.last() {
            self.previous_last = last;
        }
        self.pos += bytes.len();
    }
    pub fn finish(mut self) -> FileIndex {
        self.longest_line = self.longest_line.max(self.pos - self.line_start);
        FileIndex {
            len: self.pos,
            checkpoints: self.checkpoints,
            per_checkpoint: self.per_checkpoint,
            lines: self.lines,
            longest_line: self.longest_line,
            crlf_first: self.crlf_first,
            last: Mutex::default(),
        }
    }
}

/// How an index reads the text it describes.
pub type Reader<'a> = &'a dyn Fn(Range<usize>) -> io::Result<Vec<u8>>;

impl FileIndex {
    /// Bytes of the text (without any byte order mark).
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn line_count(&self) -> usize {
        self.lines
    }
    pub fn longest_line(&self) -> usize {
        self.longest_line
    }
    pub fn first_line_is_crlf(&self) -> bool {
        self.crlf_first
    }

    /// Byte offset where `line` starts; the end of the text for a line past the last.
    pub fn line_start(&self, line: usize, read: Reader) -> usize {
        if line >= self.lines {
            return self.len;
        }
        let checkpoint = line / self.per_checkpoint;
        let (mut start, mut have) = (
            self.checkpoints[checkpoint] as usize,
            checkpoint * self.per_checkpoint,
        );
        let last = *self.last.lock().unwrap_or_else(|e| e.into_inner());
        // Strictly before: the offset the last lookup ended at need not be where a line starts.
        if last.1 < line && last.1 > have {
            (start, have) = last;
        }
        let mut skip = line - have;
        while skip > 0 {
            let bytes = read(start..(start + LOOK).min(self.len)).unwrap_or_default();
            let mut at = 0;
            while skip > 0 {
                match newline::position(&bytes[at..]) {
                    Some(found) => {
                        at += found + 1;
                        skip -= 1;
                    }
                    None => break,
                }
            }
            if skip == 0 {
                start += at;
                break;
            }
            if bytes.is_empty() {
                return self.len;
            }
            start += bytes.len();
        }
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = (start, line);
        start
    }

    /// The line containing `offset`; a line break belongs to the line it ends.
    pub fn line_of(&self, offset: usize, read: Reader) -> usize {
        let offset = offset.min(self.len);
        let checkpoint = self
            .checkpoints
            .partition_point(|&start| start as usize <= offset)
            .saturating_sub(1);
        let (mut at, mut line) = (
            self.checkpoints[checkpoint] as usize,
            checkpoint * self.per_checkpoint,
        );
        let last = *self.last.lock().unwrap_or_else(|e| e.into_inner());
        if last.0 <= offset && last.0 > at {
            (at, line) = last;
        }
        while at < offset {
            let bytes = read(at..(at + LOOK).min(offset)).unwrap_or_default();
            if bytes.is_empty() {
                break;
            }
            line += newline::count(&bytes);
            at += bytes.len();
        }
        let line = line.min(self.lines - 1);
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = (offset, line);
        line
    }
}

/// A text held in memory as a source: for tests, and for any text that is already in memory but
/// is to be edited as pieces.
#[derive(Debug)]
pub struct MemorySource {
    bytes: Vec<u8>,
    bom: bool,
    index: FileIndex,
}

impl MemorySource {
    /// `bytes` as a file would hold them: a leading UTF-8 byte order mark is noted and dropped.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self::with_checkpoints(bytes, LINES_PER_CHECKPOINT)
    }
    pub fn from_text(text: &str) -> Self {
        Self::new(text.as_bytes().to_vec())
    }
    pub(crate) fn with_checkpoints(mut bytes: Vec<u8>, per_checkpoint: usize) -> Self {
        let bom = bytes.starts_with(&[0xef, 0xbb, 0xbf]);
        if bom {
            bytes.drain(..3);
        }
        let mut builder = IndexBuilder::with_checkpoints(per_checkpoint);
        builder.feed(&bytes);
        let index = builder.finish();
        Self { bytes, bom, index }
    }
    fn slice(&self, range: Range<usize>) -> &[u8] {
        &self.bytes[range.start.min(self.bytes.len())..range.end.min(self.bytes.len())]
    }
}

impl TextSource for MemorySource {
    fn len(&self) -> usize {
        self.bytes.len()
    }
    fn has_bom(&self) -> bool {
        self.bom
    }
    fn line_count(&self) -> usize {
        self.index.line_count()
    }
    fn longest_line(&self) -> usize {
        self.index.longest_line()
    }
    fn first_line_is_crlf(&self) -> bool {
        self.index.first_line_is_crlf()
    }
    fn read(&self, range: Range<usize>) -> io::Result<Vec<u8>> {
        Ok(self.slice(range).to_vec())
    }
    fn read_into(&self, range: Range<usize>, buffer: &mut [u8]) -> io::Result<()> {
        let bytes = self.slice(range);
        buffer[..bytes.len()].copy_from_slice(bytes);
        Ok(())
    }
    fn line_start(&self, line: usize) -> usize {
        self.index.line_start(line, &|range| self.read(range))
    }
    fn line_of(&self, offset: usize) -> usize {
        self.index.line_of(offset, &|range| self.read(range))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_match_a_plain_scan_across_checkpoints() {
        let mut text = String::new();
        for i in 0..5000 {
            text.push_str(&format!("row {i} 中文 {}\n", "x".repeat(i % 53)));
        }
        text.push_str("last line without a break");
        let source = MemorySource::with_checkpoints(text.clone().into_bytes(), 7);
        let starts: Vec<usize> = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(at, _)| at + 1))
            .collect();
        assert_eq!(source.line_count(), starts.len());
        assert_eq!(source.len(), text.len());
        for (line, &start) in starts.iter().enumerate().step_by(37) {
            assert_eq!(source.line_start(line), start, "line {line}");
            assert_eq!(source.line_of(start), line);
        }
        // Asked one after another, as drawing does, and backwards too.
        for (line, &start) in starts.iter().enumerate().take(260).skip(100) {
            assert_eq!(source.line_start(line), start);
        }
        for (line, &start) in starts.iter().enumerate().take(260).skip(100).rev() {
            assert_eq!(source.line_start(line), start);
        }
        assert_eq!(source.line_of(text.len()), starts.len() - 1);
        assert_eq!(source.line_start(starts.len()), text.len());
        assert_eq!(
            source.read(1000..2000).unwrap(),
            &text.as_bytes()[1000..2000]
        );
        let longest = text.lines().map(str::len).max().unwrap();
        assert_eq!(source.longest_line(), longest);
    }

    #[test]
    fn byte_order_mark_crlf_and_trailing_break_are_understood() {
        let source = MemorySource::new(b"\xef\xbb\xbfone\r\ntwo\r\n".to_vec());
        assert!(source.has_bom() && source.first_line_is_crlf());
        assert_eq!((source.len(), source.line_count()), (10, 3));
        assert_eq!(source.read(0..3).unwrap(), b"one");
        assert_eq!(source.line_start(2), 10);
    }

    #[test]
    fn a_builder_fed_in_pieces_learns_what_a_single_feed_does() {
        let text = "alpha\r\nbeta\ngamma\r\n\r\nlast";
        let mut whole = IndexBuilder::new();
        whole.feed(text.as_bytes());
        let whole = whole.finish();
        for cut in 0..text.len() {
            let mut parts = IndexBuilder::new();
            parts.feed(&text.as_bytes()[..cut]);
            parts.feed(&text.as_bytes()[cut..]);
            let parts = parts.finish();
            assert_eq!(
                (
                    parts.len(),
                    parts.line_count(),
                    parts.longest_line(),
                    parts.first_line_is_crlf()
                ),
                (
                    whole.len(),
                    whole.line_count(),
                    whole.longest_line(),
                    whole.first_line_is_crlf()
                ),
                "cut at {cut}"
            );
        }
    }
}

#[cfg(test)]
mod buffer_tests {
    use super::super::buffer::TextBuffer;
    use super::super::change::Edit;
    use super::*;
    use std::sync::Arc;

    /// A file-backed buffer must answer every read query as the same text in memory does.
    #[test]
    fn a_file_backed_buffer_answers_like_the_same_text_in_memory() {
        let mut text = String::new();
        for i in 0..300 {
            text.push_str(&match i % 9 {
                0 => format!("plain line {i}\n"),
                1 => format!("crlf line {i} 中文\r\n"),
                2 => "\n".to_owned(),
                3 => format!("\ttabbed\t{i}\t中\n"),
                4 => format!("cafe\u{301} 👩\u{200d}💻 🇨🇳 {i}\n"),
                5 => format!("word_one two.three 'four' {i}\r\n"),
                6 => format!("{}\n", "long ".repeat(70)),
                7 => "   leading and trailing   \n".to_owned(),
                _ => format!("end {i}\r\n"),
            });
        }
        text.push_str("last without a break");
        let memory = TextBuffer::new(text.clone());
        let file = TextBuffer::from_source(Arc::new(MemorySource::from_text(&text)));
        assert!(file.is_file_backed());
        assert_eq!(
            (file.len(), file.line_count()),
            (memory.len(), memory.line_count())
        );
        assert_eq!(file.line_ending(), memory.line_ending());
        for line in 0..memory.line_count() + 2 {
            assert_eq!(
                file.line_start(line),
                memory.line_start(line),
                "start {line}"
            );
            assert_eq!(file.line_end(line), memory.line_end(line), "end {line}");
            assert_eq!(
                file.line_range_with_break(line),
                memory.line_range_with_break(line),
                "break {line}"
            );
            assert_eq!(
                file.line_display_width(line),
                memory.line_display_width(line),
                "width {line}"
            );
            for column in [0, 1, 5, 40, 200, 10_000] {
                assert_eq!(
                    file.offset_at_char_column(line, column),
                    memory.offset_at_char_column(line, column),
                    "char col {line}:{column}"
                );
                assert_eq!(
                    file.offset_at_display_column(line, column),
                    memory.offset_at_display_column(line, column),
                    "display col {line}:{column}"
                );
                assert_eq!(
                    file.display_column_range(line, column, column + 30),
                    memory.display_column_range(line, column, column + 30),
                    "range {line}:{column}"
                );
            }
        }
        for offset in (0..=text.len() + 1).step_by(3) {
            assert_eq!(
                file.line_of(offset),
                memory.line_of(offset),
                "line_of {offset}"
            );
            assert_eq!(
                file.byte_point(offset),
                memory.byte_point(offset),
                "point {offset}"
            );
            assert_eq!(
                file.grapheme_floor(offset),
                memory.grapheme_floor(offset),
                "floor {offset}"
            );
            assert_eq!(
                file.next_grapheme(offset),
                memory.next_grapheme(offset),
                "next {offset}"
            );
            assert_eq!(
                file.prev_grapheme(offset),
                memory.prev_grapheme(offset),
                "prev {offset}"
            );
            assert_eq!(
                file.is_grapheme_boundary(offset),
                memory.is_grapheme_boundary(offset),
                "boundary {offset}"
            );
            assert_eq!(
                file.grapheme_text(offset),
                memory.grapheme_text(offset),
                "grapheme {offset}"
            );
            assert_eq!(
                file.line_col(offset),
                memory.line_col(offset),
                "line_col {offset}"
            );
            assert_eq!(
                file.display_column(offset),
                memory.display_column(offset),
                "display column {offset}"
            );
            assert_eq!(
                file.word_range(offset),
                memory.word_range(offset),
                "word {offset}"
            );
            assert_eq!(
                file.word_right(offset),
                memory.word_right(offset),
                "word right {offset}"
            );
        }
        assert_eq!(file.slice(100..400), memory.slice(100..400));
        assert!(file.clone().apply_edits(vec![Edit::insert(0, "x")]).is_ok());
    }
}

#[cfg(test)]
mod edit_tests {
    use super::super::buffer::TextBuffer;
    use super::super::editor::EditorMut;
    use super::super::history::EditKind;
    use super::super::selection::Selection;
    use super::*;
    use std::sync::Arc;

    /// The text of any buffer, however it is stored.
    fn whole(buffer: &TextBuffer) -> String {
        buffer.slice(0..buffer.len()).into_owned()
    }

    fn assert_same(file: &TextBuffer, memory: &TextBuffer, context: &str) {
        let (a, b) = (whole(file), whole(memory));
        assert_eq!(a, b, "text {context}");
        assert_eq!(file.len(), memory.len(), "len {context}");
        assert_eq!(file.line_count(), memory.line_count(), "lines {context}");
        for line in 0..memory.line_count() {
            assert_eq!(
                file.line_range_with_break(line),
                memory.line_range_with_break(line),
                "line {line} {context}"
            );
        }
        // The answers that use a line kept from an earlier question, which an edit moves or drops.
        for line in (0..memory.line_count()).step_by(5) {
            assert_eq!(
                file.line_display_width(line),
                memory.line_display_width(line),
                "width {line} {context}"
            );
            assert_eq!(
                file.offset_at_display_column(line, 5),
                memory.offset_at_display_column(line, 5),
                "column {line} {context}"
            );
            assert_eq!(
                file.offset_at_char_column(line, 3),
                memory.offset_at_char_column(line, 3),
                "char column {line} {context}"
            );
        }
        for offset in (0..=memory.len()).step_by(53) {
            if memory.is_grapheme_boundary(offset) {
                assert_eq!(
                    file.display_column(offset),
                    memory.display_column(offset),
                    "display column {offset} {context}"
                );
                assert_eq!(
                    file.line_col(offset),
                    memory.line_col(offset),
                    "line col {offset} {context}"
                );
            }
        }
        for offset in (0..=memory.len()).step_by(7) {
            assert_eq!(
                file.line_of(offset),
                memory.line_of(offset),
                "line_of {offset} {context}"
            );
            if memory.is_grapheme_boundary(offset) {
                assert_eq!(
                    file.next_grapheme(offset),
                    memory.next_grapheme(offset),
                    "next {offset} {context}"
                );
            }
        }
    }

    #[test]
    fn edits_undo_and_redo_on_a_file_match_the_same_text_in_memory() {
        let mut text = String::new();
        for i in 0..400 {
            text.push_str(&match i % 6 {
                0 => format!("plain line {i}\n"),
                1 => format!("crlf 中文 {i}\r\n"),
                2 => "\n".to_owned(),
                3 => format!("\ttab {i} cafe\u{301} 👩\u{200d}💻\n"),
                4 => format!("{}\n", "long ".repeat(40)),
                _ => format!("end {i}\r\n"),
            });
        }
        text.push_str("tail without a break");
        let mut memory = TextBuffer::new(text.clone());
        let mut file = TextBuffer::from_source(Arc::new(MemorySource::from_text(&text)));
        let (mut memory_selection, mut file_selection) = (Selection::caret(0), Selection::caret(0));
        let pieces = [
            "",
            "x",
            "\n",
            "\r\n",
            "a\nb\r\nc",
            "中文",
            "e\u{301}",
            "👩\u{200d}💻",
            "tail",
        ];
        let mut seed = 5u64;
        let mut next = |bound: usize| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) as usize) % bound.max(1)
        };
        assert_eq!(file.file_unchanged(), Some(true));
        let mut edits = 0;
        for step in 0..400 {
            let before = whole(&memory);
            let action = next(10);
            if action < 7 {
                let start = loop {
                    let at = next(before.len() + 1);
                    if before.is_char_boundary(at) {
                        break at;
                    }
                };
                let end = loop {
                    let at = (start + next(60)).min(before.len());
                    if before.is_char_boundary(at) {
                        break at;
                    }
                };
                let insert = pieces[next(pieces.len())];
                let a = EditorMut::new(&mut memory, &mut memory_selection, Vec::new()).replace(
                    start..end,
                    insert,
                    EditKind::Other,
                );
                let b = EditorMut::new(&mut file, &mut file_selection, Vec::new()).replace(
                    start..end,
                    insert,
                    EditKind::Other,
                );
                assert_eq!(a.is_ok(), b.is_ok());
                edits += usize::from(a.is_ok());
            } else if action < 9 {
                let a = EditorMut::new(&mut memory, &mut memory_selection, Vec::new()).undo();
                let b = EditorMut::new(&mut file, &mut file_selection, Vec::new()).undo();
                assert_eq!(a, b, "undo range at step {step}");
            } else {
                let a = EditorMut::new(&mut memory, &mut memory_selection, Vec::new()).redo();
                let b = EditorMut::new(&mut file, &mut file_selection, Vec::new()).redo();
                assert_eq!(a, b, "redo range at step {step}");
            }
            assert_same(&file, &memory, &format!("after step {step}"));
            assert_eq!(file_selection, memory_selection, "selection at step {step}");
        }
        assert!(edits > 100);
        // Undo everything: the text is the file again, and it knows it.
        while EditorMut::new(&mut file, &mut file_selection, Vec::new())
            .undo()
            .is_some()
        {}
        assert_eq!(whole(&file), text);
        assert_eq!(file.file_unchanged(), Some(true));
    }
}
