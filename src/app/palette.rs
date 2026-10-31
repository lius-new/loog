//! The command palette: opening it, moving through it and running a command.
//!
//! The palette lists every command by name (see `features::palette`), filtered
//! by what is typed into its field. Choosing one closes the palette and then
//! performs the command exactly as its key binding would.
use std::sync::{Arc, Mutex};

use crate::app::context::AppContext;
use crate::app::key_actions::{self, KeyEnv};
use crate::app::state::AppState;
use crate::features::keymap::bindings;
use crate::features::palette::{self, Entry, Hit, Target};
use crate::features::settings::theme_id::ThemeId;
use crate::ui::components::input::InputState;
use crate::ui::components::state_access::StateAccess;
use crate::ui::palette::{Binding, Command, Frame, Item, Row};

/// Rows (headers included) the list shows at once.
pub const VISIBLE_ROWS: usize = 12;
const RECENT_LIMIT: usize = 8;

#[derive(Clone, Default)]
pub struct PaletteState {
    pub visible: bool,
    pub input: InputState,
    /// Index into the hits for `query`.
    pub selected: usize,
    /// First list row (headers included) in view.
    pub top: usize,
    /// The text `selected` and `top` were chosen under; other text starts over.
    pub query: String,
    /// Whether the terminal had focus when the palette opened.
    pub restore_terminal: bool,
    /// Ids of the commands run most recently, newest first.
    pub recent: Vec<String>,
    pub entries: Arc<Vec<Entry>>,
    /// What the empty field asks for.
    pub placeholder: &'static str,
}

/// What the list draws, top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Line {
    Header(String),
    Hit(usize),
}

impl PaletteState {
    fn hits(&self) -> Vec<Hit> {
        palette::search(&self.entries, self.input.text(), &self.recent)
    }

    /// Selection and scroll as they apply to the text now in the field.
    fn view(&self) -> (usize, usize) {
        if self.query == self.input.text() {
            (self.selected, self.top)
        } else {
            (0, 0)
        }
    }

    fn lines(&self, hits: &[Hit]) -> Vec<Line> {
        let grouped = self.input.text().trim().is_empty();
        let mut lines = Vec::with_capacity(hits.len() * 2);
        let mut last: Option<String> = None;
        for (position, hit) in hits.iter().enumerate() {
            if grouped {
                let entry = &self.entries[hit.index];
                let label = if self.recent.contains(&entry.id) {
                    "Recent".to_owned()
                } else {
                    entry.category.clone()
                };
                if last.as_ref() != Some(&label) {
                    lines.push(Line::Header(label.clone()));
                    last = Some(label);
                }
            }
            lines.push(Line::Hit(position));
        }
        lines
    }

    /// Move the selection `delta` rows, wrapping, and keep it in view.
    pub fn move_selection(&mut self, delta: isize) {
        let hits = self.hits();
        let (selected, top) = self.view();
        self.query = self.input.text().to_owned();
        if hits.is_empty() {
            self.selected = 0;
            self.top = 0;
            return;
        }
        let count = hits.len() as isize;
        // One step wraps around the ends; a page stops at them.
        self.selected = if delta.abs() == 1 {
            (selected as isize + delta).rem_euclid(count) as usize
        } else {
            (selected as isize + delta).clamp(0, count - 1) as usize
        };
        let lines = self.lines(&hits);
        self.top = window(&lines, self.selected, top);
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.input.clear();
        self.selected = 0;
        self.top = 0;
        self.query.clear();
        self.entries = Arc::default();
    }
}

/// First line shown so the selected hit is in view, starting from `top`.
fn window(lines: &[Line], selected: usize, top: usize) -> usize {
    let at = lines
        .iter()
        .position(|line| *line == Line::Hit(selected))
        .unwrap_or(0);
    // Keep the group header above the first row of a group in view too.
    let at_top = if at > 0 && matches!(lines[at - 1], Line::Header(_)) {
        at - 1
    } else {
        at
    };
    let top = top.min(lines.len().saturating_sub(VISIBLE_ROWS));
    if at_top < top {
        at_top
    } else if at >= top + VISIBLE_ROWS {
        at + 1 - VISIBLE_ROWS
    } else {
        top
    }
}

fn read<R: Default>(state: &AppContext, f: impl FnOnce(&AppState) -> R) -> R {
    let mut out = R::default();
    state.try_update(|app| {
        out = f(app);
        false
    });
    out
}

pub fn is_open(env: &KeyEnv) -> bool {
    read(&env.state, |app| app.palette.visible)
}

