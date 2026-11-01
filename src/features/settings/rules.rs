//! User preferences, stored in `settings.json` like VS Code's user settings.
//!
//! The file is JSONC holding only the values that differ from the defaults.
//! The Settings page edits it member by member, so the user's comments and
//! layout survive; hand edits are picked up by a file watcher.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::font::FontSettings;
use super::jsonc;
use super::servers::LanguageServerSetting;
use super::theme_id::ThemeId;
use crate::domain::language::{AssociationSpec, Registry, validate_association};
use crate::features::language_service::server_set::ServerSet;
use crate::features::terminal::ShellKind;

pub const FILE_NAME: &str = "settings.json";

/// One `file_associations` rule: files whose path matches `pattern` are shown
/// in `language`. Rules are tried in order and the first match wins.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileAssociation {
    pub pattern: String,
    pub language: String,
}

impl FileAssociation {
    pub fn spec(&self) -> AssociationSpec<'_> {
        AssociationSpec {
            pattern: &self.pattern,
            language: &self.language,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Settings {
    #[serde(default)]
    pub editor: super::editing::EditorSettings,
    #[serde(default)]
    pub editor_languages: std::collections::BTreeMap<String, super::editing::EditorOverrides>,
    #[serde(default)]
    pub theme: ThemeId,
    #[serde(default)]
    pub encoding: crate::domain::encoding::EncodingSettings,
    /// Interface scale as a percentage of OS scaling; null selects automatic scaling.
    #[serde(default)]
    pub ui_scale: Option<u16>,
    #[serde(default)]
    pub default_shell: ShellKind,
    #[serde(default)]
    pub terminal_profiles: Vec<crate::features::terminal::profiles::ShellProfile>,
    #[serde(default = "default_true")]
    pub terminal_cursor_blink: bool,
    #[serde(default = "default_true")]
    pub smooth_caret: bool,
    #[serde(default)]
    pub markdown: super::markdown::MarkdownSettings,
    /// Expand and scroll the file tree to the active editor's file.
    #[serde(default = "default_true")]
    pub explorer_auto_reveal: bool,
    #[serde(default)]
    pub git_inline_blame: bool,
    #[serde(default)]
    pub git_split_diff: bool,
    #[serde(default)]
    pub git_tree_view: bool,
    #[serde(default)]
    pub vim: VimSettings,
    /// Look for new Loom releases in packaged builds.
    #[serde(default = "default_true")]
    pub check_for_updates: bool,
    #[serde(default)]
    pub editor_font: FontSettings,
    #[serde(default = "default_terminal_font")]
    pub terminal_font: FontSettings,
    /// Ordered path patterns that choose a file's language before the built-in
    /// recognition by name and extension.
    #[serde(default)]
    pub file_associations: Vec<FileAssociation>,
    /// The language servers: how catalogued ones behave, and servers of your
    /// own. An object keyed by server name. User settings only.
    #[serde(default, with = "super::servers::as_object")]
    pub language_servers: Vec<LanguageServerSetting>,
    /// Per-language choices of servers, formatter and what happens on save.
    /// User settings only.
    #[serde(default)]
    pub languages: std::collections::BTreeMap<String, super::servers::LanguageSetting>,
    /// Behaviour common to all language servers. User settings only.
    #[serde(default)]
    pub lsp: super::servers::LspSettings,
}

/// Preferences for Vim editing (`"vim"` in `settings.json`).
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default)]
pub struct VimSettings {
    pub enabled: bool,
    /// Yanks, deletes and puts without a register name use the system clipboard.
    pub system_clipboard: bool,
    pub ignore_case: bool,
    pub smart_case: bool,
}

impl From<VimSettings> for loom_editor_core::vim::Options {
    fn from(settings: VimSettings) -> Self {
        Self {
            enabled: settings.enabled,
            system_clipboard: settings.system_clipboard,
            ignore_case: settings.ignore_case,
            smart_case: settings.smart_case,
        }
    }
}

