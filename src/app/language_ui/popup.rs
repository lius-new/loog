//! The list popup's contents and behaviour: what is in it, how typing filters
//! it, which row is selected, and what choosing a row means.
use super::navigate::Place;
use crate::domain::document::DocumentStamp;
use crate::domain::identity::PaneId;
use crate::features::language_service::fuzzy::fuzzy;
use crate::features::language_service::reply::{CodeAction, Command, HierarchyItem};
use std::sync::Arc;

/// What a row stands for.
#[derive(Clone)]
pub enum Payload {
    /// Go there.
    Place(Place),
    PeekPlace(Place),
    Group(std::path::PathBuf),
    /// Run a code action of the server instance that offered it.
    Action {
        instance: u64,
        action: Arc<CodeAction>,
    },
    /// Run a server command.
    Command {
        instance: u64,
        command: Command,
    },
    Lens {
        instance: u64,
        lens: Arc<crate::features::language_service::reply::CodeLens>,
    },
    /// A call or type hierarchy item: show what it leads to.
    Hierarchy {
        instance: u64,
        item: Arc<HierarchyItem>,
        calls: bool,
    },
    /// A row that only informs (a disabled action, a heading).
    Nothing,
}

#[derive(Clone)]
pub struct Item {
    pub label: String,
    pub detail: String,
    pub badge: &'static str,
    pub payload: Payload,
    /// Shown dimmed; choosing it does nothing.
    pub faint: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Locations,
    References,
    DocumentSymbols,
    WorkspaceSymbols,
    CodeActions,
    Rename,
    Hierarchy,
    CodeLenses,
    Problems,
}

impl Kind {
    pub fn is_panel(self) -> bool {
        matches!(self, Self::References | Self::Hierarchy | Self::Problems)
    }
}

#[derive(Clone)]
pub struct Popup {
    pub kind: Kind,
    pub pane: PaneId,
    pub stamp: DocumentStamp,
    /// Where it opens: next to this offset of the text, or at the top centre.
    pub anchor: Option<usize>,
    pub title: Option<String>,
    /// The text typed into the popup, if it has a field.
    pub query: Option<String>,
    pub items: Arc<[Item]>,
    /// The rows that pass the filter: index into `items` and matched characters.
    pub shown: Vec<(usize, Vec<usize>)>,
    pub selected: usize,
    pub scroll: usize,
    pub empty: String,
    /// The caret offset the popup was opened from (what a rename is about).
    pub origin: usize,
    /// Whether typing narrows the rows here; a server that searches itself
    /// (workspace symbols) is trusted to have done so.
    pub local_filter: bool,
    pub panel_focused: bool,
    pub visible: usize,
    collapsed: std::collections::HashSet<std::path::PathBuf>,
}

/// Rows visible at once.
pub const VISIBLE: usize = 10;

impl Popup {
    pub fn new(
        kind: Kind,
        pane: PaneId,
        stamp: DocumentStamp,
        anchor: Option<usize>,
        items: Vec<Item>,
    ) -> Self {
        let mut popup = Self {
            kind,
            pane,
            stamp,
            anchor,
            title: None,
            query: None,
            items: items.into(),
            shown: Vec::new(),
            selected: 0,
            scroll: 0,
            empty: "No results".to_owned(),
            origin: 0,
            local_filter: true,
            panel_focused: true,
            visible: VISIBLE,
            collapsed: Default::default(),
        };
        popup.refilter();
        popup
    }

    pub fn with_origin(mut self, origin: usize) -> Self {
        self.origin = origin;
        self
    }

    /// A popup that is only a text field holding `text` (a rename box).
    pub fn with_text(mut self, text: String) -> Self {
        self.query = Some(text);
        self.local_filter = false;
        self.empty = "Type the new name, then press Enter".to_owned();
        self.refilter();
        self
    }

    pub fn with_field(mut self) -> Self {
        self.query = Some(String::new());
        self.refilter();
        self
    }

    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Replaces the items (a workspace search that has new results), keeping the
    /// query.
    pub fn set_items(&mut self, items: Vec<Item>) {
        self.items = items.into();
        self.refilter();
    }

