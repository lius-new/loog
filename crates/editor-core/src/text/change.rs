//! Requested edits, their validation, and the change sets they produce.

use std::fmt;
use std::ops::Range;

/// One replacement, with `range` given in the text as it is before the edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub text: String,
}

impl Edit {
    pub fn replace(range: Range<usize>, text: impl Into<String>) -> Self {
        Self {
            range,
            text: text.into(),
        }
    }

    pub fn insert(at: usize, text: impl Into<String>) -> Self {
        Self::replace(at..at, text)
    }

    pub fn delete(range: Range<usize>) -> Self {
        Self::replace(range, String::new())
    }
}

/// Why a set of edits was rejected. Nothing is applied when validation fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditError {
    /// A range is reversed or reaches past the end of the text.
    OutOfBounds { range: Range<usize>, len: usize },
    /// A range endpoint falls inside a UTF-8 encoded character.
    NotCharBoundary(usize),
    /// Two ranges overlap, or two insertions share one position.
    Overlapping(Range<usize>, Range<usize>),
    /// An edit removes more text than undo can keep.
    TooLarge,
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfBounds { range, len } => {
                write!(f, "edit range {range:?} is outside the text (length {len})")
            }
            Self::NotCharBoundary(offset) => {
                write!(f, "offset {offset} is not on a character boundary")
            }
            Self::Overlapping(a, b) => write!(f, "edit ranges {a:?} and {b:?} overlap"),
            Self::TooLarge => write!(f, "the edit removes too much text to undo"),
        }
    }
}

impl std::error::Error for EditError {}

/// Check `edits` against `text` and return them sorted, without no-ops.
pub(super) fn validate(text: &str, edits: Vec<Edit>) -> Result<Vec<Edit>, EditError> {
    validate_with(text.len(), |offset| text.is_char_boundary(offset), edits)
}

/// As `validate`, for a text kept as pieces of a file.
pub(super) fn validate_file(
    text: &super::pieces::PieceText,
    edits: Vec<Edit>,
) -> Result<Vec<Edit>, EditError> {
    validate_with(text.len(), |offset| text.is_char_boundary(offset), edits)
}

fn validate_with(
    len: usize,
    is_char_boundary: impl Fn(usize) -> bool,
    mut edits: Vec<Edit>,
) -> Result<Vec<Edit>, EditError> {
    for edit in &edits {
        let range = &edit.range;
        if range.start > range.end || range.end > len {
            return Err(EditError::OutOfBounds {
                range: range.clone(),
                len,
            });
        }
        for offset in [range.start, range.end] {
            if !is_char_boundary(offset) {
                return Err(EditError::NotCharBoundary(offset));
            }
        }
    }
    edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
    for pair in edits.windows(2) {
        let (a, b) = (&pair[0].range, &pair[1].range);
        let same_insertion_point = a.is_empty() && b.is_empty() && a.start == b.start;
        if a.end > b.start || same_insertion_point {
            return Err(EditError::Overlapping(a.clone(), b.clone()));
        }
    }
    edits.retain(|edit| !(edit.range.is_empty() && edit.text.is_empty()));
    Ok(edits)
}

/// One applied replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// The replaced range in the text before the change set.
    pub old: Range<usize>,
    /// Where the inserted text starts in the text after the change set.
    pub new_start: usize,
    pub removed: String,
    pub inserted: String,
}

impl Change {
    /// The inserted text's range in the text after the change set.
    pub fn new_range(&self) -> Range<usize> {
        self.new_start..self.new_start + self.inserted.len()
    }
}

/// Which side of a change a position on its start ends up on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Assoc {
    /// Stay before inserted text: the rule for other views' positions.
    Before,
    /// Move past inserted text.
    #[default]
    After,
}

/// The replacements one edit call applied, sorted and non-overlapping.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangeSet {
    changes: std::sync::Arc<[Change]>,
}

impl ChangeSet {
    pub(super) fn new(changes: Vec<Change>) -> Self {
        Self {
            changes: changes.into(),
        }
    }

