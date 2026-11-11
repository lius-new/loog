//! Translate framework events; protocol encoding belongs to the terminal feature.
use crate::features::terminal::input::{self, Modifiers, TerminalKey};
use lgui::core::{KeyModifiers, KeyState, KeyboardEvent, LogicalKey, NamedKey, PhysicalKey};
mod win32;

pub(crate) fn key_bytes_in_mode(
    event: &KeyboardEvent,
    application_cursor: bool,
    win32_input: bool,
) -> Option<Vec<u8>> {
    if event.is_composing {
        return None;
    }
    if win32_input {
        win32::key_bytes(event)
    } else {
        key_bytes(event, application_cursor)
    }
}

/// Releases are only forwarded for presses the keymap allowed through.
#[derive(Clone, Default)]
pub(crate) struct PressedKeys(Vec<(crate::features::terminal::SessionId, KeyboardEvent)>);

impl PressedKeys {
    pub fn press(&mut self, session: crate::features::terminal::SessionId, event: &KeyboardEvent) {
        if !self
            .0
            .iter()
            .any(|(id, key)| *id == session && same_key(key, event))
        {
            self.0.push((session, event.clone()));
        }
    }

    pub fn release(
        &mut self,
        event: &KeyboardEvent,
    ) -> Vec<(crate::features::terminal::SessionId, KeyboardEvent)> {
        let mut releases = Vec::new();
        self.0.retain(|(session, key)| {
            if !same_key(key, event) {
                return true;
            }
            let mut key = key.clone();
            key.state = KeyState::Up;
            key.modifiers = event.modifiers;
            releases.push((*session, key));
            false
        });
        releases
    }

    pub fn release_all(&mut self) -> Vec<(crate::features::terminal::SessionId, KeyboardEvent)> {
        self.0
            .drain(..)
            .map(|(session, mut key)| {
                key.state = KeyState::Up;
                key.modifiers = KeyModifiers::empty();
                (session, key)
            })
            .collect()
    }
}

pub(crate) fn is_modifier(event: &KeyboardEvent) -> bool {
    matches!(
        event.key,
        LogicalKey::Named(
            NamedKey::Control
                | NamedKey::Shift
                | NamedKey::Alt
                | NamedKey::AltGraph
                | NamedKey::Meta
        )
    )
}

fn same_key(a: &KeyboardEvent, b: &KeyboardEvent) -> bool {
    if a.code != PhysicalKey::Unidentified && b.code != PhysicalKey::Unidentified {
        a.code == b.code
    } else {
        a.key == b.key
    }
}

pub(crate) fn modifiers(value: KeyModifiers) -> Modifiers {
    let mut result = Modifiers::empty();
    if value.shift() {
        result = result | Modifiers::SHIFT;
    }
    if value.alt() {
        result = result | Modifiers::ALT;
    }
    if value.ctrl() {
        result = result | Modifiers::CONTROL;
    }
    result
}
pub(crate) fn key_bytes(event: &KeyboardEvent, application_cursor: bool) -> Option<Vec<u8>> {
    if event.state != KeyState::Down || event.is_composing {
        return None;
    }
    // AltGr-produced symbols must use the text/IME path in legacy mode too.
    if event.modifiers.ctrl()
        && event.modifiers.alt()
        && let LogicalKey::Character(value) = &event.key
        && value.chars().any(|c| !c.is_ascii_alphabetic())
    {
        return None;
    }
    let key = match &event.key {
        LogicalKey::Character(value) => TerminalKey::Character(value.as_ref()),
        LogicalKey::Named(named) => TerminalKey::Named(match named {
            NamedKey::ArrowUp => input::NamedKey::ArrowUp,
            NamedKey::ArrowDown => input::NamedKey::ArrowDown,
            NamedKey::ArrowRight => input::NamedKey::ArrowRight,
            NamedKey::ArrowLeft => input::NamedKey::ArrowLeft,
            NamedKey::Home => input::NamedKey::Home,
            NamedKey::End => input::NamedKey::End,
            NamedKey::Insert => input::NamedKey::Insert,
            NamedKey::Delete => input::NamedKey::Delete,
            NamedKey::PageUp => input::NamedKey::PageUp,
            NamedKey::PageDown => input::NamedKey::PageDown,
            NamedKey::Backspace => input::NamedKey::Backspace,
            NamedKey::Enter => input::NamedKey::Enter,
            NamedKey::Tab => input::NamedKey::Tab,
            NamedKey::Escape => input::NamedKey::Escape,
            NamedKey::F1 => input::NamedKey::Function(1),
            NamedKey::F2 => input::NamedKey::Function(2),
            NamedKey::F3 => input::NamedKey::Function(3),
            NamedKey::F4 => input::NamedKey::Function(4),
            NamedKey::F5 => input::NamedKey::Function(5),
            NamedKey::F6 => input::NamedKey::Function(6),
            NamedKey::F7 => input::NamedKey::Function(7),
            NamedKey::F8 => input::NamedKey::Function(8),
            NamedKey::F9 => input::NamedKey::Function(9),
            NamedKey::F10 => input::NamedKey::Function(10),
            NamedKey::F11 => input::NamedKey::Function(11),
            NamedKey::F12 => input::NamedKey::Function(12),
            _ => return None,
        }),
    };
    input::key_bytes(key, modifiers(event.modifiers), application_cursor)
}
