//! Reading preferences are independent of Markdown syntax and resource policy.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct MarkdownSettings {
    pub font_size: u16,
    pub line_height: u16,
    pub max_width: u16,
}
impl Default for MarkdownSettings {
    fn default() -> Self {
        Self {
            font_size: 16,
            line_height: 26,
            max_width: 840,
        }
    }
}
impl MarkdownSettings {
    pub fn validate(self) -> Result<(), String> {
        if !(10..=32).contains(&self.font_size) {
            return Err("markdown.font_size must be between 10 and 32".into());
        }
        if !(14..=80).contains(&self.line_height) {
            return Err("markdown.line_height must be between 14 and 80".into());
        }
        if !(320..=1800).contains(&self.max_width) {
            return Err("markdown.max_width must be between 320 and 1800".into());
        }
        Ok(())
    }
}
