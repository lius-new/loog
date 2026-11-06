//! Problems: the diagnostics of open documents, grouped by file.
//!
//! Only documents that are open have diagnostics, so this is never a
//! workspace-wide list. The module is independent of who produced a finding.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::features::language_service::diagnostics::{Diagnostic, Severity};
use crate::features::language_service::reply::Span;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Problem {
    pub severity: Severity,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
    /// 1-based.
    pub line: usize,
    /// 1-based. In characters for an open document; as the service counted
    /// them for a file that is not open.
    pub column: usize,
    /// Byte offset of the start, for jumping to the spot in an open document.
    pub offset: usize,
    /// Where a file that is not open was reported, in the service's units. Such
    /// a problem has no text here to turn it into bytes.
    pub span: Option<Span>,
}

/// A finding in a file that is not open, as the service placed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Finding {
    pub severity: Severity,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
    pub span: Span,
}

/// Findings of a file that is not open, worst first within the file.
pub(crate) fn closed_file_problems(path: PathBuf, findings: &[Finding]) -> Option<FileProblems> {
    let mut items = findings
        .iter()
        .map(|f| Problem {
            severity: f.severity,
            message: f.message.clone(),
            source: f.source.clone(),
            code: f.code.clone(),
            line: f.span.start.line as usize + 1,
            column: f.span.start.character as usize + 1,
            offset: 0,
            span: Some(f.span),
        })
        .collect::<Vec<_>>();
    if items.is_empty() {
        return None;
    }
    items.sort_by_key(|p| (p.severity, p.line, p.column));
    Some(FileProblems { path, items })
}

/// What names a file regardless of how its path is spelled: without Windows'
/// `\\?\` prefix, with forward slashes, and case-folded on Windows.
pub(crate) fn file_key(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.strip_prefix("//?/").unwrap_or(&text).to_owned();
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileProblems {
    pub path: PathBuf,
    pub items: Vec<Problem>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
}

impl Counts {
    pub fn of(groups: &[FileProblems]) -> Self {
        let mut counts = Self::default();
        for problem in groups.iter().flat_map(|group| &group.items) {
            match problem.severity {
                Severity::Error => counts.errors += 1,
                Severity::Warning => counts.warnings += 1,
                Severity::Information | Severity::Hint => counts.infos += 1,
            }
        }
        counts
    }
}

/// Which severities the panel shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Filter {
    pub errors: bool,
    pub warnings: bool,
    pub infos: bool,
}

impl Default for Filter {
    fn default() -> Self {
        Self {
            errors: true,
            warnings: true,
            infos: true,
        }
    }
}

impl Filter {
    pub fn allows(self, severity: Severity) -> bool {
        match severity {
            Severity::Error => self.errors,
            Severity::Warning => self.warnings,
            Severity::Information | Severity::Hint => self.infos,
        }
    }
}

/// 1-based line and character column of a byte offset. An offset past the end
/// or inside a character is clamped to the nearest earlier boundary.
pub(crate) fn line_column(text: &str, offset: usize) -> (usize, usize) {
    let mut end = offset.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let before = &text[..end];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let start = before.rfind('\n').map_or(0, |at| at + 1);
    (line, before[start..].chars().count() + 1)
}

/// One open document's findings, worst first within the file, then by position.
pub(crate) fn file_problems(
    path: PathBuf,
    text: &str,
    diagnostics: &[Diagnostic],
) -> Option<FileProblems> {
    let mut items = diagnostics
        .iter()
        .map(|d| {
            let (line, column) = line_column(text, d.range.start);
            Problem {
                severity: d.severity,
                message: d.message.clone(),
                source: d.source.clone(),
                code: d.code.clone(),
                line,
                column,
                offset: d.range.start,
                span: None,
            }
        })
        .collect::<Vec<_>>();
    if items.is_empty() {
        return None;
    }
    items.sort_by_key(|p| (p.severity, p.offset));
    Some(FileProblems { path, items })
}

/// Files with the most errors first, then by path.
pub(crate) fn order(groups: &mut [FileProblems]) {
    let errors = |group: &FileProblems| {
        group
            .items
            .iter()
            .filter(|p| p.severity == Severity::Error)
            .count()
    };
    groups.sort_by(|a, b| errors(b).cmp(&errors(a)).then_with(|| a.path.cmp(&b.path)));
}

/// The same findings seen through a filter. Files left empty disappear.
pub(crate) fn filtered(groups: &[FileProblems], filter: Filter) -> Vec<FileProblems> {
    groups
        .iter()
        .filter_map(|group| {
            let items = group
                .items
                .iter()
                .filter(|p| filter.allows(p.severity))
                .cloned()
                .collect::<Vec<_>>();
            (!items.is_empty()).then(|| FileProblems {
                path: group.path.clone(),
                items,
            })
        })
        .collect()
}

