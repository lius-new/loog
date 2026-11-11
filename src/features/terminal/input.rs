//! GUI-independent terminal protocol encoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Modifiers(u8);
impl Modifiers {
    pub const SHIFT: Self = Self(1);
    pub const ALT: Self = Self(2);
    pub const CONTROL: Self = Self(4);
    pub const fn empty() -> Self {
        Self(0)
    }
    pub fn shift(self) -> bool {
        self.0 & 1 != 0
    }
    pub fn alt(self) -> bool {
        self.0 & 2 != 0
    }
    pub fn ctrl(self) -> bool {
        self.0 & 4 != 0
    }
}
impl std::ops::BitOr for Modifiers {
    type Output = Self;
    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NamedKey {
    ArrowUp,
    ArrowDown,
    ArrowRight,
    ArrowLeft,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    Backspace,
    Enter,
    Tab,
    Escape,
    Function(u8),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TerminalKey<'a> {
    Named(NamedKey),
    Character(&'a str),
}

pub(crate) fn key_bytes(
    key: TerminalKey<'_>,
    modifiers: Modifiers,
    application_cursor: bool,
) -> Option<Vec<u8>> {
    // COMPAT(legacy-vt-keys): Sessions without a negotiated richer input mode
    // still need traditional VT control bytes and xterm key sequences. Prefer
    // the requested protocol when available; retain this fallback while legacy
    // shells are supported. No removal version is scheduled.
    let shift = modifiers.shift();
    let alt = modifiers.alt();
    let ctrl = modifiers.ctrl();
    // xterm modifier parameter: 1 + Shift(1) + Alt(2) + Ctrl(4).
    let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    if let TerminalKey::Named(named) = key {
        let cursor_final = match named {
            NamedKey::ArrowUp => Some('A'),
            NamedKey::ArrowDown => Some('B'),
            NamedKey::ArrowRight => Some('C'),
            NamedKey::ArrowLeft => Some('D'),
            NamedKey::Home => Some('H'),
            NamedKey::End => Some('F'),
            _ => None,
        };
        if let Some(final_byte) = cursor_final {
            return Some(
                if modifier > 1 {
                    format!("\x1b[1;{modifier}{final_byte}")
                } else if application_cursor {
                    format!("\x1bO{final_byte}")
                } else {
                    format!("\x1b[{final_byte}")
                }
                .into_bytes(),
            );
        }
        let tilde_code = match named {
            NamedKey::Insert => Some(2),
            NamedKey::Delete => Some(3),
            NamedKey::PageUp => Some(5),
            NamedKey::PageDown => Some(6),
            _ => None,
        };
        if let Some(code) = tilde_code {
            return Some(
                if modifier > 1 {
                    format!("\x1b[{code};{modifier}~")
                } else {
                    format!("\x1b[{code}~")
                }
                .into_bytes(),
            );
        }
        if named == NamedKey::Backspace {
            // Ctrl+Backspace deletes a word (^H), Alt+Backspace sends ESC DEL.
            return Some(with_alt(vec![if ctrl { 8 } else { 127 }], alt));
        }
        if let NamedKey::Function(number @ 1..=12) = named {
            return Some(if number <= 4 {
                let final_byte = char::from(b'P' + number - 1);
                if modifier == 1 {
                    format!("\x1bO{final_byte}").into_bytes()
                } else {
                    format!("\x1b[1;{modifier}{final_byte}").into_bytes()
                }
            } else {
                let code = [15, 17, 18, 19, 20, 21, 23, 24][usize::from(number - 5)];
                if modifier == 1 {
                    format!("\x1b[{code}~").into_bytes()
                } else {
                    format!("\x1b[{code};{modifier}~").into_bytes()
                }
            });
        }
    }

    if modifiers.ctrl()
        && let TerminalKey::Character(value) = key
        && let Some(character) = value.chars().next()
        && value.chars().count() == 1
    {
        if let Some(control) = control_character(character) {
            return Some(with_alt(vec![control], alt));
        }
    }

    if alt
        && let TerminalKey::Character(value) = key
        && !value.is_empty()
    {
        return Some(with_alt(value.as_bytes().to_vec(), true));
    }
    let bytes: &[u8] = match key {
        TerminalKey::Named(NamedKey::Enter) => b"\r",
        TerminalKey::Named(NamedKey::Tab) if shift => b"\x1b[Z",
        TerminalKey::Named(NamedKey::Tab) => b"\t",
        TerminalKey::Named(NamedKey::Escape) => b"\x1b",
        _ => return None,
    };
    Some(with_alt(bytes.to_vec(), alt))
}

fn with_alt(mut bytes: Vec<u8>, alt: bool) -> Vec<u8> {
    if alt {
        bytes.insert(0, 0x1b);
    }
    bytes
}

pub(crate) fn control_character(character: char) -> Option<u8> {
    let lower = character.to_ascii_lowercase();
    Some(match lower {
        'a'..='z' => lower as u8 & 0x1f,
        ' ' | '@' | '`' | '2' => 0,
        '[' | '3' => 27,
        '\\' | '4' => 28,
        ']' | '5' => 29,
        '^' | '6' => 30,
        '_' | '-' | '7' => 31,
        '?' | '8' => 127,
        _ => return None,
    })
}

/// Microsoft's negotiated CSI Vk;Sc;Uc;Kd;Cs;Rc_ input record format.
/// UTF-16 units are encoded separately, as in KEY_EVENT_RECORD::UnicodeChar.
pub(crate) fn win32_key_bytes(
    virtual_key: u16,
    scan_code: u16,
    text: &str,
    down: bool,
    control_state: u32,
) -> Vec<u8> {
    let units = text.encode_utf16().collect::<Vec<_>>();
    let units = if units.is_empty() { &[0][..] } else { &units };
    let mut bytes = Vec::new();
    for unit in units {
        bytes.extend_from_slice(
            format!(
                "\x1b[{virtual_key};{scan_code};{unit};{};{control_state};1_",
                u8::from(down)
            )
            .as_bytes(),
        );
    }
    bytes
}

pub(crate) fn paste_bytes(value: &str, bracketed: bool) -> Vec<u8> {
    // Shells submit lines on carriage return; a bare LF would not run them.
    let normalized = value.replace("\r\n", "\r").replace('\n', "\r");
    let mut bytes = Vec::with_capacity(normalized.len() + 12);
    if bracketed {
        bytes.extend_from_slice(b"\x1b[200~");
        // The payload must not be able to end bracketing early.
        bytes.extend_from_slice(normalized.replace("\x1b[201~", "").as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
    } else {
        bytes.extend_from_slice(normalized.as_bytes());
    }
    bytes
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paste_normalizes_newlines_and_honors_bracketed_mode() {
        assert_eq!(paste_bytes("a\r\nb\nc", false), b"a\rb\rc");
        assert_eq!(paste_bytes("ls", true), b"\x1b[200~ls\x1b[201~");
        assert_eq!(paste_bytes("x\x1b[201~y", true), b"\x1b[200~xy\x1b[201~");
    }

    #[test]
    fn every_control_letter_reaches_the_terminal_encoder() {
        for ch in 'a'..='z' {
            for value in [ch.to_string(), ch.to_ascii_uppercase().to_string()] {
                assert_eq!(
                    key_bytes(TerminalKey::Character(&value), Modifiers::CONTROL, false),
                    Some(vec![ch as u8 & 0x1f]),
                    "{value}"
                );
            }
        }
        for ch in ["中文", ""] {
            assert_eq!(
                key_bytes(TerminalKey::Character(ch), Modifiers::CONTROL, false),
                None
            );
        }
        assert_eq!(
            key_bytes(TerminalKey::Character("K"), Modifiers::CONTROL, false),
            Some(vec![11])
        );
        assert_eq!(
            key_bytes(TerminalKey::Character("a"), Modifiers::empty(), false),
            None
        );
        assert_eq!(
            key_bytes(TerminalKey::Named(NamedKey::Tab), Modifiers::SHIFT, false),
            Some(b"\x1b[Z".to_vec())
        );
        assert_eq!(
            key_bytes(
                TerminalKey::Named(NamedKey::Backspace),
                Modifiers::ALT,
                false
            ),
            Some(b"\x1b\x7f".to_vec())
        );
    }

    #[test]
    fn alt_control_punctuation_and_function_keys_preserve_their_meaning() {
        assert_eq!(
            key_bytes(TerminalKey::Character("x"), Modifiers::ALT, false),
            Some(b"\x1bx".to_vec())
        );
        assert_eq!(
            key_bytes(
                TerminalKey::Character("j"),
                Modifiers::ALT | Modifiers::CONTROL,
                false
            ),
            Some(b"\x1b\n".to_vec())
        );
        for (character, expected) in [
            (" ", 0),
            ("[", 27),
            ("\\", 28),
            ("]", 29),
            ("^", 30),
            ("_", 31),
            ("?", 127),
        ] {
            assert_eq!(
                key_bytes(TerminalKey::Character(character), Modifiers::CONTROL, false),
                Some(vec![expected])
            );
        }
        for (number, expected) in [
            (1, "\x1bOP"),
            (4, "\x1bOS"),
            (5, "\x1b[15~"),
            (12, "\x1b[24~"),
        ] {
            assert_eq!(
                key_bytes(
                    TerminalKey::Named(NamedKey::Function(number)),
                    Modifiers::empty(),
                    false
                ),
                Some(expected.as_bytes().to_vec())
            );
        }
        assert_eq!(
            key_bytes(
                TerminalKey::Named(NamedKey::Function(5)),
                Modifiers::SHIFT | Modifiers::CONTROL,
                false
            ),
            Some(b"\x1b[15;6~".to_vec())
        );
    }
}
