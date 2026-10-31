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

/// Answers to hover, definition, outline, signature and inlay requests, turned
/// into the shapes a language server would reply with, so the language UI that
/// already reads those replies shows them without knowing where they came from.
/// Positions are in UTF-8 (the part that carries them says so).
pub mod language {
    use crate::features::language_service::capabilities::PositionEncoding;
    use crate::features::language_service::position::LineIndex;
    use crate::features::language_service::uri;
    use serde_json::{Value, json};
    use std::path::Path;

    /// Most entries one answer may carry; the rest are dropped.
    pub const MAX_ENTRIES: usize = 2000;
    const MAX_TEXT: usize = 8000;

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Hover {
        pub contents: String,
        pub range: Option<(usize, usize)>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Place {
        pub path: String,
        pub line: u32,
        pub column: u32,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Symbol {
        pub name: String,
        pub detail: Option<String>,
        pub kind: u32,
        pub range: (usize, usize),
        pub selection: (usize, usize),
        pub parent: Option<usize>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Signature {
        pub label: String,
        pub documentation: Option<String>,
        pub parameters: Vec<(String, Option<String>)>,
        pub active: Option<u32>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Hint {
        pub offset: usize,
        pub label: String,
        pub kind: Option<u8>,
        pub padding_left: bool,
        pub padding_right: bool,
        pub tooltip: Option<String>,
    }

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

    fn placed(text: &str, offset: usize) -> bool {
        offset <= text.len() && text.is_char_boundary(offset)
    }

    fn position(index: &LineIndex<'_>, offset: usize) -> Value {
        let at = index.position(offset, PositionEncoding::Utf8);
        json!({ "line": at.line, "character": at.character })
    }

    fn span(text: &str, index: &LineIndex<'_>, (start, end): (usize, usize)) -> Option<Value> {
        (start <= end && placed(text, start) && placed(text, end))
            .then(|| json!({ "start": position(index, start), "end": position(index, end) }))
    }

    /// `None` when there is nothing to show.
    pub fn hover(text: &str, hover: &Hover) -> Option<Value> {
        if hover.contents.trim().is_empty() {
            return None;
        }
        let index = LineIndex::new(text);
        let mut out = json!({ "contents": { "kind": "markdown", "value": cut(&hover.contents) } });
        if let Some(range) = hover.range.and_then(|range| span(text, &index, range)) {
            out["range"] = range;
        }
        Some(out)
    }

    /// `base` is the folder of the requested document, for relative places.
    /// Returns the locations and how many were dropped.
    pub fn locations(base: &Path, places: &[Place]) -> (Value, usize) {
        let mut out = Vec::new();
        let mut dropped = 0;
        for place in places {
            let path = Path::new(&place.path);
            let full = if path.is_absolute() {
                path.to_path_buf()
            } else {
                base.join(path)
            };
            let Some(uri) = uri::file_uri(&full).filter(|_| out.len() < MAX_ENTRIES) else {
                dropped += 1;
                continue;
            };
            let at = json!({ "line": place.line, "character": place.column });
            out.push(json!({ "uri": uri, "range": { "start": at, "end": at } }));
        }
        (Value::Array(out), dropped)
    }

    /// An outline as nested document symbols. An entry whose range cannot be
    /// placed, or whose parent does not come before it or was dropped, is dropped.
    pub fn symbols(text: &str, symbols: &[Symbol]) -> (Value, usize) {
        let index = LineIndex::new(text);
        let mut kept: Vec<Option<Value>> = vec![None; symbols.len()];
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); symbols.len()];
        let mut roots = Vec::new();
        let mut dropped = 0;
        for (at, symbol) in symbols.iter().enumerate() {
            let parent_ok = match symbol.parent {
                None => true,
                Some(parent) => parent < at && kept[parent].is_some(),
            };
            let ranges = span(text, &index, symbol.range).zip(span(text, &index, symbol.selection));
            let (true, Some((range, selection)), false, true) = (
                parent_ok,
                ranges,
                symbol.name.trim().is_empty(),
                at < MAX_ENTRIES,
            ) else {
                dropped += 1;
                continue;
            };
            let mut entry = json!({
                "name": cut(&symbol.name),
                "kind": symbol.kind,
                "range": range,
                "selectionRange": selection,
            });
            if let Some(detail) = &symbol.detail {
                entry["detail"] = Value::from(cut(detail));
            }
            kept[at] = Some(entry);
            match symbol.parent {
                Some(parent) => children[parent].push(at),
                None => roots.push(at),
            }
        }
        fn build(at: usize, kept: &mut [Option<Value>], children: &[Vec<usize>]) -> Value {
            let mut entry = kept[at].take().expect("kept entry");
            let nested: Vec<Value> = children[at]
                .iter()
                .map(|child| build(*child, kept, children))
                .collect();
            if !nested.is_empty() {
                entry["children"] = Value::Array(nested);
            }
            entry
        }
        let tree = roots
            .into_iter()
            .map(|root| build(root, &mut kept, &children))
            .collect();
        (Value::Array(tree), dropped)
    }

    pub fn signature(signature: &Signature) -> Option<Value> {
        if signature.label.trim().is_empty() {
            return None;
        }
        let parameters: Vec<Value> = signature
            .parameters
            .iter()
            .take(MAX_ENTRIES)
            .map(|(label, documentation)| {
                let mut parameter = json!({ "label": cut(label) });
                if let Some(documentation) = documentation {
                    parameter["documentation"] = Value::from(cut(documentation));
                }
                parameter
            })
            .collect();
        let mut entry = json!({ "label": cut(&signature.label), "parameters": parameters });
        if let Some(documentation) = &signature.documentation {
            entry["documentation"] = Value::from(cut(documentation));
        }
        let mut out = json!({ "signatures": [entry], "activeSignature": 0 });
        if let Some(active) = signature.active {
            out["activeParameter"] = Value::from(active);
        }
        Some(out)
    }

    pub fn hints(text: &str, hints: &[Hint]) -> (Value, usize) {
        let index = LineIndex::new(text);
        let mut out = Vec::new();
        let mut dropped = 0;
        for hint in hints {
            if !placed(text, hint.offset) || hint.label.is_empty() || out.len() >= MAX_ENTRIES {
                dropped += 1;
                continue;
            }
            let mut entry = json!({
                "position": position(&index, hint.offset),
                "label": cut(&hint.label),
                "paddingLeft": hint.padding_left,
                "paddingRight": hint.padding_right,
            });
            if let Some(kind) = hint.kind.filter(|kind| matches!(kind, 1 | 2)) {
                entry["kind"] = Value::from(kind);
            }
            if let Some(tooltip) = &hint.tooltip {
                entry["tooltip"] = Value::from(cut(tooltip));
            }
            out.push(entry);
        }
        (Value::Array(out), dropped)
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Action {
        pub title: String,
        pub kind: Option<String>,
        pub preferred: bool,
        /// `(start, end, new text)` against the requested text.
        pub edits: Vec<(usize, usize, String)>,
    }

    /// Code actions whose change is part of the reply. An action whose edits
    /// cannot be placed or overlap each other, or that has no title, is dropped
    /// whole: half of a change is worse than none.
    pub fn code_actions(text: &str, document_uri: &str, actions: &[Action]) -> (Value, usize) {
        let index = LineIndex::new(text);
        let mut out = Vec::new();
        let mut dropped = 0;
        for action in actions {
            let mut sorted: Vec<&(usize, usize, String)> = action.edits.iter().collect();
            sorted.sort_by_key(|(start, end, _)| (*start, *end));
            let disjoint = sorted
                .windows(2)
                .all(|pair| pair[0].1 <= pair[1].0 && pair[0].0 <= pair[0].1);
            let changes: Option<Vec<Value>> = sorted
                .iter()
                .map(|(start, end, new_text)| {
                    span(text, &index, (*start, *end))
                        .map(|range| json!({ "range": range, "newText": cut(new_text) }))
                })
                .collect();
            let (Some(changes), true, false, true) = (
                changes,
                disjoint,
                action.title.trim().is_empty() || action.edits.is_empty(),
                out.len() < MAX_ENTRIES,
            ) else {
                dropped += 1;
                continue;
            };
            let mut entry = json!({
                "title": cut(&action.title),
                "isPreferred": action.preferred,
                "edit": { "changes": { document_uri: changes } },
            });
            if let Some(kind) = &action.kind {
                entry["kind"] = Value::from(cut(kind));
            }
            out.push(entry);
        }
        (Value::Array(out), dropped)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn an_action_is_kept_only_whole_with_edits_that_fit_the_text_and_each_other() {
            let text = "let x = 1;";
            let action = |title: &str, edits: Vec<(usize, usize, &str)>| Action {
                title: title.into(),
                kind: Some("quickfix".into()),
                preferred: true,
                edits: edits
                    .into_iter()
                    .map(|(s, e, t)| (s, e, t.to_owned()))
                    .collect(),
            };
            let (shown, dropped) = code_actions(
                text,
                "file:///a.rs",
                &[
                    action("rename", vec![(4, 5, "y"), (0, 3, "const")]),
                    action("outside", vec![(0, 99, "")]),
                    action("overlap", vec![(0, 5, ""), (4, 6, "")]),
                    action("nothing", vec![]),
                    action(" ", vec![(0, 1, "")]),
                ],
            );
            assert_eq!(dropped, 4);
            assert_eq!(shown.as_array().unwrap().len(), 1);
            assert_eq!(shown[0]["kind"], "quickfix");
            let edits = shown[0]["edit"]["changes"]["file:///a.rs"]
                .as_array()
                .unwrap();
            assert_eq!(edits[0]["newText"], "const");
            assert_eq!(edits[1]["range"]["start"]["character"], 4);
        }

        #[test]
        fn a_hover_carries_markdown_and_a_range_only_when_it_can_be_placed() {
            let text = "fn 中文() {}";
            let shown = hover(
                text,
                &Hover {
                    contents: "**f**".into(),
                    range: Some((3, 9)),
                },
            )
            .unwrap();
            assert_eq!(shown["contents"]["kind"], "markdown");
            assert_eq!(shown["range"]["start"]["character"], 3);
            // UTF-8 columns: the two characters are six bytes.
            assert_eq!(shown["range"]["end"]["character"], 9);

            // Inside a character: no range, but the text is still shown.
            let off = hover(
                text,
                &Hover {
                    contents: "x".into(),
                    range: Some((4, 9)),
                },
            )
            .unwrap();
            assert!(off.get("range").is_none());
            let blank = Hover {
                contents: "  ".into(),
                range: None,
            };
            assert!(hover(text, &blank).is_none());
        }

        #[test]
        fn relative_places_start_in_the_documents_folder() {
            let base = if cfg!(windows) {
                "C:/work/app/src"
            } else {
                "/work/app/src"
            };
            let (json, dropped) = locations(
                Path::new(base),
                &[Place {
                    path: "../lib.rs".into(),
                    line: 4,
                    column: 2,
                }],
            );
            assert_eq!(dropped, 0);
            assert_eq!(json[0]["range"]["start"]["line"], 4);
            assert!(json[0]["uri"].as_str().unwrap().starts_with("file://"));
        }

        #[test]
        fn an_outline_nests_children_and_drops_what_cannot_be_placed() {
            let text = "struct A {\n  x: u8,\n}\n";
            let symbol = |name: &str, range: (usize, usize), parent| Symbol {
                name: name.into(),
                detail: None,
                kind: 23,
                range,
                selection: range,
                parent,
            };
            let (tree, dropped) = symbols(
                text,
                &[
                    symbol("A", (0, text.len()), None),
                    symbol("x", (13, 18), Some(0)),
                    symbol("lost", (0, 9999), Some(0)),
                    // The parent was dropped, so the child goes too.
                    symbol("orphan", (13, 14), Some(2)),
                    // A parent that does not come first is not valid.
                    symbol("forward", (13, 14), Some(9)),
                ],
            );
            assert_eq!(dropped, 3);
            assert_eq!(tree.as_array().unwrap().len(), 1);
            assert_eq!(tree[0]["name"], "A");
            assert_eq!(tree[0]["children"][0]["name"], "x");
            assert_eq!(tree[0]["children"][0]["range"]["start"]["line"], 1);
        }

        #[test]
        fn a_signature_marks_the_active_parameter_and_needs_a_label() {
            let shown = signature(&Signature {
                label: "add(a, b)".into(),
                documentation: Some("sum".into()),
                parameters: vec![("a".into(), None), ("b".into(), Some("second".into()))],
                active: Some(1),
            })
            .unwrap();
            assert_eq!(shown["activeParameter"], 1);
            assert_eq!(
                shown["signatures"][0]["parameters"][1]["documentation"],
                "second"
            );
            let blank = Signature {
                label: " ".into(),
                documentation: None,
                parameters: vec![],
                active: None,
            };
            assert!(signature(&blank).is_none());
        }

        #[test]
        fn hints_keep_known_kinds_and_drop_misplaced_or_empty_ones() {
            let text = "ab\ncd";
            let hint = |offset, label: &str, kind| Hint {
                offset,
                label: label.into(),
                kind,
                padding_left: false,
                padding_right: true,
                tooltip: None,
            };
            let (shown, dropped) = hints(
                text,
                &[
                    hint(3, ": u8", Some(1)),
                    hint(99, "x", None),
                    hint(1, "", None),
                    hint(1, "n:", Some(7)),
                ],
            );
            assert_eq!(dropped, 2);
            assert_eq!(shown[0]["position"]["line"], 1);
            assert_eq!(shown[0]["kind"], 1);
            assert!(
                shown[1].get("kind").is_none(),
                "unknown kinds are not passed on"
            );
        }
    }
}
