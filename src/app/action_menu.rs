//! The menus of the title bar and the status bar: the project menu (recent folders, open and
//! add a folder) and the manage menu (Settings, Extensions, Keyboard Shortcuts, Command
//! Palette). The view draws rows; this module says what the rows are and runs the choice.
use std::path::{Path, PathBuf};

use crate::app::context::AppContext;
use crate::app::state::AppState;
use crate::features::keymap::action::Action;
use crate::ui::action_menu::{Event, Item, Tile};
use crate::ui::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Project,
    Manage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Open {
    pub kind: Kind,
    /// The row chosen with the arrow keys; none until one is pressed, so the pointer alone
    /// never leaves a row marked.
    pub selected: Option<usize>,
    /// Whether the terminal had focus when the menu opened.
    pub restore_terminal: bool,
}

enum Choice {
    Recent(PathBuf),
    OpenFolder,
    AddFolder,
    Action(Action),
}

/// The folder's name as the title bar and the menu show it.
pub fn folder_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || crate::ui::display_path(path),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The letter a project's tile shows.
pub fn initial(name: &str) -> char {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map_or('·', |c| c.to_uppercase().next().unwrap_or(c))
}

/// A color for a project other than the open one, the same every time for the same name.
fn tile_color(name: &str) -> lgui::prelude::Color {
    let palette = theme::c();
    let colors = [
        palette.git.added,
        palette.warning,
        palette.git.staged,
        palette.git.modified,
    ];
    let sum: usize = name.bytes().map(usize::from).sum();
    colors[sum % colors.len()]
}

fn shortcut(app: &AppState, action: Action) -> Option<String> {
    crate::app::key_actions::shortcut_label(
        action,
        &[crate::app::keymap_context::workspace_context(app)],
    )
}

fn rows(state: &AppContext, app: &AppState, kind: Kind) -> Vec<(Item, Choice)> {
    match kind {
        Kind::Project => {
            let roots = app.explorer.model.roots();
            let mut rows: Vec<(Item, Choice)> = app
                .explorer
                .model
                .recents()
                .iter()
                .map(|path| {
                    let name = folder_name(path);
                    let current = roots.contains(path);
                    let mut item = Item::new(name.clone());
                    item.tile = Some(Tile {
                        initial: initial(&name),
                        color: if current {
                            theme::c().accent
                        } else {
                            tile_color(&name)
                        },
                    });
                    item.hint = current.then(|| "current".to_owned());
                    (item, Choice::Recent(path.clone()))
                })
                .collect();
            let mut open = Item::new("Open Folder…");
            open.icon = Some("folder-opened");
            open.rule_before = !rows.is_empty();
            rows.push((open, Choice::OpenFolder));
            let mut add = Item::new("Add Folder to Workspace…");
            add.icon = Some("folder-add");
            rows.push((add, Choice::AddFolder));
            rows
        }
        Kind::Manage => {
            let mut rows = Vec::new();
            let mut entry = |label: &str,
                             icon: &'static str,
                             action: Action,
                             hint: Option<(String, bool)>,
                             rule: bool| {
                let mut item = Item::new(label);
                item.icon = Some(icon);
                if let Some((text, warn)) = hint {
                    item.hint = Some(text);
                    item.warn = warn;
                } else {
                    item.hint = shortcut(app, action);
                }
                item.rule_before = rule;
                rows.push((item, Choice::Action(action)));
            };
            entry("Settings", "settings", Action::OpenSettings, None, false);
            let attention = crate::app::extensions_view::needs_attention(state);
            entry(
                "Extensions",
                "extensions",
                Action::OpenExtensions,
                attention.then(|| ("Needs attention".to_owned(), true)),
                false,
            );
            entry(
                "Keyboard Shortcuts",
                "menu-keyboard",
                Action::OpenKeymap,
                None,
                false,
            );
            entry(
                "Command Palette",
                "menu-command",
                Action::OpenCommandPalette,
                None,
                true,
            );
            rows
        }
    }
}

/// The rows of the open menu, for drawing.
pub fn items(state: &AppContext, app: &AppState, kind: Kind) -> Vec<Item> {
    rows(state, app, kind)
        .into_iter()
        .map(|(item, _)| item)
        .collect()
}

/// Open `kind` from its control, or close it when that one is already open.
pub fn toggle(app: &mut AppState, kind: Kind) {
    if app.chrome.menu.is_some_and(|open| open.kind == kind) {
        app.chrome.menu = None;
        return;
    }
    let restore_terminal = app.terminal.ui.terminal_focused;
    app.close_overlays_for_palette();
    app.chrome.menu = Some(Open {
        kind,
        selected: None,
        restore_terminal,
    });
}

pub fn run(state: &AppContext, event: Event) {
    match event {
        Event::Close => state.update(|app| app.chrome.menu = None),
        Event::Select(index) => choose(state, index),
        Event::Accept => {
            if let Some(open) = state.get().chrome.menu
                && let Some(selected) = open.selected
            {
                choose(state, selected);
            }
        }
        Event::Move(down) => {
            let app = state.get();
            let Some(open) = app.chrome.menu else {
                return;
            };
            let count = rows(state, &app, open.kind).len();
            state.update(move |app| {
                if let Some(open) = app.chrome.menu.as_mut() {
                    open.selected = step(open.selected, count, down);
                }
            });
        }
    }
}

/// The row after an arrow key; the first arrow key picks the first row going down and the last
/// going up.
fn step(selected: Option<usize>, count: usize, down: bool) -> Option<usize> {
    if count == 0 {
        return None;
    }
    Some(match (selected, down) {
        (None, true) => 0,
        (None, false) => count - 1,
        (Some(selected), true) => (selected + 1) % count,
        (Some(selected), false) => (selected + count - 1) % count,
    })
}

fn choose(state: &AppContext, index: usize) {
    let app = state.get();
    let Some(open) = app.chrome.menu else {
        return;
    };
    let Some((_, choice)) = rows(state, &app, open.kind).into_iter().nth(index) else {
        return;
    };
    state.update(|app| app.chrome.menu = None);
    match choice {
        Choice::Recent(path) => {
            crate::app::explorer::open_recent_folder(state, path);
        }
        Choice::OpenFolder => {
            crate::app::explorer::choose_folder(state);
        }
        Choice::AddFolder => {
            crate::app::explorer::choose_folder_to_add(state);
        }
        // The palette is opened through its own function, which gathers the extensions'
        // commands; applying the action to the state alone does nothing.
        Choice::Action(Action::OpenCommandPalette) => crate::app::palette::open(state),
        Choice::Action(action) => {
            state.update(move |app| app.apply(action));
            if action == Action::OpenExtensions {
                crate::app::extensions_view::opened(state);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tile_shows_the_first_letter_of_the_name_in_capitals() {
        assert_eq!(initial("loog"), 'L');
        assert_eq!(initial("_notes"), 'N');
        assert_eq!(initial("..."), '·');
    }

    #[test]
    fn a_project_keeps_its_color() {
        assert_eq!(tile_color("lgui"), tile_color("lgui"));
    }

    #[test]
    fn arrow_keys_wrap_around_the_rows() {
        assert_eq!(step(Some(0), 3, false), Some(2));
        assert_eq!(step(Some(2), 3, true), Some(0));
        assert_eq!(step(None, 3, true), Some(0));
        assert_eq!(step(None, 3, false), Some(2));
        assert_eq!(step(Some(0), 0, true), None);
    }
}
