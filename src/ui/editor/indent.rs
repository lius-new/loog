//! Indent guide positions and the cursor's active block.

use std::borrow::Cow;
use std::cell::RefCell;
use std::ops::{Range, RangeInclusive};

use loog_editor_core::{identity::ViewId, text::TextBuffer};

const NEIGHBOR_SCAN_LINES: usize = 64;
const ACTIVE_GUIDE_SCAN_LINES: usize = 500;
const ACTIVE_BLOCK_RULER_SCAN_LINES: usize = 20_000;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Spacing {
    tab: usize,
    spaces: usize,
}

impl Spacing {
    fn for_buffer(buffer: &TextBuffer) -> Self {
        let options = buffer.options();
        Self {
            tab: options.tab_width,
            spaces: if options.use_tabs {
                options.tab_width
            } else {
                options.indent_size
            },
        }
    }

    fn units(self, prefix: Cow<'_, str>) -> impl Iterator<Item = Range<usize>> + '_ {
        let mut offset = 0;
        let mut column = 0;
        std::iter::from_fn(move || {
            let rest = &prefix.as_bytes()[offset..];
            let start = column;
            if *rest.first()? == b'\t' {
                // A literal tab is one indentation unit, even when it spans
                // more display columns than the configured space indent.
                offset += 1;
                column += self.tab - column % self.tab;
            } else {
                let spaces = rest
                    .iter()
                    .take(self.spaces)
                    .take_while(|&&b| b == b' ')
                    .count();
                offset += spaces;
                column += spaces;
            }
            Some(start..column)
        })
    }
}

/// The indentation of a text line. Blank lines borrow guides from neighbors.
fn prefix(buffer: &TextBuffer, line: usize) -> Option<Cow<'_, str>> {
    let text = buffer.line_text(line);
    let end = text
        .bytes()
        .take(4096)
        .take_while(|&b| b == b' ' || b == b'\t')
        .count();
    if text
        .as_bytes()
        .get(end)
        .is_none_or(|&b| b == b'\n' || b == b'\r')
    {
        return None;
    }
    Some(match text {
        Cow::Borrowed(text) => Cow::Borrowed(&text[..end]),
        Cow::Owned(mut text) => {
            text.truncate(end);
            Cow::Owned(text)
        }
    })
}

/// Start columns of each indentation unit, excluding the start of the text.
/// Blank lines carry only the guides shared by their nearest text lines.
pub(super) fn columns(buffer: &TextBuffer, line: usize) -> impl Iterator<Item = usize> + '_ {
    let spacing = Spacing::for_buffer(buffer);
    let (first, second) = match prefix(buffer, line) {
        Some(prefix) => (Some(prefix), None),
        None => {
            let above = (0..line)
                .rev()
                .take(NEIGHBOR_SCAN_LINES)
                .find_map(|line| prefix(buffer, line));
            let below = (line + 1..buffer.line_count())
                .take(NEIGHBOR_SCAN_LINES)
                .find_map(|line| prefix(buffer, line));
            match above.zip(below) {
                Some((a, b)) => (Some(a), Some(b)),
                None => (None, None),
            }
        }
    };
    let mut other = second.map(|prefix| spacing.units(prefix).peekable());
    first
        .into_iter()
        .flat_map(move |prefix| spacing.units(prefix))
        .map(|unit| unit.start)
        .filter(move |&column| {
            other.as_mut().is_none_or(|other| {
                while other.peek().is_some_and(|unit| unit.start < column) {
                    other.next();
                }
                other.peek().is_some_and(|unit| unit.start == column)
            })
        })
}

pub(super) struct ActiveGuide {
    pub(super) column: usize,
    pub(super) lines: RangeInclusive<usize>,
}

pub(super) fn active(buffer: &TextBuffer, cursor_line: usize) -> Option<ActiveGuide> {
    active_guide(buffer, cursor_line, ACTIVE_GUIDE_SCAN_LINES)
}

fn active_guide(buffer: &TextBuffer, cursor_line: usize, scan: usize) -> Option<ActiveGuide> {
    let spacing = Spacing::for_buffer(buffer);
    let opened = prefix(buffer, cursor_line).and_then(|own| {
        let width = spacing.units(own).last().map_or(0, |unit| unit.end);
        let next = (cursor_line + 1..buffer.line_count())
            .take(NEIGHBOR_SCAN_LINES)
            .find_map(|line| prefix(buffer, line))?;
        // An opener owns the first guide inside the block it opens. If the next
        // line has no new guide, use the innermost guide on the current line.
        spacing
            .units(next)
            .find(|unit| unit.start >= width)
            .map(|unit| unit.start)
    });
    let column = opened.or_else(|| columns(buffer, cursor_line).last())?;
    let carries = |line| columns(buffer, line).find(|&guide| guide >= column) == Some(column);
    let first = if carries(cursor_line) {
        cursor_line
    } else {
        cursor_line + 1
    };
    if first >= buffer.line_count() || !carries(first) {
        return None;
    }
    let mut start = first;
    while start > 0 && first - start < scan && carries(start - 1) {
        start -= 1;
    }
    let mut end = first;
    while end + 1 < buffer.line_count() && end - first < scan && carries(end + 1) {
        end += 1;
    }
    Some(ActiveGuide {
        column,
        lines: start..=end,
    })
}

/// The overview ruler follows the same active block, with a wider scan limit.
pub(super) fn block_span(
    buffer: &TextBuffer,
    document: ViewId,
    cursor_line: usize,
) -> Option<RangeInclusive<usize>> {
    type Key = (ViewId, u64, usize, Spacing);
    thread_local! {
        static LAST: RefCell<Option<(Key, Option<RangeInclusive<usize>>)>> = const { RefCell::new(None) };
    }
    let key = (
        document,
        buffer.version(),
        cursor_line,
        Spacing::for_buffer(buffer),
    );
    LAST.with(|last| {
        if let Some((cached, span)) = &*last.borrow()
            && *cached == key
        {
            return span.clone();
        }
        let span = active_guide(buffer, cursor_line, ACTIVE_BLOCK_RULER_SCAN_LINES)
            .map(|guide| guide.lines);
        *last.borrow_mut() = Some((key, span.clone()));
        span
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_count_once_and_the_active_guide_uses_the_same_columns() {
        let buffer = TextBuffer::new(
            "type Failure struct {\n\tmessage string\n\n\tcause error\n\t\tnested\n\t  aligned\n}\n",
        );
        let guides = |line| columns(&buffer, line).collect::<Vec<_>>();
        assert!(!buffer.options().use_tabs);
        assert_eq!(guides(1), [0]);
        assert_eq!(guides(2), [0]);
        assert_eq!(guides(4), [0, 4]);
        assert_eq!(guides(5), [0, 4]);
        let outer = active(&buffer, 0).unwrap();
        assert_eq!((outer.column, outer.lines), (0, 1..=5));
        let inner = active(&buffer, 3).unwrap();
        assert_eq!((inner.column, inner.lines), (4, 4..=5));
    }

    #[test]
    fn blank_lines_only_carry_columns_both_neighbors_share() {
        let buffer = TextBuffer::new("\t\ta\n\n      b\n");
        assert_eq!(columns(&buffer, 0).collect::<Vec<_>>(), [0, 4]);
        assert_eq!(columns(&buffer, 2).collect::<Vec<_>>(), [0, 2, 4]);
        assert_eq!(columns(&buffer, 1).collect::<Vec<_>>(), [0, 4]);
    }
}
