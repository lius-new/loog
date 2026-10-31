//! Provider answers as the editor needs them (design §6.3).
//!
//! A provider's answer is a suggestion for one state of one document. These
//! rules turn what the guest returned into values the language UI understands,
//! dropping what cannot be placed in the text instead of guessing.
use crate::features::language_service::diagnostics::{Diagnostic, Severity};
use serde_json::Value;
use std::ops::Range;

/// A diagnostic as the guest reported it (byte offsets of the requested text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reported {
    pub start: usize,
    pub end: usize,
    pub severity: u8,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
}

/// Most diagnostics one answer may carry; the rest are dropped and counted.
pub const MAX_DIAGNOSTICS: usize = 1000;
/// Longest message kept, in bytes (cut on a character boundary).
pub const MAX_MESSAGE: usize = 2000;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Converted {
    pub diagnostics: Vec<Diagnostic>,
    /// Entries that were outside the text, inverted or beyond the answer limit.
    pub dropped: usize,
}

fn severity(number: u8) -> Severity {
    match number {
        2 => Severity::Warning,
        3 => Severity::Information,
        4 => Severity::Hint,
        // 1, or anything unknown: an error is the safest thing to show.
        _ => Severity::Error,
    }
}

fn clipped(message: &str) -> String {
    if message.len() <= MAX_MESSAGE {
        return message.to_owned();
    }
    let mut end = MAX_MESSAGE;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message[..end].to_owned()
}

/// Convert `reported` for `text`. `fallback_source` names the provider when the
/// guest gave none, so every finding shows where it came from.
pub fn convert(text: &str, reported: &[Reported], fallback_source: &str) -> Converted {
    let mut out = Converted::default();
    for item in reported {
        let valid = item.start <= item.end
            && item.end <= text.len()
            && text.is_char_boundary(item.start)
            && text.is_char_boundary(item.end);
        if !valid || out.diagnostics.len() >= MAX_DIAGNOSTICS {
            out.dropped += 1;
            continue;
        }
        out.diagnostics.push(Diagnostic {
            range: Range {
                start: item.start,
                end: item.end,
            },
            severity: severity(item.severity),
            message: clipped(&item.message),
            source: Some(
                item.source
                    .clone()
                    .unwrap_or_else(|| fallback_source.to_owned()),
            ),
            code: item.code.clone(),
            extra: Value::Null,
        });
    }
    out.diagnostics
        .sort_by_key(|d| (d.range.start, d.range.end));
    out
}

/// Keep only the edits that lie inside `range` (a range formatting request).
pub fn within(
    edits: Vec<(Range<usize>, String)>,
    range: &Range<usize>,
) -> Vec<(Range<usize>, String)> {
    edits
        .into_iter()
        .filter(|(edit, _)| edit.start >= range.start && edit.end <= range.end)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(start: usize, end: usize) -> Reported {
        Reported {
            start,
            end,
            severity: 2,
            message: "m".into(),
            source: None,
            code: None,
        }
    }

    #[test]
    fn findings_are_ordered_attributed_and_typed() {
        let out = convert("abcdef", &[item(4, 5), item(0, 2)], "example.tools.lint");
        assert_eq!(out.dropped, 0);
        assert_eq!(out.diagnostics[0].range, 0..2);
        assert_eq!(out.diagnostics[0].severity, Severity::Warning);
        assert_eq!(
            out.diagnostics[0].source.as_deref(),
            Some("example.tools.lint")
        );
    }

    #[test]
    fn what_cannot_be_placed_in_the_text_is_dropped_and_counted() {
        let text = "中文";
        let out = convert(
            text,
            &[item(1, 2), item(3, 1), item(0, 99), item(0, 3)],
            "p",
        );
        assert_eq!(out.diagnostics.len(), 1);
        assert_eq!(out.dropped, 3);
    }

    #[test]
    fn unknown_severities_become_errors_and_long_messages_are_cut_on_a_boundary() {
        let mut long = item(0, 1);
        long.severity = 9;
        long.message = "中".repeat(1000);
        let out = convert("a", &[long], "p");
        assert_eq!(out.diagnostics[0].severity, Severity::Error);
        assert!(out.diagnostics[0].message.len() <= MAX_MESSAGE);
        assert!(out.diagnostics[0].message.chars().all(|c| c == '中'));
    }

    #[test]
    fn an_answer_is_bounded() {
        let many: Vec<_> = (0..MAX_DIAGNOSTICS + 5)
            .map(|i| item(i % 3, i % 3 + 1))
            .collect();
        let out = convert("abcd", &many, "p");
        assert_eq!(out.diagnostics.len(), MAX_DIAGNOSTICS);
        assert_eq!(out.dropped, 5);
    }

    #[test]
    fn range_formatting_keeps_only_edits_inside_the_selection() {
        let edits = vec![
            (0..1, String::new()),
            (5..6, String::new()),
            (9..12, String::new()),
        ];
        let kept = within(edits, &(4..10));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].0, 5..6);
    }
}
