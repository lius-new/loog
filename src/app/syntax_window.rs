//! Highlighting for a document too large to parse whole: the lines around the screen are cut out
//! as a small text, parsed like any document, and the spans are moved back into the document.
//! Where the cut begins inside a string or comment the colours there can be off; it is a
//! window onto the text, not a parse of it.
use crate::{
    domain::{document::DocumentStamp, identity::PaneId},
    features::syntax::{MAX_DOCUMENT_BYTES, Source, TokenSpan, Window, WindowTokens},
    infrastructure::syntax_worker::Worker,
    ui::editor_binding::ViewDocument,
};
use loog_editor_core::text::TextBuffer;
use std::{
    collections::HashMap,
    ops::Range,
    sync::{Arc, Mutex},
};

/// Lines kept above and below what is on screen, so scrolling a little needs no new cut.
const CONTEXT_LINES: usize = 120;
/// A cut is never larger than this; a screen of enormous lines is drawn plain.
const MAX_CHUNK_BYTES: usize = 512 * 1024;

/// Whether the document is highlighted by windows instead of as a whole.
pub(crate) fn is_windowed(buffer: &TextBuffer) -> bool {
    buffer.is_file_backed() || buffer.len() > MAX_DOCUMENT_BYTES
}

struct Chunk {
    /// The text it was cut from.
    identity: usize,
    version: u64,
    range: Range<usize>,
    buffer: TextBuffer,
    stamp: DocumentStamp,
}

/// The current cut of each pane's document.
#[derive(Clone, Default)]
pub(crate) struct Chunks(Arc<Mutex<HashMap<PaneId, Chunk>>>);

impl Chunks {
    pub fn query(
        &self,
        worker: &Worker,
        pane: PaneId,
        document: &ViewDocument,
        windows: Vec<Window>,
    ) -> Vec<WindowTokens> {
        let buffer = &document.buffer;
        let (Some(low), Some(high)) = (
            windows.iter().map(|w| w.line.start).min(),
            windows.iter().map(|w| w.line.end).max(),
        ) else {
            return Vec::new();
        };
        let mut chunks = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let reusable = chunks.get(&pane).is_some_and(|chunk| {
            chunk.identity == buffer.snapshot_identity()
                && chunk.version == buffer.version()
                && chunk.stamp.document == document.stamp.document
                && chunk.stamp.language_generation == document.stamp.language_generation
                && chunk.range.start <= low
                && high <= chunk.range.end
        });
        if !reusable {
            let Some(range) = cut(buffer, low, high) else {
                chunks.remove(&pane);
                return Vec::new();
            };
            let text = buffer.slice(range.clone()).into_owned();
            let cut_buffer = TextBuffer::new(text);
            let stamp = DocumentStamp::new(
                document.stamp.document,
                document.stamp.incarnation,
                document.stamp.language_generation,
                &cut_buffer,
            );
            chunks.insert(
                pane,
                Chunk {
                    identity: buffer.snapshot_identity(),
                    version: buffer.version(),
                    range,
                    buffer: cut_buffer,
                    stamp,
                },
            );
        }
        let chunk = &chunks[&pane];
        let base = chunk.range.start;
        let inside = |range: &Range<usize>| range.start - base..range.end - base;
        let shifted = windows
            .into_iter()
            .map(|w| Window {
                line: inside(&w.line),
                range: inside(&w.range),
            })
            .collect();
        let tokens = worker.query(
            pane,
            chunk.stamp,
            Source::new(&chunk.buffer),
            document.meta.lang.clone(),
            shifted,
        );
        drop(chunks);
        into_document(tokens, base)
    }
}

/// Spans found in a cut, moved to where the cut came from.
fn into_document(tokens: Vec<WindowTokens>, base: usize) -> Vec<WindowTokens> {
    tokens
        .into_iter()
        .map(|tokens| WindowTokens {
            range: tokens.range.start + base..tokens.range.end + base,
            spans: tokens
                .spans
                .iter()
                .map(|span| TokenSpan {
                    range: span.range.start + base..span.range.end + base,
                    class: span.class,
                })
                .collect::<Vec<_>>()
                .into(),
            complete: tokens.complete,
            freshness: tokens.freshness,
        })
        .collect()
}

/// The whole lines to cut for lines covering bytes `low..high`, with context if it fits.
fn cut(buffer: &TextBuffer, low: usize, high: usize) -> Option<Range<usize>> {
    let (first, last) = (
        buffer.line_of(low),
        buffer.line_of(high.saturating_sub(1).max(low)),
    );
    for context in [CONTEXT_LINES, CONTEXT_LINES / 4, 0] {
        let from = buffer.line_start(first.saturating_sub(context));
        let to = buffer
            .line_range_with_break((last + context).min(buffer.line_count() - 1))
            .end;
        if to - from <= MAX_CHUNK_BYTES {
            return Some(from..to);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use loog_editor_core::text::FileText;
    use std::io::Write;

    #[test]
    fn a_cut_holds_whole_lines_with_context_and_stays_bounded() {
        let text: String = (0..5000).map(|i| format!("fn line_{i}() {{}}\n")).collect();
        let path = std::env::temp_dir().join(format!("loog-cut-{}", std::process::id()));
        std::fs::File::create(&path)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        let buffer = TextBuffer::from_file(FileText::open(&path).unwrap());
        assert!(is_windowed(&buffer));
        // A screen of lines 2000..2040 is cut with context, on line boundaries.
        let (low, high) = (buffer.line_start(2000), buffer.line_start(2040));
        let range = cut(&buffer, low, high).unwrap();
        assert!(range.start <= low && high <= range.end);
        assert_eq!(range.start, buffer.line_start(buffer.line_of(range.start)));
        assert!(buffer.slice(range.clone()).ends_with('\n'));
        assert_eq!(buffer.line_of(range.start), 2000 - CONTEXT_LINES);
        // Near the start the context is simply cut short.
        let first = cut(&buffer, 0, buffer.line_start(30)).unwrap();
        assert_eq!(first.start, 0);
        // A screen of enormous lines is not cut at all.
        let long = TextBuffer::new(format!("{}\n", "x".repeat(MAX_CHUNK_BYTES + 1)));
        assert!(cut(&long, 0, long.len()).is_none());
        // A smaller context is used when the full one would be too much.
        let wide: String = (0..400)
            .map(|_| format!("{}\n", "y".repeat(2000)))
            .collect();
        let wide = TextBuffer::new(wide);
        let range = cut(&wide, wide.line_start(200), wide.line_start(202)).unwrap();
        assert!(range.len() <= MAX_CHUNK_BYTES);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn spans_come_back_at_their_place_in_the_document() {
        let found = vec![WindowTokens::fresh(
            5..20,
            vec![
                TokenSpan {
                    range: 5..8,
                    class: None,
                },
                TokenSpan {
                    range: 10..12,
                    class: None,
                },
            ],
        )];
        let moved = into_document(found, 1000);
        assert_eq!(moved[0].range, 1005..1020);
        assert_eq!(moved[0].spans[1].range, 1010..1012);
        assert!(moved[0].complete);
    }
}
