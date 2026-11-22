//! A text made of pieces: ranges of an unchanged file and of text that was inserted since.
//! Editing cuts and joins pieces and never copies the file, so a very large file edits as fast
//! as a small one and costs memory only for what was typed.

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use super::buffer::TextBuffer;
use super::newline;
use super::source::{FileIndex, IndexBuilder, SharedSource, TextSource};

#[derive(Debug, Default)]
struct ChunkData {
    bytes: Vec<u8>,
    /// Where its line breaks are.
    breaks: Vec<usize>,
}

/// Inserted text. It only ever grows at its end, so a range of it never changes and pieces of
/// any state of the text can share it; typing on in the same place extends it instead of
/// starting a piece for every character.
#[derive(Debug, Default)]
struct Chunk {
    data: RwLock<ChunkData>,
}

impl Chunk {
    fn new(text: &str) -> Self {
        let chunk = Self::default();
        chunk.append(0, text);
        chunk
    }
    fn len(&self) -> usize {
        self.read().bytes.len()
    }
    fn read(&self) -> std::sync::RwLockReadGuard<'_, ChunkData> {
        self.data.read().unwrap_or_else(|e| e.into_inner())
    }
    /// Add `text` at the end, if the chunk is `expected` bytes long, which is how a piece that
    /// reaches its end knows nobody else has added to it since. Returns the breaks it holds.
    fn append(&self, expected: usize, text: &str) -> Option<usize> {
        let mut data = self.data.write().unwrap_or_else(|e| e.into_inner());
        if data.bytes.len() != expected {
            return None;
        }
        let before = data.breaks.len();
        let mut at = 0;
        while let Some(found) = newline::position(&text.as_bytes()[at..]) {
            let position = expected + at + found;
            data.breaks.push(position);
            at += found + 1;
        }
        data.bytes.extend_from_slice(text.as_bytes());
        Some(data.breaks.len() - before)
    }
    /// Line breaks within `range` of it.
    fn breaks_in(&self, range: Range<usize>) -> usize {
        let data = self.read();
        data.breaks.partition_point(|&b| b < range.end)
            - data.breaks.partition_point(|&b| b < range.start)
    }
}

#[derive(Clone, Debug)]
enum Source {
    Original,
    Added(Arc<Chunk>),
}

#[derive(Clone, Debug)]
struct Piece {
    source: Source,
    start: usize,
    len: usize,
    /// Line breaks inside the piece.
    breaks: usize,
    /// For a piece of the file: the line breaks of the file before where it starts.
    base: usize,
}

/// Where each piece starts, in bytes and in line breaks, so a place is found by searching
/// instead of by going through the pieces. Made when first asked after an edit.
#[derive(Debug)]
struct Index {
    starts: Vec<usize>,
    break_starts: Vec<usize>,
}

type Lines = Mutex<VecDeque<((usize, usize), Arc<TextBuffer>)>>;

/// A file with edits. Cloning shares everything until the next edit.
#[derive(Clone, Debug)]
pub struct PieceText {
    original: SharedSource,
    pieces: Arc<Vec<Piece>>,
    len: usize,
    breaks: usize,
    /// Everything inserted so far, to bound the longest line.
    added: usize,
    /// Recent lines as small buffers, for this state of the text only.
    lines: Arc<Lines>,
    unchanged: Arc<OnceLock<bool>>,
    index: Arc<OnceLock<Index>>,
}

/// An edit that removes more than this much text cannot be undone, as undo keeps what it removed.
pub const MAX_REMOVED: usize = 64 * 1024 * 1024;