    /// Replaces the rows with an answer the server already ranked.
    pub fn set_items_keeping_order(&mut self, items: Vec<Item>) {
        self.local_filter = false;
        self.items = items.into();
        self.shown = (0..self.items.len()).map(|at| (at, Vec::new())).collect();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn set_query(&mut self, query: String) {
        self.query = Some(query);
        self.refilter();
    }

    pub fn type_text(&mut self, text: &str) -> bool {
        if !self.panel_focused {
            return false;
        }
        let Some(query) = &mut self.query else {
            return false;
        };
        query.push_str(text);
        self.refilter();
        true
    }

    pub fn backspace(&mut self) -> bool {
        if !self.panel_focused {
            return false;
        }
        let Some(query) = &mut self.query else {
            return false;
        };
        query.pop();
        self.refilter();
        true
    }

    fn refilter(&mut self) {
        let typed = if self.local_filter {
            self.query.clone().unwrap_or_default()
        } else {
            String::new()
        };
        let mut shown: Vec<(usize, i32, Vec<usize>)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(at, item)| {
                if typed.is_empty() {
                    return Some((at, 0, Vec::new()));
                }
                let found = fuzzy(&typed, &item.label)?;
                Some((at, found.score, found.positions))
            })
            .collect();
        if self.kind.is_panel() {
            let mut group = None;
            let mut folded = false;
            let mut grouped = Vec::new();
            for (at, item) in self.items.iter().enumerate() {
                if let Payload::Group(path) = &item.payload {
                    group = Some(at);
                    folded = typed.is_empty() && self.collapsed.contains(path);
                    if typed.is_empty() {
                        grouped.push((at, 0, Vec::new()));
                    }
                    continue;
                }
                if !folded && let Some(row) = shown.iter().find(|(i, _, _)| *i == at) {
                    if let Some(g) = group
                        && grouped.last().is_none_or(|(i, _, _)| *i < g)
                    {
                        grouped.push((g, 0, Vec::new()));
                    }
                    grouped.push(row.clone());
                }
            }
            shown = grouped;
        }
        if !typed.is_empty() && !self.kind.is_panel() {
            shown.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        }
        self.shown = shown.into_iter().map(|(at, _, hits)| (at, hits)).collect();
        self.selected = 0;
        self.scroll = 0;
    }

    /// How the popup is drawn.
    pub fn view(&self) -> crate::ui::editor::popup::View {
        use crate::ui::editor::popup::{Row, View};
        View {
            panel: self.kind.is_panel(),
            rows: self
                .shown
                .iter()
                .filter_map(|(at, hits)| {
                    let item = self.items.get(*at)?;
                    Some(Row {
                        label: item.label.clone(),
                        detail: item.detail.clone(),
                        badge: if let Payload::Group(path) = &item.payload {
                            if self.collapsed.contains(path) {
                                "▸"
                            } else {
                                "▾"
                            }
                        } else {
                            item.badge
                        },
                        matches: hits.clone(),
                        faint: item.faint,
                    })
                })
                .collect::<Vec<_>>()
                .into(),
            selected: self.selected,
            scroll: self.scroll,
            visible: self.visible,
            query: self.query.clone(),
            title: self.title.clone(),
            empty: self.empty.clone(),
            anchor: self.anchor,
            docs: None,
        }
    }

    pub fn selected_item(&self) -> Option<&Item> {
        self.shown
            .get(self.selected)
            .and_then(|(at, _)| self.items.get(*at))
    }

    pub fn select(&mut self, index: usize) {
        if self.shown.is_empty() {
            return;
        }
        self.selected = index.min(self.shown.len() - 1);
        self.keep_selection_visible();
    }

    pub fn move_by(&mut self, delta: isize) {
        let len = self.shown.len() as isize;
        if len == 0 {
            return;
        }
        // Wraps, like a menu.
        let next = (self.selected as isize + delta).rem_euclid(len);
        self.select(next as usize);
    }

    /// Moves without wrapping, for paging.
    pub fn page(&mut self, delta: isize) {
        let len = self.shown.len() as isize;
        if len == 0 {
            return;
        }
        let next = (self.selected as isize + delta).clamp(0, len - 1);
        self.select(next as usize);
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let max = self.shown.len().saturating_sub(self.visible) as isize;
        self.scroll = (self.scroll as isize + delta).clamp(0, max) as usize;
    }

