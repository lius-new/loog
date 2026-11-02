//! Positions between the editor's byte offsets and the protocol's
//! `{ line, character }`, in whichever encoding the server chose.
//!
//! The functions take a line's text (without its break), so they stay free of the
//! buffer type; `TextBuffer` callers pass `buffer.line(n)`.
use std::{
    borrow::Cow,
    collections::VecDeque,
    sync::{Arc, Weak},
};

/// Unit used for columns in text coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PositionEncoding {
    Utf8,
    #[default]
    Utf16,
}
impl PositionEncoding {
    pub fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Utf16 => "utf-16",
        }
    }
}

/// A zero-based protocol position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// The `character` of a byte offset inside a line. An offset inside a character
/// counts as the start of that character; one past the end counts as the end.
pub fn column_of(line: &str, byte: usize, encoding: PositionEncoding) -> u32 {
    let mut byte = byte.min(line.len());
    while !line.is_char_boundary(byte) {
        byte -= 1;
    }
    let column = match encoding {
        PositionEncoding::Utf8 => byte,
        PositionEncoding::Utf16 => line[..byte].encode_utf16().count(),
    };
    u32::try_from(column).unwrap_or(u32::MAX)
}

/// The byte offset of a `character` inside a line. A column past the end is the
/// end of the line, as the protocol asks; one inside a character (or between the
/// halves of a surrogate pair) rounds down to that character's start.
pub fn byte_of(line: &str, column: u32, encoding: PositionEncoding) -> usize {
    let column = column as usize;
    match encoding {
        PositionEncoding::Utf8 => {
            let mut byte = column.min(line.len());
            while !line.is_char_boundary(byte) {
                byte -= 1;
            }
            byte
        }
        PositionEncoding::Utf16 => {
            let mut units = 0;
            for (at, ch) in line.char_indices() {
                let next = units + ch.len_utf16();
                if next > column {
                    return at;
                }
                units = next;
            }
            line.len()
        }
    }
}

/// Where each line of a text starts, for turning protocol positions into byte
/// offsets in that text. Lines end at LF, CRLF or a lone CR, as the protocol
/// defines them.
pub struct LineIndex<'a> {
    text: &'a str,
    starts: Cow<'a, [usize]>,
}

impl<'a> LineIndex<'a> {
    pub fn new(text: &'a str) -> Self {
        let bytes = text.as_bytes();
        let mut starts = vec![0];
        for (at, &byte) in bytes.iter().enumerate() {
            match byte {
                b'\n' => starts.push(at + 1),
                b'\r' if bytes.get(at + 1) != Some(&b'\n') => starts.push(at + 1),
                _ => {}
            }
        }
        Self {
            text,
            starts: Cow::Owned(starts),
        }
    }

    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// The content of a line without its break; empty past the last line.
    pub fn line(&self, line: usize) -> &'a str {
        let Some(&start) = self.starts.get(line) else {
            return "";
        };
        let end = self
            .starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.text.len());
        self.text[start..end].trim_end_matches(['\n', '\r'])
    }

    /// The byte offset of a position. A line past the end is the end of the text,
    /// and a column past the end of a line is the end of that line.
    pub fn offset(&self, position: Position, encoding: PositionEncoding) -> usize {
        let line = position.line as usize;
        let Some(&start) = self.starts.get(line) else {
            return self.text.len();
        };
        start + byte_of(self.line(line), position.character, encoding)
    }

    /// The protocol position of a byte offset.
    pub fn position(&self, offset: usize, encoding: PositionEncoding) -> Position {
        let offset = offset.min(self.text.len());
        let line = self.starts.partition_point(|&start| start <= offset) - 1;
        let within = (offset - self.starts[line]).min(self.line(line).len());
        Position {
            line: u32::try_from(line).unwrap_or(u32::MAX),
            character: column_of(self.line(line), within, encoding),
        }
    }
}

