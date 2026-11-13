//! The command palette: opening it, moving through it and running a command.
//!
//! The palette lists every command by name (see `features::palette`), filtered
//! by what is typed into its field. Choosing one closes the palette and then
//! performs the command exactly as its key binding would.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use loog_extension_host::{PaletteActivity, PaletteCommand, PaletteMode, PaletteView};

use crate::app::context::AppContext;
use crate::app::key_actions::{self, KeyEnv};
use crate::app::state::AppState;
use crate::features::keymap::bindings;
use crate::features::palette::{self, Entry, Hit, Target};
use crate::features::settings::theme_id::ThemeId;
use crate::ui::components::input::InputState;
use crate::ui::components::state_access::StateAccess;
use crate::ui::palette::{Activity, Binding, Command, Frame, Item, Row};

/// Rows (headers included) the list shows at once.
pub const VISIBLE_ROWS: usize = 12;
const RECENT_LIMIT: usize = 8;

/// How an extension's session takes its answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionMode {
    /// Rows to filter and choose from.
    #[default]
    Choose,
    /// Free text.
    Text,
    /// Free text that is never shown.
    Secret,
}

/// A session an extension has open in the palette. The palette shows what the
/// extension last said and answers with the row or text the user gave; it
/// never interprets either.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub owner: String,
    pub generation: u64,
    pub id: u64,
    pub mode: SessionMode,
    pub tags: Vec<String>,
    pub activity: Activity,
}

/// The entries one extension added to the command list.
#[derive(Clone, Debug)]
pub struct Published {
    pub generation: u64,
    pub entries: Vec<loog_extension_host::PaletteEntry>,
}

/// What the user did that an extension is waiting to hear about.
#[derive(Clone, Debug)]
pub struct Answer {
    pub owner: String,
    pub generation: u64,
    pub event: loog_extension_host::PaletteEvent,
}

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
    pub placeholder: String,
    /// The session an extension has open, if the palette shows one.
    pub session: Option<Session>,
    /// What extensions added to the command list. Unlike `entries` it outlives one opening.
    pub published: Arc<BTreeMap<String, Published>>,
    /// Sessions that ended without an answer, to be told to their extension.
    pub ended: Vec<Session>,
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

    /// The group a match is listed under. Recently run commands form their own
    /// group, but only while nothing is typed and no extension session is open.
    fn group_of(&self, hit: &Hit) -> String {
        let entry = &self.entries[hit.index];
        let typed = !self.input.text().trim().is_empty();
        if !typed && self.session.is_none() && self.recent.contains(&entry.id) {
            "Recent".to_owned()
        } else {
            entry.category.clone()
        }
    }

    fn lines(&self, hits: &[Hit]) -> Vec<Line> {
        let groups: Vec<String> = hits.iter().map(|hit| self.group_of(hit)).collect();
        // A typed query is named by group too once its matches come from more than one.
        let typed = !self.input.text().trim().is_empty();
        let grouped = !typed || groups.iter().any(|group| Some(group) != groups.first());
        let mut lines = Vec::with_capacity(hits.len() * 2);
        let mut last: Option<&String> = None;
        for (position, group) in groups.iter().enumerate() {
            if grouped && !group.is_empty() && last != Some(group) {
                lines.push(Line::Header(group.clone()));
            }
            last = Some(group);
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
        self.input.masked = false;
        self.selected = 0;
        self.top = 0;
        self.query.clear();
        self.entries = Arc::default();
        // Closing is an answer in itself: the extension is told it was cancelled.
        if let Some(session) = self.session.take() {
            self.ended.push(session);
        }
    }

    /// Whether the open session takes text rather than a choice.
    fn takes_text(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.mode != SessionMode::Choose)
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

/// The highlighted theme is only a rendering override until it is confirmed.
pub(crate) fn display_theme(app: &AppState) -> ThemeId {
    let palette = &app.palette;
    if palette.visible
        && palette
            .entries
            .first()
            .is_some_and(|entry| matches!(entry.target, Target::Theme(_)))
        && let Some(hit) = palette.hits().get(palette.view().0)
        && let Target::Theme(id) = palette.entries[hit.index].target
    {
        return id;
    }
    app.preferences.values().theme
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
        let published = published_entries(&palette.published);
        palette.entries = Arc::new(palette::catalog_with(&extension_commands, published));
        palette.placeholder = "Search commands".to_owned();
        palette.restore_terminal = restore_terminal;
        palette.visible = true;
    });
}

