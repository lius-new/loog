//! Language badge colors are presentation policy.
use lgui::prelude::Color;

use crate::ui::theme;

use crate::domain::language::ResolvedLanguage;

impl ResolvedLanguage {
    /// The badge color for the language. Languages the theme has no color for
    /// read as muted text, which is also what plain text uses.
    pub fn badge_color(&self) -> Color {
        match self.name() {
            "csharp" => theme::c().badge.csharp,
            "rust" => theme::c().badge.rust,
            "typescript" => theme::c().badge.typescript,
            "javascript" => theme::c().badge.javascript,
            _ => theme::c().text_muted,
        }
    }
}