impl PieceText {
    pub fn new(original: SharedSource) -> Self {
        let whole = Piece {
            source: Source::Original,
            start: 0,
            len: original.len(),
            breaks: original.line_count() - 1,
            base: 0,
        };
        Self {
            len: original.len(),
            breaks: whole.breaks,
            pieces: Arc::new(if whole.len == 0 { vec![] } else { vec![whole] }),
            added: 0,
            original,
            lines: Arc::default(),
            unchanged: Arc::default(),
            index: Arc::default(),
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn line_count(&self) -> usize {
        self.breaks + 1
    }
    /// An upper bound of the longest line's bytes.
    pub fn longest_line(&self) -> usize {
        self.original.longest_line() + self.added
    }

    /// Whether the text is the file as it was opened. Undoing every edit gives the same text
    /// again, though made of new pieces, so it is checked by content, once per state.
    pub fn is_unchanged(&self) -> bool {
        *self.unchanged.get_or_init(|| {
            if self.len != self.original.len() {
                return false;
            }
            let mut at = 0;
            for piece in self.pieces.iter() {
                let same = match &piece.source {
                    Source::Original => piece.start == at,
                    Source::Added(chunk) => {
                        self.original.read(at..at + piece.len).is_ok_and(|bytes| {
                            bytes == chunk.read().bytes[piece.start..piece.start + piece.len]
                        })
                    }
                };
                if !same {
                    return false;
                }
                at += piece.len;
            }
            true
        })
    }

    /// Line breaks of the original before byte `offset` of it.
    fn original_breaks_before(&self, offset: usize) -> usize {
        self.original.line_of(offset)
    }

    fn index(&self) -> &Index {
        self.index.get_or_init(|| {
            let (mut bytes, mut breaks) = (0, 0);
            let mut starts = Vec::with_capacity(self.pieces.len());
            let mut break_starts = Vec::with_capacity(self.pieces.len());
            for piece in self.pieces.iter() {
                starts.push(bytes);
                break_starts.push(breaks);
                bytes += piece.len;
                breaks += piece.breaks;
            }
            Index {
                starts,
                break_starts,
            }
        })
    }

    /// The piece holding `offset` (the last piece for the end of the text), the bytes before it
    /// and the line breaks before it.
    fn locate(&self, offset: usize) -> (usize, usize, usize) {
        let index = self.index();
        let at = index
            .starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1);
        (at, index.starts[at], index.break_starts[at])
    }

    pub fn read(&self, range: Range<usize>) -> std::io::Result<Vec<u8>> {
        let range = range.start.min(self.len)..range.end.min(self.len);
        let mut out = Vec::with_capacity(range.len());
        if range.is_empty() {
            return Ok(out);
        }
        let (first, mut at, _) = self.locate(range.start);
        for piece in &self.pieces[first..] {
            let (from, to) = (at.max(range.start), (at + piece.len).min(range.end));
            if from < to {
                let inside = piece.start + (from - at)..piece.start + (to - at);
                match &piece.source {
                    Source::Original => out.extend(self.original.read(inside)?),
                    Source::Added(chunk) => out.extend_from_slice(&chunk.read().bytes[inside]),
                }
            }
            at += piece.len;
            if at >= range.end {
                break;
            }
        }
        Ok(out)
    }

    /// The text of `range`; replacement characters where the range cuts a character.
    pub fn text(&self, range: Range<usize>) -> String {
        match self.read(range) {
            Ok(bytes) => String::from_utf8(bytes)
                .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned()),
            Err(_) => String::new(),
        }
    }

    /// Whether `offset` is where a character starts.
    pub fn is_char_boundary(&self, offset: usize) -> bool {
        offset == 0
            || offset == self.len
            || (offset < self.len
                && self
                    .read(offset..offset + 1)
                    .is_ok_and(|byte| byte.first().is_some_and(|b| (b & 0xc0) != 0x80)))
    }

    /// The line containing `offset`; a line break belongs to the line it ends.
    pub fn line_of(&self, offset: usize) -> usize {
        let offset = offset.min(self.len);
        if self.pieces.is_empty() {
            return 0;
        }
        let (at, bytes, breaks) = self.locate(offset);
        let piece = &self.pieces[at];
        let inside = offset - bytes;
        breaks
            + match &piece.source {
                Source::Original => self.original_breaks_before(piece.start + inside) - piece.base,
                Source::Added(chunk) => chunk.breaks_in(piece.start..piece.start + inside),
            }
    }

