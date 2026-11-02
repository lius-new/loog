//! Pure owned preference values; mutable UI drafts and runtime services live elsewhere.
use super::theme_id::ThemeId;
use crate::features::terminal::ShellKind;
use std::path::PathBuf;
use std::sync::Arc;
#[derive(Clone)]
pub struct Values {
    pub editor: super::editing::EditorSettings,
    pub editor_languages: std::collections::BTreeMap<String, super::editing::EditorOverrides>,
    pub theme: ThemeId,
    pub encoding: crate::domain::encoding::EncodingSettings,
    pub ui_scale: Option<u16>,
    pub default_shell: ShellKind,
    pub terminal_profiles: Vec<crate::features::terminal::profiles::ShellProfile>,
    pub shell_options: Vec<crate::features::terminal::profiles::ShellOption>,
    pub terminal_cursor_blink: bool,
    pub smooth_caret: bool,
    pub explorer_auto_reveal: bool,
    pub markdown: super::markdown::MarkdownSettings,
    pub check_for_updates: bool,
    pub editor_font: super::font::FontSettings,
    pub terminal_font: super::font::FontSettings,
    pub file_associations: Vec<super::rules::FileAssociation>,
    pub languages: std::collections::BTreeMap<String, super::language::LanguageSetting>,
    pub language_features: super::language::LanguageFeatures,
    pub project_settings: Vec<(PathBuf, super::project::ProjectSettings)>,
}
#[derive(Clone)]
pub struct Model {
    values: Arc<Values>,
}
impl Model {
    pub fn new(values: Values) -> Self {
        Self {
            values: Arc::new(values),
        }
    }
    pub fn values(&self) -> &Values {
        &self.values
    }
    pub(crate) fn values_mut(&mut self) -> &mut Values {
        Arc::make_mut(&mut self.values)
    }
}