/// Show the palette, with the catalog as it is right now.
pub fn open(state: &AppContext) {
    let extension_commands = state
        .services()
        .and_then(|runtime| runtime.extensions())
        .map(|host| host.commands())
        .unwrap_or_default();
    state.update(move |app| {
        app.close_overlays_for_palette();
        let restore_terminal = app.terminal.ui.terminal_focused;
        let palette = &mut app.palette;
        palette.close();
        palette.entries = Arc::new(palette::catalog(&extension_commands));
        palette.placeholder = "Search commands";
        palette.restore_terminal = restore_terminal;
        palette.visible = true;
    });
}

/// The built-in and extension themes that can be chosen now.
fn theme_entries(current: ThemeId) -> Vec<Entry> {
    ThemeId::all()
        .map(|id| {
            let theme = id.theme();
            Entry {
                id: format!("theme:{}", theme.id),
                title: theme.name.to_owned(),
                category: "Theme".to_owned(),
                description: if id == current {
                    "Current theme".to_owned()
                } else {
                    theme.id.to_owned()
                },
                target: Target::Theme(id),
            }
        })
        .collect()
}

/// Show the palette listing the themes instead of the commands, with the
/// current theme selected.
pub fn open_themes(state: &AppContext) {
    state.update(|app| {
        app.close_overlays_for_palette();
        let restore_terminal = app.terminal.ui.terminal_focused;
        let current = app.preferences.values().theme;
        let selected = ThemeId::all().position(|id| id == current).unwrap_or(0);
        let palette = &mut app.palette;
        palette.close();
        palette.entries = Arc::new(theme_entries(current));
        palette.placeholder = "Search themes";
        palette.restore_terminal = restore_terminal;
        palette.visible = true;
        palette.selected = selected;
        palette.move_selection(0);
    });
}

/// Replace the field's text with the selected command's name.
pub fn complete(state: &AppContext) -> bool {
    state.update(|app| {
        let hits = app.palette.hits();
        let (selected, _) = app.palette.view();
        if let Some(hit) = hits.get(selected) {
            let title = app.palette.entries[hit.index].title.clone();
            app.palette.input.set_text(title);
        }
    });
    true
}

/// Run the selected command.
pub fn confirm(env: &KeyEnv) -> bool {
    let position = read(&env.state, |app| app.palette.view().0);
    run(env, position)
}

/// Close the palette, then run the command at `position` in the hits.
pub fn run(env: &KeyEnv, position: usize) -> bool {
    let mut chosen: Option<(String, Target)> = None;
    env.state.update(|app| {
        let hits = app.palette.hits();
        if let Some(hit) = hits.get(position) {
            let entry = &app.palette.entries[hit.index];
            chosen = Some((entry.id.clone(), entry.target.clone()));
            // Themes are a list of their own, not commands to come back to.
            if !matches!(entry.target, Target::Theme(_)) {
                let recent = &mut app.palette.recent;
                recent.retain(|id| *id != entry.id);
                recent.insert(0, entry.id.clone());
                recent.truncate(RECENT_LIMIT);
            }
            app.palette.close();
        }
    });
    let Some((_, target)) = chosen else {
        return true;
    };
    match target {
        Target::Action(action) => {
            key_actions::perform(env, action);
        }
        Target::Theme(id) => env
            .state
            .update(move |app| app.preferences.values_mut().theme = id),
        Target::Extension(id) => match env
            .state
            .services()
            .and_then(|runtime| runtime.extensions())
        {
            Some(host) => host.run_command(&env.state, &id),
            None => env
                .state
                .update(|app| app.show_error("Extensions are not available")),
        },
    }
    true
}

/// `Ctrl+Shift+P` as the chips `Ctrl`, `Shift`, `P`.
fn key_chips(label: &str) -> Vec<String> {
    let mut chips = Vec::new();
    let mut parts = label.split('+').peekable();
    while let Some(part) = parts.next() {
        if part.is_empty() {
            // "Ctrl++" splits as "Ctrl", "", "": the key is the plus itself.
            if parts.peek().is_some_and(|next| next.is_empty()) {
                parts.next();
            }
            chips.push("+".to_owned());
        } else {
            chips.push(part.to_owned());
        }
    }
    chips
}

