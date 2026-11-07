//! What the language services panel offers: the installed language services, with
//! how each one is doing, and the ones in the registry worth installing for the
//! files that are open.
//!
//! Pure: the application collects what is installed, what is open and what the
//! registry lists, and this decides the rows. It knows no service by name.

/// How a language service is doing, as the application knows it from the extension
/// lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Installed and waiting; it starts when a file it serves needs it.
    Idle,
    Starting,
    Ready,
    /// Busy, such as indexing a project.
    Working,
    /// Turned off; it runs nothing until it is turned on again.
    Stopped,
    Failed,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Starting => "Starting",
            Self::Ready => "Ready",
            Self::Working => "Working",
            Self::Stopped => "Stopped",
            Self::Failed => "Error",
        }
    }

    /// A service that runs or is on its way to run can be stopped.
    pub fn is_on(self) -> bool {
        matches!(
            self,
            Self::Idle | Self::Starting | Self::Ready | Self::Working
        )
    }

    /// How much the state asks for attention: a service in error before one that works.
    fn urgency(self) -> u8 {
        match self {
            Self::Stopped => 0,
            Self::Idle => 1,
            Self::Ready => 2,
            Self::Starting => 3,
            Self::Working => 4,
            Self::Failed => 5,
        }
    }

    /// The one of `states` that asks for the most attention.
    pub fn most_urgent(states: impl IntoIterator<Item = State>) -> Option<State> {
        states.into_iter().max_by_key(|state| state.urgency())
    }
}

/// An installed extension that answers for languages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    pub extension: String,
    /// What it is called in the list.
    pub name: String,
    /// The canonical names of the languages it answers for.
    pub languages: Vec<String>,
    pub state: State,
    /// What there is to say about it now, such as why it failed.
    pub note: String,
}

/// A language service of the registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub extension: String,
    pub name: String,
    pub languages: Vec<String>,
    pub installed: bool,
    /// The registry has a version for this Loog and this computer.
    pub installable: bool,
    pub installing: bool,
}

pub struct Input<'a> {
    pub services: &'a [Service],
    /// The canonical language of the file being edited.
    pub active_language: Option<&'a str>,
    /// The canonical language of each open file, with how many files have it.
    pub open_languages: &'a [(String, usize)],
    pub candidates: &'a [Candidate],
}

/// An installed service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub extension: String,
    pub name: String,
    /// The language whose mark stands for the service now.
    pub language: String,
    pub state: State,
    pub note: String,
}

/// A service that can be installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub extension: String,
    pub name: String,
    pub language: String,
    pub note: String,
    pub installing: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Menu {
    pub installed: Vec<Row>,
    pub recommended: Vec<Offer>,
}

/// The language that marks a service: the one of the file being edited when the
/// service answers for it, else the first it answers for.
pub fn mark_language(languages: &[String], active: Option<&str>) -> String {
    active
        .filter(|active| languages.iter().any(|language| language == active))
        .or_else(|| languages.first().map(String::as_str))
        .unwrap_or_default()
        .to_owned()
}

/// The state the status bar shows: the one of the services that answer for the file being
/// edited that asks for the most attention. `None` when none answers for it, so the bar
/// shows nothing.
pub fn summary(services: &[Service], active_language: Option<&str>) -> Option<State> {
    let active = active_language?;
    State::most_urgent(
        services
            .iter()
            .filter(|service| service.languages.iter().any(|language| language == active))
            .map(|service| service.state),
    )
}

