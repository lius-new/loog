//! Design tokens for the "Aura Code" editor (ported from code-preview.html).
//!
//! Centralizes color palettes, layout metrics and text-style helpers so every UI
//! module renders from a single source of truth.

use std::cell::Cell;

use crate::features::highlighting::{HighlightId, HighlightModifiers, HighlightStyle, StyleTable};
use lgui::prelude::{Color, Element, Stroke, TextAlign, TextStyle, UiRect, VisualStyle, panel};

// ---- Palettes ----------------------------------------------------------

/// Every color the UI draws with. Built-in themes are `Palette` values in
/// `THEMES`; the active one is read through `c()`.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    // Editor chrome
    pub bg: Color,          // primary editor background
    pub sidebar: Color,     // slightly darker panel
    pub surface: Color,     // elevated cards / inputs
    pub border: Color,      // divider borders
    pub active_line: Color, // active row / hover highlight
    pub active_tab: Color,  // contrasting editor tab background
    pub selection: Color,
    pub accent: Color,
    pub accent_hover: Color,
    pub on_accent: Color, // text and knobs drawn on an accent fill
    pub scrollbar: Color,
    pub shadow_alpha: u8, // opacity of popup drop shadows

    // Text, from most to least prominent
    pub text_bright: Color,
    pub text: Color,
    pub text_soft: Color,
    pub text_muted: Color,
    pub text_dim: Color,
    pub text_faint: Color,
    pub text_ghost: Color,

    // Status
    pub error: Color,
    pub error_text: Color,
    pub warning: Color,

    /// Syntax styles by highlight class; read through `syntax_style`.
    pub(crate) syntax: StyleTable,
    pub diff: DiffColors,
    pub conflict: ConflictColors,
    pub git: GitColors,
    pub badge: BadgeColors,
    /// The 16 ANSI terminal colors, as `0xRRGGBB`.
    pub ansi: [u32; 16],
}

impl Palette {
    /// The class's foreground color, or the normal text color when this theme
    /// gives the class none.
    pub(crate) fn syntax_color(&self, class: HighlightId) -> Color {
        self.syntax
            .resolve(class, HighlightModifiers::NONE)
            .fg
            .map_or(self.text, Color)
    }
}

/// Diff rows and the editor gutter's change markers.
#[derive(Clone, Copy, Debug)]
pub struct DiffColors {
    pub add_bg: Color,
    pub add_fg: Color,
    pub add_mark: Color,
    pub del_bg: Color,
    pub del_fg: Color,
    pub del_mark: Color,
    pub mod_mark: Color,
}

/// Merge-conflict rows. The current side reuses the diff's added colors and
/// the incoming side its modified mark; these are the tints with no diff twin.
#[derive(Clone, Copy, Debug)]
pub struct ConflictColors {
    pub incoming_bg: Color,
    pub marker_bg: Color,
}

/// Change-kind letters in Source Control and Explorer label colors.
#[derive(Clone, Copy, Debug)]
pub struct GitColors {
    pub added: Color,
    /// Green foreground for added and untracked files across Git views and Explorer.
    pub added_text: Color,
    pub modified: Color,
    pub deleted: Color,
    pub staged: Color,
    /// Labels of git-ignored paths in the Explorer.
    pub ignored: Color,
}

/// Tab badges.
#[derive(Clone, Copy, Debug)]
pub struct BadgeColors {
    pub csharp: Color,
    pub rust: Color,
    pub typescript: Color,
    pub javascript: Color,
    pub diff: Color,
}

pub struct Theme {
    pub id: &'static str,
    pub name: &'static str,
    pub palette: Palette,
}

/// Built-in themes; the first entry is the default. Every other theme comes
/// from an extension (Loom's own are in the bundled `loom.aura` package), and
/// starts from one of these.
pub static THEMES: &[Theme] = &[Theme {
    id: "aura-dark",
    name: "Aura Dark",
    palette: AURA_DARK,
}];

