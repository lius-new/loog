//! A text made of pieces: ranges of an unchanged file and of text that was inserted since.
//! Editing cuts and joins pieces and never copies the file, so a very large file edits as fast
//! as a small one and costs memory only for what was typed.

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::{Arc, Mutex};

use super::buffer::TextBuffer;
use super::file_text::FileText;
use super::newline;

/// Inserted text, never changed once made. Pieces share it, so a cut piece costs nothing.
#[derive(Debug)]
struct Chunk {
    bytes: Box<[u8]>,
    /// Where its line breaks are.
    breaks: Vec<usize>,
}

impl Chunk {
    fn new(text: &str) -> Self {
        let bytes: Box<[u8]> = text.as_bytes().into();
        let mut breaks = Vec::new();
        let mut at = 0;
        while let Some(found) = newline::position(&bytes[at..]) {
            breaks.push(at + found);
            at += found + 1;
        }
        Self { bytes, breaks }
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
}

type Lines = Mutex<VecDeque<((usize, usize), Arc<TextBuffer>)>>;

/// A file with edits. Cloning shares everything until the next edit.
#[derive(Clone, Debug)]
pub struct PieceText {
    original: Arc<FileText>,
    pieces: Arc<Vec<Piece>>,
    len: usize,
    breaks: usize,
    /// Everything inserted so far, to bound the longest line.
    added: usize,
    /// Recent lines as small buffers, for this state of the text only.
    lines: Arc<Lines>,
    unchanged: Arc<std::sync::OnceLock<bool>>,
}

/// An edit that removes more than this much text cannot be undone, as undo keeps what it removed.
pub const MAX_REMOVED: usize = 64 * 1024 * 1024;

impl PieceText {
    pub fn new(file: FileText) -> Self {
        let original = Arc::new(file);
        let whole = Piece {
            source: Source::Original,
            start: 0,
            len: original.len(),
            breaks: original.line_count() - 1,
        };
        Self {
            len: original.len(),
            breaks: whole.breaks,
            pieces: Arc::new(if whole.len == 0 { vec![] } else { vec![whole] }),
            added: 0,
            original,
            lines: Arc::default(),
            unchanged: Arc::default(),
        }
    }

