//! Command palette catalog and fuzzy search: what can be run, and how a
//! query ranks it. Pure data; the palette's state and drawing live elsewhere.
use serde_json::Value;

use crate::features::keymap::action::{ACTIONS, Action};
use crate::features::settings::theme_id::ThemeId;

/// What choosing an entry does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Action(Action),
    /// An extension command, by its `vendor.name.command` id.
    Extension(String),
    /// A color theme, listed while the palette is choosing one.
    Theme(ThemeId),
    /// An entry an extension published for the command list. The id is the
    /// extension's own and is handed back unchanged when the entry is chosen.
    ExtensionEntry {
        owner: String,
        generation: u64,
        id: String,
    },
    /// A choice of the session an extension has open in the palette.
    Choice {
        id: String,
        /// A short word shown at the row's end.
        trailing: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Stable across sessions: the action name or the extension command id.
    pub id: String,
    pub title: String,
    pub category: String,
    pub description: String,
    pub target: Target,
}

/// One entry that matched, with the title characters the query hit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub index: usize,
    pub positions: Vec<usize>,
}

/// Namespaces whose actions only act inside an open menu, dialog or field.
const CONTEXT_NAMESPACES: &[&str] = &["menu", "dialog", "search", "terminal", "vim", "extension"];

/// Editor actions that exist to be bound to a key, not to be picked by name.
const KEYSTROKE_ONLY: &[&str] = &[
    "editor::Backspace",
    "editor::Delete",
    "editor::DeleteToPreviousWordStart",
    "editor::DeleteToNextWordEnd",
    "editor::Newline",
    "editor::Tab",
    "editor::Backtab",
    "editor::Cancel",
    "editor::NextSnippetTabstop",
    "editor::PreviousSnippetTabstop",
    "editor::ShowSnippetChoices",
    "editor::NextSignature",
    "editor::PreviousSignature",
    "editor::OpenPeekTarget",
    "editor::ClosePeek",
    "workspace::CloseOverlay",
    "workspace::OpenCommandPalette",
];

/// Cursor movement and selection growth, except the ones worth searching for.
fn is_motion(name: &str) -> bool {
    const KEEP: &[&str] = &[
        "editor::MoveLineUp",
        "editor::MoveLineDown",
        "editor::SelectAll",
        "editor::SelectNextOccurrence",
        "editor::SelectLargerSyntaxNode",
        "editor::SelectSmallerSyntaxNode",
    ];
    (name.starts_with("editor::Move") || name.starts_with("editor::Select"))
        && !KEEP.contains(&name)
}

fn listed(name: &str) -> bool {
    let namespace = name.split("::").next().unwrap_or_default();
    !CONTEXT_NAMESPACES.contains(&namespace) && !KEYSTROKE_ONLY.contains(&name) && !is_motion(name)
}

/// `OpenSettings` as `Open Settings`.
pub fn humanize(name: &str) -> String {
    let mut title = String::new();
    let mut previous: Option<char> = None;
    for c in name.chars() {
        if c.is_uppercase() && previous.is_some_and(|p| p.is_lowercase() || p.is_ascii_digit()) {
            title.push(' ');
        }
        title.push(c);
        previous = Some(c);
    }
    title
}

