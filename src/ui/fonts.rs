//! Code fonts for the editor and terminal, chosen on the Settings page.
//!
//! `FontSettings` is the persisted preference; `Metrics` is the resolved
//! family list, size, weight, line height and character advance every code surface
//! lays out with. The active metrics are set once per frame from `AppState`
//! and read through `editor()` and `terminal()`, like the palette in `theme`.

#[cfg(test)]
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use lgui::prelude::{Color, TextAlign, TextStyle, UiRect};
use lgui::text::{self as text_layout, TextLayoutRequest};

use crate::ui::theme::MONO_FAMILIES;

pub use crate::features::settings::font::*;

/// Shared proportional stack for UI and rich documents. Latin text uses the
/// platform UI face; missing scripts fall back without changing code fonts.
#[cfg(target_os = "windows")]
pub const SANS_FAMILIES: &[&str] = &["Segoe UI", "Microsoft YaHei", "sans-serif"];
#[cfg(target_os = "macos")]
pub const SANS_FAMILIES: &[&str] = &["Helvetica Neue", "PingFang SC", "sans-serif"];
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub const SANS_FAMILIES: &[&str] = &["Noto Sans", "Noto Sans CJK SC", "DejaVu Sans", "sans-serif"];

/// Resolved layout values of one code font.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub families: &'static [&'static str],
    pub size: f32,
    pub weight: i32,
    pub line_h: f32,
    /// Monospace advance, measured with the renderer when it is available.
    pub char_w: f32,
}

impl Metrics {
    fn resolve(font: &FontSettings, inherited_family: &str) -> Self {
        let font = font.clamped();
        let family = if font.family.is_empty() {
            inherited_family
        } else {
            &font.family
        };
        let families = families(family);
        let size = f32::from(font.size);
        let weight = i32::from(font.weight);
        Self {
            families,
            size,
            weight,
            line_h: font.line_px(),
            char_w: measured_advance(families, size, weight).unwrap_or(size * 0.6),
        }
    }

    pub fn style(&self, color: Color) -> TextStyle {
        TextStyle::new(color, self.size, self.weight).font_families(self.families)
    }

    pub fn bold(&self, color: Color) -> TextStyle {
        TextStyle::new(color, self.size, self.weight.max(700)).font_families(self.families)
    }

    pub fn right(&self, color: Color) -> TextStyle {
        let mut style = self.style(color);
        style.align = TextAlign::Right;
        style
    }

    /// Scale line numbers with the font, then add a fixed narrow folding column.
    pub fn gutter_w(&self) -> f32 {
        (crate::ui::theme::GUTTER_W * self.size / 12.0)
            .round()
            .max(crate::ui::theme::GUTTER_W)
            + crate::ui::theme::GUTTER_FOLD_W
    }

    /// Reserve the widest line number without changing width while scrolling.
    pub fn gutter_w_for_lines(&self, lines: usize) -> f32 {
        let digits = lines.max(1).ilog10() + 1;
        let numbers = (digits as f32 * self.char_w).ceil() + 4.0;
        self.gutter_w().max(
            crate::ui::theme::GUTTER_ACTION_W
                + numbers
                + crate::ui::theme::GUTTER_NUMBER_PAD
                + crate::ui::theme::GUTTER_FOLD_W
                + crate::ui::theme::GUTTER_FOLD_PAD,
        )
    }
}

/// The family stack for `family`: the user's choice first, then the
/// built-in monospace stack so a missing font still renders monospace.
pub fn families(family: &str) -> &'static [&'static str] {
    if family.is_empty() {
        return MONO_FAMILIES;
    }
    // Leaked once per distinct name; a session only ever sees a handful.
    static INTERNED: OnceLock<Mutex<HashMap<String, &'static [&'static str]>>> = OnceLock::new();
    let mut interned = INTERNED
        .get_or_init(Default::default)
        .lock()
        .expect("font family cache poisoned");
    interned.entry(family.to_owned()).or_insert_with(|| {
        let name: &'static str = Box::leak(family.to_owned().into_boxed_str());
        let stack: Vec<&'static str> = std::iter::once(name)
            .chain(MONO_FAMILIES.iter().copied().filter(|f| *f != name))
            .collect();
        Box::leak(stack.into_boxed_slice())
    })
}

/// Average advance of a digit run, or `None` before a text system exists.
fn measured_advance(families: &'static [&'static str], size: f32, weight: i32) -> Option<f32> {
    const SAMPLE: &str = "0000000000000000";
    let mut request = TextLayoutRequest::single_line(
        SAMPLE,
        UiRect::new(0.0, 0.0, 10_000.0, size * 2.0),
        size,
        weight,
    );
    request.font_families = families;
    text_layout::layout(&request)
        .map(|layout| layout.width / SAMPLE.len() as f32)
        .filter(|width| width.is_finite() && *width > 0.0)
}

