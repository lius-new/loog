//! Translate modifier-aware key events only after the peer requests mode 9001.
use crate::features::terminal::input;
use lgui::core::{KeyModifiers, KeyState, KeyboardEvent, LogicalKey, NamedKey, PhysicalKey};

pub(super) fn key_bytes(event: &KeyboardEvent) -> Option<Vec<u8>> {
    let mut text = String::new();
    let virtual_key = match &event.key {
        LogicalKey::Character(value) => {
            let mut characters = value.chars();
            let character = characters.next()?;
            if characters.next().is_some() {
                return None;
            }
            // Text input owns ordinary typing, IME commits and AltGr symbols.
            // Preventing the key's default only when we encode it avoids doubles.
            if event.state == KeyState::Down
                && ((!event.modifiers.ctrl() && !event.modifiers.alt())
                    || (event.modifiers.ctrl()
                        && event.modifiers.alt()
                        && !character.is_ascii_alphabetic())
                    || !character.is_ascii())
            {
                return None;
            }
            let key = virtual_character(character)?;
            text.push(if event.modifiers.ctrl() {
                input::control_character(character)
                    .map(char::from)
                    .unwrap_or(character)
            } else {
                character
            });
            key
        }
        LogicalKey::Named(named) => {
            let (key, character) = match named {
                NamedKey::Backspace => (8, Some('\x08')),
                NamedKey::Tab => (9, Some('\t')),
                NamedKey::Enter => (13, Some('\r')),
                NamedKey::Shift => (16, None),
                NamedKey::Control => (17, None),
                NamedKey::Alt | NamedKey::AltGraph => (18, None),
                NamedKey::Pause => (19, None),
                NamedKey::CapsLock => (20, None),
                NamedKey::Escape => (27, Some('\x1b')),
                NamedKey::PageUp => (33, None),
                NamedKey::PageDown => (34, None),
                NamedKey::End => (35, None),
                NamedKey::Home => (36, None),
                NamedKey::ArrowLeft => (37, None),
                NamedKey::ArrowUp => (38, None),
                NamedKey::ArrowRight => (39, None),
                NamedKey::ArrowDown => (40, None),
                NamedKey::PrintScreen => (44, None),
                NamedKey::Insert => (45, None),
                NamedKey::Delete => (46, None),
                NamedKey::Meta => (91, None),
                NamedKey::ContextMenu => (93, None),
                NamedKey::NumLock => (144, None),
                NamedKey::ScrollLock => (145, None),
                _ => {
                    let number = named.to_string().strip_prefix('F')?.parse::<u16>().ok()?;
                    if !(1..=24).contains(&number) {
                        return None;
                    }
                    (111 + number, None)
                }
            };
            if let Some(character) = character {
                text.push(character);
            }
            key
        }
    };
    let mut control = u32::from(event.modifiers.shift()) * 16
        | u32::from(event.modifiers.alt()) * 2
        | u32::from(event.modifiers.ctrl()) * 8
        | u32::from(event.modifiers.contains(KeyModifiers::CAPS_LOCK)) * 128
        | u32::from(event.modifiers.contains(KeyModifiers::NUM_LOCK)) * 32;
    let (scan, enhanced) = scan_code(event.code, virtual_key);
    if enhanced {
        control |= 256;
    }
    Some(input::win32_key_bytes(
        virtual_key,
        scan,
        &text,
        event.state == KeyState::Down,
        control,
    ))
}

fn virtual_character(character: char) -> Option<u16> {
    Some(match character {
        'a'..='z' | 'A'..='Z' => character.to_ascii_uppercase() as u16,
        '0'..='9' => character as u16,
        '\x01'..='\x1a' => u16::from(character as u8 - 1 + b'A'),
        ' ' | '\0' => 32,
        ';' | ':' => 186,
        '=' | '+' => 187,
        ',' | '<' => 188,
        '-' | '_' => 189,
        '.' | '>' => 190,
        '/' | '?' => 191,
        '\u{60}' | '~' => 192,
        '[' | '{' => 219,
        '\\' | '|' => 220,
        ']' | '}' => 221,
        '\'' | '"' => 222,
        '!' => 49,
        '@' => 50,
        '#' => 51,
        '$' => 52,
        '%' => 53,
        '^' => 54,
        '&' => 55,
        '*' => 56,
        '(' => 57,
        ')' => 48,
        _ => return None,
    })
}

