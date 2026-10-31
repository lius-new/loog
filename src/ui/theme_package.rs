//! Themes that an extension package contributes (design §5.2, §6.6).
//!
//! A package theme is data: a built-in theme to start from and the colors and
//! syntax styles it changes. Everything it does not mention is the base theme's,
//! so a small file is a complete theme and a damaged one cannot leave a color
//! undefined. Names are checked strictly: a misspelled color is an error that
//! rejects the theme, not a color that silently stays as it was.
use super::theme::{Palette, THEMES};
use crate::features::highlighting::{HighlightId, HighlightStyle, StyleTable};
use lgui::prelude::Color;
use serde_json::{Map, Value};

/// A theme as the package wrote it, resolved against its base.
pub struct Parsed {
    pub name: String,
    pub palette: Palette,
}

/// Most syntax entries and the longest text taken from a theme file.
const MAX_ENTRIES: usize = 256;

fn color(value: &Value, what: &str) -> Result<u32, String> {
    let text = value
        .as_str()
        .ok_or_else(|| format!("{what} must be a color such as \"#1e2128\""))?;
    let digits = text
        .strip_prefix('#')
        .filter(|digits| digits.len() == 6)
        .ok_or_else(|| format!("{what}: \"{text}\" is not a #rrggbb color"))?;
    u32::from_str_radix(digits, 16)
        .map_err(|_| format!("{what}: \"{text}\" is not a #rrggbb color"))
}

/// The palette field a color name refers to.
fn slot<'a>(palette: &'a mut Palette, name: &str) -> Option<&'a mut Color> {
    Some(match name {
        "bg" => &mut palette.bg,
        "sidebar" => &mut palette.sidebar,
        "surface" => &mut palette.surface,
        "border" => &mut palette.border,
        "active_line" => &mut palette.active_line,
        "active_tab" => &mut palette.active_tab,
        "selection" => &mut palette.selection,
        "accent" => &mut palette.accent,
        "accent_hover" => &mut palette.accent_hover,
        "on_accent" => &mut palette.on_accent,
        "scrollbar" => &mut palette.scrollbar,
        "text_bright" => &mut palette.text_bright,
        "text" => &mut palette.text,
        "text_soft" => &mut palette.text_soft,
        "text_muted" => &mut palette.text_muted,
        "text_dim" => &mut palette.text_dim,
        "text_faint" => &mut palette.text_faint,
        "text_ghost" => &mut palette.text_ghost,
        "error" => &mut palette.error,
        "error_text" => &mut palette.error_text,
        "warning" => &mut palette.warning,
        "diff.add_bg" => &mut palette.diff.add_bg,
        "diff.add_fg" => &mut palette.diff.add_fg,
        "diff.add_mark" => &mut palette.diff.add_mark,
        "diff.del_bg" => &mut palette.diff.del_bg,
        "diff.del_fg" => &mut palette.diff.del_fg,
        "diff.del_mark" => &mut palette.diff.del_mark,
        "diff.mod_mark" => &mut palette.diff.mod_mark,
        "conflict.incoming_bg" => &mut palette.conflict.incoming_bg,
        "conflict.marker_bg" => &mut palette.conflict.marker_bg,
        "git.added" => &mut palette.git.added,
        "git.added_text" => &mut palette.git.added_text,
        "git.modified" => &mut palette.git.modified,
        "git.deleted" => &mut palette.git.deleted,
        "git.staged" => &mut palette.git.staged,
        "git.ignored" => &mut palette.git.ignored,
        "badge.csharp" => &mut palette.badge.csharp,
        "badge.rust" => &mut palette.badge.rust,
        "badge.typescript" => &mut palette.badge.typescript,
        "badge.javascript" => &mut palette.badge.javascript,
        "badge.diff" => &mut palette.badge.diff,
        _ => return None,
    })
}

fn object<'a>(value: &'a Value, what: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{what} must be an object"))
}

fn style(value: &Value, class: &str) -> Result<HighlightStyle, String> {
    // A bare color is shorthand for a style with only a foreground.
    if value.is_string() {
        return Ok(HighlightStyle::fg(color(value, class)?));
    }
    let fields = object(value, class)?;
    let mut out = HighlightStyle::NONE;
    for (key, value) in fields {
        let flag = || {
            value
                .as_bool()
                .ok_or_else(|| format!("{class}.{key} must be true or false"))
        };
        match key.as_str() {
            "fg" => out.fg = Some(color(value, &format!("{class}.fg"))?),
            "bold" => out.bold = Some(flag()?),
            "italic" => out.italic = Some(flag()?),
            "underline" => out.underline = Some(flag()?),
            "strikethrough" => out.strikethrough = Some(flag()?),
            other => return Err(format!("{class}: unknown style attribute \"{other}\"")),
        }
    }
    Ok(out)
}