    pub fn changes(&self) -> &[Change] {
        &self.changes
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Map an offset in the text before the changes into the text after them.
    /// Offsets after a change shift by its length difference; offsets inside a
    /// replaced range collapse onto it; an offset on a change's start stays
    /// before the inserted text or moves past it, depending on `assoc`.
    pub fn map(&self, offset: usize, assoc: Assoc) -> usize {
        let mut delta = 0isize;
        for change in self.changes.iter() {
            let shifted = (offset as isize + delta) as usize;
            if offset < change.old.start {
                return shifted;
            }
            if offset == change.old.start || offset < change.old.end {
                return match assoc {
                    Assoc::Before => change.new_start,
                    Assoc::After => change.new_range().end,
                };
            }
            delta += change.inserted.len() as isize - change.removed.len() as isize;
        }
        (offset as isize + delta) as usize
    }

    /// Edits that turn the text after the changes back into the text before.
    pub fn inverse(&self) -> Vec<Edit> {
        self.changes
            .iter()
            .map(|change| Edit::replace(change.new_range(), change.removed.clone()))
            .collect()
    }

    /// Edits that apply these changes again to the text before them.
    pub fn forward(&self) -> Vec<Edit> {
        self.changes
            .iter()
            .map(|change| Edit::replace(change.old.clone(), change.inserted.clone()))
            .collect()
    }

    /// The smallest range of the text after the changes that covers them all.
    pub fn new_extent(&self) -> Option<Range<usize>> {
        let first = self.changes.first()?;
        let last = self.changes.last()?;
        Some(first.new_start..last.new_range().end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloned_changes_share_immutable_payloads_and_replay_independently() {
        let original = ChangeSet::new(vec![change(0..3, 0, "old", "new")]);
        let snapshot = original.clone();
        assert!(std::sync::Arc::ptr_eq(&original.changes, &snapshot.changes));
        drop(original);
        assert_eq!(snapshot.inverse(), [Edit::replace(0..3, "old")]);
        assert_eq!(snapshot.forward(), [Edit::replace(0..3, "new")]);
    }

    fn change(old: Range<usize>, new_start: usize, removed: &str, inserted: &str) -> Change {
        Change {
            old,
            new_start,
            removed: removed.into(),
            inserted: inserted.into(),
        }
    }

    #[test]
    fn validation_rejects_bad_ranges_before_anything_applies() {
        let text = "a中b";
        assert!(matches!(
            validate(text, vec![Edit::delete(2..9)]),
            Err(EditError::OutOfBounds { .. })
        ));
        assert_eq!(
            validate(text, vec![Edit::delete(0..2)]),
            Err(EditError::NotCharBoundary(2))
        );
        assert_eq!(
            validate(text, vec![Edit::delete(0..4), Edit::insert(1, "x")]),
            Err(EditError::Overlapping(0..4, 1..1))
        );
        assert_eq!(
            validate(text, vec![Edit::insert(1, "x"), Edit::insert(1, "y")]),
            Err(EditError::Overlapping(1..1, 1..1))
        );
        let sorted = validate(
            text,
            vec![
                Edit::delete(4..5),
                Edit::insert(0, ""),
                Edit::insert(1, "x"),
                Edit::delete(1..4),
            ],
        )
        .unwrap();
        assert_eq!(
            sorted,
            [Edit::insert(1, "x"), Edit::delete(1..4), Edit::delete(4..5)]
        );
    }

    #[test]
    fn mapping_follows_the_peer_rule_and_both_sides_of_an_insertion() {
        // "0123456789" with "34" -> "X" and an insertion of "ab" at 7.
        let set = ChangeSet::new(vec![change(3..5, 3, "34", "X"), change(7..7, 6, "", "ab")]);
        assert_eq!(set.map(2, Assoc::Before), 2);
        assert_eq!(set.map(3, Assoc::Before), 3);
        assert_eq!(set.map(3, Assoc::After), 4);
        assert_eq!(
            set.map(4, Assoc::Before),
            3,
            "inside collapses to the start"
        );
        assert_eq!(set.map(5, Assoc::Before), 4);
        assert_eq!(set.map(7, Assoc::Before), 6);
        assert_eq!(set.map(7, Assoc::After), 8);
        assert_eq!(set.map(10, Assoc::Before), 11);
        assert_eq!(set.new_extent(), Some(3..8));
        assert_eq!(
            set.inverse(),
            [Edit::replace(3..4, "34"), Edit::replace(6..8, "")]
        );
    }
}