const AURA_DARK: Palette = Palette {
    bg: Color(0x14161b),
    sidebar: Color(0x111216),
    surface: Color(0x1c1f26),
    border: Color(0x262a35),
    active_line: Color(0x1a1d24),
    active_tab: Color(0x303030),
    selection: Color(0x273449),
    accent: Color(0x818cf8), // electric indigo
    accent_hover: Color(0x6366f1),
    on_accent: Color(0xf4f4f5),
    scrollbar: Color(0x27272a),
    shadow_alpha: 80,

    text_bright: Color(0xf4f4f5),
    text: Color(0xe4e4e7),
    text_soft: Color(0xd4d4d8),
    text_muted: Color(0xa1a1aa),
    text_dim: Color(0x71717a),
    text_faint: Color(0x52525b),
    text_ghost: Color(0x3f3f46),

    error: Color(0xf43f5e),
    error_text: Color(0xfda4af),
    warning: Color(0xfbbf24),

    syntax: StyleTable {
        classes: &[
            (HighlightId::KEYWORD, HighlightStyle::fg(0xf472b6)),
            (HighlightId::FUNCTION, HighlightStyle::fg(0x60a5fa)),
            (HighlightId::TYPE, HighlightStyle::fg(0xfbbf24)),
            (HighlightId::STRING, HighlightStyle::fg(0x4ade80)),
            (HighlightId::COMMENT, HighlightStyle::fg(0x64748b)),
            (HighlightId::NUMBER, HighlightStyle::fg(0xfb923c)),
            (HighlightId::PROPERTY, HighlightStyle::fg(0xa78bfa)),
            (HighlightId::CONSTANT_BUILTIN, HighlightStyle::fg(0xf472b6)),
            (HighlightId::VARIABLE_BUILTIN, HighlightStyle::fg(0xf472b6)),
            (HighlightId::CONSTRUCTOR, HighlightStyle::fg(0xfbbf24)),
            (HighlightId::MODULE, HighlightStyle::fg(0xfbbf24)),
        ],
        modifiers: &[],
    },
    diff: DiffColors {
        add_bg: Color(0x142a27), // rgba(16,185,129,.12) over bg
        add_fg: Color(0x6ee7b7),
        add_mark: Color(0x10b981),
        del_bg: Color(0x2f1b23), // rgba(244,63,94,.12) over bg
        del_fg: Color(0xfda4af),
        del_mark: Color(0xf43f5e),
        mod_mark: Color(0x60a5fa),
    },
    conflict: ConflictColors {
        incoming_bg: Color(0x1d2736),
        marker_bg: Color(0x342e1c),
    },
    git: GitColors {
        added: Color(0x38bdf8),
        added_text: Color(0x34d399),
        modified: Color(0xeab308),
        deleted: Color(0xf43f5e),
        staged: Color(0x34d399),
        ignored: Color(0x5f5f68),
    },
    badge: BadgeColors {
        csharp: Color(0xc084fc),
        rust: Color(0xfb923c),
        typescript: Color(0x60a5fa),
        javascript: Color(0xfbbf24),
        diff: Color(0xc084fc),
    },
    ansi: [
        0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, 0x666666,
        0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xffffff,
    ],
};

pub use crate::features::settings::theme_id::ThemeId;
impl ThemeId {
    /// The theme to draw with. A contributed theme that is gone draws as the default.
    pub fn theme(self) -> &'static Theme {
        match self.index().checked_sub(THEMES.len()) {
            None => &THEMES[self.index()],
            Some(at) => CONTRIBUTED
                .read()
                .unwrap()
                .get(at)
                .copied()
                .flatten()
                .unwrap_or(&THEMES[0]),
        }
    }
}

/// Themes from extension packages, by the index `ThemeId` gives them (after the
/// built-in ones). `None` once the package that provided one is gone.
static CONTRIBUTED: std::sync::RwLock<Vec<Option<&'static Theme>>> =
    std::sync::RwLock::new(Vec::new());

