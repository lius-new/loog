//! Pure persisted font preferences and clamping rules.
use serde::{Deserialize, Serialize};

pub const MIN_SIZE: u16 = 8;
pub const MAX_SIZE: u16 = 32;
pub const MIN_WEIGHT: u16 = 100;
pub const MAX_WEIGHT: u16 = 900;
pub const WEIGHT_STEP: u16 = 100;
pub const MIN_LINE_HEIGHT: u16 = 100;
pub const MAX_LINE_HEIGHT: u16 = 250;
pub const LINE_HEIGHT_STEP: u16 = 10;

/// A code font preference (`"editor_font"` / `"terminal_font"` in
/// `settings.json`).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default)]
pub struct FontSettings {
    /// Preferred family. Empty inherits: the editor falls back to the
    /// built-in monospace stack, the terminal to the editor's family.
    pub family: String,
    /// Font size in logical pixels.
    pub size: u16,
    /// Font weight: 400 is regular, 700 is bold.
    pub weight: u16,
    /// Line height as a percentage of `size`.
    pub line_height: u16,
}

impl FontSettings {
    pub const EDITOR: Self = Self::new(12, 160);
    pub const TERMINAL: Self = Self::new(12, 160);

    const fn new(size: u16, line_height: u16) -> Self {
        Self {
            family: String::new(),
            size,
            weight: 400,
            line_height,
        }
    }

    /// The same preference with every field in its supported range.
    pub fn clamped(&self) -> Self {
        Self {
            family: self.family.trim().to_owned(),
            size: self.size.clamp(MIN_SIZE, MAX_SIZE),
            weight: self.weight.clamp(MIN_WEIGHT, MAX_WEIGHT),
            line_height: self.line_height.clamp(MIN_LINE_HEIGHT, MAX_LINE_HEIGHT),
        }
    }

    /// Line height in whole pixels, so rows stay on the pixel grid.
    pub fn line_px(&self) -> f32 {
        let font = self.clamped();
        (f32::from(font.size) * f32::from(font.line_height) / 100.0).round()
    }
}

impl Default for FontSettings {
    fn default() -> Self {
        Self::EDITOR
    }
}
