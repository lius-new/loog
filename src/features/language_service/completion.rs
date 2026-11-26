//! The completion list the person sees: the servers' items merged, filtered by
//! what has been typed since the list was asked for, and turned into an edit
//! when one is accepted.
use super::fuzzy::fuzzy;
use super::reply::{CompletionItem, CompletionList, Span, TextEdit};
use std::ops::Range;

/// One item and where it came from.
#[derive(Clone, Debug)]
pub struct Entry {
    pub item: CompletionItem,
    /// The server instance that produced it, for `completionItem/resolve`.
    pub instance: u64,
    /// The server's place in the listing; lower is preferred.
    pub rank: usize,
    /// Whether the item was resolved already.
    pub resolved: bool,
}

/// What a request asked about, so the list can be kept while typing continues.
#[derive(Clone, Debug, Default)]
pub struct Completion {
    /// Where the word being completed starts, as a byte offset in the text the
    /// list was requested for.
    pub word_start: usize,
    /// The cursor when the list was requested.
    pub cursor: usize,
    pub entries: Vec<Entry>,
    /// Some server says typing more may bring other items: ask again then.
    pub incomplete: bool,
}

/// Where the identifier before `cursor` starts.
pub fn word_start(text: &str, cursor: usize) -> usize {
    let mut start = cursor.min(text.len());
    while start > 0 && !text.is_char_boundary(start) {
        start -= 1;
    }
    let before = &text[..start];
    let kept: usize = before
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .map(char::len_utf8)
        .sum();
    start - kept
}

/// Where the identifier at `cursor` ends.
pub fn word_end(text: &str, cursor: usize) -> usize {
    let mut at = cursor.min(text.len());
    while at < text.len() && !text.is_char_boundary(at) {
        at += 1;
    }
    at + text[at..]
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .map(char::len_utf8)
        .sum::<usize>()
}

impl Completion {
    /// Merges the lists of several servers. An item another server already offered
    /// (same label and kind) is dropped; the better-ranked server's stays.
    pub fn merge(
        word_start: usize,
        cursor: usize,
        sources: Vec<(u64, usize, CompletionList)>,
    ) -> Self {
        let mut sources = sources;
        sources.sort_by_key(|(instance, rank, _)| (*rank, *instance));
        let mut completion = Self {
            word_start,
            cursor,
            ..Self::default()
        };
        completion.incomplete = sources.iter().any(|(_, _, list)| list.incomplete);
        let mut seen = std::collections::HashSet::new();
        for (instance, rank, list) in sources {
            for item in list.items {
                if seen.insert((item.label.clone(), item.kind)) {
                    completion.entries.push(Entry {
                        item,
                        instance,
                        rank,
                        resolved: false,
                    });
                }
            }
        }
        completion
    }

    /// Adds a late server's items without reordering anything already shown: the
    /// caller re-filters, and ranking decides.
    pub fn add(&mut self, instance: u64, rank: usize, list: CompletionList) {
        self.incomplete |= list.incomplete;
        for item in list.items {
            if !self
                .entries
                .iter()
                .any(|e| e.item.label == item.label && e.item.kind == item.kind)
            {
                self.entries.push(Entry {
                    item,
                    instance,
                    rank,
                    resolved: false,
                });
            }
        }
    }
}

/// An entry that passed the filter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    /// Index into [`Completion::entries`].
    pub entry: usize,
    pub score: i32,
    /// Matched character positions in the item's label, for highlighting.
    pub positions: Vec<usize>,
}

/// The entries matching `typed`, best first: a preselected item leads among
/// equals, then by match, server order and the server's own sort text.
pub fn filter(completion: &Completion, typed: &str) -> Vec<Shown> {
    let mut shown: Vec<(Shown, (bool, i32, usize, String))> = completion
        .entries
        .iter()
        .enumerate()
        .filter_map(|(at, entry)| {
            let item = &entry.item;
            let matched = fuzzy(typed, item.filter())?;
            // Highlight positions are in the label; the filter text may differ.
            let positions = if item.filter() == item.label {
                matched.positions.clone()
            } else {
                fuzzy(typed, &item.label)
                    .map(|m| m.positions)
                    .unwrap_or_default()
            };
            let key = (
                !item.preselect,
                -matched.score,
                entry.rank,
                item.sort_text.clone().unwrap_or_else(|| item.label.clone()),
            );
            Some((
                Shown {
                    entry: at,
                    score: matched.score,
                    positions,
                },
                key,
            ))
        })
        .collect();
    // A preselected item leads only when it matches as well as the best one.
    let best = shown.iter().map(|(s, _)| s.score).max().unwrap_or(0);
    shown.sort_by(|(a, ka), (b, kb)| {
        let lead =
            |shown: &Shown, key: &(bool, i32, usize, String)| !key.0 && shown.score >= best - 8;
        (!lead(a, ka), &ka.1, ka.2, &ka.3).cmp(&(!lead(b, kb), &kb.1, kb.2, &kb.3))
    });
    shown.into_iter().map(|(shown, _)| shown).collect()
}