/// Read a theme file. `base` names a built-in theme (default: the first); the
/// rest of the file changes it.
pub fn parse(source: &str) -> Result<Parsed, String> {
    let value: Value =
        serde_json::from_str(source).map_err(|error| format!("invalid JSON: {error}"))?;
    let root = object(&value, "the theme")?;
    for key in root.keys() {
        if !["name", "base", "colors", "ansi", "shadow_alpha", "syntax"].contains(&key.as_str()) {
            return Err(format!("unknown field \"{key}\""));
        }
    }
    let base_id = match root.get("base") {
        None => THEMES[0].id,
        Some(base) => base
            .as_str()
            .ok_or("\"base\" must be the id of a built-in theme")?,
    };
    let base = THEMES
        .iter()
        .find(|theme| theme.id == base_id)
        .ok_or_else(|| format!("\"base\": \"{base_id}\" is not a built-in theme"))?;
    let mut palette = base.palette;
    let name = match root.get("name") {
        None => String::new(),
        Some(name) => name
            .as_str()
            .filter(|name| !name.trim().is_empty() && name.len() <= 80)
            .ok_or("\"name\" must be short text")?
            .to_owned(),
    };

    if let Some(colors) = root.get("colors") {
        for (key, value) in object(colors, "\"colors\"")? {
            let target =
                slot(&mut palette, key).ok_or_else(|| format!("unknown color \"{key}\""))?;
            *target = Color(color(value, key)?);
        }
    }
    if let Some(ansi) = root.get("ansi") {
        let list = ansi
            .as_array()
            .filter(|list| list.len() == 16)
            .ok_or("\"ansi\" must list 16 colors")?;
        for (at, value) in list.iter().enumerate() {
            palette.ansi[at] = color(value, &format!("ansi[{at}]"))?;
        }
    }
    if let Some(alpha) = root.get("shadow_alpha") {
        palette.shadow_alpha = alpha
            .as_u64()
            .and_then(|alpha| u8::try_from(alpha).ok())
            .ok_or("\"shadow_alpha\" must be a number from 0 to 255")?;
    }
    if let Some(syntax) = root.get("syntax") {
        let fields = object(syntax, "\"syntax\"")?;
        if fields.len() > MAX_ENTRIES {
            return Err(format!("\"syntax\" has more than {MAX_ENTRIES} entries"));
        }
        let mut classes: Vec<(HighlightId, HighlightStyle)> = palette.syntax.classes.to_vec();
        for (key, value) in fields {
            let class = HighlightId::all()
                .find(|class| class.name() == key)
                .ok_or_else(|| format!("unknown syntax class \"{key}\""))?;
            let style = style(value, key)?;
            match classes.iter_mut().find(|(known, _)| *known == class) {
                Some((_, existing)) => *existing = existing.overlay(style),
                None => classes.push((class, style)),
            }
        }
        // A theme lives as long as the program, like the built-in ones.
        palette.syntax = StyleTable {
            classes: Box::leak(classes.into_boxed_slice()),
            modifiers: palette.syntax.modifiers,
        };
    }
    Ok(Parsed { name, palette })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_file_changes_only_what_it_names() {
        let theme = parse(
            r##"{ "name": "Night", "base": "aura-dark",
                  "colors": { "bg": "#101418", "git.added": "#00ff00" },
                  "syntax": { "keyword": "#ff0000", "comment": { "fg": "#888888", "italic": true } } }"##,
        )
        .unwrap();
        let base = THEMES.iter().find(|t| t.id == "aura-dark").unwrap();
        assert_eq!(theme.name, "Night");
        assert_eq!(theme.palette.bg.0, 0x101418);
        assert_eq!(theme.palette.git.added.0, 0x00ff00);
        assert_eq!(
            theme.palette.text.0, base.palette.text.0,
            "the rest is the base's"
        );
        assert_eq!(theme.palette.syntax_color(HighlightId::KEYWORD).0, 0xff0000);
        let comment = theme
            .palette
            .syntax
            .resolve(HighlightId::COMMENT, Default::default());
        assert_eq!((comment.fg, comment.italic), (Some(0x888888), Some(true)));
        // A class the file does not mention keeps the base's style.
        assert_eq!(
            theme.palette.syntax_color(HighlightId::STRING).0,
            base.palette.syntax_color(HighlightId::STRING).0
        );
    }

    #[test]
    fn a_mistake_rejects_the_whole_theme_and_says_where() {
        for (source, expected) in [
            (
                r##"{ "colors": { "bgg": "#000000" } }"##,
                "unknown color \"bgg\"",
            ),
            (r##"{ "colors": { "bg": "red" } }"##, "not a #rrggbb"),
            (r##"{ "colors": { "bg": "#12345" } }"##, "not a #rrggbb"),
            (r##"{ "base": "nope" }"##, "not a built-in theme"),
            (r##"{ "ansi": ["#000000"] }"##, "16 colors"),
            (
                r##"{ "syntax": { "keywrd": "#000000" } }"##,
                "unknown syntax class",
            ),
            (
                r##"{ "syntax": { "keyword": { "blink": true } } }"##,
                "unknown style attribute",
            ),
            (r#"{ "shadow_alpha": 300 }"#, "0 to 255"),
            (r#"{ "extra": 1 }"#, "unknown field"),
            (r#"[1]"#, "must be an object"),
            ("{", "invalid JSON"),
        ] {
            let error = parse(source)
                .err()
                .unwrap_or_else(|| panic!("{source} was accepted"));
            assert!(error.contains(expected), "{source}: {error}");
        }
    }

    #[test]
    fn an_empty_theme_is_the_default_one() {
        let theme = parse("{}").unwrap();
        assert_eq!(theme.palette.bg.0, THEMES[0].palette.bg.0);
        assert!(theme.name.is_empty());
    }
}
