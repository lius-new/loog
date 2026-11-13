//! Pure matching, glob policy, previews and stale-line location.
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use unicode_segmentation::UnicodeSegmentation;
const PREVIEW_GRAPHEMES: usize = 160;
pub const MAX_PREVIEW_BYTES: usize = 2048;
pub const MAX_MATCHES: usize = 10_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchSpec {
    pub query: String,
    pub include: String,
    pub exclude: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub include_hidden: bool,
    pub include_ignored: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Match {
    /// UTF-8 byte offsets in decoded text, independent of disk encoding.
    pub range: Range<usize>,
    pub line: usize,
    pub column: usize,
    pub preview: String,
    pub highlight: Range<usize>,
    pub line_hash: u64,
}

pub struct Matcher {
    regex: Regex,
    include: GlobSet,
    exclude: GlobSet,
    has_include: bool,
}

impl Matcher {
    pub fn new(spec: &SearchSpec) -> Result<Self, String> {
        if spec.query.len() > 16 * 1024
            || spec.include.len() > 16 * 1024
            || spec.exclude.len() > 16 * 1024
        {
            return Err("Search expression exceeds 16 KiB".into());
        }
        let pattern = if spec.regex {
            spec.query.clone()
        } else {
            regex::escape(&spec.query)
        };
        let pattern = if spec.whole_word {
            format!(r"\b(?:{pattern})\b")
        } else {
            pattern
        };
        let regex = RegexBuilder::new(&pattern)
            .case_insensitive(!spec.case_sensitive)
            .multi_line(true)
            .crlf(true)
            .size_limit(8 * 1024 * 1024)
            .build()
            .map_err(|error| format!("Invalid search pattern: {error}"))?;
        let include = compile_globs(&spec.include)?;
        let exclude = compile_globs(&spec.exclude)?;
        Ok(Self {
            regex,
            include,
            exclude,
            has_include: !spec.include.trim().is_empty(),
        })
    }

    pub fn includes(&self, relative: &Path) -> bool {
        (!self.has_include || self.include.is_match(relative)) && !self.excludes(relative)
    }

    pub fn excludes(&self, relative: &Path) -> bool {
        relative
            .ancestors()
            .filter(|path| !path.as_os_str().is_empty())
            .any(|path| self.exclude.is_match(path))
    }

    /// First version is line oriented; a regex cannot match across line breaks.
    pub fn matches(&self, text: &str, cancel: &AtomicBool, limit: usize) -> Vec<Match> {
        let mut matches = Vec::new();
        let mut offset = 0;
        for (line, with_break) in text.split_inclusive('\n').enumerate() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let value = with_break.strip_suffix('\n').unwrap_or(with_break);
            let value = value.strip_suffix('\r').unwrap_or(value);
            let mut found_matches = self.regex.find_iter(value).peekable();
            if found_matches.peek().is_none() {
                offset += with_break.len();
                continue;
            }
            let boundaries = value
                .grapheme_indices(true)
                .map(|(offset, _)| offset)
                .chain(std::iter::once(value.len()))
                .collect::<Vec<_>>();
            let line_hash = hash(value);
            for found in found_matches {
                if cancel.load(Ordering::Relaxed) || matches.len() >= limit {
                    return matches;
                }
                let (preview, highlight) = preview(value, found.range(), &boundaries);
                matches.push(Match {
                    range: offset + found.start()..offset + found.end(),
                    line,
                    column: found.start(),
                    preview,
                    highlight,
                    line_hash,
                });
            }
            offset += with_break.len();
        }
        // split_inclusive has no last empty line. Permit $/^ matches there.
        if (text.is_empty() || text.ends_with('\n'))
            && matches.len() < limit
            && !cancel.load(Ordering::Relaxed)
            && self.regex.is_match("")
        {
            matches.push(Match {
                range: text.len()..text.len(),
                line: text.bytes().filter(|b| *b == b'\n').count(),
                column: 0,
                preview: String::new(),
                highlight: 0..0,
                line_hash: hash(""),
            });
        }
        matches
    }

    /// Unchanged lines can move after an edit. Never select an unrelated line.
    pub fn locate(&self, text: &str, content_hash: u64, found: &Match) -> Option<Range<usize>> {
        if hash(text) == content_hash {
            return (found.range.start <= found.range.end
                && text.is_char_boundary(found.range.start)
                && text.is_char_boundary(found.range.end))
            .then(|| found.range.clone());
        }
        let candidates = self.matches(text, &AtomicBool::new(false), MAX_MATCHES + 1);
        candidates
            .into_iter()
            .filter(|candidate| {
                candidate.line_hash == found.line_hash && candidate.column == found.column
            })
            .min_by_key(|candidate| candidate.line.abs_diff(found.line))
            .map(|candidate| candidate.range)
    }
}