fn frame(app: &AppState) -> Frame {
    let palette = &app.palette;
    let hits = palette.hits();
    let (selected, top) = palette.view();
    if hits.is_empty() {
        return Frame {
            rows: Vec::new(),
            position: "0".to_owned(),
            empty: Some(palette.input.text().trim().to_owned()),
            placeholder: palette.placeholder,
        };
    }
    let lines = palette.lines(&hits);
    let top = window(&lines, selected, top);
    let keymap = bindings::current();
    let rows = lines
        .iter()
        .skip(top)
        .take(VISIBLE_ROWS)
        .map(|line| match line {
            Line::Header(label) => Row::Header(label.clone()),
            Line::Hit(position) => {
                let hit = &hits[*position];
                let entry = &palette.entries[hit.index];
                let keys = match &entry.target {
                    Target::Action(action) => keymap
                        .bindings_for_action(*action)
                        .next_back()
                        .map(|binding| key_chips(&binding.label()))
                        .unwrap_or_default(),
                    Target::Extension(_) | Target::Theme(_) => Vec::new(),
                };
                Row::Item(Item {
                    index: *position,
                    title: entry.title.clone(),
                    category: entry.category.clone(),
                    description: entry.description.clone(),
                    keys,
                    positions: hit.positions.clone(),
                    selected: *position == selected,
                    extension: matches!(entry.target, Target::Extension(_)),
                })
            }
        })
        .collect();
    Frame {
        rows,
        position: format!("{} / {}", selected + 1, hits.len()),
        empty: None,
        placeholder: palette.placeholder,
    }
}

/// The palette as the UI sees it. `env` runs a chosen command, so clicking a
/// row does what pressing Enter on it does.
pub fn binding(state: &AppContext, env: KeyEnv) -> Binding {
    let reader = state.clone();
    let writer = state.clone();
    let env = Mutex::new(env);
    Binding::new(
        StateAccess::project(
            state.model(),
            |app| &app.palette.input,
            |app| &mut app.palette.input,
        ),
        move || read(&reader, frame),
        move |command| match command {
            Command::Close => writer.update(|app| app.palette.close()),
            Command::Scroll(down) => {
                writer.update(|app| app.palette.move_selection(if down { 1 } else { -1 }))
            }
            Command::Hover(position) => {
                writer.try_update(|app| {
                    let palette = &mut app.palette;
                    if !palette.visible || palette.view().0 == position {
                        return false;
                    }
                    let (_, top) = palette.view();
                    palette.query = palette.input.text().to_owned();
                    palette.selected = position;
                    palette.top = top;
                    true
                });
            }
            Command::Run(position) => {
                let env = env.lock().unwrap_or_else(|error| error.into_inner()).clone();
                run(&env, position);
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened() -> PaletteState {
        let entries = palette::catalog(&[]);
        PaletteState {
            visible: true,
            entries: Arc::new(entries),
            ..Default::default()
        }
    }

    #[test]
    fn the_theme_list_offers_every_theme_and_marks_the_current_one() {
        let current = ThemeId::default();
        let entries = theme_entries(current);
        assert!(!entries.is_empty());
        assert!(entries
            .iter()
            .all(|entry| matches!(entry.target, Target::Theme(_)) && entry.category == "Theme"));
        let marked: Vec<_> = entries
            .iter()
            .filter(|entry| entry.description == "Current theme")
            .collect();
        assert_eq!(marked.len(), 1);
        assert_eq!(marked[0].target, Target::Theme(current));
    }

    #[test]
    fn the_command_list_offers_the_theme_picker() {
        let entries = palette::catalog(&[]);
        let select = entries
            .iter()
            .find(|entry| entry.id == "workspace::SelectTheme")
            .expect("Select Theme is listed");
        assert_eq!(select.title, "Select Theme");
    }

    #[test]
    fn key_chips_split_a_label_and_keep_a_plus_key() {
        assert_eq!(key_chips("Ctrl+Shift+P"), ["Ctrl", "Shift", "P"]);
        assert_eq!(key_chips("Ctrl++"), ["Ctrl", "+"]);
    }

    #[test]
    fn moving_wraps_and_keeps_the_selection_in_view() {
        let mut palette = opened();
        palette.move_selection(-1);
        let hits = palette.hits();
        assert_eq!(palette.selected, hits.len() - 1);
        let lines = palette.lines(&hits);
        assert!(palette.top + VISIBLE_ROWS > lines.len() - 1);
        palette.move_selection(1);
        assert_eq!(palette.selected, 0);
        assert_eq!(palette.top, 0);
    }

    #[test]
    fn a_new_query_starts_from_the_first_match() {
        let mut palette = opened();
        palette.move_selection(5);
        assert_eq!(palette.selected, 5);
        palette.input.set_text("save");
        assert_eq!(palette.view(), (0, 0));
    }

    #[test]
    fn empty_query_groups_rows_under_headers() {
        let palette = opened();
        let lines = palette.lines(&palette.hits());
        assert!(matches!(lines.first(), Some(Line::Header(_))));
    }

    #[test]
    fn typed_query_lists_matches_without_headers() {
        let mut palette = opened();
        palette.input.set_text("save");
        let lines = palette.lines(&palette.hits());
        assert!(lines.iter().all(|line| matches!(line, Line::Hit(_))));
        assert!(!lines.is_empty());
    }
}