fn category(namespace: &str) -> String {
    match namespace {
        "outline" | "project_symbols" => "Go To".to_owned(),
        "git" => "Git".to_owned(),
        other => {
            let mut chars = other.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    }
}

/// Every command that makes sense to pick by name, then the extensions'
/// commands (`id`, `title`).
pub fn catalog(extension_commands: &[(String, String)]) -> Vec<Entry> {
    catalog_with(extension_commands, Vec::new())
}

/// [`catalog`] followed by `published`, the entries extensions added to the list.
pub fn catalog_with(extension_commands: &[(String, String)], published: Vec<Entry>) -> Vec<Entry> {
    let mut entries = Vec::new();
    for meta in ACTIONS
        .iter()
        .filter(|meta| !meta.takes_argument && listed(meta.name))
    {
        let Ok(action) = Action::from_json(&Value::String(meta.name.to_owned())) else {
            continue;
        };
        let (namespace, name) = meta.name.split_once("::").unwrap_or(("", meta.name));
        entries.push(Entry {
            id: meta.name.to_owned(),
            title: humanize(name),
            category: category(namespace),
            description: meta.description.to_owned(),
            target: Target::Action(action),
        });
    }
    for (id, title) in extension_commands {
        entries.push(Entry {
            id: id.clone(),
            title: title.clone(),
            category: "Extension".to_owned(),
            description: id.clone(),
            target: Target::Extension(id.clone()),
        });
    }
    entries.extend(published);
    entries
}

/// Characters of `title` that `query` spells in order, case-insensitively,
/// with a score that favours runs and word starts.
fn fuzzy(query: &[char], title: &str) -> Option<(i32, Vec<usize>)> {
    let chars: Vec<char> = title.chars().collect();
    let mut positions = Vec::with_capacity(query.len());
    let mut score = 0;
    let mut next = 0;
    for &wanted in query {
        let found =
            (next..chars.len()).find(|&i| chars[i].to_lowercase().eq(wanted.to_lowercase()))?;
        if positions.last().is_some_and(|&last| last + 1 == found) {
            score += 4;
        }
        if found == 0 || !chars[found - 1].is_alphanumeric() {
            score += 3;
        }
        // The earlier the first hit and the tighter the spread, the better.
        score -= i32::try_from(found - next).unwrap_or(i32::MAX).min(8);
        positions.push(found);
        next = found + 1;
    }
    Some((
        score - i32::try_from(chars.len()).unwrap_or(0) / 8,
        positions,
    ))
}

/// The entries `query` selects, best first. An empty query lists everything,
/// recently run entries first (most recent first), then in catalog order.
pub fn search(entries: &[Entry], query: &str, recent: &[String]) -> Vec<Hit> {
    let query = query.trim();
    let recency = |entry: &Entry| recent.iter().position(|id| *id == entry.id);
    let mut scored: Vec<(i32, Option<usize>, Hit)> = Vec::new();
    if query.is_empty() {
        for (index, entry) in entries.iter().enumerate() {
            scored.push((
                0,
                recency(entry),
                Hit {
                    index,
                    positions: Vec::new(),
                },
            ));
        }
        // Recent first, then each category together in the order they first appear.
        let mut categories: Vec<&str> = Vec::new();
        for entry in entries {
            if !categories.contains(&entry.category.as_str()) {
                categories.push(&entry.category);
            }
        }
        let group = |index: usize| {
            categories
                .iter()
                .position(|category| *category == entries[index].category)
                .unwrap_or(usize::MAX)
        };
        scored.sort_by_key(|(_, recent, hit)| {
            (recent.unwrap_or(usize::MAX), group(hit.index), hit.index)
        });
        return scored.into_iter().map(|(_, _, hit)| hit).collect();
    }
    let wanted: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).collect();
    let terms: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    for (index, entry) in entries.iter().enumerate() {
        if let Some((score, positions)) = fuzzy(&wanted, &entry.title) {
            scored.push((score + 100, recency(entry), Hit { index, positions }));
        } else {
            // Not in the title: the description may still say what it does.
            let haystack = format!("{} {}", entry.category, entry.description).to_lowercase();
            if terms.iter().all(|term| haystack.contains(term)) {
                scored.push((
                    0,
                    recency(entry),
                    Hit {
                        index,
                        positions: Vec::new(),
                    },
                ));
            }
        }
    }
    scored.sort_by_key(|(score, recent, hit)| {
        (
            std::cmp::Reverse(*score),
            recent.unwrap_or(usize::MAX),
            hit.index,
        )
    });
    // Matches of one category sit together, the best match deciding where the
    // category stands, so the list can name each group once.
    let mut order: Vec<&str> = Vec::new();
    for (_, _, hit) in &scored {
        let category = entries[hit.index].category.as_str();
        if !order.contains(&category) {
            order.push(category);
        }
    }
    let rank = |hit: &Hit| {
        order
            .iter()
            .position(|category| *category == entries[hit.index].category)
            .unwrap_or(usize::MAX)
    };
    let mut hits: Vec<Hit> = scored.into_iter().map(|(_, _, hit)| hit).collect();
    hits.sort_by_key(rank);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, title: &str) -> Entry {
        Entry {
            id: id.to_owned(),
            title: title.to_owned(),
            category: "Test".to_owned(),
            description: String::new(),
            target: Target::Extension(id.to_owned()),
        }
    }

    #[test]
    fn humanize_splits_camel_case() {
        assert_eq!(humanize("OpenSettings"), "Open Settings");
        assert_eq!(humanize("GoToDefinition"), "Go To Definition");
        assert_eq!(humanize("Save"), "Save");
    }

    #[test]
    fn catalog_lists_commands_and_hides_keystroke_plumbing() {
        let entries = catalog(&[("acme.sample.run".into(), "Sample: Run".into())]);
        let has = |id: &str| entries.iter().any(|entry| entry.id == id);
        assert!(has("workspace::OpenSettings"));
        assert!(has("editor::Save"));
        assert!(has("editor::MoveLineUp"));
        assert!(has("acme.sample.run"));
        assert!(!has("menu::Confirm"));
        assert!(!has("editor::MoveLeft"));
        assert!(!has("editor::SelectLeft"));
        assert!(!has("workspace::OpenCommandPalette"));
        let settings = entries
            .iter()
            .find(|entry| entry.id == "workspace::OpenSettings")
            .unwrap();
        assert_eq!(settings.title, "Open Settings");
        assert_eq!(settings.category, "Workspace");
    }

    #[test]
    fn fuzzy_query_marks_the_matched_title_characters() {
        let entries = [entry("a", "Open Settings"), entry("b", "Toggle Terminal")];
        let hits = search(&entries, "ost", &[]);
        assert_eq!(hits[0].index, 0);
        assert_eq!(hits[0].positions, vec![0, 5, 7]);
    }

    #[test]
    fn word_starts_and_runs_rank_first() {
        let entries = [
            entry("a", "Close Active Tab"),
            entry("b", "Toggle Terminal"),
        ];
        let hits = search(&entries, "tt", &[]);
        assert_eq!(hits[0].index, 1);
    }

    #[test]
    fn empty_query_puts_recent_entries_first() {
        let entries = [entry("a", "A"), entry("b", "B"), entry("c", "C")];
        let hits = search(&entries, "", &["c".into(), "b".into()]);
        let order: Vec<usize> = hits.iter().map(|hit| hit.index).collect();
        assert_eq!(order, vec![2, 1, 0]);
    }

    #[test]
    fn matches_of_one_category_are_listed_together_best_group_first() {
        let in_category = |id: &str, title: &str, category: &str| Entry {
            category: category.to_owned(),
            ..entry(id, title)
        };
        let entries = [
            in_category("a", "Show Log", "Editor"),
            in_category("b", "Show Hosts", "Hosts"),
            in_category("c", "Show Signature", "Editor"),
            in_category("d", "Show", "Hosts"),
        ];
        let hits = search(&entries, "show", &[]);
        let categories: Vec<&str> = hits
            .iter()
            .map(|hit| entries[hit.index].category.as_str())
            .collect();
        assert_eq!(categories, ["Hosts", "Hosts", "Editor", "Editor"]);
    }

    #[test]
    fn unmatched_query_is_empty_and_description_still_matches() {
        let mut described = entry("a", "Zoom");
        described.description = "Make the text bigger".into();
        let entries = [described];
        assert!(search(&entries, "qqq", &[]).is_empty());
        assert_eq!(search(&entries, "bigger", &[]).len(), 1);
    }
}
