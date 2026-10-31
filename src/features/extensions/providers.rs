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

/// A completion candidate as the guest offered it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Offered {
    pub label: String,
    pub detail: Option<String>,
    pub documentation: Option<String>,
    pub kind: Option<u32>,
    pub insert_text: Option<String>,
    pub sort_text: Option<String>,
    pub filter_text: Option<String>,
    /// Opaque to the host; returned unchanged when the item is resolved.
    pub data: Option<String>,
}

/// Most items one answer may carry.
pub const MAX_COMPLETIONS: usize = 500;
const MAX_TEXT: usize = 4000;

fn cut(text: &str) -> &str {
    if text.len() <= MAX_TEXT {
        return text;
    }
    let mut end = MAX_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The items as the JSON a completion list is read from. Each carries
/// `x-loom` with its origin, so a later resolve goes back to the extension and
/// provider that offered it and nowhere else. Empty labels and items past the
/// limit are dropped; the count of dropped items is returned.
pub fn completion_json(extension: &str, provider: &str, items: &[Offered]) -> (Vec<Value>, usize) {
    let mut out = Vec::new();
    let mut dropped = 0;
    for item in items {
        if item.label.trim().is_empty() || out.len() >= MAX_COMPLETIONS {
            dropped += 1;
            continue;
        }
        let mut json = serde_json::json!({
            "label": cut(&item.label),
            "x-loom": { "extension": extension, "provider": provider, "data": item.data },
        });
        let object = json.as_object_mut().expect("an object");
        for (key, value) in [
            ("detail", item.detail.as_deref()),
            ("documentation", item.documentation.as_deref()),
            ("insertText", item.insert_text.as_deref()),
            ("sortText", item.sort_text.as_deref()),
            ("filterText", item.filter_text.as_deref()),
        ] {
            if let Some(value) = value {
                object.insert(key.to_owned(), Value::from(cut(value)));
            }
        }
        if let Some(kind) = item.kind {
            object.insert("kind".to_owned(), Value::from(kind));
        }
        out.push(json);
    }
    (out, dropped)
}

#[cfg(test)]
mod completion_tests {
    use super::*;

    #[test]
    fn items_carry_their_origin_and_only_what_was_given() {
        let item = Offered {
            label: "loom_hello".into(),
            insert_text: Some("loom_hello()".into()),
            kind: Some(3),
            data: Some("hello".into()),
            ..Offered::default()
        };
        let (json, dropped) = completion_json("example.tools", "example.tools.complete", &[item]);
        assert_eq!(dropped, 0);
        assert_eq!(json[0]["insertText"], "loom_hello()");
        assert_eq!(json[0]["x-loom"]["extension"], "example.tools");
        assert_eq!(json[0]["x-loom"]["data"], "hello");
        assert!(json[0].get("detail").is_none());
    }

    #[test]
    fn empty_labels_and_overflow_are_dropped_and_counted() {
        let mut items: Vec<Offered> = (0..MAX_COMPLETIONS + 2)
            .map(|i| Offered {
                label: format!("item{i}"),
                ..Offered::default()
            })
            .collect();
        items.push(Offered {
            label: "  ".into(),
            ..Offered::default()
        });
        let (json, dropped) = completion_json("a.b", "a.b.c", &items);
        assert_eq!(json.len(), MAX_COMPLETIONS);
        assert_eq!(dropped, 3);
    }

    #[test]
    fn long_text_is_cut_on_a_character_boundary() {
        let item = Offered {
            label: "x".into(),
            documentation: Some("中".repeat(3000)),
            ..Offered::default()
        };
        let (json, _) = completion_json("a.b", "a.b.c", &[item]);
        let docs = json[0]["documentation"].as_str().unwrap();
        assert!(docs.len() <= MAX_TEXT && docs.chars().all(|c| c == '中'));
    }
}