fn compile_globs(value: &str) -> Result<GlobSet, String> {
    let mut builder = GlobSetBuilder::new();
    for pattern in split_globs(value) {
        let pattern = pattern.replace('\\', "/");
        let glob = GlobBuilder::new(&pattern)
            .literal_separator(false)
            .build()
            .map_err(|error| format!("Invalid file filter: {error}"))?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|error| format!("Invalid file filter: {error}"))
}

fn split_globs(value: &str) -> Vec<&str> {
    let mut start = 0;
    let mut depth: usize = 0;
    let mut output = Vec::new();
    for (index, ch) in value.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                output.push(value[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    output.push(value[start..].trim());
    output
        .into_iter()
        .filter(|pattern| !pattern.is_empty())
        .collect()
}

fn preview(line: &str, found: Range<usize>, boundaries: &[usize]) -> (String, Range<usize>) {
    let at = boundaries
        .partition_point(|offset| *offset <= found.start)
        .saturating_sub(1);
    let first = at.saturating_sub(40);
    let last = (first + PREVIEW_GRAPHEMES).min(boundaries.len() - 1);
    let mut start = boundaries[first];
    let mut end = boundaries[last];
    // One extended grapheme can contain megabytes of combining marks. Keep
    // previews bounded even then, cutting only at UTF-8 character boundaries.
    if end - start > MAX_PREVIEW_BYTES {
        start = found.start.saturating_sub(MAX_PREVIEW_BYTES / 4).max(start);
        while !line.is_char_boundary(start) {
            start += 1;
        }
        end = (start + MAX_PREVIEW_BYTES).min(end);
        while !line.is_char_boundary(end) {
            end -= 1;
        }
    }
    let prefix = if start > 0 { "…" } else { "" };
    let suffix = if end < line.len() { "…" } else { "" };
    let preview = format!("{prefix}{}{suffix}", &line[start..end]);
    let highlight = prefix.len() + found.start.saturating_sub(start).min(end - start)
        ..prefix.len() + found.end.saturating_sub(start).min(end - start);
    (preview, highlight)
}

pub fn hash(text: &str) -> u64 {
    // FNV-1a is a version-independent navigation fingerprint, not authentication.
    text.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec(query: &str) -> SearchSpec {
        SearchSpec {
            query: query.into(),
            include: String::new(),
            exclude: String::new(),
            case_sensitive: true,
            whole_word: false,
            regex: false,
            include_hidden: false,
            include_ignored: false,
        }
    }
    #[test]
    fn giant_graphemes_keep_preview_and_offsets_bounded() {
        let text = format!("a{} needle", "\u{301}".repeat(200_000));
        let matches =
            Matcher::new(&spec("needle"))
                .unwrap()
                .matches(&text, &AtomicBool::new(false), 2);
        assert_eq!(matches.len(), 1);
        let found = &matches[0];
        assert!(found.preview.len() <= MAX_PREVIEW_BYTES + 6);
        assert_eq!(&text[found.range.clone()], "needle");
        assert_eq!(&found.preview[found.highlight.clone()], "needle");
    }
    #[test]
    fn navigation_fingerprints_are_stable_and_provider_offsets_are_validated() {
        assert_eq!(hash("hello"), 0xa430d84680aabd0b);
        let matcher = Matcher::new(&spec("é")).unwrap();
        let mut found = matcher.matches("é", &AtomicBool::new(false), 1).remove(0);
        found.range = 1..2;
        assert!(matcher.locate("é", hash("é"), &found).is_none());
    }
}