/// The rows extensions added to the command list, in owner order.
fn published_entries(published: &BTreeMap<String, Published>) -> Vec<Entry> {
    published
        .iter()
        .flat_map(|(owner, published)| {
            published.entries.iter().map(move |entry| Entry {
                id: format!("{owner}:{}", entry.id),
                title: entry.title.clone(),
                category: entry.category.clone(),
                description: entry.detail.clone().unwrap_or_default(),
                target: Target::ExtensionEntry {
                    owner: owner.clone(),
                    generation: published.generation,
                    id: entry.id.clone(),
                },
            })
        })
        .collect()
}

/// The rows of a session's choices.
fn choice_entries(view: &PaletteView) -> Vec<Entry> {
    view.choices
        .iter()
        .map(|choice| Entry {
            id: choice.id.clone(),
            title: choice.title.clone(),
            category: choice.group.clone().unwrap_or_default(),
            description: choice.detail.clone().unwrap_or_default(),
            target: Target::Choice {
                id: choice.id.clone(),
                trailing: choice.trailing.clone(),
            },
        })
        .collect()
}

fn session_of(owner: &str, generation: u64, id: u64, view: &PaletteView) -> Session {
    Session {
        owner: owner.to_owned(),
        generation,
        id,
        mode: match view.mode {
            PaletteMode::Choose => SessionMode::Choose,
            PaletteMode::Text => SessionMode::Text,
            PaletteMode::Secret => SessionMode::Secret,
        },
        tags: view.tags.clone(),
        activity: match view.activity {
            PaletteActivity::Idle => Activity::Idle,
            PaletteActivity::Busy => Activity::Busy,
            PaletteActivity::Failed => Activity::Failed,
        },
    }
}

/// Apply what an extension asked of the palette, in order. Returns the file
/// dialogs it asked for, which only the caller can show.
pub fn apply_commands(
    app: &mut AppState,
    owner: &str,
    generation: u64,
    commands: Vec<PaletteCommand>,
) -> Vec<(u64, String)> {
    let mut dialogs = Vec::new();
    for command in commands {
        match command {
            PaletteCommand::Entries(entries) => {
                let published = Arc::make_mut(&mut app.palette.published);
                if entries.is_empty() {
                    published.remove(owner);
                } else {
                    published.insert(owner.to_owned(), Published { generation, entries });
                }
            }
            PaletteCommand::Open { session, view } => {
                app.close_overlays_for_palette();
                let restore_terminal = app.terminal.ui.terminal_focused;
                let palette = &mut app.palette;
                palette.restore_terminal = restore_terminal;
                palette.session = Some(session_of(owner, generation, session, &view));
                palette.input.masked = view.mode == PaletteMode::Secret;
                palette.entries = Arc::new(choice_entries(&view));
                palette.placeholder = view.placeholder;
                palette.visible = true;
            }
            PaletteCommand::Update { session, view } => {
                let palette = &mut app.palette;
                let current = palette.session.as_ref().is_some_and(|open| {
                    open.owner == owner && open.generation == generation && open.id == session
                });
                if !current {
                    continue;
                }
                let next = session_of(owner, generation, session, &view);
                // A new step, or a different way of answering, starts a fresh field.
                if palette
                    .session
                    .as_ref()
                    .is_some_and(|open| open.mode != next.mode || open.tags != next.tags)
                {
                    palette.input.clear();
                    palette.input.masked = next.mode == SessionMode::Secret;
                }
                palette.session = Some(next);
                palette.entries = Arc::new(choice_entries(&view));
                palette.placeholder = view.placeholder;
                palette.move_selection(0);
            }
            PaletteCommand::Close { session } => {
                let palette = &mut app.palette;
                let current = palette.session.as_ref().is_some_and(|open| {
                    open.owner == owner && open.generation == generation && open.id == session
                });
                if current {
                    // The extension ended it: there is nobody left to tell.
                    palette.session = None;
                    palette.close();
                }
            }
            PaletteCommand::PickFile { request, title } => dialogs.push((request, title)),
        }
    }
    dialogs
}

