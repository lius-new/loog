//! Search over a text that is too large to hold: it is read in line-aligned pieces, each matched
//! like a small text, and the matches are moved to where the piece came from.
use super::{HASH_SEED, Match, Matcher, hash_continue};
use std::io;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

/// A text read by byte ranges. It must be valid UTF-8 as a whole.
pub trait ByteSource {
    fn len(&self) -> usize;
    fn read(&self, range: Range<usize>) -> io::Result<Vec<u8>>;
}

impl ByteSource for loog_editor_core::text::PieceText {
    fn len(&self) -> usize {
        self.len()
    }
    fn read(&self, range: Range<usize>) -> io::Result<Vec<u8>> {
        loog_editor_core::text::PieceText::read(self, range)
    }
}

impl ByteSource for loog_editor_core::text::FileText {
    fn len(&self) -> usize {
        self.len()
    }
    fn read(&self, range: Range<usize>) -> io::Result<Vec<u8>> {
        loog_editor_core::text::FileText::read(self, range)
    }
}

/// How much is matched at once.
pub const CHUNK_BYTES: usize = 1024 * 1024;
/// A line longer than this is not searched.
const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;

pub struct Found {
    pub matches: Vec<Match>,
    /// `hash` of the whole text, made on the way.
    pub content_hash: u64,
    /// More matches exist than were asked for, or a line was too long to search.
    pub truncated: bool,
}

/// Matches of `matcher` in all of `source`, at most `limit`, as `Matcher::matches` would give for
/// the whole text: offsets and line numbers are those of the whole text.
pub fn search(
    matcher: &Matcher,
    source: &dyn ByteSource,
    cancel: &AtomicBool,
    limit: usize,
) -> io::Result<Found> {
    search_in_chunks(matcher, source, cancel, limit, CHUNK_BYTES)
}

fn search_in_chunks(
    matcher: &Matcher,
    source: &dyn ByteSource,
    cancel: &AtomicBool,
    limit: usize,
    chunk_bytes: usize,
) -> io::Result<Found> {
    let total = source.len();
    let mut found = Found {
        matches: Vec::new(),
        content_hash: HASH_SEED,
        truncated: false,
    };
    let (mut at, mut line_base) = (0, 0);
    // Matching goes on to the end only to finish the hash while there is room for more matches.
    while at < total && !cancel.load(Ordering::Relaxed) {
        let mut bytes = source.read(at..(at + chunk_bytes).min(total))?;
        let last = at + bytes.len() >= total;
        if !last {
            // End on a line break, reading on if a single line is longer than a chunk.
            while !bytes.contains(&b'\n') {
                if bytes.len() >= MAX_LINE_BYTES {
                    found.truncated = true;
                    return Ok(found);
                }
                let more =
                    source.read(at + bytes.len()..(at + bytes.len() + chunk_bytes).min(total))?;
                if more.is_empty() {
                    break;
                }
                bytes.extend(more);
            }
            if let Some(end) = bytes.iter().rposition(|&b| b == b'\n') {
                bytes.truncate(end + 1);
            }
        }
        let last = at + bytes.len() >= total;
        if bytes.contains(&0) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Binary file"));
        }
        found.content_hash = hash_continue(found.content_hash, &bytes);
        let text = std::str::from_utf8(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let room = limit.saturating_sub(found.matches.len());
        if room > 0 {
            for mut hit in matcher.matches(text, cancel, room) {
                // The empty match the matcher adds after a final line break belongs to the
                // end of the whole text only.
                if !last && hit.range.is_empty() && hit.range.start == text.len() {
                    continue;
                }
                hit.range = at + hit.range.start..at + hit.range.end;
                hit.line += line_base;
                found.matches.push(hit);
            }
        } else {
            found.truncated = true;
        }
        line_base += text.matches('\n').count();
        at += bytes.len();
    }
    Ok(found)
}