struct Active {
    editor: FontSettings,
    terminal: FontSettings,
    editor_metrics: Metrics,
    terminal_metrics: Metrics,
    /// Whether the advances came from the renderer; estimates are retried.
    measured: bool,
}

thread_local! {
    static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) };
    #[cfg(test)]
    static OVERRIDE: Cell<Option<Metrics>> = const { Cell::new(None) };
}

/// Applies the preferences; cheap when nothing changed.
pub fn set(editor: &FontSettings, terminal: &FontSettings) {
    ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        if let Some(current) = active.as_ref()
            && current.measured
            && current.editor == *editor
            && current.terminal == *terminal
        {
            return;
        }
        *active = Some(resolve(editor, terminal));
    });
}

fn resolve(editor: &FontSettings, terminal: &FontSettings) -> Active {
    let editor_metrics = Metrics::resolve(editor, "");
    let terminal_metrics = Metrics::resolve(terminal, editor.family.trim());
    Active {
        editor: editor.clone(),
        terminal: terminal.clone(),
        measured: measured_advance(
            editor_metrics.families,
            editor_metrics.size,
            editor_metrics.weight,
        )
        .is_some(),
        editor_metrics,
        terminal_metrics,
    }
}

fn active<T>(read: impl FnOnce(&Active) -> T) -> T {
    ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        let active =
            active.get_or_insert_with(|| resolve(&FontSettings::EDITOR, &FontSettings::TERMINAL));
        read(active)
    })
}

/// Metrics of the editor font.
pub fn editor() -> Metrics {
    active(|active| active.editor_metrics)
}

/// Metrics of the terminal font.
pub fn terminal() -> Metrics {
    #[cfg(test)]
    if let Some(metrics) = OVERRIDE.with(Cell::get) {
        return metrics;
    }
    active(|active| active.terminal_metrics)
}

/// Replaces the terminal metrics on this test thread until the guard drops.
#[cfg(test)]
pub fn override_terminal(metrics: Metrics) -> impl Drop {
    struct Restore(Option<Metrics>);
    impl Drop for Restore {
        fn drop(&mut self) {
            OVERRIDE.with(|cell| cell.set(self.0));
        }
    }
    Restore(OVERRIDE.with(|cell| cell.replace(Some(metrics))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_keep_the_original_metrics() {
        assert_eq!(FontSettings::EDITOR.line_px(), 19.0);
        assert_eq!(FontSettings::TERMINAL.line_px(), 19.0);
        let editor = Metrics::resolve(&FontSettings::EDITOR, "");
        assert_eq!(editor.families, MONO_FAMILIES);
        assert_eq!(editor.size, 12.0);
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let font = FontSettings {
            family: "  Fira Code ".into(),
            size: 200,
            weight: 0,
            line_height: 10,
        }
        .clamped();
        assert_eq!(font.family, "Fira Code");
        assert_eq!(font.size, MAX_SIZE);
        assert_eq!(font.weight, MIN_WEIGHT);
        assert_eq!(font.line_height, MIN_LINE_HEIGHT);
    }

    #[test]
    fn chosen_family_leads_the_fallback_stack() {
        let stack = families("Fira Code");
        assert_eq!(stack[0], "Fira Code");
        assert_eq!(&stack[1..], MONO_FAMILIES);
        assert!(std::ptr::eq(stack, families("Fira Code")));
        assert_eq!(families("Consolas")[0], "Consolas");
        assert_eq!(families("Consolas").len(), MONO_FAMILIES.len());
    }

    #[test]
    fn empty_terminal_family_follows_the_editor() {
        let editor = FontSettings {
            family: "Fira Code".into(),
            ..FontSettings::EDITOR
        };
        set(&editor, &FontSettings::TERMINAL);
        assert_eq!(terminal().families[0], "Fira Code");
        assert_eq!(terminal().line_h, 19.0);
        set(&FontSettings::EDITOR, &FontSettings::TERMINAL);
        assert_eq!(terminal().families, MONO_FAMILIES);
    }

    #[test]
    fn missing_fields_use_defaults() {
        let font: FontSettings = serde_json::from_str(r#"{"size": 14}"#).unwrap();
        assert_eq!(font.size, 14);
        assert_eq!(font.weight, 400);
        assert_eq!(font.line_height, FontSettings::EDITOR.line_height);
    }
}
