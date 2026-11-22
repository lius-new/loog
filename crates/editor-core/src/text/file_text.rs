//! A text that stays in its file. Opening reads it once to find the line starts and to check that
//! it is UTF-8; after that, only the ranges that are asked for are read, through a small cache of
//! blocks. A very large file therefore costs a few bytes per thousand lines, not its own size.
//!
//! The text is read only: nothing here changes the file.

use std::collections::VecDeque;
use std::fs::File;
use std::io;
use std::ops::Range;
use std::path::Path;
use std::sync::Mutex;

use super::newline;

/// A line start is kept for every this many lines; a lookup scans at most this many lines.
const LINES_PER_CHECKPOINT: usize = 64;
const BLOCK: usize = 64 * 1024;
const CACHED_BLOCKS: usize = 8;
const SCAN_CHUNK: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub struct FileText {
    file: File,
    /// Bytes before the text itself: a UTF-8 byte order mark.
    skip: usize,
    len: usize,
    /// Start of line `n * LINES_PER_CHECKPOINT`, relative to the text.
    checkpoints: Vec<u64>,
    per_checkpoint: usize,
    lines: usize,
    longest_line: usize,
    crlf_first: bool,
    cache: Mutex<VecDeque<(usize, std::sync::Arc<Vec<u8>>)>>,
}

#[cfg(windows)]
fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buffer, offset)
}
#[cfg(unix)]
fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buffer, offset)
}

fn read_exact_at(file: &File, mut buffer: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !buffer.is_empty() {
        let read = read_at(file, buffer, offset)?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        offset += read as u64;
        buffer = &mut buffer[read..];
    }
    Ok(())
}

