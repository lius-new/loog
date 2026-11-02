//! Compose generic highlight spans over a tiled native syntax projection.
use crate::features::syntax::TokenSpan;
use std::ops::Range;

pub fn compose(base: &[TokenSpan], overlay: &[TokenSpan], window: Range<usize>) -> Vec<TokenSpan> {
    let first = overlay.partition_point(|span| span.range.end <= window.start);
    let overlay = &overlay[first..];
    if overlay
        .first()
        .is_none_or(|span| span.range.start >= window.end)
    {
        return base.to_vec();
    }
    let mut out: Vec<TokenSpan> = Vec::with_capacity(base.len() + overlay.len());
    let mut push = |range: Range<usize>, class| {
        if range.is_empty() {
            return;
        }
        match out.last_mut() {
            Some(last) if last.range.end == range.start && last.class == class => {
                last.range.end = range.end
            }
            _ => out.push(TokenSpan { range, class }),
        }
    };
    let mut next = 0;
    for span in base {
        let mut at = span.range.start;
        while at < span.range.end {
            while next < overlay.len() && overlay[next].range.end <= at {
                next += 1;
            }
            match overlay.get(next) {
                Some(over) if over.range.start < span.range.end => {
                    if over.range.start > at {
                        push(at..over.range.start, span.class);
                        at = over.range.start;
                    }
                    let end = over.range.end.min(span.range.end);
                    push(at..end, over.class);
                    at = end;
                }
                _ => {
                    push(at..span.range.end, span.class);
                    at = span.range.end;
                }
            }
        }
    }
    out
}