    /// Byte offset where `line` starts; the end of the text for a line past the last.
    pub fn line_start(&self, line: usize) -> usize {
        if line == 0 {
            return 0;
        }
        if line > self.breaks {
            return self.len;
        }
        // The start of line `n` is just after line break number `n - 1`.
        let wanted = line - 1;
        let index = self.index();
        let at = index
            .break_starts
            .partition_point(|&breaks| breaks <= wanted)
            .saturating_sub(1);
        let piece = &self.pieces[at];
        let nth = wanted - index.break_starts[at];
        let inside = match &piece.source {
            Source::Original => {
                let global = piece.base + nth;
                self.original.line_start(global + 1) - 1 - piece.start
            }
            Source::Added(chunk) => {
                let data = chunk.read();
                let first = data.breaks.partition_point(|&b| b < piece.start);
                data.breaks[first + nth] - piece.start
            }
        };
        index.starts[at] + inside + 1
    }

    /// Write the whole text, as a file would hold it, reading the original in large steps.
    pub fn write_to(&self, out: &mut dyn std::io::Write) -> std::io::Result<()> {
        self.write_with(out, None)
    }

    /// As `write_to`, also learning the lines of what is written, so that the file can be opened
    /// as the new text without reading it again.
    pub fn write_indexed(&self, out: &mut dyn std::io::Write) -> std::io::Result<FileIndex> {
        let mut builder = IndexBuilder::new();
        self.write_with(out, Some(&mut builder))?;
        Ok(builder.finish())
    }

    fn write_with(
        &self,
        out: &mut dyn std::io::Write,
        mut index: Option<&mut IndexBuilder>,
    ) -> std::io::Result<()> {
        if self.original.has_bom() {
            out.write_all(&[0xef, 0xbb, 0xbf])?;
        }
        let mut buffer = vec![0u8; (4 << 20).min(self.len.max(1))];
        for piece in self.pieces.iter() {
            match &piece.source {
                Source::Original => {
                    let (mut at, end) = (piece.start, piece.start + piece.len);
                    while at < end {
                        let step = (end - at).min(buffer.len());
                        self.original
                            .read_into(at..at + step, &mut buffer[..step])?;
                        if let Some(index) = index.as_deref_mut() {
                            index.feed(&buffer[..step]);
                        }
                        out.write_all(&buffer[..step])?;
                        at += step;
                    }
                }
                Source::Added(chunk) => {
                    let data = chunk.read();
                    let bytes = &data.bytes[piece.start..piece.start + piece.len];
                    if let Some(index) = index.as_deref_mut() {
                        index.feed(bytes);
                    }
                    out.write_all(bytes)?
                }
            }
        }
        Ok(())
    }

    /// Replace `range` with `text`. Both are in the current text; the result shares what it can.
    pub fn replace(&mut self, range: Range<usize>, text: &str) {
        self.replace_many(&[(range, text)]);
    }

    /// Replace several ranges at once, in one pass over the pieces however many there are. The
    /// ranges are in the current text, in order and not overlapping.
    pub fn replace_many(&mut self, edits: &[(Range<usize>, &str)]) {
        if edits.is_empty() {
            return;
        }
        // A few edits change a few pieces near them; the rest of the text is left alone.
        if edits.len() <= 8 && !self.pieces.is_empty() {
            self.replace_near(edits);
            self.keep_lines(edits);
            return;
        }
        let old = std::mem::take(Arc::make_mut(&mut self.pieces));
        let mut walk = Walk {
            pieces: old.into_iter(),
            current: None,
            at: 0,
            original: &*self.original,
        };
        let mut out: Vec<Piece> = Vec::with_capacity(walk.pieces.len() + 2 * edits.len() + 1);
        let (mut removed, mut inserted) = (0, 0);
        for (range, text) in edits {
            walk.take_until(range.start, Some(&mut out));
            walk.take_until(range.end, None);
            removed += range.len();
            inserted += text.len();
            if !text.is_empty() {
                insert(&mut out, text);
            }
        }
        walk.take_until(usize::MAX, Some(&mut out));
        self.len = self.len - removed + inserted;
        self.breaks = out.iter().map(|piece| piece.breaks).sum();
        self.added += inserted;
        self.unchanged = Arc::default();
        self.index = Arc::default();
        *Arc::make_mut(&mut self.pieces) = out;
        self.keep_lines(edits);
    }