fn scan_code(code: PhysicalKey, virtual_key: u16) -> (u16, bool) {
    use lgui::core::PhysicalKey::*;
    let scan = match code {
        Escape => 1,
        Digit1 => 2,
        Digit2 => 3,
        Digit3 => 4,
        Digit4 => 5,
        Digit5 => 6,
        Digit6 => 7,
        Digit7 => 8,
        Digit8 => 9,
        Digit9 => 10,
        Digit0 => 11,
        Minus => 12,
        Equal => 13,
        Backspace => 14,
        Tab => 15,
        KeyQ => 16,
        KeyW => 17,
        KeyE => 18,
        KeyR => 19,
        KeyT => 20,
        KeyY => 21,
        KeyU => 22,
        KeyI => 23,
        KeyO => 24,
        KeyP => 25,
        BracketLeft => 26,
        BracketRight => 27,
        Enter | NumpadEnter => 28,
        ControlLeft | ControlRight => 29,
        KeyA => 30,
        KeyS => 31,
        KeyD => 32,
        KeyF => 33,
        KeyG => 34,
        KeyH => 35,
        KeyJ => 36,
        KeyK => 37,
        KeyL => 38,
        Semicolon => 39,
        Quote => 40,
        Backquote => 41,
        ShiftLeft => 42,
        Backslash => 43,
        KeyZ => 44,
        KeyX => 45,
        KeyC => 46,
        KeyV => 47,
        KeyB => 48,
        KeyN => 49,
        KeyM => 50,
        Comma => 51,
        Period => 52,
        Slash | NumpadDivide => 53,
        ShiftRight => 54,
        NumpadMultiply => 55,
        AltLeft | AltRight => 56,
        Space => 57,
        CapsLock => 58,
        F1 => 59,
        F2 => 60,
        F3 => 61,
        F4 => 62,
        F5 => 63,
        F6 => 64,
        F7 => 65,
        F8 => 66,
        F9 => 67,
        F10 => 68,
        NumLock => 69,
        ScrollLock => 70,
        Home | Numpad7 => 71,
        ArrowUp | Numpad8 => 72,
        PageUp | Numpad9 => 73,
        NumpadSubtract => 74,
        ArrowLeft | Numpad4 => 75,
        Numpad5 => 76,
        ArrowRight | Numpad6 => 77,
        NumpadAdd => 78,
        End | Numpad1 => 79,
        ArrowDown | Numpad2 => 80,
        PageDown | Numpad3 => 81,
        Insert | Numpad0 => 82,
        Delete | NumpadDecimal => 83,
        IntlBackslash => 86,
        F11 => 87,
        F12 => 88,
        MetaLeft => 91,
        MetaRight => 92,
        ContextMenu => 93,
        _ => match virtual_key {
            8 => 14,
            9 => 15,
            13 => 28,
            16 => 42,
            17 => 29,
            18 => 56,
            27 => 1,
            32 => 57,
            _ => 0,
        },
    };
    let enhanced = matches!(
        code,
        NumpadEnter
            | NumpadDivide
            | ControlRight
            | AltRight
            | Home
            | End
            | Insert
            | Delete
            | PageUp
            | PageDown
            | ArrowLeft
            | ArrowRight
            | ArrowUp
            | ArrowDown
            | MetaLeft
            | MetaRight
            | ContextMenu
    );
    (scan, enhanced)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn modified_enter_and_control_j_keep_native_identity() {
        for (key, code, modifiers, expected) in [
            (
                LogicalKey::Named(NamedKey::Enter),
                PhysicalKey::Enter,
                KeyModifiers::SHIFT,
                "\x1b[13;28;13;1;16;1_",
            ),
            (
                LogicalKey::Named(NamedKey::Enter),
                PhysicalKey::Enter,
                KeyModifiers::CONTROL,
                "\x1b[13;28;13;1;8;1_",
            ),
            (
                LogicalKey::Character("j".into()),
                PhysicalKey::KeyJ,
                KeyModifiers::CONTROL,
                "\x1b[74;36;10;1;8;1_",
            ),
        ] {
            let mut event = KeyboardEvent {
                state: KeyState::Down,
                key,
                code,
                modifiers,
                ..Default::default()
            };
            assert_eq!(key_bytes(&event).unwrap(), expected.as_bytes());
            event.state = KeyState::Up;
            assert!(
                String::from_utf8(key_bytes(&event).unwrap())
                    .unwrap()
                    .contains(";0;")
            );
        }
    }

    #[test]
    fn text_and_altgr_keep_the_text_input_path() {
        for (value, modifiers) in [
            ("a", KeyModifiers::empty()),
            ("中文", KeyModifiers::empty()),
            ("é", KeyModifiers::CONTROL | KeyModifiers::ALT),
            ("@", KeyModifiers::CONTROL | KeyModifiers::ALT),
        ] {
            let event = KeyboardEvent {
                state: KeyState::Down,
                key: LogicalKey::Character(value.into()),
                modifiers,
                ..Default::default()
            };
            for mode in [false, true] {
                assert_eq!(
                    crate::ui::terminal::keys::key_bytes_in_mode(&event, false, mode),
                    None
                );
            }
        }
    }
}