impl From<loom_editor_core::vim::Options> for VimSettings {
    fn from(options: loom_editor_core::vim::Options) -> Self {
        Self {
            enabled: options.enabled,
            system_clipboard: options.system_clipboard,
            ignore_case: options.ignore_case,
            smart_case: options.smart_case,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeId::default(),
            editor: Default::default(),
            editor_languages: Default::default(),
            encoding: Default::default(),
            ui_scale: None,
            default_shell: ShellKind::default(),
            terminal_profiles: Vec::new(),
            terminal_cursor_blink: true,
            smooth_caret: true,
            markdown: Default::default(),
            explorer_auto_reveal: true,
            git_inline_blame: false,
            git_split_diff: false,
            git_tree_view: false,
            vim: VimSettings::default(),
            check_for_updates: true,
            editor_font: FontSettings::EDITOR,
            terminal_font: FontSettings::TERMINAL,
            file_associations: Vec::new(),
            language_servers: Vec::new(),
            languages: Default::default(),
            lsp: Default::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_terminal_font() -> FontSettings {
    FontSettings::TERMINAL
}

/// Settings read from one file, plus messages for values that were ignored.
#[derive(Debug, Default)]
pub struct LoadOutcome {
    pub settings: Settings,
    pub errors: Vec<String>,
}

/// Settings a project's `.loom/settings.json` may override. Everything else
/// is a personal preference and only read from the user's file.
pub const PROJECT_KEYS: &[&str] = &[
    "default_shell",
    "file_associations",
    "encoding",
    "editor",
    "editor_languages",
];

/// Which file a settings source is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    User,
    Project,
}

/// The values a settings file sets, validated, keyed like `Settings`; a
/// nested object holds only the fields the file sets.
pub type Overrides = Map<String, Value>;

/// Parse file contents over the defaults. An unknown key or a bad value is
/// reported and skipped, so one mistake never resets the other settings.
/// `Err` means the file is not a JSONC object at all.
pub fn parse(source: &str) -> Result<LoadOutcome, String> {
    parse_with(source, &ServerSet::current())
}

/// Like `parse`, checking server names against `servers`.
pub fn parse_with(source: &str, servers: &ServerSet) -> Result<LoadOutcome, String> {
    let (overrides, errors) = parse_overrides_with(source, Scope::User, servers)?;
    Ok(LoadOutcome {
        settings: overlay(&Settings::default(), &overrides),
        errors,
    })
}

/// The valid values of a settings file for `scope`, plus messages for the
/// ones that were skipped.
pub fn parse_overrides(source: &str, scope: Scope) -> Result<(Overrides, Vec<String>), String> {
    parse_overrides_with(source, scope, &ServerSet::current())
}

fn parse_overrides_with(
    source: &str,
    scope: Scope,
    servers: &ServerSet,
) -> Result<(Overrides, Vec<String>), String> {
    let stripped = jsonc::strip(source);
    if stripped.trim().is_empty() {
        return Ok(Default::default());
    }
    let value: Value = serde_json::from_str(&stripped).map_err(|error| error.to_string())?;
    let Value::Object(file) = value else {
        return Err("expected an object of settings".to_owned());
    };
    let defaults = to_map(&Settings::default());
    let mut accepted = Overrides::new();
    let mut errors = Vec::new();
    for (key, value) in file {
        let Some(default) = defaults.get(&key) else {
            errors.push(format!("unknown setting \"{key}\""));
            continue;
        };
        if scope == Scope::Project && !PROJECT_KEYS.contains(&key.as_str()) {
            errors.push(format!(
                "\"{key}\" can only be set in the user settings.json"
            ));
            continue;
        }
        let keyed = matches!(
            key.as_str(),
            "language_servers" | "languages" | "editor_languages"
        );
        if let (Value::Object(default_fields), Value::Object(fields)) = (default, &value)
            && !keyed
        {
            for (field, value) in fields {
                let name = format!("{key}.{field}");
                if !default_fields.contains_key(field) {
                    errors.push(format!("unknown setting \"{name}\""));
                    continue;
                }
                let mut candidate = accepted.clone();
                let nested = candidate
                    .entry(key.clone())
                    .or_insert_with(|| Value::Object(Map::new()));
                nested[field] = value.clone();
                accept(
                    &defaults,
                    &mut accepted,
                    candidate,
                    &name,
                    value,
                    &mut errors,
                );
            }
            continue;
        }
        // A bad rule is dropped with a message; the rules around it still apply.
        let value = match key.as_str() {
            "file_associations" => valid_associations(value, &mut errors),
            "language_servers" => valid_servers(value, &mut errors, servers),
            "languages" => valid_languages(value, &mut errors),
            _ => value,
        };
        let mut candidate = accepted.clone();
        candidate.insert(key.clone(), value.clone());
        accept(
            &defaults,
            &mut accepted,
            candidate,
            &key,
            &value,
            &mut errors,
        );
    }
    Ok((accepted, errors))
}

/// The rules of a `file_associations` array that are well formed and name a
/// known language. Anything that is not an array is left for `accept` to report.
fn valid_associations(value: Value, errors: &mut Vec<String>) -> Value {
    let Value::Array(items) = value else {
        return value;
    };
    let registry = Registry::builtin();
    let mut kept = Vec::new();
    for (index, item) in items.into_iter().enumerate() {
        let checked = serde_json::from_value::<FileAssociation>(item.clone())
            .map_err(|error| error.to_string())
            .and_then(|rule| validate_association(registry, rule.spec()));
        match checked {
            Ok(()) => kept.push(item),
            Err(error) => errors.push(format!("file_associations[{index}]: {error}")),
        }
    }
    Value::Array(kept)
}

/// The entries of the `language_servers` object that are well formed: the
/// catalogued ones with allowed fields, and valid registrations of your own.
fn valid_servers(value: Value, errors: &mut Vec<String>, servers: &ServerSet) -> Value {
    let Value::Object(entries) = value else {
        return value;
    };
    let mut kept = Map::new();
    for (name, item) in entries {
        let checked = serde_json::from_value::<LanguageServerSetting>(item.clone())
            .map_err(|error| error.to_string())
            .and_then(|mut entry| {
                entry.server = name.clone();
                entry.validate(servers)
            });
        match checked {
            Ok(()) => {
                kept.insert(name, item);
            }
            Err(error) => errors.push(format!("language_servers.{name}: {error}")),
        }
    }
    Value::Object(kept)
}

/// The entries of the `languages` object that are well formed.
fn valid_languages(value: Value, errors: &mut Vec<String>) -> Value {
    let Value::Object(entries) = value else {
        return value;
    };
    let mut kept = Map::new();
    for (name, item) in entries {
        match serde_json::from_value::<super::servers::LanguageSetting>(item.clone()) {
            Ok(_) => {
                kept.insert(name, item);
            }
            Err(error) => errors.push(format!("languages.{name}: {error}")),
        }
    }
    Value::Object(kept)
}

/// Keep `candidate` when it still deserializes, otherwise report why.
fn accept(
    defaults: &Map<String, Value>,
    accepted: &mut Overrides,
    candidate: Overrides,
    name: &str,
    value: &Value,
    errors: &mut Vec<String>,
) {
    // Unknown theme ids deserialize as the default, so check them here.
    if name == "theme"
        && let Some(id) = value.as_str()
        && ThemeId::parse(id).is_none()
    {
        errors.push(format!("theme: unknown theme \"{id}\""));
        return;
    }
    let mut merged = defaults.clone();
    overlay_map(&mut merged, &candidate);
    match serde_json::from_value::<Settings>(Value::Object(merged)) {
        Ok(settings) => match settings
            .lsp
            .validate()
            .and_then(|()| settings.markdown.validate())
            .and_then(|()| settings.encoding.new_file.validate())
            .and_then(|()| settings.editor.validate())
            .and_then(|()| {
                settings
                    .editor_languages
                    .values()
                    .try_for_each(|v| v.validate())
            }) {
            Ok(()) => *accepted = candidate,
            Err(error) => errors.push(error),
        },
        Err(error) => errors.push(format!("{name}: {error}")),
    }
}

/// `base` with `overrides` applied on top.
pub fn overlay(base: &Settings, overrides: &Overrides) -> Settings {
    if overrides.is_empty() {
        return base.clone();
    }
    let mut merged = to_map(base);
    overlay_map(&mut merged, overrides);
    serde_json::from_value(Value::Object(merged)).unwrap_or_else(|_| base.clone())
}

fn overlay_map(base: &mut Map<String, Value>, overrides: &Overrides) {
    for (key, value) in overrides {
        match (base.get_mut(key), value) {
            (Some(Value::Object(fields)), Value::Object(overrides)) => {
                for (field, value) in overrides {
                    fields.insert(field.clone(), value.clone());
                }
            }
            _ => {
                base.insert(key.clone(), value.clone());
            }
        }
    }
}

fn to_map(settings: &Settings) -> Map<String, Value> {
    match serde_json::to_value(settings) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

/// Edit `text` so it parses to `new` instead of `current`, touching only
/// the members that changed. A value set back to its default is removed.
pub(crate) fn patch(text: &str, current: &Settings, new: &Settings) -> Option<String> {
    let defaults = to_map(&Settings::default());
    let current = to_map(current);
    let mut text = text.to_owned();
    for (key, value) in to_map(new) {
        let (Some(old), Some(default)) = (current.get(&key), defaults.get(&key)) else {
            continue;
        };
        if *old == value {
            continue;
        }
        match (&value, default, old) {
            (Value::Object(fields), Value::Object(default_fields), Value::Object(old_fields)) => {
                if fields == default_fields {
                    text = jsonc::set(&text, &[&key], None)?;
                    continue;
                }
                for (field, value) in fields {
                    if old_fields.get(field) == Some(value) {
                        continue;
                    }
                    let stored = (default_fields.get(field) != Some(value)).then_some(value);
                    text = jsonc::set(&text, &[&key, field], stored)?;
                }
            }
            _ => {
                let stored = (value != *default).then_some(&value);
                text = jsonc::set(&text, &[&key], stored)?;
            }
        }
    }
    Some(text)
}

/// Contents of a new `settings.json`: a header listing every setting with
/// its default, then an empty object.
pub fn template() -> String {
    let defaults = serde_json::to_string_pretty(&Settings::default()).expect("settings serialize");
    let mut text = String::from(
        "// Loom settings. Only values that differ from the defaults belong here;\n\
         // the Settings page writes its changes to this file as well.\n\
         //\n\
         // Every setting with its default value:\n",
    );
    for line in defaults.lines() {
        text.push_str("//   ");
        text.push_str(line);
        text.push('\n');
    }
    text.push_str("{\n}\n");
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::settings::theme_id::REGISTRY_TURN;

    #[test]
    fn settings_round_trip_as_json() {
        let _turn = REGISTRY_TURN.lock().unwrap_or_else(|e| e.into_inner());
        ThemeId::set_contributed(&["test.settings.ember"]);
        let settings = Settings {
            theme: ThemeId::parse("test.settings.ember").unwrap(),
            editor: Default::default(),
            editor_languages: Default::default(),
            encoding: Default::default(),
            ui_scale: Some(130),
            default_shell: ShellKind::Bash,
            terminal_profiles: vec![crate::features::terminal::profiles::ShellProfile {
                id: 7,
                name: "My shell".into(),
                program: "/custom/fish".into(),
                args: vec!["--login".into()],
            }],
            terminal_cursor_blink: false,
            smooth_caret: false,
            markdown: Default::default(),
            explorer_auto_reveal: false,
            git_inline_blame: true,
            git_split_diff: true,
            git_tree_view: true,
            vim: VimSettings {
                enabled: true,
                smart_case: true,
                ..VimSettings::default()
            },
            check_for_updates: false,
            editor_font: FontSettings {
                family: "Fira Code".into(),
                size: 14,
                weight: 600,
                line_height: 150,
            },
            terminal_font: FontSettings {
                size: 13,
                ..FontSettings::TERMINAL
            },
            file_associations: vec![FileAssociation {
                pattern: "**/*.custom".into(),
                language: "rust".into(),
            }],
            language_servers: vec![LanguageServerSetting {
                enabled: false,
                args: Some(vec!["-v".into()]),
                initialization_options: Some(serde_json::json!({ "a": 1 })),
                ..LanguageServerSetting::named("rust-analyzer")
            }],
            languages: Default::default(),
            lsp: Default::default(),
        };

        let json = serde_json::to_string(&settings).unwrap();
        let decoded: Settings = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, settings);
        ThemeId::set_contributed(&[]);
    }

    #[test]
    fn file_associations_keep_good_rules_and_report_each_bad_one() {
        let source = r##"{
            "file_associations": [
                { "pattern": "**/*.custom", "language": "rust" },
                { "pattern": "[x].a", "language": "rust" },
                { "pattern": "*.b", "language": "klingon" },
                { "pattern": "*.c" },
                { "pattern": "*.d", "language": "ts" },
                { "pattern": "*.e", "language": "rust", "extra": true },
                "not an object"
            ]
        }"##;
        let outcome = parse(source).unwrap();
        let kept: Vec<_> = outcome
            .settings
            .file_associations
            .iter()
            .map(|rule| (rule.pattern.as_str(), rule.language.as_str()))
            .collect();
        assert_eq!(kept, [("**/*.custom", "rust"), ("*.d", "ts")]);
        assert_eq!(outcome.errors.len(), 5, "{:?}", outcome.errors);
        assert!(
            outcome.errors[0].contains("file_associations[1]")
                && outcome.errors[0].contains("not supported")
        );
        assert!(
            outcome.errors[1].contains("file_associations[2]")
                && outcome.errors[1].contains("unknown language")
        );
        assert!(outcome.errors[2].contains("file_associations[3]"));
        assert!(outcome.errors[3].contains("file_associations[5]"));
        assert!(outcome.errors[4].contains("file_associations[6]"));
    }

    #[test]
    fn language_servers_keep_good_entries_and_report_each_bad_one() {
        let outcome = parse_with(
            r#"{ "language_servers": {
                "rust-analyzer": { "args": ["-v"] },
                "nope": { "enabled": false },
                "bad": { "initialization_options": 3 },
                "worse": { "surprise": true }
            } }"#,
            &crate::features::language_service::server_set::rust_analyzer_for_tests(),
        )
        .unwrap();
        assert_eq!(outcome.settings.language_servers.len(), 1);
        let kept = &outcome.settings.language_servers[0];
        assert_eq!(kept.server, "rust-analyzer");
        assert_eq!(kept.args.as_deref(), Some(&["-v".to_owned()][..]));
        assert_eq!(outcome.errors.len(), 3, "{:?}", outcome.errors);
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.contains("language_servers.nope"))
        );
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.contains("language_servers.bad"))
        );
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.contains("language_servers.worse"))
        );
    }

    #[test]
    fn the_program_of_a_catalogued_server_cannot_be_set() {
        let outcome = parse_with(
            r#"{ "language_servers": { "rust-analyzer": { "command": "/bin/evil" } } }"#,
            &crate::features::language_service::server_set::rust_analyzer_for_tests(),
        )
        .unwrap();
        assert!(outcome.settings.language_servers.is_empty());
        assert!(outcome.errors[0].contains("cannot be changed"));
    }

    #[test]
    fn a_server_of_your_own_needs_an_absolute_program() {
        let own = |command: &str| {
            format!(
                r#"{{ "language_servers": {{ "mine": {{ "command": {command:?},
                    "languages": [{{ "language": "rust", "language_id": "rust" }}],
                    "root": {{ "markers": ["Cargo.toml"] }} }} }} }}"#
            )
        };
        let servers = crate::features::language_service::server_set::rust_analyzer_for_tests();
        let relative = parse_with(&own("mine"), &servers).unwrap();
        assert!(relative.settings.language_servers.is_empty());
        assert!(relative.errors[0].contains("absolute"));
        let absolute = if cfg!(windows) {
            "C:\\tools\\mine.exe"
        } else {
            "/opt/mine"
        };
        let good = parse_with(&own(absolute), &servers).unwrap();
        assert!(good.errors.is_empty(), "{:?}", good.errors);
        assert_eq!(good.settings.language_servers[0].server, "mine");
    }

    #[test]
    fn languages_and_lsp_are_checked_entry_by_entry() {
        let outcome = parse_with(
            r#"{ "languages": {
                    "rust": { "language_servers": ["a", "..."], "format_on_save": true },
                    "bad": { "surprise": 1 } },
                 "lsp": { "completion": { "auto": false }, "hover_delay_ms": 100 } }"#,
            &crate::features::language_service::server_set::rust_analyzer_for_tests(),
        )
        .unwrap();
        assert_eq!(outcome.settings.languages.len(), 1);
        assert!(outcome.settings.languages["rust"].format_on_save);
        assert!(outcome.errors.iter().any(|e| e.contains("languages.bad")));
        assert!(!outcome.settings.lsp.completion.auto);
        assert_eq!(outcome.settings.lsp.hover_delay_ms, 100);
        let bad = parse_with(
            r#"{ "lsp": { "semantic_highlighting": "loud" } }"#,
            &crate::features::language_service::server_set::rust_analyzer_for_tests(),
        )
        .unwrap();
        assert_eq!(bad.settings.lsp.semantic_highlighting, "combined");
        assert!(bad.errors[0].contains("semantic_highlighting"));
    }

    #[test]
    fn projects_cannot_choose_which_program_runs() {
        let (overrides, errors) = parse_overrides(
            r#"{ "language_servers": { "rust-analyzer": { "args": ["--evil"] } } }"#,
            Scope::Project,
        )
        .unwrap();
        assert!(overrides.is_empty());
        assert!(errors[0].contains("can only be set in the user settings.json"));
    }

    #[test]
    fn file_associations_must_be_a_list() {
        let outcome = parse(r#"{ "file_associations": "rust", "smooth_caret": false }"#).unwrap();
        assert!(outcome.settings.file_associations.is_empty());
        assert!(!outcome.settings.smooth_caret, "other settings still apply");
        assert_eq!(outcome.errors.len(), 1);
        assert!(outcome.errors[0].contains("file_associations"));
    }

    #[test]
    fn projects_may_set_file_associations_and_nothing_personal() {
        let (overrides, errors) = parse_overrides(
            r#"{ "file_associations": [{ "pattern": "*.x", "language": "csharp" }], "theme": "ember" }"#,
            Scope::Project,
        )
        .unwrap();
        assert_eq!(overrides.len(), 1);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("theme"));
        let rules: Vec<FileAssociation> =
            serde_json::from_value(overrides["file_associations"].clone()).unwrap();
        assert_eq!(rules.len(), 1);
    }

    #[test]
    fn unchanged_file_associations_are_never_written_back() {
        let text = "{\n  // mine\n  \"file_associations\": [{ \"pattern\": \"*.x\", \"language\": \"rust\" }, { \"pattern\": \"*.y\", \"language\": \"nope\" }]\n}\n";
        let current = parse(text).unwrap().settings;
        assert_eq!(
            current.file_associations.len(),
            1,
            "the bad rule is not loaded"
        );
        let mut new = current.clone();
        new.smooth_caret = false;
        let patched = patch(text, &current, &new).unwrap();
        assert!(
            patched.contains("// mine") && patched.contains("*.y"),
            "the file keeps its own text: {patched}"
        );
        assert!(patched.contains("smooth_caret"));
    }

    #[test]
    fn missing_fields_use_defaults() {
        let decoded: Settings = serde_json::from_str("{}").unwrap();

        assert_eq!(decoded, Settings::default());
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;
    use crate::features::settings::theme_id::REGISTRY_TURN;

    #[test]
    fn interface_scale_saves_reloads_and_resets_preserving_comments() {
        let text = "{\n  // keep my settings\n  \"smooth_caret\": false\n}\n";
        let original = parse(text).unwrap().settings;
        let mut scaled = original.clone();
        scaled.ui_scale = Some(150);
        let saved = patch(text, &original, &scaled).unwrap();
        assert!(saved.contains("// keep my settings"));
        assert_eq!(parse(&saved).unwrap().settings, scaled);
        assert_eq!(patch(&saved, &scaled, &original).unwrap(), text);
        assert_eq!(
            parse(r#"{"ui_scale": null}"#).unwrap().settings.ui_scale,
            None
        );
        let invalid = parse(r#"{"ui_scale": "large", "smooth_caret": false}"#).unwrap();
        assert_eq!(invalid.errors.len(), 1);
        assert_eq!(invalid.settings.ui_scale, None);
        assert!(!invalid.settings.smooth_caret);
        let (overrides, errors) = parse_overrides(r#"{"ui_scale": 150}"#, Scope::Project).unwrap();
        assert!(overrides.is_empty());
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn a_partial_file_overrides_only_its_values() {
        let _turn = REGISTRY_TURN.lock().unwrap_or_else(|e| e.into_inner());
        ThemeId::set_contributed(&["test.settings.ember"]);
        let outcome = parse(
            r#"// mine
            {
                "theme": "test.settings.ember",
                "terminal_font": { "size": 14 },
                "vim": { "enabled": true, },
            }"#,
        )
        .unwrap();
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        let settings = outcome.settings;
        assert_eq!(
            settings.theme,
            ThemeId::parse("test.settings.ember").unwrap()
        );
        ThemeId::set_contributed(&[]);
        assert_eq!(settings.terminal_font.size, 14);
        // Unlisted nested fields keep their own defaults, not the editor's.
        assert_eq!(
            settings.terminal_font.line_height,
            FontSettings::TERMINAL.line_height
        );
        assert!(settings.vim.enabled);
        assert!(settings.smooth_caret);
    }

    #[test]
    fn font_weights_load_patch_and_reset_without_losing_other_preferences() {
        let source = "{\n  // keep my font\n  \"editor_font\": {\"family\": \"Consolas\", \"size\": 14},\n  \"terminal_font\": {\"line_height\": 180}\n}\n";
        let original = parse(source).unwrap().settings;
        assert_eq!(original.editor_font.weight, 400);
        assert_eq!(original.terminal_font.weight, 400);
        let mut changed = original.clone();
        changed.editor_font.weight = 600;
        changed.terminal_font.weight = 300;
        let saved = patch(source, &original, &changed).unwrap();
        assert!(saved.contains("// keep my font"));
        let loaded = parse(&saved).unwrap();
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert_eq!(loaded.settings, changed);
        let reset = patch(&saved, &changed, &original).unwrap();
        assert!(!reset.contains("\"weight\""));
        assert_eq!(parse(&reset).unwrap().settings, original);

        let invalid =
            parse(r#"{"editor_font":{"weight":"bold","size":16},"terminal_font":{"weight":700}}"#)
                .unwrap();
        assert_eq!(invalid.errors.len(), 1);
        assert!(invalid.errors[0].starts_with("editor_font.weight:"));
        assert_eq!(invalid.settings.editor_font.weight, 400);
        assert_eq!(invalid.settings.editor_font.size, 16);
        assert_eq!(invalid.settings.terminal_font.weight, 700);
    }

    #[test]
    fn bad_values_are_reported_without_resetting_the_rest() {
        let outcome = parse(
            r#"{
                "smooth_caret": false,
                "theme": "no-such-theme",
                "git_tree_view": "yes",
                "thme": "ember",
                "editor_font": { "size": "big", "line_height": 150, "width": 7 }
            }"#,
        )
        .unwrap();
        let settings = outcome.settings;
        assert!(!settings.smooth_caret);
        assert_eq!(settings.theme, ThemeId::default());
        assert!(!settings.git_tree_view);
        assert_eq!(settings.editor_font.size, FontSettings::EDITOR.size);
        assert_eq!(settings.editor_font.line_height, 150);
        assert_eq!(outcome.errors.len(), 5, "{:?}", outcome.errors);
        assert!(outcome.errors.iter().any(|e| e.contains("\"thme\"")));
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.contains("editor_font.width"))
        );
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.starts_with("editor_font.size:"))
        );
    }

    #[test]
    fn unusable_files_are_errors() {
        assert!(parse("{ \"theme\": ").is_err());
        assert!(parse("[]").is_err());
        assert_eq!(
            parse("// nothing yet\n").unwrap().settings,
            Settings::default()
        );
    }

    #[test]
    fn patch_writes_only_changed_members_and_drops_defaults() {
        let text = "{\n  // keep me\n  \"theme\": \"ember\",\n  \"smooth_caret\": false\n}\n";
        let current = parse(text).unwrap().settings;

        let mut new = current.clone();
        new.git_tree_view = true;
        new.editor_font.size = 14;
        let patched = patch(text, &current, &new).unwrap();
        assert_eq!(
            patched,
            "{\n  // keep me\n  \"theme\": \"ember\",\n  \"smooth_caret\": false,\n  \"git_tree_view\": true,\n  \"editor_font\": {\n    \"size\": 14\n  }\n}\n"
        );
        assert_eq!(parse(&patched).unwrap().settings, new);

        let mut reset = new.clone();
        reset.smooth_caret = true;
        reset.editor_font = FontSettings::EDITOR;
        let patched = patch(&patched, &new, &reset).unwrap();
        assert_eq!(
            patched,
            "{\n  // keep me\n  \"theme\": \"ember\",\n  \"git_tree_view\": true\n}\n"
        );
        assert_eq!(parse(&patched).unwrap().settings, reset);
    }

    #[test]
    fn a_byte_order_mark_is_read_and_kept_by_patches() {
        let text = "\u{feff}{ \"smooth_caret\": false }\n";
        let outcome = parse(text).unwrap();
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert!(!outcome.settings.smooth_caret);

        let mut new = outcome.settings.clone();
        new.git_tree_view = true;
        let patched = patch(text, &outcome.settings, &new).unwrap();
        assert!(patched.starts_with("\u{feff}{"), "{patched:?}");
        assert_eq!(patched.matches('\u{feff}').count(), 1);
        assert_eq!(parse(&patched).unwrap().settings, new);
    }

    #[test]
    fn template_parses_to_the_defaults_and_accepts_patches() {
        let template = template();
        assert_eq!(parse(&template).unwrap().settings, Settings::default());
        let new = Settings {
            vim: VimSettings {
                enabled: true,
                ..VimSettings::default()
            },
            ..Settings::default()
        };
        let patched = patch(&template, &Settings::default(), &new).unwrap();
        assert!(patched.starts_with(&template[..template.len() - "{\n}\n".len()]));
        assert!(patched.ends_with("{\n  \"vim\": {\n    \"enabled\": true\n  }\n}\n"));
        assert_eq!(parse(&patched).unwrap().settings, new);
    }
}