    /// The recent lines described the text before the edits. Those before them are still right.
    /// After a single edit the lines that start beyond it are too, only a little further on or
    /// back, so typing does not make every line on the screen be read again.
    fn keep_lines(&mut self, edits: &[(Range<usize>, &str)]) {
        let first_start = edits.first().map_or(0, |edit| edit.0.start);
        let single = match edits {
            [(range, text)] => Some((range.end, text.len() as isize - range.len() as isize)),
            _ => None,
        };
        let kept: VecDeque<_> = self
            .lines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(|((start, tab), line)| {
                if start + line.len() <= first_start {
                    return Some(((*start, *tab), line.clone()));
                }
                // A line that starts after the edit ended, not at its end, is the same text.
                let (end, shift) = single?;
                (*start > end).then(|| (((*start as isize + shift) as usize, *tab), line.clone()))
            })
            .collect();
        self.lines = Arc::new(Mutex::new(kept));
    }

    /// `replace_many` for a few edits: the pieces from the one before the first edit to the one
    /// after the last are rebuilt and put back, and the index is shifted rather than made again.
    fn replace_near(&mut self, edits: &[(Range<usize>, &str)]) {
        let first = edits.first().map_or(0, |e| e.0.start);
        let last = edits.last().map_or(0, |e| e.0.end);
        let (first_piece, ..) = self.locate(first);
        let (last_piece, ..) = self.locate(last);
        let lo = first_piece.saturating_sub(1);
        let hi = (last_piece + 1).min(self.pieces.len() - 1);
        let index = self.index();
        let (at, breaks_at) = (index.starts[lo], index.break_starts[lo]);
        let region: Vec<Piece> = self.pieces[lo..=hi].to_vec();
        let region_len: usize = region.iter().map(|piece| piece.len).sum();
        let region_breaks: usize = region.iter().map(|piece| piece.breaks).sum();
        let mut walk = Walk {
            pieces: region.into_iter(),
            current: None,
            at,
            original: &*self.original,
        };
        let mut out: Vec<Piece> = Vec::with_capacity(hi - lo + 2 * edits.len() + 1);
        let (mut removed, mut inserted) = (0, 0);
        for (range, text) in edits {
            walk.take_until(range.start, Some(&mut out));
            walk.take_until(range.end, None);
            removed += range.len();
            inserted += text.len();
            if !text.is_empty() {
                insert(&mut out, text);
            }
        }
        walk.take_until(usize::MAX, Some(&mut out));
        let out_len: usize = out.iter().map(|piece| piece.len).sum();
        let out_breaks: usize = out.iter().map(|piece| piece.breaks).sum();
        debug_assert_eq!(out_len + removed, region_len + inserted);

        // Where the pieces start after the edit: those before it as they were, the rebuilt ones,
        // and the rest moved by what was added and removed.
        let shift = out_len as isize - region_len as isize;
        let break_shift = out_breaks as isize - region_breaks as isize;
        let (mut bytes, mut breaks) = (at, breaks_at);
        let (new_starts, new_break_starts): (Vec<usize>, Vec<usize>) = out
            .iter()
            .map(|piece| {
                let here = (bytes, breaks);
                bytes += piece.len;
                breaks += piece.breaks;
                here
            })
            .unzip();
        let rebuilt = out.len();
        // The index is changed where it is when nobody else holds it, which for a few edits to a
        // text of many pieces is the difference between a moment and a pause.
        let kept = Arc::get_mut(&mut self.index)
            .and_then(OnceLock::get_mut)
            .map(|index| {
                index.starts.splice(lo..=hi, new_starts.iter().copied());
                index
                    .break_starts
                    .splice(lo..=hi, new_break_starts.iter().copied());
                for s in &mut index.starts[lo + rebuilt..] {
                    *s = (*s as isize + shift) as usize;
                }
                for b in &mut index.break_starts[lo + rebuilt..] {
                    *b = (*b as isize + break_shift) as usize;
                }
            })
            .is_some();
        let made = (!kept).then(|| {
            let old = self.index();
            let mut starts = Vec::with_capacity(self.pieces.len() + rebuilt);
            let mut break_starts = Vec::with_capacity(self.pieces.len() + rebuilt);
            starts.extend_from_slice(&old.starts[..lo]);
            break_starts.extend_from_slice(&old.break_starts[..lo]);
            starts.extend_from_slice(&new_starts);
            break_starts.extend_from_slice(&new_break_starts);
            starts.extend(
                old.starts[hi + 1..]
                    .iter()
                    .map(|s| (*s as isize + shift) as usize),
            );
            break_starts.extend(
                old.break_starts[hi + 1..]
                    .iter()
                    .map(|b| (*b as isize + break_shift) as usize),
            );
            Index {
                starts,
                break_starts,
            }
        });
        Arc::make_mut(&mut self.pieces).splice(lo..=hi, out);
        self.len = self.len - removed + inserted;
        self.breaks = (self.breaks as isize + break_shift) as usize;
        self.added += inserted;
        self.unchanged = Arc::default();
        if let Some(index) = made {
            self.index = Arc::new(OnceLock::from(index));
        }
    }

