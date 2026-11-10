//! What the language panel offers: the languages in use, every language Loog has
//! syntax for, the ones worth installing for the files that are open, and the rest
//! the registry has.
//!
//! Pure: the application collects what is installed, what is open and what the
//! registry lists, and this decides the four groups.

/// A language Loog has syntax for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    /// The canonical name, such as `rust`.
    pub name: String,
    pub display: String,
    pub badge: String,
}

/// A language extension of the registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub extension: String,
    /// What it is called, such as `Rust`.
    pub name: String,
    /// The names of the languages it adds.
    pub languages: Vec<String>,
    /// The file types it recognizes, lowercase and without the dot.
    pub file_types: Vec<String>,
    pub installed: bool,
    /// The registry has a version for this Loog and this computer.
    pub installable: bool,
    pub installing: bool,
}

pub struct Input<'a> {
    pub installed: &'a [Installed],
    /// Canonical names of the languages of the open files.
    pub open_languages: &'a [String],
    /// File names of the open files.
    pub open_files: &'a [String],
    pub candidates: &'a [Candidate],
    /// What the filter of the last group says.
    pub filter: &'a str,
}

/// A language that works now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Language {
    pub name: String,
    pub display: String,
    pub badge: String,
}

/// A language that can be installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub extension: String,
    pub name: String,
    pub badge: String,
    /// What colours the badge, the same as when the language is installed.
    pub key: String,
    /// Why it is recommended, when it is.
    pub note: String,
    pub installing: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Menu {
    /// Languages of the open files.
    pub active: Vec<Language>,
    /// Every language with syntax.
    pub all: Vec<Language>,
    /// Not installed, and the open files have its file types.
    pub recommended: Vec<Offer>,
    /// The other languages the registry has, after the filter.
    pub more: Vec<Offer>,
    /// How many there are before the filter.
    pub more_total: usize,
}

/// The file types a file name is recognized by, longest first: `a.d.ts` is `d.ts`
/// and `ts`; a name without a dot (`Dockerfile`, `.gitignore`) stands for itself.
pub fn file_types(name: &str) -> Vec<String> {
    let lower = name.to_lowercase();
    let trimmed = lower.trim_start_matches('.');
    let parts: Vec<&str> = trimmed.split('.').collect();
    if parts.len() == 1 {
        return vec![trimmed.to_owned()];
    }
    (1..parts.len())
        .map(|from| parts[from..].join("."))
        .collect()
}

/// The short mark of a language that is not installed: its first file type when
/// that is short (`yml`, `toml`), else the start of its name.
pub fn badge_text(name: &str, file_types: &[String]) -> String {
    let short = file_types
        .iter()
        .map(|file_type| file_type.trim_start_matches('.'))
        .find(|file_type| !file_type.is_empty() && file_type.chars().count() <= 4);
    match short {
        Some(file_type) => file_type.to_uppercase(),
        None => name
            .chars()
            .filter(|c| c.is_alphanumeric())
            .take(3)
            .collect::<String>()
            .to_uppercase(),
    }
}

