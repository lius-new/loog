//! The language services panel: which language services are installed and how each
//! is doing, and which the registry offers for the files that are open. Opened with
//! `workspace::ShowLanguageServices`.
use crate::app::context::AppContext;
use crate::app::state::AppState;
use crate::domain::language::Registry;
use crate::features::highlight_menu::badge_text;
use crate::features::keymap::action::Action;
use crate::features::language_services_menu::{self, Input, Service, State};
use crate::ui::language_services_menu::{Binding, Command, Frame, Offer, Row};

#[derive(Clone, Default)]
pub struct LanguageServicesMenuState {
    pub visible: bool,
    /// Whether the terminal had focus when the panel opened.
    pub restore_terminal: bool,
}

impl LanguageServicesMenuState {
    pub fn close(&mut self) {
        self.visible = false;
    }
}

/// The longest reason for a failure the panel shows.
const NOTE_CHARS: usize = 48;

pub(crate) fn state_of(state: crate::features::extensions::lifecycle::State) -> State {
    use crate::features::extensions::lifecycle::State as Lifecycle;
    match state {
        Lifecycle::Discovered | Lifecycle::Validated | Lifecycle::Dormant => State::Idle,
        Lifecycle::Activating | Lifecycle::Stopping => State::Starting,
        Lifecycle::Active => State::Ready,
        Lifecycle::Stopped => State::Stopped,
        Lifecycle::Failed => State::Failed,
    }
}

/// How an extension says its service is doing.
pub(crate) fn report_state(state: loog_extension_host::ServiceState) -> State {
    use loog_extension_host::ServiceState as Reported;
    match state {
        Reported::Stopped => State::Stopped,
        Reported::Starting => State::Starting,
        Reported::Ready => State::Ready,
        Reported::Working => State::Working,
        Reported::Error => State::Failed,
    }
}

/// The first line of a failure, cut short.
pub(crate) fn short(error: &str) -> String {
    let line = error.lines().next().unwrap_or_default().trim();
    if line.chars().count() > NOTE_CHARS {
        let mut cut: String = line.chars().take(NOTE_CHARS - 1).collect();
        cut.push('…');
        cut
    } else {
        line.to_owned()
    }
}

/// The path (spelled as extensions are told it) and the language of the file being edited.
fn active_file(app: &AppState) -> (Option<String>, Option<String>) {
    let path = app
        .workspace
        .active_path()
        .map(|path| path.to_string_lossy().into_owned());
    let language = app
        .workspace
        .active()
        .and_then(|id| app.workspace.view_meta(id))
        .map(|meta| meta.lang.name().to_owned());
    (path, language)
}

/// The installed extensions that answer for languages, as they do for `document`.
fn services(state: &AppContext, document: Option<&str>) -> Vec<Service> {
    state
        .services()
        .and_then(|runtime| runtime.extensions())
        .map(|host| host.language_services(document))
        .unwrap_or_default()
}

/// How the language services are doing for the file being edited, for the status bar:
/// the state that asks for the most attention, or `None` when no service answers for it.
pub fn status(state: &AppContext, app: &AppState) -> Option<State> {
    let (path, language) = active_file(app);
    language_services_menu::summary(&services(state, path.as_deref()), language.as_deref())
}

/// The short mark of a language: the one Loog knows it by, else made from its name.
fn badge(language: &str) -> String {
    Registry::builtin()
        .languages()
        .iter()
        .find(|known| &*known.name == language)
        .map(|known| known.badge.to_string())
        .unwrap_or_else(|| badge_text(language, &[]))
}

fn frame(state: &AppContext, app: &AppState) -> Frame {
    let mut open_languages: Vec<(String, usize)> = Vec::new();
    for document in app.workspace.documents.values().filter(|d| d.is_text()) {
        let name = document.meta.lang.name();
        match open_languages.iter_mut().find(|(known, _)| known == name) {
            Some((_, count)) => *count += 1,
            None => open_languages.push((name.to_owned(), 1)),
        }
    }
    let (path, active_language) = active_file(app);
    let candidates = state
        .services()
        .and_then(|runtime| runtime.extensions())
        .and_then(|host| host.service_candidates());
    let registry_ready = candidates.is_some();
    let installed = services(state, path.as_deref());
    let menu = language_services_menu::build(Input {
        services: &installed,
        active_language: active_language.as_deref(),
        open_languages: &open_languages,
        candidates: candidates.as_deref().unwrap_or_default(),
    });
    Frame {
        installed: menu
            .installed
            .into_iter()
            .map(|row| Row {
                extension: row.extension,
                name: row.name,
                text: badge(&row.language),
                key: row.language,
                state: row.state,
                note: row.note,
            })
            .collect(),
        recommended: menu
            .recommended
            .into_iter()
            .map(|offer| Offer {
                extension: offer.extension,
                name: offer.name,
                text: badge(&offer.language),
                key: offer.language,
                note: offer.note,
                installing: offer.installing,
            })
            .collect(),
        registry_ready,
    }
}

/// Show the panel and look for a newer registry index in the background.
pub fn open(state: &AppContext) {
    state.update(|app| {
        app.close_overlays_for_palette();
        let restore_terminal = app.terminal.ui.terminal_focused;
        let menu = &mut app.language_services_menu;
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
        move || {
            let app = reader.get();
            frame(&reader, &app)
        },
        move |command| {
            let host = writer.services().and_then(|runtime| runtime.extensions());
            match command {
                Command::Close => writer.update(|app| app.language_services_menu.close()),
                Command::Restart(extension) => {
                    if let Some(host) = host {
                        host.restart(&writer, &extension);
                    }
                }
                Command::Switch(extension, true) => {
                    if let Some(host) = host {
                        host.turn_on(&writer, &extension);
                    }
                }
                Command::Switch(extension, false) => {
                    if let Some(host) = host {
                        host.set_enabled(&writer, &extension, false);
                    }
                }
                Command::Install(extension) => {
                    if let Some(host) = host {
                        host.install(&writer, &extension);
                    }
                }
                Command::OpenExtensions => {
                    writer.update(|app| {
                        app.language_services_menu.close();
                        app.apply(Action::OpenExtensions);
                    });
                    crate::app::extensions_view::opened(&writer);
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_hides_the_panel() {
        let mut menu = LanguageServicesMenuState {
            visible: true,
            restore_terminal: true,
        };
        menu.close();
        assert!(!menu.visible);
    }

    #[test]
    fn the_lifecycle_of_an_extension_is_told_as_how_the_service_is_doing() {
        use crate::features::extensions::lifecycle::State as Lifecycle;
        assert_eq!(state_of(Lifecycle::Dormant), State::Idle);
        assert_eq!(state_of(Lifecycle::Activating), State::Starting);
        assert_eq!(state_of(Lifecycle::Active), State::Ready);
        assert_eq!(state_of(Lifecycle::Stopped), State::Stopped);
        assert_eq!(state_of(Lifecycle::Failed), State::Failed);
    }

    #[test]
    fn a_failure_is_told_in_one_short_line() {
        assert_eq!(
            short("exited with code 101\nbacktrace…"),
            "exited with code 101"
        );
        let long = "x".repeat(100);
        assert_eq!(short(&long).chars().count(), NOTE_CHARS);
        assert!(short(&long).ends_with('…'));
    }
}