impl FileText {
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::open_with(path, LINES_PER_CHECKPOINT)
    }

    fn open_with(path: &Path, per_checkpoint: usize) -> io::Result<Self> {
        let file = File::open(path)?;
        let total = file.metadata()?.len() as usize;
        let mut chunk = vec![0u8; SCAN_CHUNK.min(total.max(1)) + 4];
        let mut head = [0u8; 3];
        let head_len = read_at(&file, &mut head, 0)?;
        let skip = if head[..head_len].starts_with(&[0xef, 0xbb, 0xbf]) {
            3
        } else {
            0
        };
        let len = total - skip;
        let (mut checkpoints, mut lines) = (vec![0u64], 1usize);
        let (mut line_start, mut longest_line) = (0usize, 0usize);
        let mut crlf_first = false;
        let mut seen_first_break = false;
        let mut previous_last = 0u8;
        // A UTF-8 sequence cut by the end of a chunk is carried over to the next one.
        let mut carry = Vec::<u8>::new();
        let mut at = 0usize;
        while at < len {
            let want = (chunk.len() - 4).min(len - at);
            let read = &mut chunk[carry.len()..carry.len() + want];
            read_exact_at(&file, read, (skip + at) as u64)?;
            chunk[..carry.len()].copy_from_slice(&carry);
            let filled = carry.len() + want;
            let valid = match std::str::from_utf8(&chunk[..filled]) {
                Ok(_) => filled,
                Err(error) if error.error_len().is_none() && at + want < len => error.valid_up_to(),
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "file is not valid UTF-8",
                    ));
                }
            };
            let base = at - carry.len();
            let mut from = 0;
            while let Some(found) = newline::position(&chunk[from..valid]) {
                let end = base + from + found;
                longest_line = longest_line.max(end - line_start);
                if !seen_first_break {
                    seen_first_break = true;
                    let before = if from + found > 0 {
                        chunk[from + found - 1]
                    } else {
                        previous_last
                    };
                    crlf_first = before == b'\r';
                }
                line_start = end + 1;
                if lines % per_checkpoint == 0 {
                    checkpoints.push(line_start as u64);
                }
                lines += 1;
                from += found + 1;
            }
            if valid > 0 {
                previous_last = chunk[valid - 1];
            }
            carry.clear();
            carry.extend_from_slice(&chunk[valid..filled]);
            at += want;
        }
        longest_line = longest_line.max(len - line_start);
        Ok(Self {
            file,
            skip,
            len,
            checkpoints,
            per_checkpoint,
            lines,
            longest_line,
            crlf_first,
            cache: Mutex::default(),
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn has_bom(&self) -> bool {
        self.skip > 0
    }
    pub fn line_count(&self) -> usize {
        self.lines
    }
    /// The longest line's length in bytes: an upper bound of its width for sizing scroll ranges.
    pub fn longest_line(&self) -> usize {
        self.longest_line
    }
    pub fn first_line_is_crlf(&self) -> bool {
        self.crlf_first
    }

    /// The bytes of `range`, read through the block cache.
    pub fn read(&self, range: Range<usize>) -> io::Result<Vec<u8>> {
        let range = range.start.min(self.len)..range.end.min(self.len);
        let mut out = Vec::with_capacity(range.len());
        let mut at = range.start;
        while at < range.end {
            let index = at / BLOCK;
            let block = self.block(index)?;
            let from = at - index * BLOCK;
            let to = (range.end - index * BLOCK).min(block.len());
            if to <= from {
                break;
            }
            out.extend_from_slice(&block[from..to]);
            at = index * BLOCK + to;
        }
        Ok(out)
    }

    /// Read `range` straight from the file into `buffer`, past the cache: for copying a whole
    /// file, which would only push out the blocks the display uses.
    pub fn read_into(&self, range: Range<usize>, buffer: &mut [u8]) -> io::Result<()> {
        let range = range.start.min(self.len)..range.end.min(self.len);
        read_exact_at(
            &self.file,
            &mut buffer[..range.len()],
            (self.skip + range.start) as u64,
        )
    }

    fn block(&self, index: usize) -> io::Result<std::sync::Arc<Vec<u8>>> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(position) = cache.iter().position(|(i, _)| *i == index) {
            let entry = cache.remove(position).unwrap();
            let block = entry.1.clone();
            cache.push_front(entry);
            return Ok(block);
        }
        let start = index * BLOCK;
        let mut bytes = vec![0u8; BLOCK.min(self.len.saturating_sub(start))];
        read_exact_at(&self.file, &mut bytes, (self.skip + start) as u64)?;
        let block = std::sync::Arc::new(bytes);
        cache.push_front((index, block.clone()));
        cache.truncate(CACHED_BLOCKS);
        Ok(block)
    }

    /// The text of `range`. The file was checked to be UTF-8 when it was opened, so a range on
    /// character boundaries always decodes; a range that is not (or a file that changed since)
    /// shows replacement characters instead of failing.
    pub fn text(&self, range: Range<usize>) -> String {
        match self.read(range) {
            Ok(bytes) => String::from_utf8(bytes)
                .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned()),
            Err(_) => String::new(),
        }
    }

    /// Byte offset where `line` starts; the end of the text for a line past the last.
    pub fn line_start(&self, line: usize) -> usize {
        if line >= self.lines {
            return self.len;
        }
        let checkpoint = line / self.per_checkpoint;
        let mut start = self.checkpoints[checkpoint] as usize;
        let mut skip = line - checkpoint * self.per_checkpoint;
        while skip > 0 {
            // Lines are short next to a block, so scan in block-sized steps.
            let bytes = self
                .read(start..(start + BLOCK).min(self.len))
                .unwrap_or_default();
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
                return start + at;
            }
            if bytes.is_empty() {
                return self.len;
            }
            start += bytes.len();
        }
        start
    }

    /// The line containing `offset`; a line break belongs to the line it ends.
    pub fn line_of(&self, offset: usize) -> usize {
        let offset = offset.min(self.len);
        let checkpoint = self
            .checkpoints
            .partition_point(|&start| start as usize <= offset)
            .saturating_sub(1);
        let mut line = checkpoint * self.per_checkpoint;
        let mut at = self.checkpoints[checkpoint] as usize;
        while at < offset {
            let bytes = self.read(at..(at + BLOCK).min(offset)).unwrap_or_default();
            if bytes.is_empty() {
                break;
            }
            line += newline::count(&bytes);
            at += bytes.len();
        }
        line.min(self.lines - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("loog-file-text-{}-{name}", std::process::id()));
        std::fs::File::create(&path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
        path
    }

    #[test]
    fn lines_match_an_in_memory_scan_across_checkpoints_and_blocks() {
        let mut text = String::new();
        for i in 0..5000 {
            text.push_str(&format!("row {i} 中文 {}\n", "x".repeat(i % 53)));
        }
        text.push_str("last line without a break");
        let path = temp("lines", text.as_bytes());
        let file = FileText::open_with(&path, 7).unwrap();
        let starts: Vec<usize> = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(at, _)| at + 1))
            .collect();
        assert_eq!(file.line_count(), starts.len());
        assert_eq!(file.len(), text.len());
        for (line, &start) in starts.iter().enumerate().step_by(37) {
            assert_eq!(file.line_start(line), start, "line {line}");
            assert_eq!(file.line_of(start), line);
        }
        assert_eq!(file.line_of(text.len()), starts.len() - 1);
        assert_eq!(file.line_start(starts.len()), text.len());
        // A range across several blocks reads back exactly.
        let range = 1000..200_000;
        assert_eq!(file.read(range.clone()).unwrap(), &text.as_bytes()[range]);
        let longest = text.lines().map(str::len).max().unwrap();
        assert_eq!(file.longest_line(), longest);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn byte_order_mark_crlf_and_trailing_break_are_understood() {
        let path = temp("bom", b"\xef\xbb\xbfone\r\ntwo\r\n");
        let file = FileText::open(&path).unwrap();
        assert!(file.has_bom() && file.first_line_is_crlf());
        assert_eq!((file.len(), file.line_count()), (10, 3));
        assert_eq!(file.text(0..3), "one");
        assert_eq!(file.line_start(2), 10);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn invalid_utf8_is_refused_even_when_split_by_a_scan_chunk() {
        let mut bytes = "é".repeat(SCAN_CHUNK).into_bytes();
        bytes.push(0xff);
        let path = temp("bad", &bytes);
        assert_eq!(
            FileText::open(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        // Multi-byte characters straddling chunk boundaries are fine.
        let good = "é".repeat(SCAN_CHUNK);
        let path2 = temp("good", good.as_bytes());
        assert_eq!(FileText::open(&path2).unwrap().len(), good.len());
        std::fs::remove_file(path).ok();
        std::fs::remove_file(path2).ok();
    }
}

#[cfg(test)]
mod large_file_probe {
    use super::*;

    /// `LOOG_FILE_TEXT_PROBE=<path>` times opening a real file. Not run by default.
    #[test]
    #[ignore = "reads the file named by LOOG_FILE_TEXT_PROBE"]
    fn open_time_of_a_real_file() {
        let path = std::env::var("LOOG_FILE_TEXT_PROBE").expect("set LOOG_FILE_TEXT_PROBE");
        let started = std::time::Instant::now();
        let file = FileText::open(Path::new(&path)).unwrap();
        println!(
            "FILE_TEXT opened {} bytes, {} lines, longest {} in {:?}",
            file.len(),
            file.line_count(),
            file.longest_line(),
            started.elapsed()
        );
        let started = std::time::Instant::now();
        let middle = file.line_start(file.line_count() / 2);
        let text = file.text(middle..middle + 200);
        println!(
            "FILE_TEXT middle line lookup {:?}: {:?}",
            started.elapsed(),
            &text[..text.len().min(60)]
        );
    }
}

#[cfg(test)]
mod buffer_tests {
    use super::super::buffer::TextBuffer;
    use super::super::change::Edit;
    use super::*;
    use std::io::Write;

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
                7 => format!("   leading and trailing   \n"),
                _ => format!("end {i}\r\n"),
            });
        }
        text.push_str("last without a break");
        let path = std::env::temp_dir().join(format!("loog-buffer-diff-{}", std::process::id()));
        std::fs::File::create(&path)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        let memory = TextBuffer::new(text.clone());
        let file = TextBuffer::from_file(FileText::open(&path).unwrap());
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
        std::fs::remove_file(path).ok();
    }
}

#[cfg(test)]
mod edit_tests {
    use super::super::buffer::TextBuffer;
    use super::super::editor::EditorMut;
    use super::super::history::EditKind;
    use super::super::selection::Selection;
    use super::*;
    use std::io::Write;

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
        let path = std::env::temp_dir().join(format!("loog-edit-diff-{}", std::process::id()));
        std::fs::File::create(&path)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        let mut memory = TextBuffer::new(text.clone());
        let mut file = TextBuffer::from_file(FileText::open(&path).unwrap());
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
        std::fs::remove_file(path).ok();
    }
}
