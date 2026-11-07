//! Platform key conversion stays outside the independent editing core.
use crate::features::keymap::keystroke::Keystroke;
use loog_editor_core::vim::Key;

/// The Vim key for a keystroke, or `None` for chords Vim leaves alone
/// (Alt, Cmd, shifted named keys).
pub(crate) fn vim_key(keystroke: &Keystroke) -> Option<Key> {
    let modifiers = keystroke.modifiers;
    if modifiers.alt || modifiers.platform {
        return None;
    }
    if modifiers.ctrl {
        let mut chars = keystroke.key.chars();
        let character = chars.next()?;
        if chars.next().is_some() || modifiers.shift {
            return None;
        }
        return Some(if character == '[' {
            Key::Esc
        } else {
            Key::Ctrl(character)
        });
    }
    if let Some(text) = keystroke.text() {
        let mut chars = text.chars();
        let character = chars.next()?;
        return chars.next().is_none().then_some(Key::Char(character));
    }
    if modifiers.shift {
        return None;
    }
    Some(match keystroke.key.as_str() {
        "escape" => Key::Esc,
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "delete" => Key::Delete,
        "left" => Key::Left,
        "right" => Key::Right,
        "up" => Key::Up,
        "down" => Key::Down,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "insert" => Key::Insert,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keystrokes_map_to_vim_keys() {
        let key = |source: &str| vim_key(&Keystroke::parse(source).unwrap());
        assert_eq!(key("G"), Some(Key::Char('G')));
        assert_eq!(key("$"), Some(Key::Char('$')));
        assert_eq!(key("ctrl-r"), Some(Key::Ctrl('r')));
        assert_eq!(key("ctrl-["), Some(Key::Esc));
        assert_eq!(key("escape"), Some(Key::Esc));
        assert_eq!(key("space"), Some(Key::Char(' ')));
        assert_eq!(key("alt-x"), None);
        assert_eq!(key("shift-left"), None);
    }
}