/// An accepted item, as edits to the document as it is now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Insertion {
    /// What the item replaces: the word typed so far, or the server's range.
    pub range: Range<usize>,
    /// The item's text, a snippet's source when `snippet` is set.
    pub text: String,
    pub snippet: bool,
    /// Other edits (an import, for example), in ascending order, non-overlapping
    /// with `range`.
    pub additional: Vec<(Range<usize>, String)>,
}

/// Moves a range that was computed for the text at `cursor` to the text now,
/// when `typed` bytes were inserted at the cursor since.
fn follow(range: Range<usize>, cursor: usize, typed: isize) -> Range<usize> {
    let shift = |at: usize| {
        if at >= cursor {
            (at as isize + typed).max(0) as usize
        } else {
            at
        }
    };
    shift(range.start)..shift(range.end)
}

/// The edit for accepting `entry`. `text` is the document now, `now` the cursor,
/// and `requested` the text the list was computed for (to read the server's
/// ranges). `replace` chooses a server's replace range over its insert range.
pub fn insertion(
    completion: &Completion,
    entry: &Entry,
    requested: &str,
    text: &str,
    now: usize,
    replace: bool,
) -> Insertion {
    let item = &entry.item;
    let typed = now as isize - completion.cursor as isize;
    let (range, new_text) = match &item.edit {
        Some(edit) => {
            let span: Span = if replace { edit.replace } else { edit.insert };
            let range = follow(span.bytes(requested), completion.cursor, typed);
            (range, edit.new_text.clone())
        }
        None => (
            // Without a range: the word typed so far.
            word_start(text, now).min(now)..now,
            item.plain_insert().to_owned(),
        ),
    };
    let range =
        range.start.min(text.len())..range.end.min(text.len()).max(range.start.min(text.len()));
    let mut additional: Vec<(Range<usize>, String)> = item
        .additional_edits
        .iter()
        .map(|TextEdit { span, new_text }| {
            (
                follow(span.bytes(requested), completion.cursor, typed),
                new_text.clone(),
            )
        })
        .filter(|(r, _)| r.end <= range.start || r.start >= range.end)
        .collect();
    additional.sort_by_key(|(r, _)| (r.start, r.end));
    Insertion {
        range,
        text: new_text,
        snippet: item.snippet,
        additional,
    }
}

