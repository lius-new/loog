//! Text edit validation and application (design §6.2).
//!
//! All edits of one request are interpreted against the same pre-edit text.
use crate::error::{ErrorKind, ExtensionError};

/// Half-open UTF-8 byte range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

impl ByteRange {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub fn len(self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(self) -> bool {
        self.start >= self.end
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub range: ByteRange,
    pub text: String,
}

fn invalid(message: String) -> ExtensionError {
    ExtensionError::new(ErrorKind::InvalidEdit, message)
}

/// Validate `edits` against `text` and return their indices in application order.
///
/// Rejects inverted ranges, ranges past the end, offsets inside a character
/// and overlaps. Insertions at the same offset keep their request order, and
/// an insertion at the edge of a replaced range is not an overlap.
pub fn validate(text: &str, edits: &[TextEdit]) -> Result<Vec<usize>, ExtensionError> {
    for (index, edit) in edits.iter().enumerate() {
        let ByteRange { start, end } = edit.range;
        if start > end {
            return Err(invalid(format!(
                "edit {index}: start {start} is after end {end}"
            )));
        }
        if end > text.len() {
            return Err(invalid(format!(
                "edit {index}: end {end} is past the text length {}",
                text.len()
            )));
        }
        if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            return Err(invalid(format!(
                "edit {index}: range {start}..{end} splits a character"
            )));
        }
    }
    let mut order: Vec<usize> = (0..edits.len()).collect();
    order.sort_by_key(|&i| (edits[i].range.start, edits[i].range.end));
    for pair in order.windows(2) {
        let (a, b) = (&edits[pair[0]], &edits[pair[1]]);
        if a.range.end > b.range.start {
            return Err(invalid(format!(
                "edits {} and {} overlap",
                pair[0], pair[1]
            )));
        }
    }
    Ok(order)
}

/// Apply validated edits and return the resulting text.
pub fn apply(text: &str, edits: &[TextEdit]) -> Result<String, ExtensionError> {
    let order = validate(text, edits)?;
    let insert: usize = edits.iter().map(|edit| edit.text.len()).sum();
    let removed: usize = edits.iter().map(|edit| edit.range.len()).sum();
    let mut out = String::with_capacity((text.len() + insert).saturating_sub(removed));
    let mut cursor = 0;
    for index in order {
        let edit = &edits[index];
        out.push_str(&text[cursor..edit.range.start]);
        out.push_str(&edit.text);
        cursor = edit.range.end;
    }
    out.push_str(&text[cursor..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(start: usize, end: usize, text: &str) -> TextEdit {
        TextEdit {
            range: ByteRange::new(start, end),
            text: text.into(),
        }
    }

    #[test]
    fn applies_out_of_order_edits_against_the_original_text() {
        let result = apply("hello world", &[edit(6, 11, "Loog"), edit(0, 5, "HELLO")]).unwrap();
        assert_eq!(result, "HELLO Loog");
    }

    #[test]
    fn same_offset_insertions_keep_request_order() {
        let result = apply("ab", &[edit(1, 1, "X"), edit(1, 1, "Y")]).unwrap();
        assert_eq!(result, "aXYb");
    }

    #[test]
    fn insertion_at_a_replaced_range_edge_is_not_an_overlap() {
        let result = apply("abcd", &[edit(1, 3, "-"), edit(1, 1, "<"), edit(3, 3, ">")]).unwrap();
        assert_eq!(result, "a<->d");
    }

    #[test]
    fn rejects_overlap_bounds_and_split_characters() {
        let kind = |text: &str, edits: &[TextEdit]| apply(text, edits).unwrap_err().kind;
        assert_eq!(
            kind("abcdef", &[edit(0, 3, ""), edit(2, 4, "")]),
            ErrorKind::InvalidEdit
        );
        assert_eq!(kind("abc", &[edit(0, 9, "")]), ErrorKind::InvalidEdit);
        assert_eq!(kind("abcdef", &[edit(3, 1, "")]), ErrorKind::InvalidEdit);
        assert_eq!(kind("中", &[edit(1, 2, "")]), ErrorKind::InvalidEdit);
    }

    #[test]
    fn handles_crlf_and_emoji() {
        let text = "a\r\n😀b";
        let emoji = text.find('😀').unwrap();
        let result = apply(text, &[edit(emoji, emoji + 4, "x")]).unwrap();
        assert_eq!(result, "a\r\nxb");
    }
}