    pub fn len(&self) -> usize {
        self.len
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
                            bytes == chunk.bytes[piece.start..piece.start + piece.len]
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

    /// The piece holding `offset` (the last piece for the end of the text), the bytes before it
    /// and the line breaks before it.
    fn locate(&self, offset: usize) -> (usize, usize, usize) {
        let (mut bytes, mut breaks) = (0, 0);
        for (index, piece) in self.pieces.iter().enumerate() {
            if offset < bytes + piece.len || index + 1 == self.pieces.len() {
                return (index, bytes, breaks);
            }
            bytes += piece.len;
            breaks += piece.breaks;
        }
        (0, 0, 0)
    }

    pub fn read(&self, range: Range<usize>) -> std::io::Result<Vec<u8>> {
        let range = range.start.min(self.len)..range.end.min(self.len);
        let mut out = Vec::with_capacity(range.len());
        let mut at = 0;
        for piece in self.pieces.iter() {
            let (from, to) = (at.max(range.start), (at + piece.len).min(range.end));
            if from < to {
                let inside = piece.start + (from - at)..piece.start + (to - at);
                match &piece.source {
                    Source::Original => out.extend(self.original.read(inside)?),
                    Source::Added(chunk) => out.extend_from_slice(&chunk.bytes[inside]),
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
        let (index, bytes, breaks) = self.locate(offset);
        let piece = &self.pieces[index];
        let inside = offset - bytes;
        breaks
            + match &piece.source {
                Source::Original => {
                    self.original_breaks_before(piece.start + inside)
                        - self.original_breaks_before(piece.start)
                }
                Source::Added(chunk) => {
                    chunk
                        .breaks
                        .partition_point(|&at| at < piece.start + inside)
                        - chunk.breaks.partition_point(|&at| at < piece.start)
                }
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
        let (mut bytes, mut breaks) = (0, 0);
        for piece in self.pieces.iter() {
            if wanted < breaks + piece.breaks {
                let nth = wanted - breaks;
                let at = match &piece.source {
                    Source::Original => {
                        let global = self.original_breaks_before(piece.start) + nth;
                        self.original.line_start(global + 1) - 1 - piece.start
                    }
                    Source::Added(chunk) => {
                        let first = chunk.breaks.partition_point(|&at| at < piece.start);
                        chunk.breaks[first + nth] - piece.start
                    }
                };
                return bytes + at + 1;
            }
            bytes += piece.len;
            breaks += piece.breaks;
        }
        self.len
    }

    /// Write the whole text, as a file would hold it, reading the original in large steps.
    pub fn write_to(&self, out: &mut dyn std::io::Write) -> std::io::Result<()> {
        self.write_with(out, None)
    }

    /// As `write_to`, also learning the lines of what is written, so that the file can be opened
    /// as the new text without reading it again.
    pub fn write_indexed(
        &self,
        out: &mut dyn std::io::Write,
    ) -> std::io::Result<super::file_text::FileIndex> {
        let mut builder = super::file_text::IndexBuilder::new();
        self.write_with(out, Some(&mut builder))?;
        Ok(builder.finish())
    }

    fn write_with(
        &self,
        out: &mut dyn std::io::Write,
        mut index: Option<&mut super::file_text::IndexBuilder>,
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
                    let bytes = &chunk.bytes[piece.start..piece.start + piece.len];
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
        let pieces = Arc::make_mut(&mut self.pieces);
        let mut out: Vec<Piece> = Vec::with_capacity(pieces.len() + 2);
        let inserted = (!text.is_empty()).then(|| {
            let chunk = Arc::new(Chunk::new(text));
            Piece {
                breaks: chunk.breaks.len(),
                len: chunk.bytes.len(),
                start: 0,
                source: Source::Added(chunk),
            }
        });
        let (mut at, mut placed) = (0, false);
        let cut = |piece: &Piece, from: usize, to: usize, original: &FileText| Piece {
            source: piece.source.clone(),
            start: piece.start + from,
            len: to - from,
            breaks: match &piece.source {
                Source::Original => {
                    original.line_of(piece.start + to) - original.line_of(piece.start + from)
                }
                Source::Added(chunk) => {
                    chunk.breaks.partition_point(|&b| b < piece.start + to)
                        - chunk.breaks.partition_point(|&b| b < piece.start + from)
                }
            },
        };
        for piece in pieces.drain(..) {
            let end = at + piece.len;
            if end <= range.start {
                out.push(piece);
            } else if at >= range.end {
                if !placed {
                    out.extend(inserted.clone());
                    placed = true;
                }
                out.push(piece);
            } else {
                if at < range.start {
                    out.push(cut(&piece, 0, range.start - at, &self.original));
                }
                if !placed {
                    out.extend(inserted.clone());
                    placed = true;
                }
                if end > range.end {
                    out.push(cut(&piece, range.end - at, piece.len, &self.original));
                }
            }
            at = end;
        }
        if !placed {
            out.extend(inserted);
        }
        // Join neighbours that continue each other, so undoing an edit restores the file's own
        // single piece.
        let mut joined: Vec<Piece> = Vec::with_capacity(out.len());
        for piece in out {
            if piece.len == 0 {
                continue;
            }
            if let Some(last) = joined.last_mut()
                && matches!(
                    (&last.source, &piece.source),
                    (Source::Original, Source::Original)
                )
                && last.start + last.len == piece.start
            {
                last.len += piece.len;
                last.breaks += piece.breaks;
                continue;
            }
            joined.push(piece);
        }
        self.len = self.len - range.len() + text.len();
        self.breaks = joined.iter().map(|piece| piece.breaks).sum();
        self.added += text.len();
        self.unchanged = Arc::default();
        *pieces = joined;
        // The recent lines described the text before this edit. Keep those before it.
        let kept: VecDeque<_> = self
            .lines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|((start, _), line)| start + line.len() <= range.start)
            .cloned()
            .collect();
        self.lines = Arc::new(Mutex::new(kept));
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
