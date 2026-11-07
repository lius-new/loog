//! Pure project scope, path recognition and JSONC template.
use super::rules::{self as settings_persistence, Overrides, PROJECT_KEYS};
use std::path::{Path, PathBuf};

pub const DIR_NAME: &str = ".loog";
pub const FILE_NAME: &str = "settings.json";

/// Overrides read from one project, plus messages for skipped values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProjectSettings {
    pub overrides: Overrides,
    pub errors: Vec<String>,
}
/// The project settings file of the workspace folder `root`.
pub fn path(root: &Path) -> PathBuf {
    root.join(DIR_NAME).join(FILE_NAME)
}

/// Whether `file` is some workspace folder's project settings file.
pub fn is_settings_file(file: &Path) -> bool {
    file.file_name().is_some_and(|name| name == FILE_NAME)
        && file
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == DIR_NAME)
}

/// Contents of a new project settings file.
pub fn template() -> String {
    let mut text = String::from(
        "// Loog project settings, shared with everyone who opens this folder.\n\
         // Values here override your user settings.json for this folder.\n\
         //\n\
         // Settings that can be set per project:\n",
    );
    let defaults = serde_json::to_value(settings_persistence::Settings::default())
        .expect("settings serialize");
    for key in PROJECT_KEYS {
        text.push_str(&format!("//   \"{key}\": {}\n", defaults[key]));
    }
    text.push_str("{\n}\n");
    text
}
#[cfg(test)]
mod tests {
    use super::settings_persistence::Scope;
    use super::*;
    use crate::features::terminal::ShellKind;
    #[test]
    fn only_project_keys_apply() {
        let (overrides, errors) = settings_persistence::parse_overrides(
            r#"{ "default_shell": "bash", "theme": "ember", "terminal_profiles": [] }"#,
            Scope::Project,
        )
        .unwrap();
        assert_eq!(overrides.len(), 1);
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors[0].contains("\"theme\" can only be set in the user settings.json"));

        let user = settings_persistence::Settings {
            smooth_caret: false,
            ..Default::default()
        };
        let merged = settings_persistence::overlay(&user, &overrides);
        assert_eq!(merged.default_shell, ShellKind::Bash);
        assert!(!merged.smooth_caret);
    }

    #[test]
    fn template_lists_project_keys_and_parses_empty() {
        let template = template();
        for key in PROJECT_KEYS {
            assert!(template.contains(&format!("\"{key}\"")));
        }
        let (overrides, errors) =
            settings_persistence::parse_overrides(&template, Scope::Project).unwrap();
        assert!(overrides.is_empty() && errors.is_empty());
    }

    #[test]
    fn recognizes_settings_files() {
        assert!(is_settings_file(Path::new("/repo/.loog/settings.json")));
        assert!(!is_settings_file(Path::new("/repo/settings.json")));
        assert!(!is_settings_file(Path::new("/repo/.loog/keymap.json")));
    }
}