/// Whether `found` still describes the text: its line, which is all that is read, must be what it
/// was when found, and in the same place. The text as a whole is not read again.
pub fn locate(source: &dyn ByteSource, found: &Match) -> Option<Range<usize>> {
    const WINDOW: usize = 1 << 20;
    let total = source.len();
    if found.range.start > found.range.end || found.range.end > total {
        return None;
    }
    // The line around the match, bounded: a match inside a longer line is not located.
    let from = found.range.start.saturating_sub(WINDOW);
    let to = (found.range.end + WINDOW).min(total);
    let bytes = source.read(from..to).ok()?;
    let at = found.range.start - from;
    let start = bytes[..at]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or_else(|| (from == 0).then_some(0), |previous| Some(previous + 1))?;
    let end = bytes[at..].iter().position(|&b| b == b'\n').map_or_else(
        || (to == total).then_some(bytes.len()),
        |next| Some(at + next),
    )?;
    let line = std::str::from_utf8(&bytes[start..end]).ok()?;
    let line = line.strip_suffix('\r').unwrap_or(line);
    (super::hash(line) == found.line_hash
        && found.range.start - from - start == found.column
        && found.range.end - from <= start + line.len())
    .then(|| found.range.clone())
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::features::search::SearchSpec;

    struct Memory(Vec<u8>);
    impl ByteSource for Memory {
        fn len(&self) -> usize {
            self.0.len()
        }
        fn read(&self, range: Range<usize>) -> io::Result<Vec<u8>> {
            Ok(self.0[range].to_vec())
        }
    }

    pub(super) fn matcher(query: &str, regex: bool) -> Matcher {
        Matcher::new(&SearchSpec {
            query: query.into(),
            include: String::new(),
            exclude: String::new(),
            case_sensitive: true,
            whole_word: false,
            regex,
            include_hidden: false,
            include_ignored: false,
        })
        .unwrap()
    }

    /// Matching in pieces must give what matching the whole text gives, for every piece size.
    #[test]
    fn matches_in_pieces_equal_matches_in_the_whole_text() {
        let mut text = String::new();
        for i in 0..600 {
            text.push_str(&match i % 7 {
                0 => format!("needle {i} 中文 needle\n"),
                1 => "\n".to_owned(),
                2 => format!("crlf {i} needle\r\n"),
                3 => format!("{} needle {i}\n", "long ".repeat(30)),
                _ => format!("nothing here {i}\n"),
            });
        }
        for tail in ["", "last needle", "\n"] {
            let whole = format!("{text}{tail}");
            for (query, regex) in [("needle", false), ("^$", true), ("e+d", true), ("", true)] {
                let m = matcher(query, regex);
                let expected = m.matches(&whole, &AtomicBool::new(false), usize::MAX);
                for chunk in [64, 333, 1000, 5000, 1 << 20] {
                    let got = search_in_chunks(
                        &m,
                        &Memory(whole.clone().into_bytes()),
                        &AtomicBool::new(false),
                        usize::MAX,
                        chunk,
                    )
                    .unwrap();
                    assert_eq!(got.matches.len(), expected.len(), "{query:?} chunk {chunk}");
                    for (a, b) in got.matches.iter().zip(&expected) {
                        assert_eq!(
                            (a.range.clone(), a.line, a.column, &a.preview, a.line_hash),
                            (b.range.clone(), b.line, b.column, &b.preview, b.line_hash),
                            "{query:?} chunk {chunk}"
                        );
                    }
                    assert_eq!(got.content_hash, super::super::hash(&whole));
                }
            }
        }
    }

    #[test]
    fn a_limit_stops_early_and_cancel_stops_at_once() {
        let text = "needle\n".repeat(1000);
        let m = matcher("needle", false);
        let source = Memory(text.clone().into_bytes());
        let few = search_in_chunks(&m, &source, &AtomicBool::new(false), 10, 100).unwrap();
        assert_eq!(few.matches.len(), 10);
        assert!(few.truncated);
        let none = search_in_chunks(&m, &source, &AtomicBool::new(true), 10, 100).unwrap();
        assert!(none.matches.is_empty());
    }

    #[test]
    fn a_match_is_located_by_its_line_and_refused_when_the_line_changed() {
        let text = "alpha\r\nbeta needle gamma\r\nomega\n";
        let m = matcher("needle", false);
        let source = Memory(text.as_bytes().to_vec());
        let found = search(&m, &source, &AtomicBool::new(false), 10)
            .unwrap()
            .matches;
        assert_eq!(found.len(), 1);
        assert_eq!(locate(&source, &found[0]), Some(found[0].range.clone()));
        // Another line moved it: the same line elsewhere is a different place.
        let moved = Memory(format!("new\n{text}").into_bytes());
        assert_eq!(locate(&moved, &found[0]), None);
        // The line itself changed.
        let edited = Memory(text.replace("beta", "BETA").into_bytes());
        assert_eq!(locate(&edited, &found[0]), None);
        // The last line without a break is understood too.
        let last = search(
            &m,
            &Memory(b"x\nneedle".to_vec()),
            &AtomicBool::new(false),
            1,
        )
        .unwrap()
        .matches;
        assert!(locate(&Memory(b"x\nneedle".to_vec()), &last[0]).is_some());
    }

    #[test]
    fn invalid_text_is_an_error_not_a_match() {
        let source = Memory(b"fine\n\xff broken\n".to_vec());
        let error = search(
            &matcher("fine", false),
            &source,
            &AtomicBool::new(false),
            10,
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}

#[cfg(test)]
mod probe {
    use super::*;

    /// `LOOG_FILE_TEXT_PROBE=<path>`: where the time of searching a real file goes.
    #[test]
    #[ignore = "reads the file named by LOOG_FILE_TEXT_PROBE"]
    fn where_the_time_of_a_large_search_goes() {
        let path = std::env::var("LOOG_FILE_TEXT_PROBE").expect("set LOOG_FILE_TEXT_PROBE");
        let file = loog_editor_core::text::FileText::open(std::path::Path::new(&path)).unwrap();
        let total = file.len();
        let started = std::time::Instant::now();
        let mut at = 0;
        let mut checksum = 0usize;
        while at < total {
            let bytes = file.read(at..(at + CHUNK_BYTES).min(total)).unwrap();
            checksum += bytes.len();
            at += bytes.len();
        }
        println!(
            "PROBE read only: {:?} ({checksum} bytes)",
            started.elapsed()
        );
        let started = std::time::Instant::now();
        let mut at = 0;
        let mut hash = HASH_SEED;
        while at < total {
            let bytes = file.read(at..(at + CHUNK_BYTES).min(total)).unwrap();
            hash = hash_continue(hash, &bytes);
            at += bytes.len();
        }
        println!("PROBE read + hash: {:?} ({hash})", started.elapsed());
        let matcher = super::tests::matcher("row 2217465", false);
        let started = std::time::Instant::now();
        let found = search(&matcher, &file, &AtomicBool::new(false), 100).unwrap();
        println!(
            "PROBE full search: {:?}, {} matches",
            started.elapsed(),
            found.matches.len()
        );
    }
}
