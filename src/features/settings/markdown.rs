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
    pub const MIN_FONT_SIZE: u16 = 10;
    pub const MAX_FONT_SIZE: u16 = 32;
    pub const MIN_LINE_HEIGHT: u16 = 14;
    pub const MAX_LINE_HEIGHT: u16 = 80;
    pub fn validate(self) -> Result<(), String> {
        if !(Self::MIN_FONT_SIZE..=Self::MAX_FONT_SIZE).contains(&self.font_size) {
            return Err("markdown.font_size must be between 10 and 32".into());
        }
        if !(Self::MIN_LINE_HEIGHT..=Self::MAX_LINE_HEIGHT).contains(&self.line_height) {
            return Err("markdown.line_height must be between 14 and 80".into());
        }
        if !(320..=1800).contains(&self.max_width) {
            return Err("markdown.max_width must be between 320 and 1800".into());
        }
        Ok(())
    }
}