pub fn build(input: Input) -> Menu {
    let serves_active = |service: &Service| {
        input
            .active_language
            .is_some_and(|active| service.languages.iter().any(|language| language == active))
    };
    let mut installed: Vec<&Service> = input.services.iter().collect();
    // Those that serve the file being edited first, then by name.
    installed.sort_by_key(|service| (!serves_active(service), service.name.to_lowercase()));
    let installed = installed
        .into_iter()
        .map(|service| Row {
            extension: service.extension.clone(),
            name: service.name.clone(),
            language: mark_language(&service.languages, input.active_language),
            state: service.state,
            note: service.note.clone(),
        })
        .collect();

    let mut recommended: Vec<Offer> = input
        .candidates
        .iter()
        .filter(|candidate| !candidate.installed && candidate.installable)
        .filter_map(|candidate| {
            let wanted: Vec<&(String, usize)> = input
                .open_languages
                .iter()
                .filter(|(language, _)| candidate.languages.contains(language))
                .collect();
            let files: usize = wanted.iter().map(|(_, count)| count).sum();
            // Only what the open files could use.
            let (language, _) = wanted.first()?;
            Some(Offer {
                extension: candidate.extension.clone(),
                name: candidate.name.clone(),
                language: language.clone(),
                note: format!("{files} open {}", if files == 1 { "file" } else { "files" }),
                installing: candidate.installing,
            })
        })
        .collect();
    recommended.sort_by_key(|offer| offer.name.to_lowercase());
    Menu {
        installed,
        recommended,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(name: &str, languages: &[&str], state: State) -> Service {
        Service {
            extension: format!("loog.lsp.{}", name.to_lowercase()),
            name: name.into(),
            languages: languages
                .iter()
                .map(|language| (*language).into())
                .collect(),
            state,
            note: String::new(),
        }
    }

    fn candidate(name: &str, languages: &[&str]) -> Candidate {
        Candidate {
            extension: format!("loog.lsp.{}", name.to_lowercase()),
            name: name.into(),
            languages: languages
                .iter()
                .map(|language| (*language).into())
                .collect(),
            installed: false,
            installable: true,
            installing: false,
        }
    }

    #[test]
    fn services_of_the_file_being_edited_come_first_then_by_name() {
        let services = [
            service("Taplo", &["toml"], State::Idle),
            service("Rust Analyzer", &["rust"], State::Ready),
            service("Pyright", &["python"], State::Failed),
        ];
        let menu = build(Input {
            services: &services,
            active_language: Some("rust"),
            open_languages: &[],
            candidates: &[],
        });
        let names: Vec<_> = menu.installed.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["Rust Analyzer", "Pyright", "Taplo"]);
    }

    #[test]
    fn a_service_for_several_languages_is_marked_by_the_file_being_edited() {
        let languages: Vec<String> = ["javascript", "typescript", "json"]
            .iter()
            .map(|language| (*language).into())
            .collect();
        assert_eq!(mark_language(&languages, Some("json")), "json");
        // Not one of its languages, or no file: the first it answers for.
        assert_eq!(mark_language(&languages, Some("rust")), "javascript");
        assert_eq!(mark_language(&languages, None), "javascript");
        assert_eq!(mark_language(&[], Some("rust")), "");
    }

    #[test]
    fn only_services_the_open_files_could_use_are_recommended() {
        let mut installed = candidate("Pyright", &["python"]);
        installed.installed = true;
        let mut unavailable = candidate("Gopls", &["go"]);
        unavailable.installable = false;
        let candidates = [
            candidate("TypeScript Language Server", &["typescript", "javascript"]),
            candidate("Marksman", &["markdown"]),
            installed,
            unavailable,
        ];
        let menu = build(Input {
            services: &[],
            active_language: None,
            open_languages: &[
                ("typescript".into(), 1),
                ("javascript".into(), 2),
                ("python".into(), 4),
                ("go".into(), 1),
            ],
            candidates: &candidates,
        });
        assert_eq!(menu.recommended.len(), 1);
        let offer = &menu.recommended[0];
        assert_eq!(offer.name, "TypeScript Language Server");
        assert_eq!(offer.language, "typescript");
        assert_eq!(offer.note, "3 open files", "both of its languages count");
    }

    #[test]
    fn one_open_file_is_told_in_the_singular() {
        let menu = build(Input {
            services: &[],
            active_language: None,
            open_languages: &[("markdown".into(), 1)],
            candidates: &[candidate("Marksman", &["markdown"])],
        });
        assert_eq!(menu.recommended[0].note, "1 open file");
    }

    #[test]
    fn a_stopped_service_is_off_and_the_others_are_on() {
        assert!(State::Idle.is_on() && State::Starting.is_on());
        assert!(State::Ready.is_on() && State::Working.is_on());
        assert!(!State::Stopped.is_on() && !State::Failed.is_on());
    }

    #[test]
    fn the_status_bar_shows_the_state_that_asks_for_the_most_attention() {
        let services = [
            service("Rust Analyzer", &["rust"], State::Ready),
            service("Other", &["rust"], State::Working),
            service("Pyright", &["python"], State::Failed),
        ];
        // Only what answers for the file being edited counts: not the failed Python one.
        assert_eq!(summary(&services, Some("rust")), Some(State::Working));
        assert_eq!(summary(&services, Some("python")), Some(State::Failed));
        // Nothing answers for it, or no file is open: nothing is shown.
        assert_eq!(summary(&services, Some("toml")), None);
        assert_eq!(summary(&services, None), None);
        assert_eq!(
            State::most_urgent([State::Ready, State::Starting, State::Failed]),
            Some(State::Failed)
        );
        assert_eq!(State::most_urgent([]), None);
    }
}