/// The answers waiting for extensions: sessions that were closed or replaced.
pub fn take_answers(app: &mut AppState) -> Vec<Answer> {
    std::mem::take(&mut app.palette.ended)
        .into_iter()
        .map(|session| Answer {
            owner: session.owner,
            generation: session.generation,
            event: loog_extension_host::PaletteEvent::Cancelled(session.id),
        })
        .collect()
}

/// Tell extensions about sessions that ended since the last time.
pub fn flush_ended(state: &AppContext) {
    let mut answers = Vec::new();
    state.try_update(|app| {
        answers = take_answers(app);
        false
    });
    send(state, answers);
}

/// Hand `answers` to the extensions they are for.
fn send(state: &AppContext, answers: Vec<Answer>) {
    if answers.is_empty() {
        return;
    }
    let Some(host) = state
        .services()
        .and_then(|runtime| runtime.extensions())
    else {
        return;
    };
    for answer in answers {
        host.palette_event(&answer.owner, answer.generation, answer.event);
    }
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
        palette.placeholder = "Search themes".to_owned();
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
    // An open session takes the answer; the palette stays up until the extension
    // says what comes next.
    let mut session_answer: Option<Answer> = None;
    let mut in_session = false;
    env.state.update(|app| {
        let Some(session) = app.palette.session.clone() else {
            return;
        };
        in_session = true;
        let text = app.palette.input.text().to_owned();
        let choice = if session.mode == SessionMode::Choose {
            let hits = app.palette.hits();
            let Some(hit) = hits.get(position) else {
                return;
            };
            match &app.palette.entries[hit.index].target {
                Target::Choice { id, .. } => Some(id.clone()),
                _ => return,
            }
        } else {
            None
        };
        if session.mode == SessionMode::Secret {
            app.palette.input.clear();
        }
        session_answer = Some(Answer {
            owner: session.owner,
            generation: session.generation,
            event: loog_extension_host::PaletteEvent::Accepted(
                loog_extension_host::PaletteAccepted {
                    session: session.id,
                    choice,
                    text,
                },
            ),
        });
    });
    if in_session {
        send(&env.state, session_answer.into_iter().collect());
        return true;
    }
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
        Target::ExtensionEntry {
            owner,
            generation,
            id,
        } => send(
            &env.state,
            vec![Answer {
                owner,
                generation,
                event: loog_extension_host::PaletteEvent::Entry(id),
            }],
        ),
        // A choice only means something to the session it belongs to.
        Target::Choice { .. } => {}
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
    let (tags, activity) = palette
        .session
        .as_ref()
        .map(|session| (session.tags.clone(), session.activity.clone()))
        .unwrap_or_default();
    let placeholder = palette.placeholder.clone();
    let input_empty = palette.input.text().is_empty();
    let nothing_to_list = palette.session.is_some()
        && hits.is_empty()
        && palette.input.text().trim().is_empty();
    if palette.takes_text() || nothing_to_list {
        // The field is the whole answer, or the session has no rows yet: there is
        // nothing to count or list.
        return Frame {
            rows: Vec::new(),
            position: String::new(),
            empty: None,
            placeholder,
            tags,
            activity,
            input_empty,
        };
    }
    if hits.is_empty() {
        return Frame {
            rows: Vec::new(),
            position: "0".to_owned(),
            empty: Some(palette.input.text().trim().to_owned()),
            placeholder,
            tags,
            activity,
            input_empty,
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
                    Target::Extension(_)
                    | Target::Theme(_)
                    | Target::ExtensionEntry { .. }
                    | Target::Choice { .. } => Vec::new(),
                };
                Row::Item(Item {
                    index: *position,
                    title: entry.title.clone(),
                    category: entry.category.clone(),
                    description: entry.description.clone(),
                    keys,
                    positions: hit.positions.clone(),
                    selected: *position == selected,
                    extension: matches!(
                        entry.target,
                        Target::Extension(_)
                            | Target::ExtensionEntry { .. }
                            | Target::Choice { .. }
                    ),
                    trailing: match &entry.target {
                        Target::Choice { trailing, .. } => trailing.clone(),
                        _ => None,
                    },
                })
            }
        })
        .collect();
    Frame {
        rows,
        position: format!("{} / {}", selected + 1, hits.len()),
        empty: None,
        placeholder,
        tags,
        activity,
        input_empty,
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
            Command::Close => {
                writer.update(|app| app.palette.close());
                flush_ended(&writer);
            }
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
                let env = env
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone();
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
        assert!(
            entries
                .iter()
                .all(|entry| matches!(entry.target, Target::Theme(_)) && entry.category == "Theme")
        );
        let marked: Vec<_> = entries
            .iter()
            .filter(|entry| entry.description == "Current theme")
            .collect();
        assert_eq!(marked.len(), 1);
        assert_eq!(marked[0].target, Target::Theme(current));
    }

    #[test]
    fn the_theme_preview_follows_selection_and_clears_on_cancel() {
        use crate::features::keymap::action::Action;

        let mut app = AppState::new();
        let saved = app.preferences.values().theme;
        let preview = ThemeId::from_index(1);
        app.palette.visible = true;
        app.palette.entries = Arc::new(vec![
            Entry {
                id: "theme:saved".into(),
                title: "Saved".into(),
                category: "Theme".into(),
                description: String::new(),
                target: Target::Theme(saved),
            },
            Entry {
                id: "theme:preview".into(),
                title: "Preview".into(),
                category: "Theme".into(),
                description: String::new(),
                target: Target::Theme(preview),
            },
        ]);
        assert_eq!(display_theme(&app), saved);
        app.apply(Action::MenuSelectNext);
        assert_eq!(display_theme(&app), preview);
        app.palette.input.set_text("no matches");
        assert_eq!(display_theme(&app), saved);
        app.palette.input.set_text("preview");
        assert_eq!(display_theme(&app), preview);
        app.apply(Action::CloseOverlay);
        assert_eq!(display_theme(&app), saved);
        assert_eq!(app.preferences.values().theme, saved);
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
    fn typed_query_names_each_group_once_when_matches_come_from_several() {
        let mut palette = opened();
        palette.input.set_text("save");
        let hits = palette.hits();
        let lines = palette.lines(&hits);
        let categories: std::collections::BTreeSet<_> = hits
            .iter()
            .map(|hit| palette.entries[hit.index].category.clone())
            .collect();
        let headers = lines
            .iter()
            .filter(|line| matches!(line, Line::Header(_)))
            .count();
        assert!(!hits.is_empty());
        assert_eq!(headers, if categories.len() > 1 { categories.len() } else { 0 });
    }

    fn extension_view(mode: PaletteMode, tags: &[&str]) -> PaletteView {
        PaletteView {
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            placeholder: "Pick".into(),
            mode,
            choices: vec![
                loog_extension_host::PaletteChoice {
                    id: "one".into(),
                    title: "One".into(),
                    detail: None,
                    group: Some("Group".into()),
                    trailing: Some("Tip".into()),
                },
                loog_extension_host::PaletteChoice {
                    id: "two".into(),
                    title: "Two".into(),
                    detail: None,
                    group: Some("Group".into()),
                    trailing: None,
                },
            ],
            activity: PaletteActivity::Idle,
            note: None,
        }
    }

    #[test]
    fn an_extension_session_shows_its_choices_under_the_palette() {
        let mut app = AppState::new();
        apply_commands(
            &mut app,
            "acme.tool",
            1,
            vec![PaletteCommand::Open {
                session: 7,
                view: extension_view(PaletteMode::Choose, &["Step"]),
            }],
        );
        assert!(app.palette.visible);
        let frame = frame(&app);
        assert_eq!(frame.tags, ["Step"]);
        assert_eq!(frame.placeholder, "Pick");
        let items: Vec<_> = frame
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Item(item) => Some(item),
                Row::Header(_) => None,
            })
            .collect();
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|item| item.extension));
        assert_eq!(items[0].trailing.as_deref(), Some("Tip"));
    }

    #[test]
    fn a_session_without_rows_shows_no_count_or_empty_message() {
        let mut app = AppState::new();
        let mut view = extension_view(PaletteMode::Choose, &["Step"]);
        view.choices.clear();
        view.activity = PaletteActivity::Busy;
        apply_commands(
            &mut app,
            "acme.tool",
            1,
            vec![PaletteCommand::Open { session: 1, view }],
        );
        let frame = frame(&app);
        assert!(frame.position.is_empty());
        assert!(frame.empty.is_none());
        assert_eq!(frame.activity, Activity::Busy);
    }

    #[test]
    fn closing_the_palette_cancels_the_session_but_an_extension_close_does_not() {
        let mut app = AppState::new();
        let open = |app: &mut AppState, session| {
            apply_commands(
                app,
                "acme.tool",
                1,
                vec![PaletteCommand::Open {
                    session,
                    view: extension_view(PaletteMode::Choose, &[]),
                }],
            )
        };
        open(&mut app, 1);
        app.palette.close();
        let answers = take_answers(&mut app);
        assert!(matches!(
            answers[0].event,
            loog_extension_host::PaletteEvent::Cancelled(1)
        ));
        open(&mut app, 2);
        apply_commands(
            &mut app,
            "acme.tool",
            1,
            vec![PaletteCommand::Close { session: 2 }],
        );
        assert!(!app.palette.visible);
        assert!(take_answers(&mut app).is_empty());
    }

    #[test]
    fn replacing_a_session_cancels_the_earlier_one() {
        let mut app = AppState::new();
        for session in [1, 2] {
            apply_commands(
                &mut app,
                "acme.tool",
                1,
                vec![PaletteCommand::Open {
                    session,
                    view: extension_view(PaletteMode::Choose, &[]),
                }],
            );
        }
        let answers = take_answers(&mut app);
        assert_eq!(answers.len(), 1);
        assert!(matches!(
            answers[0].event,
            loog_extension_host::PaletteEvent::Cancelled(1)
        ));
    }

    #[test]
    fn a_secret_session_masks_the_field_and_a_new_step_starts_it_empty() {
        let mut app = AppState::new();
        apply_commands(
            &mut app,
            "acme.tool",
            1,
            vec![PaletteCommand::Open {
                session: 1,
                view: extension_view(PaletteMode::Secret, &["Password"]),
            }],
        );
        assert!(app.palette.input.masked);
        assert!(frame(&app).rows.is_empty(), "a text session lists no rows");
        app.palette.input.set_text("hunter2");
        apply_commands(
            &mut app,
            "acme.tool",
            1,
            vec![PaletteCommand::Update {
                session: 1,
                view: extension_view(PaletteMode::Choose, &["Next"]),
            }],
        );
        assert!(!app.palette.input.masked);
        assert!(app.palette.input.text().is_empty());
    }

    #[test]
    fn published_entries_join_the_command_list_and_go_when_withdrawn() {
        let mut app = AppState::new();
        let entry = loog_extension_host::PaletteEntry {
            id: "a".into(),
            title: "Open A".into(),
            detail: Some("detail".into()),
            category: "Places".into(),
        };
        apply_commands(
            &mut app,
            "acme.tool",
            3,
            vec![PaletteCommand::Entries(vec![entry])],
        );
        let listed = published_entries(&app.palette.published);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].category, "Places");
        assert!(matches!(
            &listed[0].target,
            Target::ExtensionEntry { owner, generation: 3, id } if owner == "acme.tool" && id == "a"
        ));
        apply_commands(&mut app, "acme.tool", 3, vec![PaletteCommand::Entries(vec![])]);
        assert!(published_entries(&app.palette.published).is_empty());
    }
}