/// Make exactly these package themes available: `(id, name, palette)`. A theme
/// keeps its place when it is still listed; an unlisted one stops being offered.
pub(crate) fn set_package_themes(themes: Vec<(String, String, Palette)>) {
    let ids: Vec<&str> = themes.iter().map(|(id, ..)| id.as_str()).collect();
    let places = ThemeId::set_contributed(&ids);
    let mut slots = CONTRIBUTED.write().unwrap();
    for slot in slots.iter_mut() {
        *slot = None;
    }
    for ((id, name, palette), place) in themes.into_iter().zip(places) {
        let at = place - THEMES.len();
        if slots.len() <= at {
            slots.resize(at + 1, None);
        }
        let theme: &'static Theme = Box::leak(Box::new(Theme {
            id: Box::leak(id.into_boxed_str()),
            name: Box::leak(name.into_boxed_str()),
            palette,
        }));
        slots[at] = Some(theme);
    }
}

// Rendering state follows the UI thread, as font metrics do. Background
// Markdown requests carry their own Ink snapshot instead of consulting this.
thread_local! {
    static CURRENT: Cell<usize> = const { Cell::new(0) };
}

/// The active theme's style for a highlight class and its modifiers. Unset
/// attributes are left for the caller to default.
pub(crate) fn syntax_style(class: HighlightId, modifiers: HighlightModifiers) -> HighlightStyle {
    c().syntax.resolve(class, modifiers)
}

/// The active theme's foreground for a highlight class.
pub(crate) fn syntax_color(class: HighlightId) -> Color {
    c().syntax_color(class)
}

/// The active palette.
pub fn c() -> &'static Palette {
    &current().theme().palette
}

pub fn current() -> ThemeId {
    ThemeId::from_index(CURRENT.get())
}

pub fn set(id: ThemeId) {
    CURRENT.set(id.index());
}

/// Loom's visual policy. The library only receives explicit coverage parameters;
/// it does not inspect themes or choose a curve from the window background.
pub fn text_rendering(id: ThemeId) -> lgui::text::TextRenderingOptions {
    use super::text_rendering::{ColorPair, coverage};
    let defaults = super::text_rendering::defaults();
    let palette = &id.theme().palette;
    let pair = |foreground: Color, background: Color, weight| ColorPair {
        foreground: foreground.0,
        background: background.0,
        weight,
    };
    let primary = [
        pair(palette.text, palette.bg, 5.0),
        pair(palette.text_bright, palette.bg, 1.0),
        pair(palette.text_soft, palette.sidebar, 1.0),
        pair(palette.text_muted, palette.sidebar, 1.0),
        pair(palette.text_dim, palette.sidebar, 0.5),
        pair(palette.text, palette.surface, 1.0),
        pair(palette.text, palette.active_line, 0.5),
        pair(palette.text, palette.selection, 0.5),
        pair(palette.diff.add_fg, palette.diff.add_bg, 0.25),
        pair(palette.diff.del_fg, palette.diff.del_bg, 0.25),
        pair(palette.on_accent, palette.accent, 0.25),
    ];
    let syntax = [
        HighlightId::KEYWORD,
        HighlightId::FUNCTION,
        HighlightId::TYPE,
        HighlightId::STRING,
        HighlightId::COMMENT,
        HighlightId::NUMBER,
        HighlightId::PROPERTY,
    ]
    .map(|class| pair(palette.syntax_color(class), palette.bg, 0.25));
    let Some(profile) = coverage(primary.into_iter().chain(syntax)) else {
        return defaults;
    };
    defaults
        .with_lcd_gamma(profile.gamma)
        .and_then(|options| options.with_lcd_contrast(profile.contrast))
        .and_then(|options| options.with_grayscale_gamma(profile.gamma))
        .and_then(|options| options.with_grayscale_contrast(profile.contrast))
        .expect("Loom's bounded coverage profile is valid")
}

