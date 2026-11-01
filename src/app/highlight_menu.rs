//! The highlight panel: which languages are highlighted now, which Loom has syntax
//! for, and which the registry offers to install. Opened from the language badge
//! of the status bar or with `workspace::ShowHighlighting`.
use crate::app::context::AppContext;
use crate::app::state::AppState;
use crate::domain::language::Registry;
use crate::features::highlight_menu::{self, Input, Installed};
use crate::features::keymap::action::Action;
use crate::ui::components::input::InputState;
use crate::ui::components::state_access::StateAccess;
use crate::ui::highlight_menu::{Badge, Binding, Command, Frame, Row};

#[derive(Clone)]
pub struct HighlightMenuState {
    pub visible: bool,
    /// The filter of the last group.
    pub input: InputState,
    /// The last group is open even without a filter. It is open each time the
    /// panel opens; folding it lasts until the panel closes.
    pub more_open: bool,
    /// Whether the terminal had focus when the panel opened.
    pub restore_terminal: bool,
    /// The language whose badge the pointer is over, named in the footer.
    pub hover: Option<String>,
}

impl Default for HighlightMenuState {
    fn default() -> Self {
        Self {
            visible: false,
            input: InputState::default(),
            more_open: true,
            restore_terminal: false,
            hover: None,
        }
    }
}

impl HighlightMenuState {
    pub fn close(&mut self) {
        self.visible = false;
        self.input.clear();
        self.more_open = true;
        self.hover = None;
    }
}

/// The languages Loom has syntax for now: those with a grammar, plain text left out.
fn installed_languages() -> Vec<Installed> {
    let registry = Registry::builtin();
    let plain = registry.plain_text().language;
    registry
        .languages()
        .iter()
        .filter(|language| {
            language.id != plain
                && language
                    .variants
                    .iter()
                    .any(|variant| variant.grammar.is_some())
        })
        .map(|language| Installed {
            name: language.name.to_string(),
            display: language.display_name.to_string(),
            badge: language.badge.to_string(),
        })
        .collect()
}

fn menu(state: &AppContext, app: &AppState) -> (highlight_menu::Menu, bool) {
    let installed = installed_languages();
    let mut open_languages = Vec::new();
    let mut open_files = Vec::new();
    for document in app.workspace.documents.values().filter(|d| d.is_text()) {
        let name = document.meta.lang.name().to_owned();
        if !open_languages.contains(&name) {
            open_languages.push(name);
        }
        if let Some(file) = document.meta.path.file_name() {
            open_files.push(file.to_string_lossy().into_owned());
        }
    }
    let candidates = state
        .services()
        .and_then(|runtime| runtime.extensions())
        .and_then(|host| host.language_candidates());
    let ready = candidates.is_some();
    let menu = highlight_menu::build(Input {
        installed: &installed,
        open_languages: &open_languages,
        open_files: &open_files,
        candidates: candidates.as_deref().unwrap_or_default(),
        filter: app.highlight_menu.input.text(),
    });
    (menu, ready)
}

fn frame(state: &AppContext, app: &AppState) -> Frame {
    let (menu, registry_ready) = menu(state, app);
    let badge = |language: &highlight_menu::Language| Badge {
        name: language.display.clone(),
        text: language.badge.clone(),
        key: language.name.clone(),
    };
    let row = |offer: highlight_menu::Offer| Row {
        extension: offer.extension,
        name: offer.name,
        text: offer.badge,
        key: offer.key,
        note: offer.note,
        installing: offer.installing,
    };
    Frame {
        restart: menu.restart,
        active: menu.active.iter().map(badge).collect(),
        all: menu.all.iter().map(badge).collect(),
        recommended: menu.recommended.into_iter().map(row).collect(),
        more: menu.more.into_iter().map(row).collect(),
        more_total: menu.more_total,
        more_open: app.highlight_menu.more_open,
        filtering: !app.highlight_menu.input.text().trim().is_empty(),
        hover: app.highlight_menu.hover.clone(),
        registry_ready,
    }
}

/// Show the panel and look for a newer registry index in the background.
pub fn open(state: &AppContext) {
    state.update(|app| {
        app.close_overlays_for_palette();
        let restore_terminal = app.terminal.ui.terminal_focused;
        let menu = &mut app.highlight_menu;
        menu.close();
        menu.restore_terminal = restore_terminal;
        menu.visible = true;
    });
    if let Some(host) = state.services().and_then(|runtime| runtime.extensions()) {
        host.refresh_registry(state, false);
    }
}

pub fn binding(state: &AppContext) -> Binding {
    let reader = state.clone();
    let writer = state.clone();
    Binding::new(
        StateAccess::project(
            state.model(),
            |app| &app.highlight_menu.input,
            |app| &mut app.highlight_menu.input,
        ),
        move || {
            let app = reader.get();
            frame(&reader, &app)
        },
        move |command| match command {
            Command::Close => writer.update(|app| app.highlight_menu.close()),
            Command::ToggleMore => writer.update(|app| {
                app.highlight_menu.more_open = !app.highlight_menu.more_open;
            }),
            Command::Hover(name) => {
                writer.try_update(|app| {
                    if app.highlight_menu.hover == name {
                        return false;
                    }
                    app.highlight_menu.hover = name;
                    true
                });
            }
            Command::Install(extension) => {
                if let Some(host) = writer.services().and_then(|runtime| runtime.extensions()) {
                    host.install(&writer, &extension);
                }
            }
            Command::OpenExtensions => {
                writer.update(|app| {
                    app.highlight_menu.close();
                    app.apply(Action::OpenExtensions);
                });
                crate::app::extensions_view::opened(&writer);
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_group_is_open_each_time_and_closing_clears_the_filter() {
        assert!(HighlightMenuState::default().more_open);
        let mut menu = HighlightMenuState {
            visible: true,
            more_open: false,
            hover: Some("Rust".into()),
            ..Default::default()
        };
        menu.input.set_text("py");
        menu.close();
        assert!(!menu.visible && menu.hover.is_none());
        assert!(menu.more_open, "the next opening starts with it open");
        assert_eq!(menu.input.text(), "");
    }

    #[test]
    fn plain_text_is_not_a_language_with_syntax() {
        let languages = installed_languages();
        assert!(
            languages
                .iter()
                .all(|language| language.name != "plaintext")
        );
    }
}