/// Bounded coordinates for exact immutable snapshot allocations. Weak references
/// do not extend document lifetimes or retain a superseded snapshot's text.
pub(crate) struct IndexCache {
    entries: VecDeque<(Weak<String>, Vec<usize>)>,
    bytes: usize,
    max_bytes: usize,
}
impl Default for IndexCache {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            max_bytes: 32 * 1024 * 1024,
        }
    }
}
impl IndexCache {
    pub fn prune(&mut self) {
        self.entries.retain(|(text, _)| text.strong_count() > 0);
        self.bytes = self
            .entries
            .iter()
            .map(|(_, starts)| starts.capacity() * std::mem::size_of::<usize>())
            .sum();
    }
    pub fn index<'a>(&'a mut self, text: &'a Arc<String>) -> LineIndex<'a> {
        self.prune();
        if let Some(at) = self
            .entries
            .iter()
            .position(|(snapshot, _)| snapshot.as_ptr() == Arc::as_ptr(text))
        {
            let entry = self.entries.remove(at).unwrap();
            self.entries.push_back(entry);
        } else {
            let starts = LineIndex::new(text).starts.into_owned();
            let bytes = starts.capacity() * std::mem::size_of::<usize>();
            if bytes > self.max_bytes {
                return LineIndex {
                    text,
                    starts: Cow::Owned(starts),
                };
            }
            while self.entries.len() >= 4 || self.bytes + bytes > self.max_bytes {
                let (_, old) = self.entries.pop_front().unwrap();
                self.bytes -= old.capacity() * std::mem::size_of::<usize>();
            }
            self.bytes += bytes;
            self.entries.push_back((Arc::downgrade(text), starts));
        }
        LineIndex {
            text,
            starts: Cow::Borrowed(&self.entries.back().unwrap().1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PositionEncoding::{Utf8, Utf16};
    use super::*;
    #[test]
    fn cached_coordinates_follow_exact_snapshots_and_release_closed_documents() {
        let mut cache = IndexCache::default();
        let mut text = Arc::new("中\r\nsecond".to_owned());
        let first = cache.index(&text).starts.as_ptr();
        assert_eq!(cache.index(&text).starts.as_ptr(), first);
        Arc::make_mut(&mut text).insert(0, '\n');
        assert_eq!(cache.index(&text).position(1, Utf8).line, 1);
        assert_eq!(cache.entries.len(), 1);
        drop(text);
        cache.prune();
        assert!(cache.entries.is_empty());
        assert_eq!(cache.bytes, 0);
    }
    #[test]
    fn coordinate_cache_bounds_allocated_capacity_and_skips_an_oversized_index() {
        let mut cache = IndexCache {
            max_bytes: 64,
            ..Default::default()
        };
        let texts = (0..5)
            .map(|n| Arc::new(format!("{n}\nline")))
            .collect::<Vec<_>>();
        for text in &texts {
            cache.index(text);
        }
        assert!(cache.bytes <= 64 && cache.entries.len() <= 2);
        let large = Arc::new("\n".repeat(32));
        assert_eq!(cache.index(&large).line_count(), 33);
        assert!(cache.bytes <= 64 && cache.entries.len() <= 2);
    }

    // 'a' 1 byte / 1 unit, 'é' 2 / 1, '中' 3 / 1, '😀' 4 / 2.
    const LINE: &str = "aé中😀z";

    #[test]
    fn columns_count_in_the_chosen_encoding() {
        let at = |needle: &str| LINE.find(needle).unwrap();
        for (needle, utf8, utf16) in [
            ("a", 0, 0),
            ("é", 1, 1),
            ("中", 3, 2),
            ("😀", 6, 3),
            ("z", 10, 5),
        ] {
            assert_eq!(column_of(LINE, at(needle), Utf8), utf8, "{needle}");
            assert_eq!(column_of(LINE, at(needle), Utf16), utf16, "{needle}");
            assert_eq!(byte_of(LINE, utf8, Utf8), at(needle), "{needle}");
            assert_eq!(byte_of(LINE, utf16, Utf16), at(needle), "{needle}");
        }
        assert_eq!(column_of(LINE, LINE.len(), Utf8), 11);
        assert_eq!(column_of(LINE, LINE.len(), Utf16), 6);
    }

    #[test]
    fn offsets_inside_a_character_round_down() {
        assert_eq!(column_of(LINE, 2, Utf8), 1, "inside é");
        assert_eq!(column_of(LINE, 8, Utf16), 3, "inside the emoji");
        assert_eq!(byte_of(LINE, 2, Utf8), 1);
        // Between the two halves of the emoji's surrogate pair.
        assert_eq!(byte_of(LINE, 4, Utf16), 6);
        assert_eq!(byte_of(LINE, 8, Utf8), 6);
    }

    #[test]
    fn columns_past_the_end_clamp_to_the_end() {
        assert_eq!(byte_of(LINE, 999, Utf8), LINE.len());
        assert_eq!(byte_of(LINE, 999, Utf16), LINE.len());
        assert_eq!(byte_of("", 3, Utf16), 0);
        assert_eq!(column_of(LINE, 999, Utf16), 6);
        assert_eq!(column_of("", 0, Utf8), 0);
    }

    #[test]
    fn line_index_follows_the_protocol_line_rules() {
        let text = "ab\r\ncd\rxy\n😀z";
        let index = LineIndex::new(text);
        assert_eq!(index.line_count(), 4);
        assert_eq!(
            (
                index.line(0),
                index.line(1),
                index.line(2),
                index.line(3),
                index.line(9)
            ),
            ("ab", "cd", "xy", "😀z", "")
        );
        let at = |line, character| index.offset(Position { line, character }, Utf16);
        assert_eq!(at(0, 1), 1);
        assert_eq!(at(0, 99), 2, "past the end of a line is its end");
        assert_eq!(at(1, 0), 4);
        assert_eq!(at(3, 2), text.find('z').unwrap());
        assert_eq!(at(9, 0), text.len(), "past the last line is the end");
    }

    #[test]
    fn line_index_positions_round_trip() {
        let text = "ab\r\ncd\nä😀z";
        let index = LineIndex::new(text);
        for encoding in [Utf8, Utf16] {
            for (offset, _) in text.char_indices().chain([(text.len(), ' ')]) {
                if text[..offset].ends_with('\r') && text[offset..].starts_with('\n') {
                    continue; // between CR and LF is not a position
                }
                let position = index.position(offset, encoding);
                assert_eq!(
                    index.offset(position, encoding),
                    offset,
                    "{offset} {encoding:?}"
                );
            }
        }
    }

    #[test]
    fn every_boundary_round_trips() {
        for encoding in [Utf8, Utf16] {
            for (byte, _) in LINE.char_indices().chain([(LINE.len(), ' ')]) {
                let column = column_of(LINE, byte, encoding);
                assert_eq!(byte_of(LINE, column, encoding), byte, "{encoding:?} {byte}");
            }
        }
    }
}