    pub(super) fn cached_line(&self, key: (usize, usize)) -> Option<Arc<TextBuffer>> {
        let mut cache = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        let position = cache.iter().position(|(k, _)| *k == key)?;
        let entry = cache.remove(position).unwrap();
        let line = entry.1.clone();
        cache.push_front(entry);
        Some(line)
    }
    pub(super) fn cache_line(&self, key: (usize, usize), line: Arc<TextBuffer>) {
        // About a screenful, and only lines of ordinary length.
        const LINES: usize = 256;
        if line.len() > 16 * 1024 {
            return;
        }
        let mut cache = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        cache.push_front((key, line));
        cache.truncate(LINES);
    }
}

/// Add a piece, joining it to the one before when they continue each other, so undoing an edit
/// gives the file's own single piece back and a run of typing is one piece.
fn push(out: &mut Vec<Piece>, piece: Piece) {
    if piece.len == 0 {
        return;
    }
    if let Some(last) = out.last_mut() {
        let continues = match (&last.source, &piece.source) {
            (Source::Original, Source::Original) => last.start + last.len == piece.start,
            (Source::Added(a), Source::Added(b)) => {
                Arc::ptr_eq(a, b) && last.start + last.len == piece.start
            }
            _ => false,
        };
        if continues {
            last.len += piece.len;
            last.breaks += piece.breaks;
            return;
        }
    }
    out.push(piece);
}

/// Put `text` after what is already in `out`. A piece of inserted text that reaches the end of
/// its chunk is extended in place, which costs nothing however long the typing goes on.
fn insert(out: &mut Vec<Piece>, text: &str) {
    if let Some(last) = out.last_mut()
        && let Source::Added(chunk) = &last.source
        && last.start + last.len == chunk.len()
        && let Some(breaks) = chunk.append(last.start + last.len, text)
    {
        last.len += text.len();
        last.breaks += breaks;
        return;
    }
    let chunk = Arc::new(Chunk::new(text));
    let breaks = chunk.read().breaks.len();
    push(
        out,
        Piece {
            len: text.len(),
            start: 0,
            breaks,
            base: 0,
            source: Source::Added(chunk),
        },
    );
}