    fn keep_selection_visible(&mut self) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + self.visible {
            self.scroll = self.selected + 1 - self.visible;
        }
    }

    pub fn resize(&mut self, height: f32) {
        if self.kind.is_panel() {
            self.visible = (((height * 0.45).max(90.0).min((height - 8.0).max(0.0)) - 56.0)
                / crate::ui::editor::popup::ROW_H)
                .floor()
                .max(1.0) as usize;
            self.keep_selection_visible();
        }
    }
    pub fn toggle_group(&mut self, path: std::path::PathBuf) {
        if !self.collapsed.remove(&path) {
            self.collapsed.insert(path);
        }
        self.refilter();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str) -> Item {
        Item {
            label: label.into(),
            detail: String::new(),
            badge: "",
            payload: Payload::Nothing,
            faint: false,
        }
    }

    fn popup(labels: &[&str]) -> Popup {
        Popup::new(
            Kind::Locations,
            PaneId::new(1),
            DocumentStamp::new(
                crate::domain::identity::FileId::new(1),
                1,
                0,
                &loog_editor_core::text::TextBuffer::new(""),
            ),
            None,
            labels.iter().map(|l| item(l)).collect(),
        )
    }

    #[test]
    fn without_a_query_every_row_shows_in_order() {
        let p = popup(&["b", "a", "c"]);
        assert_eq!(p.shown.len(), 3);
        assert_eq!(p.selected_item().unwrap().label, "b");
    }

    #[test]
    fn typing_filters_and_ranks_and_resets_the_selection() {
        let mut p = popup(&["hashmap_len", "HashMap", "other"]).with_field();
        p.move_by(1);
        assert_eq!(p.selected, 1);
        assert!(p.type_text("hm"));
        assert_eq!(p.shown.len(), 2);
        assert_eq!(p.selected, 0, "the selection returns to the best match");
        assert_eq!(p.selected_item().unwrap().label, "HashMap");
        assert!(p.backspace() && p.backspace());
        assert_eq!(p.shown.len(), 3);
    }

    #[test]
    fn a_popup_without_a_field_ignores_typing() {
        let mut p = popup(&["a"]);
        assert!(!p.type_text("x"));
        assert!(!p.backspace());
    }

    #[test]
    fn the_selection_wraps_and_the_list_scrolls_with_it() {
        let labels: Vec<String> = (0..30).map(|i| format!("row {i}")).collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let mut p = popup(&refs);
        p.move_by(-1);
        assert_eq!(p.selected, 29);
        assert_eq!(p.scroll, 30 - VISIBLE);
        p.move_by(1);
        assert_eq!((p.selected, p.scroll), (0, 0));
        p.page(15);
        assert_eq!(p.selected, 15);
        assert!(p.scroll <= 15 && 15 < p.scroll + VISIBLE);
        p.page(1000);
        assert_eq!(p.selected, 29);
    }

    #[test]
    fn scrolling_with_the_wheel_is_bounded() {
        let labels: Vec<String> = (0..15).map(|i| i.to_string()).collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let mut p = popup(&refs);
        p.scroll_by(100);
        assert_eq!(p.scroll, 15 - VISIBLE);
        p.scroll_by(-100);
        assert_eq!(p.scroll, 0);
    }

    #[test]
    fn an_empty_popup_has_nothing_to_select() {
        let mut p = popup(&[]);
        p.move_by(1);
        p.page(1);
        assert!(p.selected_item().is_none());
    }

    #[test]
    fn reference_groups_fold_and_filter_without_taking_editor_text_after_navigation() {
        let path = std::path::PathBuf::from("a.rs");
        let mut p = popup(&[]);
        p.kind = Kind::References;
        p.query = Some(String::new());
        let mut a = item("alpha");
        a.payload = Payload::Place(Place::bytes(path.clone(), 0..1));
        let mut b = item("beta");
        b.payload = Payload::Place(Place::bytes(path.clone(), 2..3));
        let mut header = item("a.rs");
        header.payload = Payload::Group(path.clone());
        p.set_items(vec![header, a, b]);
        assert_eq!(p.shown.len(), 3);
        p.toggle_group(path);
        assert_eq!(p.shown.len(), 1);
        assert_eq!(p.view().rows[0].badge, "▸");
        assert!(p.type_text("alpha"));
        assert_eq!(p.shown.len(), 2);
        assert_eq!(p.view().rows[1].label, "alpha");
        p.panel_focused = false;
        assert!(!p.type_text("editor text"));
        assert!(!p.backspace());
    }
}