// ---- Layout metrics (px) ----------------------------------------------
pub const TITLEBAR_H: f32 = 32.0;
pub const TABS_H: f32 = 28.0;
pub const STATUS_H: f32 = 24.0;
/// Default height for file and folder rows in side drawers.
pub const DRAWER_ROW_H: f32 = 20.0;
pub const SIDEBAR_W: f32 = 288.0;
pub const SIDEBAR_MIN_W: f32 = 160.0;
pub const SIDEBAR_MAX_W: f32 = 400.0;
pub const TERMINAL_H: f32 = 176.0;
pub const TERMINAL_MIN_H: f32 = 120.0;
pub const TERMINAL_MAX_H: f32 = 480.0;
pub const EDITOR_MIN_H: f32 = 96.0;
/// Base line-number gutter width; `fonts::Metrics::gutter_w` adds the fold column.
/// Line numbers and Git marks retain 32 pixels beside the action lane.
pub const GUTTER_W: f32 = 46.0;
pub const GUTTER_ACTION_W: f32 = 14.0;
// A narrow folding column follows the original line-number and Git gutter.
pub const GUTTER_FOLD_W: f32 = 10.0;
// Redistribute existing spacing without moving line numbers or source text.
pub const GUTTER_NUMBER_PAD: f32 = 7.0;
pub const GUTTER_FOLD_PAD: f32 = 3.0;
pub const CODE_PAD: f32 = 4.0;
/// Estimated monospace advance of UI text at `UI_SIZE`. Code surfaces use
/// the measured `fonts::Metrics::char_w` instead.
pub const UI_CHAR_W: f32 = UI_SIZE * 0.6;
/// UI glyph size. Codicons use a 16px grid, so any other size smears strokes.
pub const ICON_SIZE: f32 = 16.0;

/// An `ICON_SIZE` square sharing `slot`'s center.
pub fn icon_rect(slot: UiRect) -> UiRect {
    let left = (slot.left + slot.right - ICON_SIZE) / 2.0;
    let top = (slot.top + slot.bottom - ICON_SIZE) / 2.0;
    UiRect::new(left, top, left + ICON_SIZE, top + ICON_SIZE)
}
pub const MONO_FAMILIES: &[&str] = &["Cascadia Mono", "Cascadia Code", "Consolas", "Courier New"];

// ---- Font sizes --------------------------------------------------------
pub const UI_SIZE: f32 = 12.0;
pub const SMALL: f32 = 10.0;

// ---- Text-style helpers ------------------------------------------------
pub fn mono(color: Color, size: f32) -> TextStyle {
    TextStyle::new(color, size, 400).font_families(MONO_FAMILIES)
}

pub fn mono_bold(color: Color, size: f32) -> TextStyle {
    TextStyle::new(color, size, 700).font_families(MONO_FAMILIES)
}

pub fn mono_right(color: Color, size: f32) -> TextStyle {
    let mut s = TextStyle::new(color, size, 400).font_families(MONO_FAMILIES);
    s.align = TextAlign::Right;
    s
}

pub fn sans(color: Color, size: f32) -> TextStyle {
    TextStyle::new(color, size, 400).font_families(super::fonts::SANS_FAMILIES)
}

pub fn sans_semibold(color: Color, size: f32) -> TextStyle {
    TextStyle::new(color, size, 600).font_families(super::fonts::SANS_FAMILIES)
}

pub fn hairline(color: Color) -> Stroke {
    Stroke::new(color, 1.0, 255)
}

// ---- Surfaces ----------------------------------------------------------

