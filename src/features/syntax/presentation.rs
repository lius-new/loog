//! Display-only projection of known spans onto a newer version of the text.
//!
//! After an edit the parser needs a moment. Until it finishes, the spans that
//! were correct for the previous version are carried across the recorded edits
//! so the unchanged text keeps its colour instead of falling back to plain.
//! The result is a display aid: it is never stored as a parse result, never feeds
//! a cache and must not be used for structure (folding, text objects, indentation).
//!
//! Rules, per revision, with all edit positions in the text before that revision:
//!
//! * text outside every edit keeps its class and moves with the edits before it;
//! * replaced or deleted text disappears, with no zero-length leftovers;
//! * inserted text takes the class of the character just before the insertion
//!   point (or the one after it at the very start), so a neighbouring span never
//!   swallows it by being stretched; it is a guess that the next parse corrects.
use super::TokenSpan;
use crate::features::highlighting::HighlightId;
use loog_editor_core::text::{RevisionEdit, TextRevision};
use std::ops::Range;

/// Largest inputs the display path projects without measurement showing it fits
/// the frame budget (see the P3a record in the design). Beyond them the caller
/// shows plain text until the parse is ready.
pub(crate) const MAX_SPANS: usize = 4_000;
pub(crate) const MAX_REVISIONS: usize = 16;
pub(crate) const MAX_EDITS: usize = 64;

/// Why a projection was not made: the input exceeded a limit above.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    Spans,
    Revisions,
    Edits,
}

/// `spans` tile `window` exactly, in the text before `revisions`. Returns the
/// window in the text after them, and spans that tile it exactly.
pub(crate) fn project_window(
    window: Range<usize>,
    spans: &[TokenSpan],
    revisions: &[TextRevision],
    new_len: usize,
) -> Result<(Range<usize>, Vec<TokenSpan>), Refused> {
    if spans.len() > MAX_SPANS {
        return Err(Refused::Spans);
    }
    if revisions.len() > MAX_REVISIONS {
        return Err(Refused::Revisions);
    }
    if revisions.iter().map(|r| r.edits.len()).sum::<usize>() > MAX_EDITS {
        return Err(Refused::Edits);
    }
    let mut range = window;
    let mut current: Vec<TokenSpan> = spans.to_vec();
    for revision in revisions {
        current = shift(&current, &revision.edits);
        range = revision.map(range.start, false)..revision.map(range.end, true);
    }
    let range = range.start.min(new_len)..range.end.min(new_len);
    Ok((range.clone(), tile(range, current)))
}

/// One revision's effect on `spans`: survivors shifted, insertions added.
fn shift(spans: &[TokenSpan], edits: &[RevisionEdit]) -> Vec<TokenSpan> {
    if edits.is_empty() {
        return spans.to_vec();
    }
    // delta[k]: how far text after the first k edits has moved.
    let mut delta = Vec::with_capacity(edits.len() + 1);
    delta.push(0isize);
    for edit in edits {
        let last = *delta.last().unwrap();
        delta.push(last + edit.new.len() as isize - edit.old.len() as isize);
    }
    let class_before = |at: usize| -> Option<HighlightId> {
        let probe = at.checked_sub(1).unwrap_or(at);
        let index = spans.partition_point(|span| span.range.end <= probe);
        spans
            .get(index)
            .filter(|span| span.range.start <= probe)
            .and_then(|span| span.class)
    };

    let mut survivors: Vec<TokenSpan> = Vec::with_capacity(spans.len() + edits.len());
    let mut first = 0; // first edit that ends after the current span starts
    for span in spans {
        while first < edits.len() && edits[first].old.end <= span.range.start {
            first += 1;
        }
        let mut piece_start = span.range.start;
        let mut next = first;
        while next < edits.len() && edits[next].old.start < span.range.end {
            let edit = &edits[next];
            if edit.old.start > piece_start {
                push(
                    &mut survivors,
                    piece_start..edit.old.start,
                    span.class,
                    delta[next],
                );
            }
            piece_start = piece_start.max(edit.old.end);
            next += 1;
        }
        if piece_start < span.range.end {
            push(
                &mut survivors,
                piece_start..span.range.end,
                span.class,
                delta[next],
            );
        }
    }

    let insertions: Vec<TokenSpan> = edits
        .iter()
        .filter(|edit| !edit.new.is_empty())
        .map(|edit| TokenSpan {
            range: edit.new.clone(),
            class: class_before(edit.old.start),
        })
        .collect();

    // Both lists are sorted by position; merge them and join equal neighbours.
    let mut merged: Vec<TokenSpan> = Vec::with_capacity(survivors.len() + insertions.len());
    let (mut a, mut b) = (
        survivors.into_iter().peekable(),
        insertions.into_iter().peekable(),
    );
    loop {
        let take_a = match (a.peek(), b.peek()) {
            (Some(x), Some(y)) => x.range.start <= y.range.start,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        let span = if take_a { a.next() } else { b.next() }.unwrap();
        match merged.last_mut() {
            Some(last) if last.class == span.class && last.range.end == span.range.start => {
                last.range.end = span.range.end;
            }
            _ => merged.push(span),
        }
    }
    merged
}

fn push(out: &mut Vec<TokenSpan>, old: Range<usize>, class: Option<HighlightId>, delta: isize) {
    let start = (old.start as isize + delta) as usize;
    out.push(TokenSpan {
        range: start..start + old.len(),
        class,
    });
}

/// `spans` clipped to `window` and gaps filled with plain text, so the result
/// tiles the window exactly.
fn tile(window: Range<usize>, spans: Vec<TokenSpan>) -> Vec<TokenSpan> {
    let mut out: Vec<TokenSpan> = Vec::with_capacity(spans.len() + 1);
    let mut cursor = window.start;
    let add = |out: &mut Vec<TokenSpan>, range: Range<usize>, class: Option<HighlightId>| {
        if range.is_empty() {
            return;
        }
        match out.last_mut() {
            Some(last) if last.class == class && last.range.end == range.start => {
                last.range.end = range.end;
            }
            _ => out.push(TokenSpan { range, class }),
        }
    };
    for span in spans {
        let start = span.range.start.max(cursor).min(window.end);
        let end = span.range.end.min(window.end);
        if start >= end {
            continue;
        }
        add(&mut out, cursor..start, None);
        add(&mut out, start..end, span.class);
        cursor = end;
    }
    add(&mut out, cursor..window.end, None);
    out
}