pub fn build(input: Input) -> Menu {
    let mut all: Vec<Language> = input
        .installed
        .iter()
        .map(|language| Language {
            name: language.name.clone(),
            display: language.display.clone(),
            badge: language.badge.clone(),
        })
        .collect();
    all.sort_by_key(|language| language.display.to_lowercase());
    let active = all
        .iter()
        .filter(|language| input.open_languages.contains(&language.name))
        .cloned()
        .collect();

    let mut recommended = Vec::new();
    let mut more = Vec::new();
    for candidate in input.candidates {
        if candidate.installed || (!candidate.installable && !candidate.installing) {
            continue;
        }
        let matching = input
            .open_files
            .iter()
            .filter(|file| {
                file_types(file)
                    .iter()
                    .any(|file_type| candidate.file_types.contains(file_type))
            })
            .count();
        let offer = Offer {
            extension: candidate.extension.clone(),
            name: candidate.name.clone(),
            badge: badge_text(&candidate.name, &candidate.file_types),
            key: candidate
                .languages
                .first()
                .map_or_else(|| candidate.name.to_lowercase(), |name| name.to_lowercase()),
            note: match matching {
                0 => String::new(),
                1 => "1 open file".to_owned(),
                n => format!("{n} open files"),
            },
            installing: candidate.installing,
        };
        if matching > 0 {
            recommended.push(offer);
        } else {
            more.push(offer);
        }
    }
    let by_name = |offer: &Offer| offer.name.to_lowercase();
    recommended.sort_by_key(by_name);
    more.sort_by_key(by_name);
    let more_total = more.len();
    let filter = input.filter.trim().to_lowercase();
    if !filter.is_empty() {
        more.retain(|offer| offer.name.to_lowercase().contains(&filter));
    }
    Menu {
        active,
        all,
        recommended,
        more,
        more_total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed(name: &str, display: &str, badge: &str) -> Installed {
        Installed {
            name: name.into(),
            display: display.into(),
            badge: badge.into(),
        }
    }

    fn candidate(extension: &str, name: &str, types: &[&str]) -> Candidate {
        Candidate {
            extension: extension.into(),
            name: name.into(),
            languages: vec![name.to_lowercase()],
            file_types: types.iter().map(|t| (*t).to_owned()).collect(),
            installed: false,
            installable: true,
            installing: false,
        }
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn badges_of_languages_not_installed_come_from_a_short_file_type_or_the_name() {
        assert_eq!(badge_text("YAML", &strings(&["yaml", "yml"])), "YAML");
        assert_eq!(badge_text("Python", &strings(&["py"])), "PY");
        assert_eq!(badge_text("Dockerfile", &strings(&["dockerfile"])), "DOC");
        assert_eq!(badge_text("Go", &[]), "GO");
    }

    #[test]
    fn file_types_run_from_the_longest_suffix_to_the_shortest() {
        assert_eq!(file_types("a.d.ts"), ["d.ts", "ts"]);
        assert_eq!(file_types(".gitignore"), ["gitignore"]);
        assert_eq!(file_types("Main.RS"), ["rs"]);
    }

    #[test]
    fn active_languages_are_those_of_open_files_and_all_lists_every_installed_one() {
        let installed = [
            installed("rust", "Rust", "RS"),
            installed("json", "JSON", "JSON"),
            installed("bash", "Bash", "SH"),
        ];
        let menu = build(Input {
            installed: &installed,
            open_languages: &strings(&["rust", "plaintext"]),
            open_files: &[],
            candidates: &[],
            filter: "",
        });
        assert_eq!(
            menu.all
                .iter()
                .map(|l| l.display.as_str())
                .collect::<Vec<_>>(),
            ["Bash", "JSON", "Rust"]
        );
        assert_eq!(menu.active.len(), 1);
        assert_eq!(menu.active[0].name, "rust");
    }

    #[test]
    fn a_language_is_recommended_when_open_files_have_its_types_and_the_rest_are_more() {
        let candidates = [
            candidate("loog.lang.yaml", "YAML", &["yaml", "yml"]),
            candidate("loog.lang.toml", "TOML", &["toml"]),
            candidate("loog.lang.go", "Go", &["go"]),
        ];
        let menu = build(Input {
            installed: &[],
            open_languages: &[],
            open_files: &strings(&["ci.yml", "deploy.yaml", "Cargo.lock"]),
            candidates: &candidates,
            filter: "",
        });
        assert_eq!(menu.recommended.len(), 1);
        assert_eq!(menu.recommended[0].name, "YAML");
        assert_eq!(menu.recommended[0].note, "2 open files");
        assert_eq!(
            menu.more
                .iter()
                .map(|o| o.name.as_str())
                .collect::<Vec<_>>(),
            ["Go", "TOML"]
        );
        assert_eq!(menu.more_total, 2);
    }

    #[test]
    fn the_filter_narrows_only_the_more_group() {
        let candidates = [
            candidate("loog.lang.go", "Go", &["go"]),
            candidate("loog.lang.python", "Python", &["py"]),
        ];
        let menu = build(Input {
            installed: &[],
            open_languages: &[],
            open_files: &strings(&["a.go"]),
            candidates: &candidates,
            filter: " PY ",
        });
        assert_eq!(
            menu.recommended.len(),
            1,
            "the recommendation is not filtered"
        );
        assert_eq!(menu.more.len(), 1);
        assert_eq!(menu.more[0].name, "Python");
        assert_eq!(menu.more_total, 1);
    }

    #[test]
    fn an_installed_extension_is_not_offered_again() {
        let mut waiting = candidate("loog.lang.yaml", "YAML", &["yaml"]);
        waiting.installed = true;
        let mut loaded = candidate("loog.lang.rust", "Rust", &["rs"]);
        loaded.installed = true;
        let installed = [installed("rust", "Rust", "RS")];
        let menu = build(Input {
            installed: &installed,
            open_languages: &[],
            open_files: &[],
            candidates: &[waiting, loaded],
            filter: "",
        });
        assert!(menu.recommended.is_empty() && menu.more.is_empty());
    }

    #[test]
    fn what_the_registry_has_no_version_of_is_not_offered_and_one_being_installed_is() {
        let mut none = candidate("loog.lang.a", "Alpha", &["a"]);
        none.installable = false;
        let mut busy = candidate("loog.lang.b", "Beta", &["b"]);
        busy.installable = false;
        busy.installing = true;
        let menu = build(Input {
            installed: &[],
            open_languages: &[],
            open_files: &[],
            candidates: &[none, busy],
            filter: "",
        });
        assert_eq!(menu.more.len(), 1);
        assert!(menu.more[0].installing);
    }
}