/// Shared so a panel can hold the result of one scan across frames.
pub(crate) type Snapshot = Arc<Vec<FileProblems>>;

static ERRORS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// How many errors the open documents have, for the tab's badge. Written once
/// per frame by the shell; read by the dock's header.
pub(crate) fn set_error_count(count: usize) {
    ERRORS.store(
        count.min(u32::MAX as usize) as u32,
        std::sync::atomic::Ordering::Relaxed,
    );
}

pub(crate) fn error_count() -> u32 {
    ERRORS.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(range: std::ops::Range<usize>, severity: Severity, message: &str) -> Diagnostic {
        Diagnostic {
            range,
            severity,
            message: message.into(),
            source: None,
            code: None,
            documentation_url: None,
        }
    }

    #[test]
    fn line_and_column_count_characters_not_bytes() {
        let text = "ab\n字符x\n";
        assert_eq!(line_column(text, 0), (1, 1));
        assert_eq!(line_column(text, 3), (2, 1));
        // After "字符" (6 bytes): column 3.
        assert_eq!(line_column(text, 3 + 6), (2, 3));
        // Inside a character clamps back to its start.
        assert_eq!(line_column(text, 3 + 1), (2, 1));
        // Past the end clamps.
        assert_eq!(line_column(text, 999), (3, 1));
    }

    #[test]
    fn file_orders_errors_first_then_position() {
        let text = "aaaa\nbbbb\ncccc\n";
        let file = file_problems(
            "a.rs".into(),
            text,
            &[
                finding(10..11, Severity::Warning, "w"),
                finding(5..6, Severity::Error, "late error"),
                finding(0..1, Severity::Error, "early error"),
            ],
        )
        .unwrap();
        let messages = file
            .items
            .iter()
            .map(|p| p.message.as_str())
            .collect::<Vec<_>>();
        assert_eq!(messages, ["early error", "late error", "w"]);
        assert_eq!((file.items[1].line, file.items[1].column), (2, 1));
    }

    #[test]
    fn closed_files_keep_the_services_own_positions() {
        use crate::features::providers::PositionEncoding;
        use crate::features::providers::position::Position;
        let at = |line, character| Position { line, character };
        let finding = |line, severity| Finding {
            severity,
            message: "m".into(),
            source: Some("rustc".into()),
            code: None,
            span: Span {
                start: at(line, 4),
                end: at(line, 9),
                encoding: PositionEncoding::Utf16,
            },
        };
        let file = closed_file_problems(
            "b.rs".into(),
            &[
                finding(9, Severity::Warning),
                finding(20, Severity::Error),
                finding(2, Severity::Error),
            ],
        )
        .unwrap();
        let places = file
            .items
            .iter()
            .map(|p| (p.line, p.column))
            .collect::<Vec<_>>();
        // Errors first, then by place; lines and columns count from one.
        assert_eq!(places, [(3, 5), (21, 5), (10, 5)]);
        assert!(file.items.iter().all(|p| p.span.is_some()));
        assert_eq!(closed_file_problems("c.rs".into(), &[]), None);
    }

    #[test]
    fn one_file_has_one_key_however_its_path_is_spelled() {
        let plain = file_key(Path::new(r"D:\Work\src\main.rs"));
        let verbatim = file_key(Path::new(r"\\?\D:\Work\src\main.rs"));
        assert_eq!(plain, verbatim);
        assert_ne!(plain, file_key(Path::new(r"D:\Work\src\lib.rs")));
    }

    #[test]
    fn empty_findings_make_no_file() {
        assert_eq!(file_problems("a.rs".into(), "x", &[]), None);
    }

    #[test]
    fn files_with_errors_come_first() {
        let one = |name: &str, severity| {
            file_problems(name.into(), "x", &[finding(0..1, severity, "m")]).unwrap()
        };
        let mut groups = vec![
            one("a.rs", Severity::Warning),
            one("c.rs", Severity::Error),
            one("b.rs", Severity::Error),
        ];
        order(&mut groups);
        let names = groups
            .iter()
            .map(|g| g.path.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names, ["b.rs", "c.rs", "a.rs"]);
    }

    #[test]
    fn filter_drops_severities_and_empty_files() {
        let groups = vec![
            file_problems("a.rs".into(), "x", &[finding(0..1, Severity::Warning, "w")]).unwrap(),
            file_problems(
                "b.rs".into(),
                "x",
                &[
                    finding(0..1, Severity::Error, "e"),
                    finding(0..1, Severity::Hint, "h"),
                ],
            )
            .unwrap(),
        ];
        let only_errors = Filter {
            errors: true,
            warnings: false,
            infos: false,
        };
        let shown = filtered(&groups, only_errors);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].items.len(), 1);
        assert_eq!(
            Counts::of(&groups),
            Counts {
                errors: 1,
                warnings: 1,
                infos: 1
            }
        );
    }
}
