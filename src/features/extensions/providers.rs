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
    pub documentation_url: Option<String>,
}

// The component host admits whole answers under its shared byte/queue budget.
// Consumers validate positions without silently truncating accepted findings.

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

/// Convert `reported` for `text`. `fallback_source` names the provider when the
/// guest gave none, so every finding shows where it came from.
pub fn convert(text: &str, reported: &[Reported], fallback_source: &str) -> Converted {
    let mut out = Converted::default();
    for item in reported {
        let valid = item.start <= item.end
            && item.end <= text.len()
            && text.is_char_boundary(item.start)
            && text.is_char_boundary(item.end);
        if !valid {
            out.dropped += 1;
            continue;
        }
        let (mut start, mut end) = (item.start, item.end);
        if start == end {
            if let Some(next) = text[end..]
                .chars()
                .next()
                .filter(|c| !matches!(c, '\n' | '\r'))
            {
                end += next.len_utf8();
            } else if let Some(previous) = text[..start]
                .chars()
                .next_back()
                .filter(|c| !matches!(c, '\n' | '\r'))
            {
                start -= previous.len_utf8();
            }
        }
        out.diagnostics.push(Diagnostic {
            range: Range { start, end },
            severity: severity(item.severity),
            message: item.message.clone(),
            source: Some(
                item.source
                    .clone()
                    .unwrap_or_else(|| fallback_source.to_owned()),
            ),
            code: item.code.clone(),
            documentation_url: item.documentation_url.clone(),
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
            documentation_url: None,
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
    fn empty_findings_mark_whole_characters_and_keep_documentation_links() {
        let mut finding = item(0, 0);
        finding.documentation_url = Some("https://example.com/finding".into());
        let out = convert("😀\r\n", &[finding, item(4, 4), item(6, 6)], "p");
        assert_eq!(out.diagnostics[0].range, 0..4);
        assert_eq!(out.diagnostics[1].range, 0..4);
        assert_eq!(out.diagnostics[2].range, 6..6);
        assert_eq!(
            out.diagnostics[0].documentation_url.as_deref(),
            Some("https://example.com/finding")
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
    fn unknown_severities_become_errors_and_admitted_messages_remain_complete() {
        let mut long = item(0, 1);
        long.severity = 9;
        long.message = "中".repeat(1000);
        let out = convert("a", &[long], "p");
        assert_eq!(out.diagnostics[0].severity, Severity::Error);
        assert_eq!(out.diagnostics[0].message, "中".repeat(1000));
        assert!(out.diagnostics[0].message.chars().all(|c| c == '中'));
    }

    #[test]
    fn admitted_findings_are_not_cut_at_the_old_entry_threshold() {
        let many: Vec<_> = (0..1005).map(|i| item(i % 3, i % 3 + 1)).collect();
        let out = convert("abcd", &many, "p");
        assert_eq!(out.diagnostics.len(), 1005);
        assert_eq!(out.dropped, 0);
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
    pub label_detail: Option<String>,
    pub label_description: Option<String>,
    pub deprecated: Option<bool>,
    pub preselect: Option<bool>,
    pub insert_text_mode: Option<u8>,
    pub detail: Option<String>,
    pub documentation: Option<String>,
    pub kind: Option<u32>,
    pub insert_text: Option<String>,
    pub text_edit: Option<(Range<usize>, String)>,
    pub additional_text_edits: Vec<(Range<usize>, String)>,
    pub insert_text_format: Option<u8>,
    pub commit_characters: Vec<String>,
    pub command: Option<(String, String)>,
    pub sort_text: Option<String>,
    pub filter_text: Option<String>,
    /// Opaque to the host; returned unchanged when the item is resolved.
    pub data: Option<String>,
}

/// The items as the JSON a completion list is read from. Each carries
/// `x-loog` with its origin, so a later resolve goes back to the extension and
/// provider that offered it and nowhere else. The host import bounds the whole
/// answer; conversion preserves accepted strings and candidates in full.
pub fn completion_json(
    extension: &str,
    provider: &str,
    text: &str,
    items: &[Offered],
) -> (Vec<Value>, usize) {
    use crate::features::providers::position::LineIndex;
    let index = LineIndex::new(text);
    completion_json_indexed(extension, provider, text, items, &index)
}

pub fn completion_json_indexed(
    extension: &str,
    provider: &str,
    text: &str,
    items: &[Offered],
    index: &crate::features::providers::position::LineIndex<'_>,
) -> (Vec<Value>, usize) {
    use crate::features::providers::PositionEncoding;
    let edit_json = |(range, replacement): &(Range<usize>, String)| {
        let valid = range.start <= range.end
            && range.end <= text.len()
            && text.is_char_boundary(range.start)
            && text.is_char_boundary(range.end);
        if !valid {
            return None;
        }
        let start = index.position(range.start, PositionEncoding::Utf8);
        let end = index.position(range.end, PositionEncoding::Utf8);
        Some(serde_json::json!({"range": {
            "start": {"line": start.line, "character": start.character},
            "end": {"line": end.line, "character": end.character}
        }, "newText": replacement}))
    };
    let mut out = Vec::new();
    let mut dropped = 0;
    for item in items {
        if item.label.trim().is_empty() {
            dropped += 1;
            continue;
        }
        let mut json = serde_json::json!({
            "label": item.label,
            "x-loog": { "extension": extension, "provider": provider, "data": item.data,
                "text-edit": item.text_edit.as_ref().map(|(r, t)| serde_json::json!({"start": r.start, "end": r.end, "text": t})),
                "additional-text-edits": item.additional_text_edits.iter().map(|(r,t)| serde_json::json!({"start":r.start,"end":r.end,"text":t})).collect::<Vec<_>>() },
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
                object.insert(key.to_owned(), Value::from(value));
            }
        }
        if let Some(kind) = item.kind {
            object.insert("kind".to_owned(), Value::from(kind));
        }
        if item.label_detail.is_some() || item.label_description.is_some() {
            object.insert("labelDetails".into(), serde_json::json!({"detail":item.label_detail,"description":item.label_description}));
        }
        if let Some(value) = item.deprecated {
            object.insert("deprecated".into(), value.into());
        }
        if let Some(value) = item.preselect {
            object.insert("preselect".into(), value.into());
        }
        if let Some(value) = item.insert_text_mode.filter(|value| matches!(value, 1 | 2)) {
            object.insert("insertTextMode".into(), value.into());
        }
        let primary = item.text_edit.as_ref().map(&edit_json);
        let additional = item
            .additional_text_edits
            .iter()
            .map(&edit_json)
            .collect::<Option<Vec<_>>>();
        let valid_format = matches!(item.insert_text_format, None | Some(1 | 2));
        let command = item.command.as_ref().map(|(name, args)| {
            (args.len() <= 64 * 1024 && !name.is_empty()).then(|| serde_json::from_str::<Value>(args).ok()).flatten()
                .filter(Value::is_array)
                .map(|args| serde_json::json!({"title": item.label, "command": name, "arguments": args}))
        });
        let mut ranges = item
            .text_edit
            .iter()
            .chain(item.additional_text_edits.iter())
            .map(|(range, _)| range.clone())
            .collect::<Vec<_>>();
        ranges.sort_by_key(|range| (range.start, range.end));
        let overlap = ranges.windows(2).any(|pair| {
            pair[0].end > pair[1].start
                || pair[0].is_empty() && pair[1].is_empty() && pair[0].start == pair[1].start
        });
        if primary.as_ref().is_some_and(Option::is_none)
            || additional.is_none()
            || !valid_format
            || overlap
            || command.as_ref().is_some_and(Option::is_none)
        {
            dropped += 1;
            continue;
        }
        if let Some(Some(edit)) = primary {
            object.insert("textEdit".into(), edit);
        }
        object.insert(
            "additionalTextEdits".into(),
            Value::Array(additional.unwrap()),
        );
        if let Some(format) = item.insert_text_format {
            object.insert("insertTextFormat".into(), format.into());
        }
        object.insert(
            "commitCharacters".into(),
            Value::Array(
                item.commit_characters
                    .iter()
                    .filter(|s| s.chars().count() == 1)
                    .map(|s| Value::String(s.clone()))
                    .collect(),
            ),
        );
        if let Some(Some(command)) = command {
            object.insert("command".into(), command);
        }
        out.push(json);
    }
    (out, dropped)
}

#[cfg(test)]
mod completion_tests {
    use super::*;

    #[test]
    fn edit_ranges_are_utf8_and_malformed_items_are_rejected_atomically() {
        let item = Offered {
            label: "hello".into(),
            text_edit: Some((3..5, "hello(${1:x})$0".into())),
            additional_text_edits: vec![(0..0, "import x;\n".into())],
            insert_text_format: Some(2),
            commit_characters: vec!["(".into()],
            command: Some(("accepted".into(), "[42]".into())),
            ..Default::default()
        };
        let (items, dropped) = completion_json("a.b", "a.b.c", "中lo", std::slice::from_ref(&item));
        assert_eq!(dropped, 0);
        assert_eq!(items[0]["textEdit"]["range"]["start"]["character"], 3);
        assert_eq!(items[0]["insertTextFormat"], 2);
        assert_eq!(items[0]["commitCharacters"][0], "(");
        assert_eq!(items[0]["command"]["arguments"][0], 42);
        let mut invalid = item.clone();
        invalid.text_edit.as_mut().unwrap().0 = 1..5;
        assert_eq!(completion_json("a.b", "a.b.c", "中lo", &[invalid]).1, 1);
        let mut overlap = item;
        overlap.additional_text_edits.push((4..5, "bad".into()));
        assert_eq!(completion_json("a.b", "a.b.c", "中lo", &[overlap]).1, 1);
    }

    #[test]
    fn items_carry_their_origin_and_only_what_was_given() {
        let item = Offered {
            label: "loog_hello".into(),
            insert_text: Some("loog_hello()".into()),
            kind: Some(3),
            data: Some("hello".into()),
            ..Offered::default()
        };
        let (json, dropped) =
            completion_json("example.tools", "example.tools.complete", "", &[item]);
        assert_eq!(dropped, 0);
        assert_eq!(json[0]["insertText"], "loog_hello()");
        assert_eq!(json[0]["x-loog"]["extension"], "example.tools");
        assert_eq!(json[0]["x-loog"]["data"], "hello");
        assert!(json[0].get("detail").is_none());
    }

    #[test]
    fn empty_labels_are_rejected_without_losing_candidates_after_the_old_500_item_cutoff() {
        let mut items: Vec<Offered> = (0..1002)
            .map(|i| Offered {
                label: format!("item{i}"),
                ..Offered::default()
            })
            .collect();
        items.push(Offered {
            label: "  ".into(),
            ..Offered::default()
        });
        let (json, dropped) = completion_json("a.b", "a.b.c", "", &items);
        assert_eq!(json.len(), 1002);
        assert_eq!(dropped, 1);
        assert_eq!(json[1001]["label"], "item1001");
    }

    #[test]
    fn long_insertion_and_documentation_are_preserved_in_full() {
        let item = Offered {
            label: "x".into(),
            documentation: Some("中".repeat(3000)),
            insert_text: Some("x".repeat(6000)),
            ..Offered::default()
        };
        let (json, _) = completion_json("a.b", "a.b.c", "", &[item]);
        let docs = json[0]["documentation"].as_str().unwrap();
        assert_eq!(docs, "中".repeat(3000));
        assert_eq!(json[0]["insertText"], "x".repeat(6000));
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

    // Admission budgets live at the host boundary; accepted text remains intact.

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
        hover_indexed(text, hover, &LineIndex::new(text))
    }
    pub fn hover_indexed(text: &str, hover: &Hover, index: &LineIndex<'_>) -> Option<Value> {
        if hover.contents.trim().is_empty() {
            return None;
        }
        let mut out = json!({ "contents": { "kind": "markdown", "value": hover.contents } });
        if let Some(range) = hover.range.and_then(|range| span(text, index, range)) {
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
            let Some(uri) = uri::file_uri(&full) else {
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
        symbols_with_deprecated(text, symbols, &[])
    }

    pub fn symbols_with_deprecated(
        text: &str,
        symbols: &[Symbol],
        deprecated: &[bool],
    ) -> (Value, usize) {
        let index = LineIndex::new(text);
        let mut kept: Vec<Option<Value>> = vec![None; symbols.len()];
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); symbols.len()];
        let mut roots = Vec::new();
        let mut depths = vec![0usize; symbols.len()];
        let mut dropped = 0;
        for (at, symbol) in symbols.iter().enumerate() {
            let parent_ok = match symbol.parent {
                None => true,
                Some(parent) => parent < at && kept[parent].is_some() && depths[parent] < 128,
            };
            let ranges = span(text, &index, symbol.range).zip(span(text, &index, symbol.selection));
            let (true, Some((range, selection)), false) =
                (parent_ok, ranges, symbol.name.trim().is_empty())
            else {
                dropped += 1;
                continue;
            };
            let mut entry = json!({
                "name": symbol.name,
                "kind": symbol.kind,
                "range": range,
                "selectionRange": selection,
                "deprecated": deprecated.get(at).copied().unwrap_or(false),
            });
            if let Some(detail) = &symbol.detail {
                entry["detail"] = Value::from(detail.as_str());
            }
            kept[at] = Some(entry);
            depths[at] = symbol.parent.map_or(1, |parent| depths[parent] + 1);
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
            .map(|(label, documentation)| {
                let mut parameter = json!({ "label": label });
                if let Some(documentation) = documentation {
                    parameter["documentation"] = Value::from(documentation.as_str());
                }
                parameter
            })
            .collect();
        let mut entry = json!({ "label": signature.label, "parameters": parameters });
        if let Some(documentation) = &signature.documentation {
            entry["documentation"] = Value::from(documentation.as_str());
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
            if !placed(text, hint.offset) || hint.label.is_empty() {
                dropped += 1;
                continue;
            }
            let mut entry = json!({
                "position": position(&index, hint.offset),
                "label": hint.label,
                "paddingLeft": hint.padding_left,
                "paddingRight": hint.padding_right,
            });
            if let Some(kind) = hint.kind.filter(|kind| matches!(kind, 1 | 2)) {
                entry["kind"] = Value::from(kind);
            }
            if let Some(tooltip) = &hint.tooltip {
                entry["tooltip"] = Value::from(tooltip.as_str());
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
                        .map(|range| json!({ "range": range, "newText": new_text }))
                })
                .collect();
            let (Some(changes), true, false) = (
                changes,
                disjoint,
                action.title.trim().is_empty() || action.edits.is_empty(),
            ) else {
                dropped += 1;
                continue;
            };
            let mut entry = json!({
                "title": action.title,
                "isPreferred": action.preferred,
                "edit": { "changes": { document_uri: changes } },
            });
            if let Some(kind) = &action.kind {
                entry["kind"] = Value::from(kind.as_str());
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
