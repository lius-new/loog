//! Generic language-provider preferences; process configuration belongs to extensions.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LanguageSetting {
    /// Provider kind to ordered extension/provider IDs; `...` includes the remainder.
    /// An absent or empty list uses normal priority order.
    pub providers: BTreeMap<String, Vec<String>>,
    pub format_on_save: bool,
    pub code_actions_on_save: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CompletionSettings {
    pub auto: bool,
    pub accept_on: String,
    pub words_fallback: bool,
}
impl Default for CompletionSettings {
    fn default() -> Self {
        Self {
            auto: true,
            accept_on: "enter_and_tab".into(),
            words_fallback: true,
        }
    }
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LanguageFeatures {
    pub hover_delay_ms: u64,
    pub completion: CompletionSettings,
    pub inlay_hints: bool,
    pub semantic_highlighting: String,
    pub max_document_bytes: usize,
}
impl Default for LanguageFeatures {
    fn default() -> Self {
        Self {
            hover_delay_ms: 500,
            completion: Default::default(),
            inlay_hints: false,
            semantic_highlighting: "combined".into(),
            max_document_bytes: 8 * 1024 * 1024,
        }
    }
}
impl LanguageFeatures {
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(
            self.completion.accept_on.as_str(),
            "enter_and_tab" | "enter" | "tab"
        ) {
            return Err(
                "language_features.completion.accept_on must be enter_and_tab, enter or tab".into(),
            );
        }
        if !matches!(self.semantic_highlighting.as_str(), "combined" | "off") {
            return Err("language_features.semantic_highlighting must be combined or off".into());
        }
        if self.hover_delay_ms > 10_000 {
            return Err("language_features.hover_delay_ms must be at most 10000".into());
        }
        if self.max_document_bytes < 1024 {
            return Err("language_features.max_document_bytes must be at least 1024".into());
        }
        Ok(())
    }
}
