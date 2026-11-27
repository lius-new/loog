//! How the right-hand drawer divides its height: the file tree takes what the
//! Search and Outline sections below it leave. A collapsed section is only its
//! bar; an open one takes its share of the drawer's height, which its divider
//! changes.
use super::state::{AppState, DrawerChrome};
use crate::ui::drawer::{DrawerSection, SECTION_H};
use lgui::prelude::UiRect;

/// Share of the drawer's height an open section takes until it is dragged.
pub(crate) const DEFAULT_SHARE: f32 = 1.0 / 3.0;
/// The file tree never shrinks below this while sections are open.
const MIN_TREE_H: f32 = 88.0;
/// The content under an open section's bar keeps room for a few rows.
const MIN_BODY_H: f32 = 66.0;

pub(crate) struct Stack {
    pub tree: UiRect,
    pub search: UiRect,
    pub outline: UiRect,
}

fn open(chrome: &DrawerChrome, section: DrawerSection) -> bool {
    match section {
        DrawerSection::Search => chrome.search_open,
        DrawerSection::Outline => chrome.outline_open,
    }
}

fn share(chrome: &DrawerChrome, section: DrawerSection) -> f32 {
    match section {
        DrawerSection::Search => chrome.search_share,
        DrawerSection::Outline => chrome.outline_share,
    }
}

/// Divides `drawer`, top to bottom, into the file tree, Search and Outline.
pub(crate) fn stack(drawer: UiRect, chrome: &DrawerChrome) -> Stack {
    let height = drawer.height();
    // What each open section takes below its bar.
    let mut body = [DrawerSection::Search, DrawerSection::Outline].map(|section| {
        if open(chrome, section) {
            (height * share(chrome, section) - SECTION_H).max(MIN_BODY_H)
        } else {
            0.0
        }
    });
    // A short drawer squeezes the open sections before the file tree.
    let room = (height - MIN_TREE_H - 2.0 * SECTION_H).max(0.0);
    let wanted = body[0] + body[1];
    if wanted > room {
        let scale = if wanted > 0.0 { room / wanted } else { 0.0 };
        body = body.map(|each| each * scale);
    }
    let outline_top = drawer.bottom - SECTION_H - body[1];
    let search_top = outline_top - SECTION_H - body[0];
    let across = |top: f32, bottom: f32| UiRect::new(drawer.left, top, drawer.right, bottom);
    Stack {
        tree: across(drawer.top, search_top),
        search: across(search_top, outline_top),
        outline: across(outline_top, drawer.bottom),
    }
}

// The file tree keeps its top in place when its height changes. A pending
// "bring the open file into sight" request would instead scroll it as soon as
// that row leaves the shrinking viewport, so folding or dragging drops it.

/// Folds an open section to its bar, or opens a folded one.
pub(crate) fn toggle(app: &mut AppState, section: DrawerSection) {
    app.explorer.ui.reveal = None;
    app.drawer.splitting = None;
    match section {
        DrawerSection::Search => {
            app.drawer.search_open = !app.drawer.search_open;
            if !app.drawer.search_open {
                // Its inputs leave the screen, so they must not keep the keyboard.
                app.search.blur();
            }
        }
        DrawerSection::Outline => app.drawer.outline_open = !app.drawer.outline_open,
    }
}

pub(crate) fn begin_drag(app: &mut AppState, section: DrawerSection) {
    app.explorer.ui.reveal = None;
    app.drawer.splitting = Some(section);
}

/// Follows the pointer while the divider above `section` is being dragged.
pub(crate) fn drag_to(app: &mut AppState, drawer: UiRect, section: DrawerSection, y: f32) {
    if app.drawer.splitting == Some(section) {
        drag(&mut app.drawer, drawer, section, y);
    }
}

pub(crate) fn end_drag(app: &mut AppState) {
    app.drawer.splitting = None;
}

/// Moves the divider above `section` to the pointer's vertical position `y`.
pub(crate) fn drag(chrome: &mut DrawerChrome, drawer: UiRect, section: DrawerSection, y: f32) {
    if drawer.height() <= 0.0 {
        return;
    }
    let below = match section {
        DrawerSection::Search => stack(drawer, chrome).outline.height(),
        DrawerSection::Outline => 0.0,
    };
    let share = ((drawer.bottom - below - y) / drawer.height()).clamp(0.1, 0.8);
    match section {
        DrawerSection::Search => chrome.search_share = share,
        DrawerSection::Outline => chrome.outline_share = share,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawer() -> UiRect {
        UiRect::new(0.0, 100.0, 300.0, 700.0)
    }

    #[test]
    fn sections_start_collapsed_so_the_tree_has_the_drawer_minus_two_bars() {
        let chrome = DrawerChrome::default();
        assert!(!chrome.search_open && !chrome.outline_open);
        let stack = stack(drawer(), &chrome);
        assert_eq!(stack.search.height(), SECTION_H);
        assert_eq!(stack.outline.height(), SECTION_H);
        assert_eq!(stack.tree.height(), 600.0 - 2.0 * SECTION_H);
        assert_eq!(stack.tree.bottom, stack.search.top);
        assert_eq!(stack.search.bottom, stack.outline.top);
        assert_eq!(stack.outline.bottom, drawer().bottom);
    }

    #[test]
    fn an_open_section_takes_a_third_and_both_open_leave_the_tree_a_third() {
        let mut chrome = DrawerChrome::default();
        chrome.outline_open = true;
        let one = stack(drawer(), &chrome);
        assert!((one.outline.height() - 200.0).abs() < 0.01);
        assert_eq!(one.search.height(), SECTION_H);
        chrome.search_open = true;
        let both = stack(drawer(), &chrome);
        assert!((both.search.height() - 200.0).abs() < 0.01);
        assert!((both.outline.height() - 200.0).abs() < 0.01);
        assert!((both.tree.height() - 200.0).abs() < 0.01);
    }

    #[test]
    fn a_short_drawer_squeezes_the_sections_before_the_tree() {
        let mut chrome = DrawerChrome::default();
        chrome.search_open = true;
        chrome.outline_open = true;
        chrome.search_share = 0.8;
        chrome.outline_share = 0.8;
        let stack = stack(drawer(), &chrome);
        assert!(stack.tree.height() >= MIN_TREE_H - 0.01, "{:?}", stack.tree);
        assert_eq!(stack.outline.bottom, drawer().bottom);
        assert!(stack.search.height() > SECTION_H);
    }

    #[test]
    fn dragging_a_divider_sets_that_sections_share_and_keeps_the_one_below() {
        let mut chrome = DrawerChrome::default();
        chrome.search_open = true;
        chrome.outline_open = true;
        let outline_before = stack(drawer(), &chrome).outline.height();
        // Pull the Search divider up by 50px.
        let top = stack(drawer(), &chrome).search.top;
        drag(&mut chrome, drawer(), DrawerSection::Search, top - 50.0);
        let after = stack(drawer(), &chrome);
        assert!(
            (after.search.height() - 250.0).abs() < 0.5,
            "{:?}",
            after.search
        );
        assert!((after.outline.height() - outline_before).abs() < 0.01);
        // The Outline divider only moves the Outline.
        let top = after.outline.top;
        drag(&mut chrome, drawer(), DrawerSection::Outline, top + 40.0);
        let moved = stack(drawer(), &chrome);
        assert!(
            (moved.outline.height() - 160.0).abs() < 0.5,
            "{:?}",
            moved.outline
        );
    }
}