/// The characters that accept an item without Enter: the item's own, or none.
pub fn commits(entry: &Entry, typed: char) -> bool {
    entry
        .item
        .commit_characters
        .iter()
        .any(|c| c.chars().eq([typed]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::language_service::capabilities::PositionEncoding::Utf16;
    use crate::features::language_service::reply::{self, CompletionEdit};
    use serde_json::json;

    fn list(items: serde_json::Value, incomplete: bool) -> CompletionList {
        let mut list = reply::completion(&items, Utf16);
        list.incomplete = incomplete;
        list
    }

    fn labels(completion: &Completion, typed: &str) -> Vec<String> {
        filter(completion, typed)
            .iter()
            .map(|s| completion.entries[s.entry].item.label.clone())
            .collect()
    }

    #[test]
    fn the_word_before_the_cursor_is_the_identifier_under_it() {
        assert_eq!(word_start("let foo_bar", 11), 4);
        assert_eq!(word_start("a.bc", 4), 2);
        assert_eq!(word_start("x ", 2), 2);
        assert_eq!(
            word_start("变量名 变量", "变量名 变量".len()),
            "变量名 ".len()
        );
        assert_eq!(word_start("abc", 99), 0);
    }

    #[test]
    fn servers_are_merged_best_first_and_duplicates_dropped() {
        let a = list(
            json!([{ "label": "foo", "kind": 3 }, { "label": "bar" }]),
            false,
        );
        let b = list(
            json!([{ "label": "foo", "kind": 3 }, { "label": "baz" }]),
            true,
        );
        // The second server has the better rank even though it was handed over last.
        let merged = Completion::merge(0, 0, vec![(2, 5, a), (1, 0, b)]);
        let names: Vec<_> = merged
            .entries
            .iter()
            .map(|e| (&*e.item.label, e.instance))
            .collect();
        assert_eq!(names, [("foo", 1), ("baz", 1), ("bar", 2)]);
        assert!(merged.incomplete);
    }

    #[test]
    fn a_late_server_adds_items_without_duplicates() {
        let mut merged =
            Completion::merge(0, 0, vec![(1, 0, list(json!([{ "label": "a" }]), false))]);
        merged.add(
            2,
            3,
            list(json!([{ "label": "a" }, { "label": "b" }]), false),
        );
        assert_eq!(merged.entries.len(), 2);
        assert_eq!(merged.entries[1].instance, 2);
    }

    #[test]
    fn filtering_ranks_matches_then_servers_then_sort_text() {
        let merged = Completion::merge(
            0,
            0,
            vec![(
                1,
                0,
                list(
                    json!([
                        { "label": "zeta", "sortText": "1" },
                        { "label": "alpha", "sortText": "2" },
                        { "label": "hashmap_len" },
                        { "label": "HashMap" },
                    ]),
                    false,
                ),
            )],
        );
        // No input: sort text decides.
        assert_eq!(labels(&merged, "")[..2], ["zeta", "alpha"]);
        // Typing: better matches first, and non-matches vanish.
        assert_eq!(labels(&merged, "hm")[0], "HashMap");
        assert_eq!(labels(&merged, "zz"), Vec::<String>::new());
    }

    #[test]
    fn a_preselected_item_leads_among_close_matches() {
        let merged = Completion::merge(
            0,
            0,
            vec![(
                1,
                0,
                list(
                    json!([{ "label": "fooa" }, { "label": "foob", "preselect": true }]),
                    false,
                ),
            )],
        );
        assert_eq!(labels(&merged, "foo")[0], "foob");
        // Not when another item matches clearly better.
        let merged = Completion::merge(
            0,
            0,
            vec![(
                1,
                0,
                list(
                    json!([{ "label": "foo" }, { "label": "xfxoxo", "preselect": true }]),
                    false,
                ),
            )],
        );
        assert_eq!(labels(&merged, "foo")[0], "foo");
    }

    #[test]
    fn filter_text_decides_the_match_and_the_label_the_highlight() {
        let merged = Completion::merge(
            0,
            0,
            vec![(
                1,
                0,
                list(json!([{ "label": "Vec<T>", "filterText": "vec" }]), false),
            )],
        );
        let shown = filter(&merged, "vec");
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].positions, [0, 1, 2]);
    }

    fn entry(json: serde_json::Value) -> Entry {
        Entry {
            item: reply::completion_item(&json, Utf16).unwrap(),
            instance: 1,
            rank: 0,
            resolved: false,
        }
    }

    #[test]
    fn without_a_range_the_typed_word_is_replaced() {
        let requested = "let fo";
        let completion = Completion {
            word_start: 4,
            cursor: 6,
            ..Completion::default()
        };
        let e = entry(json!({ "label": "foo", "insertText": "foo()" }));
        let at = insertion(&completion, &e, requested, "let fo", 6, false);
        assert_eq!((at.range.clone(), at.text.as_str()), (4..6, "foo()"));
        // Typing continued: the word grew by one character since.
        let at = insertion(&completion, &e, requested, "let foo", 7, false);
        assert_eq!(at.range, 4..7);
    }

    #[test]
    fn a_servers_range_follows_what_was_typed_after_the_request() {
        let requested = "x.fo";
        let completion = Completion {
            word_start: 2,
            cursor: 4,
            ..Completion::default()
        };
        let e = entry(json!({ "label": "foo", "textEdit": { "newText": "foo",
            "range": { "start": { "line": 0, "character": 2 }, "end": { "line": 0, "character": 4 } } } }));
        let at = insertion(&completion, &e, requested, "x.foob", 5, false);
        assert_eq!(at.range, 2..5, "the end moved with the cursor");
        assert_eq!(at.text, "foo");
    }

    #[test]
    fn insert_and_replace_ranges_differ_by_the_setting() {
        let requested = "x.fobar";
        let completion = Completion {
            word_start: 2,
            cursor: 4,
            ..Completion::default()
        };
        let e = entry(json!({ "label": "foo", "textEdit": { "newText": "foo",
            "insert": { "start": { "line": 0, "character": 2 }, "end": { "line": 0, "character": 4 } },
            "replace": { "start": { "line": 0, "character": 2 }, "end": { "line": 0, "character": 7 } } } }));
        assert_eq!(
            insertion(&completion, &e, requested, requested, 4, false).range,
            2..4
        );
        assert_eq!(
            insertion(&completion, &e, requested, requested, 4, true).range,
            2..7
        );
        let _ = CompletionEdit {
            insert: e.item.edit.as_ref().unwrap().insert,
            replace: e.item.edit.as_ref().unwrap().replace,
            new_text: String::new(),
        };
    }

    #[test]
    fn additional_edits_stay_where_they_were_and_never_overlap_the_main_edit() {
        let requested = "use a;\nfn f() { Ha }";
        let cursor = requested.find("Ha").unwrap() + 2;
        let completion = Completion {
            word_start: cursor - 2,
            cursor,
            ..Completion::default()
        };
        let e = entry(json!({ "label": "HashMap", "additionalTextEdits": [
            { "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
              "newText": "use std::collections::HashMap;\n" },
            { "range": { "start": { "line": 1, "character": 10 }, "end": { "line": 1, "character": 12 } },
              "newText": "overlaps" } ] }));
        let at = insertion(&completion, &e, requested, requested, cursor, false);
        assert_eq!(at.additional.len(), 1, "the overlapping one is dropped");
        assert_eq!(at.additional[0].0, 0..0);
    }

    #[test]
    fn commit_characters_accept_only_the_items_own() {
        let e = entry(json!({ "label": "x", "commitCharacters": [".", "("] }));
        assert!(commits(&e, '.'));
        assert!(!commits(&e, ';'));
        assert!(!commits(&entry(json!({ "label": "y" })), '.'));
    }
}
