//! Turning the end result of a save-participant chain into edits.
//!
//! The participants work on a temporary copy, one after another. What the real
//! document needs is one set of edits against the text it still holds, so the
//! old and new text are compared by lines. Several small hunks keep cursors
//! outside them where they are, which one big replacement would not.
use std::ops::Range;

/// Largest line grid the exact comparison is done for; beyond it the changed
/// region between the common start and end is replaced as one edit.
const GRID_LIMIT: usize = 1_000_000;

fn lines(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for (at, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            out.push(start..at + 1);
            start = at + 1;
        }
    }
    if start < text.len() {
        out.push(start..text.len());
    }
    out
}

/// Edits (byte ranges of `old`, replacement text) that turn `old` into `new`.
pub fn edits(old: &str, new: &str) -> Vec<(Range<usize>, String)> {
    if old == new {
        return Vec::new();
    }
    let (a, b) = (lines(old), lines(new));
    let same = |i: usize, j: usize| old[a[i].clone()] == new[b[j].clone()];
    let mut prefix = 0;
    while prefix < a.len() && prefix < b.len() && same(prefix, prefix) {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < a.len() - prefix
        && suffix < b.len() - prefix
        && same(a.len() - 1 - suffix, b.len() - 1 - suffix)
    {
        suffix += 1;
    }
    let (a_mid, b_mid) = (prefix..a.len() - suffix, prefix..b.len() - suffix);
    let (n, m) = (a_mid.len(), b_mid.len());

    // Longest common subsequence of the lines in between.
    let mut matches: Vec<(usize, usize)> = Vec::new();
    if n > 0 && m > 0 && n * m <= GRID_LIMIT {
        let width = m + 1;
        let mut table = vec![0u32; (n + 1) * width];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                table[i * width + j] = if same(a_mid.start + i, b_mid.start + j) {
                    table[(i + 1) * width + j + 1] + 1
                } else {
                    table[(i + 1) * width + j].max(table[i * width + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if same(a_mid.start + i, b_mid.start + j) {
                matches.push((a_mid.start + i, b_mid.start + j));
                i += 1;
                j += 1;
            } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
    }

    // Everything between two matched lines is one hunk.
    let offset_a = |line: usize| a.get(line).map_or(old.len(), |r| r.start);
    let offset_b = |line: usize| b.get(line).map_or(new.len(), |r| r.start);
    let mut out = Vec::new();
    let (mut from_a, mut from_b) = (a_mid.start, b_mid.start);
    let boundaries = matches
        .into_iter()
        .chain(std::iter::once((a_mid.end, b_mid.end)));
    for (match_a, match_b) in boundaries {
        if match_a > from_a || match_b > from_b {
            out.push((
                offset_a(from_a)..offset_a(match_a),
                new[offset_b(from_b)..offset_b(match_b)].to_owned(),
            ));
        }
        from_a = match_a + 1;
        from_b = match_b + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(old: &str, edits: &[(Range<usize>, String)]) -> String {
        let mut out = String::new();
        let mut cursor = 0;
        for (range, text) in edits {
            out.push_str(&old[cursor..range.start]);
            out.push_str(text);
            cursor = range.end;
        }
        out.push_str(&old[cursor..]);
        out
    }

    fn check(old: &str, new: &str) -> Vec<(Range<usize>, String)> {
        let found = edits(old, new);
        assert_eq!(apply(old, &found), new, "{old:?} -> {new:?}");
        found
    }

    #[test]
    fn identical_text_needs_no_edits() {
        assert!(edits("a\nb\n", "a\nb\n").is_empty());
        assert!(edits("", "").is_empty());
    }

    #[test]
    fn separate_changes_stay_separate_hunks() {
        let old = "keep1  \nkeep2\nkeep3  \nkeep4\n";
        let new = "keep1\nkeep2\nkeep3\nkeep4\n";
        let found = check(old, new);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].0, 0..8);
        assert_eq!(found[1].0, 14..22);
    }

    #[test]
    fn insertions_deletions_and_replacements_round_trip() {
        for (old, new) in [
            ("", "new\n"),
            ("gone\n", ""),
            ("a\nb\nc\n", "a\nX\nb\nc\nY\n"),
            ("a\nb\nc\n", "c\nb\na\n"),
            ("no newline", "no newline!"),
            ("a\r\nb\r\n", "a\r\nB\r\n"),
            ("中文\n行\n", "中文\n新行\n"),
            ("a\nb", "a\nb\n"),
        ] {
            check(old, new);
        }
    }

    #[test]
    fn a_huge_difference_falls_back_to_one_replacement_between_the_common_ends() {
        let old = format!("top\n{}bottom\n", "x\n".repeat(3000));
        let new = format!("top\n{}bottom\n", "y\n".repeat(3000));
        let found = check(&old, &new);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0.start, 4);
    }
}