/// Goes through the old pieces once, handing out stretches of them.
struct Walk<'a> {
    pieces: std::vec::IntoIter<Piece>,
    /// What is left of the piece being taken from, which starts at `at`.
    current: Option<Piece>,
    at: usize,
    original: &'a dyn TextSource,
}

impl Walk<'_> {
    /// Take the text from where the walk is up to byte `to`, adding it to `out` or dropping it.
    fn take_until(&mut self, to: usize, mut out: Option<&mut Vec<Piece>>) {
        while self.at < to {
            let Some(piece) = self.current.take().or_else(|| self.pieces.next()) else {
                break;
            };
            let end = self.at + piece.len;
            if end <= to {
                if let Some(out) = out.as_deref_mut() {
                    push(out, piece);
                }
                self.at = end;
            } else {
                let step = to - self.at;
                if let Some(out) = out.as_deref_mut() {
                    push(out, self.cut(&piece, 0, step));
                }
                self.current = Some(self.cut(&piece, step, piece.len));
                self.at = to;
            }
        }
    }

    /// The part of `piece` from `from` to `to`, counted from its start.
    fn cut(&self, piece: &Piece, from: usize, to: usize) -> Piece {
        let (breaks, base) = match &piece.source {
            Source::Original => {
                let before = self.original.line_of(piece.start + from);
                (self.original.line_of(piece.start + to) - before, before)
            }
            Source::Added(chunk) => (chunk.breaks_in(piece.start + from..piece.start + to), 0),
        };
        Piece {
            source: piece.source.clone(),
            start: piece.start + from,
            len: to - from,
            breaks,
            base,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::source::MemorySource;
    use super::*;

    fn source(text: &str) -> SharedSource {
        Arc::new(MemorySource::from_text(text))
    }

    fn assert_matches(pieces: &PieceText, model: &str, context: &str) {
        assert_eq!(pieces.len(), model.len(), "len {context}");
        assert_eq!(pieces.text(0..pieces.len()), model, "text {context}");
        let starts: Vec<usize> = std::iter::once(0)
            .chain(model.match_indices('\n').map(|(at, _)| at + 1))
            .collect();
        assert_eq!(pieces.line_count(), starts.len(), "lines {context}");
        for line in (0..starts.len() + 1).step_by(1.max(starts.len() / 40)) {
            assert_eq!(
                pieces.line_start(line),
                starts.get(line).copied().unwrap_or(model.len()),
                "line_start {line} {context}"
            );
        }
        for offset in (0..=model.len()).step_by(1.max(model.len() / 60)) {
            let expected = starts.partition_point(|&s| s <= offset) - 1;
            assert_eq!(
                pieces.line_of(offset),
                expected,
                "line_of {offset} {context}"
            );
        }
    }

    /// Batches of edits, typing runs, deletions and re-insertions agree with plain strings.
    #[test]
    fn edits_in_batches_and_runs_equal_the_same_edits_on_a_string() {
        let mut model: String = (0..300).map(|i| format!("row {i} 中文\n")).collect();
        model.push_str("tail");
        let mut pieces = PieceText::new(source(&model));
        let mut seed = 9u64;
        let mut next = |bound: usize| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) as usize) % bound.max(1)
        };
        let words = [
            "",
            "a",
            "bc\n",
            "\n\n",
            "中",
            "x\r\ny",
            "long line without a break at all",
        ];
        for step in 0..300 {
            let kind = next(4);
            let mut edits: Vec<(Range<usize>, String)> = Vec::new();
            if kind == 0 {
                // A run of typing: one character after another at the same place.
                let at = loop {
                    let at = next(model.len() + 1);
                    if model.is_char_boundary(at) {
                        break at;
                    }
                };
                let mut place = at;
                for ch in ["t", "y", "p", "e", "\n", "d"] {
                    let mut single = pieces.clone();
                    single.replace(place..place, ch);
                    pieces = single;
                    model.insert_str(place, ch);
                    place += ch.len();
                }
                assert_matches(&pieces, &model, &format!("typing {step}"));
                continue;
            }
            let mut cursor = 0;
            let wanted = if next(5) == 0 {
                12 + next(5)
            } else {
                1 + next(6)
            };
            while cursor <= model.len() && edits.len() < wanted {
                let start = loop {
                    let at = cursor + next(model.len() - cursor + 1);
                    if model.is_char_boundary(at) {
                        break at;
                    }
                };
                let end = loop {
                    let at = (start + next(40)).min(model.len());
                    if model.is_char_boundary(at) {
                        break at;
                    }
                };
                let text = words[next(words.len())].to_owned();
                edits.push((start..end, text));
                cursor = end + 1;
                if cursor > model.len() {
                    break;
                }
            }
            let borrowed: Vec<(Range<usize>, &str)> =
                edits.iter().map(|(r, t)| (r.clone(), t.as_str())).collect();
            pieces.replace_many(&borrowed);
            for (range, text) in edits.iter().rev() {
                model.replace_range(range.clone(), text);
            }
            assert_matches(&pieces, &model, &format!("batch {step}"));
        }
    }

    /// An older state is not changed by edits made after it, including typing that extends a
    /// chunk the older state also uses.
    #[test]
    fn an_older_state_keeps_its_text_while_typing_goes_on() {
        let mut pieces = PieceText::new(source("one\ntwo\nthree\n"));
        pieces.replace(3..3, "X");
        let after_x = pieces.clone();
        pieces.replace(4..4, "Y");
        let after_y = pieces.clone();
        pieces.replace(5..5, "Z\n");
        assert_eq!(after_x.text(0..after_x.len()), "oneX\ntwo\nthree\n");
        assert_eq!(after_y.text(0..after_y.len()), "oneXY\ntwo\nthree\n");
        assert_eq!(pieces.text(0..pieces.len()), "oneXYZ\n\ntwo\nthree\n");
        // From the older state, typing somewhere else starts its own chunk.
        let mut branch = after_x.clone();
        branch.replace(4..4, "B");
        assert_eq!(branch.text(0..branch.len()), "oneXB\ntwo\nthree\n");
        assert_eq!(after_y.text(0..after_y.len()), "oneXY\ntwo\nthree\n");
    }

    /// Many thousand edits stay quick: a replace of every row in one batch, and typing that does
    /// not leave a piece for each character.
    #[test]
    fn a_hundred_thousand_edits_take_a_moment_not_minutes() {
        let text: String = (0..200_000).map(|i| format!("row {i}\n")).collect();
        let mut pieces = PieceText::new(source(&text));
        let started = std::time::Instant::now();
        let edits: Vec<(Range<usize>, &str)> = text
            .match_indices("row")
            .map(|(at, found)| (at..at + found.len(), "line"))
            .collect();
        assert_eq!(edits.len(), 200_000);
        pieces.replace_many(&edits);
        assert_eq!(pieces.line_count(), 200_001);
        assert_eq!(pieces.text(0..16), "line 0\nline 1\nli");
        let batch = started.elapsed();
        // Typing 5,000 characters at one place leaves it as a single added piece.
        let started = std::time::Instant::now();
        let before = pieces.pieces.len();
        for at in (7..).take(5_000) {
            pieces.replace(at..at, "q");
        }
        assert!(
            pieces.pieces.len() <= before + 2,
            "typing made {} pieces",
            pieces.pieces.len() - before
        );
        let typing = started.elapsed();
        println!("PIECES replace-all of 200000 rows {batch:?}, 5000 typed characters {typing:?}");
        assert!(
            batch.as_secs() < 20 && typing.as_secs() < 20,
            "{batch:?} {typing:?}"
        );
        // Lines are found in a text of 200,000 pieces without going through them.
        let started = std::time::Instant::now();
        let mut checksum = 0;
        for line in (0..200_000).step_by(997) {
            checksum += pieces.line_start(line);
        }
        assert!(checksum > 0);
        assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
    }
}