/// A panel with a crisp `width`-px border, drawn as a filled ring.
///
/// WHY A FILLED RING INSTEAD OF `VisualStyle::stroked`?
/// Skia strokes are centered on the rect edge and anti-aliased. A 1px stroke
/// therefore smears half a pixel onto each side of the edge, and anti-aliasing
/// spreads the color across two pixel rows — so it renders as a blurry ~2px
/// line. Tuning the stroke width (0.5 / 0.75 / 1.0) only makes it fainter or
/// fatter; it can never land on exactly 1px.
///
/// Instead we paint an outer rect in `border` and an inner rect in `fill`,
/// inset by `width`, leaving a `width`-px ring. Filled rects are pixel-aligned
/// (the same technique as the 1px divider panels in the title/status bars), so
/// the border is exactly `width` px and stays crisp at any scale factor.
///
/// Use this anywhere you need a hairline border on a (possibly rounded)
/// surface. `radius` is the OUTER corner radius; the inner corner uses
/// `radius - width` so the ring thickness stays uniform through the corners.
pub fn bordered(rect: UiRect, fill: Color, border: Color, radius: f32, width: f32) -> Element {
    let outer = panel(rect, VisualStyle::filled(border).radius(radius));
    let inner_rect = UiRect::new(
        rect.left + width,
        rect.top + width,
        rect.right - width,
        rect.bottom - width,
    );
    outer.child(panel(
        inner_rect,
        VisualStyle::filled(fill).radius((radius - width).max(0.0)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The colors the former `SyntaxColors` fields held, per built-in theme, in
    /// the order keyword, function, type, string, comment, number, property.
    /// Moving them into the class table must not change how any theme looks.
    #[test]
    fn built_in_themes_keep_their_syntax_colors() {
        let classes = [
            HighlightId::KEYWORD,
            HighlightId::FUNCTION,
            HighlightId::TYPE,
            HighlightId::STRING,
            HighlightId::COMMENT,
            HighlightId::NUMBER,
            HighlightId::PROPERTY,
        ];
        let expected: [(&str, [u32; 7]); 1] = [(
            "aura-dark",
            [
                0xf472b6, 0x60a5fa, 0xfbbf24, 0x4ade80, 0x64748b, 0xfb923c, 0xa78bfa,
            ],
        )];
        assert_eq!(expected.len(), THEMES.len());
        for (id, colors) in expected {
            let theme = THEMES.iter().find(|theme| theme.id == id).unwrap();
            for (class, rgb) in classes.into_iter().zip(colors) {
                assert_eq!(
                    theme.palette.syntax_color(class),
                    Color(rgb),
                    "{id} {class:?}"
                );
            }
            // Built-in constants and variables read like keywords, constructors and
            // modules like types, as the former lexer drew true, this and Names.
            for (class, like) in [
                (HighlightId::CONSTANT_BUILTIN, HighlightId::KEYWORD),
                (HighlightId::VARIABLE_BUILTIN, HighlightId::KEYWORD),
                (HighlightId::CONSTRUCTOR, HighlightId::TYPE),
                (HighlightId::MODULE, HighlightId::TYPE),
                (HighlightId::FUNCTION_BUILTIN, HighlightId::FUNCTION),
                (HighlightId::FUNCTION_MACRO, HighlightId::FUNCTION),
            ] {
                assert_eq!(
                    theme.palette.syntax_color(class),
                    theme.palette.syntax_color(like),
                    "{id} {class:?}"
                );
            }
            // A class a theme does not style reads as ordinary text.
            assert_eq!(
                theme.palette.syntax_color(HighlightId::VARIABLE),
                theme.palette.text
            );
        }
    }

    #[test]
    fn theme_ids_are_unique() {
        // Built-in themes come first; contributed ones, if any test added them, follow.
        assert_eq!(
            ThemeId::all()
                .take(THEMES.len())
                .map(ThemeId::id)
                .collect::<Vec<_>>(),
            THEMES.iter().map(|theme| theme.id).collect::<Vec<_>>()
        );
        for (index, theme) in THEMES.iter().enumerate() {
            assert!(
                THEMES[index + 1..].iter().all(|other| other.id != theme.id),
                "duplicate theme id {}",
                theme.id
            );
        }
    }

    #[test]
    fn default_theme_is_aura_dark() {
        assert_eq!(ThemeId::default().theme().id, "aura-dark");
    }

    #[test]
    fn theme_ids_round_trip_as_json_strings() {
        for id in ThemeId::all().take(THEMES.len()) {
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(json, format!("\"{}\"", id.theme().id));
            assert_eq!(serde_json::from_str::<ThemeId>(&json).unwrap(), id);
        }
    }

    #[test]
    fn unknown_theme_id_loads_as_the_default() {
        assert_eq!(ThemeId::parse("no-such-theme"), None);
        let decoded: ThemeId = serde_json::from_str("\"no-such-theme\"").unwrap();
        assert_eq!(decoded, ThemeId::default());
    }
}
