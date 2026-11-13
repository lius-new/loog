//! Compare already decoded text without filesystem or process access.
use super::{
    diff::{DiffHunk, parse_unified},
    error::{GitError, GitErrorKind, GitResult},
};
use std::{io::Write, time::Duration};

pub const MAX_SIDE_BYTES: usize = 4 * 1024 * 1024;
const MAX_LINES: usize = 128_000;
const MAX_OUTPUT: usize = 16 * 1024 * 1024;

pub fn compare(old: &str, new: &str, context: usize) -> GitResult<Vec<DiffHunk>> {
    if old.len() > MAX_SIDE_BYTES
        || new.len() > MAX_SIDE_BYTES
        || old.lines().count().saturating_add(new.lines().count()) > MAX_LINES
    {
        return Err(limit());
    }
    if old == new {
        return Ok(Vec::new());
    }
    // On its deadline Myers falls back to a valid, possibly coarser edit script.
    // https://docs.rs/similar/3.2.1/similar/struct.TextDiffConfig.html#method.timeout
    let diff = similar::TextDiff::configure()
        .timeout(Duration::from_millis(500))
        .diff_lines(old, new);
    struct Bounded(Vec<u8>);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0.len().saturating_add(bytes.len()) > MAX_OUTPUT {
                return Err(std::io::Error::other("Text diff output exceeds its budget"));
            }
            self.0.extend(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded(Vec::new());
    diff.unified_diff()
        .context_radius(context.min(MAX_LINES))
        .header("old", "new")
        .to_writer(&mut output)
        .map_err(|_| limit())?;
    let text = std::str::from_utf8(&output.0)
        .map_err(|_| GitError::new(GitErrorKind::InvalidOutput, "Decoded diff is not UTF-8"))?;
    Ok(parse_unified(text)?
        .files
        .into_iter()
        .flat_map(|file| file.hunks)
        .collect())
}
fn limit() -> GitError {
    GitError::new(
        GitErrorKind::Unsupported,
        "Text diff exceeds its 4 MiB per side, 128,000 line or 16 MiB output budget",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::git::diff::DiffLine;
    #[test]
    fn decoded_comparison_keeps_unicode_ranges_and_missing_newlines() {
        let diff = compare("same\n中文旧", "same\n中文新\n", 3).unwrap();
        assert_eq!(diff.len(), 1);
        assert_eq!(
            (diff[0].old_start, diff[0].old_lines, diff[0].new_lines),
            (1, 2, 2)
        );
        assert!(diff[0].lines.contains(&DiffLine::Deletion("中文旧".into())));
        assert!(diff[0].lines.contains(&DiffLine::Addition("中文新".into())));
        assert!(diff[0].lines.contains(&DiffLine::NoNewline));
        assert!(compare("same", "same", 3).unwrap().is_empty());
        assert!(compare(&"x\n".repeat(MAX_LINES + 1), "", 3).is_err());
    }
}
