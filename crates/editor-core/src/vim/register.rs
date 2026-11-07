//! Registers: the unnamed register, `"0`, named, numbered and small-delete
//! registers, and the system clipboard behind `"+` and `"*`.

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterKind {
    Char,
    Line,
    /// One string per line of a block; `text` joins them with `\n`.
    Block,
}

/// Register text always uses `\n` line breaks; puts convert to the document's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Register {
    pub text: String,
    pub kind: RegisterKind,
}

impl Register {
    pub fn new(text: impl Into<String>, kind: RegisterKind) -> Self {
        let text: String = text.into();
        Self {
            text: text.replace("\r\n", "\n"),
            kind,
        }
    }
}

/// Access to the system clipboard. Errors are messages for the user.
pub trait Clipboard {
    fn read(&mut self) -> Result<Option<String>, String>;
    fn write(&mut self, text: &str) -> Result<(), String>;
}

/// How text came to be stored, which decides the registers it fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Store {
    Yank,
    Delete,
}

#[derive(Clone, Debug, Default)]
pub struct Registers {
    unnamed: Option<Register>,
    yank: Option<Register>,
    named: HashMap<char, Register>,
    /// `"1` to `"9`, most recent first.
    numbered: Vec<Register>,
    small_delete: Option<Register>,
    /// What Loog last wrote to the clipboard, to restore line and block
    /// types when the same text is read back.
    clipboard_written: Option<Register>,
    pub last_inserted: Option<String>,
    pub last_search: Option<String>,
    pub last_command: Option<String>,
}

impl Registers {
    /// Store `register` as register `name` (the unnamed register when
    /// `None`). With `sync_clipboard`, unnamed stores also reach the system
    /// clipboard. The internal registers are updated even when the clipboard
    /// fails; the error is returned for display.
    pub fn store(
        &mut self,
        name: Option<char>,
        register: Register,
        how: Store,
        sync_clipboard: bool,
        clipboard: &mut dyn Clipboard,
    ) -> Result<(), String> {
        let name = name.filter(|name| *name != '"');
        match name {
            Some('_') => return Ok(()),
            Some(upper @ 'A'..='Z') => {
                let lower = upper.to_ascii_lowercase();
                let appended = match self.named.remove(&lower) {
                    Some(previous) => append(previous, register),
                    None => register,
                };
                self.named.insert(lower, appended.clone());
                self.unnamed = Some(appended);
                return Ok(());
            }
            Some(named @ ('a'..='z' | '0'..='9' | '-')) => {
                self.set(named, register.clone());
                self.unnamed = Some(register);
                return Ok(());
            }
            Some('+' | '*') => {
                self.unnamed = Some(register.clone());
                return self.write_clipboard(register, clipboard);
            }
            Some(_) => return Err(format!("E354: Invalid register name: '{}'", name.unwrap())),
            None => {}
        }
        match how {
            Store::Yank => self.yank = Some(register.clone()),
            Store::Delete => {
                let small = register.kind == RegisterKind::Char && !register.text.contains('\n');
                if small {
                    self.small_delete = Some(register.clone());
                } else {
                    self.numbered.insert(0, register.clone());
                    self.numbered.truncate(9);
                }
            }
        }
        self.unnamed = Some(register.clone());
        if sync_clipboard {
            self.write_clipboard(register, clipboard)
        } else {
            Ok(())
        }
    }

    fn set(&mut self, name: char, register: Register) {
        match name {
            '0' => self.yank = Some(register),
            '1'..='9' => {
                let index = name as usize - '1' as usize;
                if index < self.numbered.len() {
                    self.numbered[index] = register;
                } else {
                    self.numbered.push(register);
                }
            }
            '-' => self.small_delete = Some(register),
            _ => {
                self.named.insert(name, register);
            }
        }
    }

    fn write_clipboard(
        &mut self,
        register: Register,
        clipboard: &mut dyn Clipboard,
    ) -> Result<(), String> {
        let text = clipboard_text(&register);
        let result = clipboard.write(&text);
        self.clipboard_written = Some(register);
        result
    }

    /// The content of register `name` (the unnamed register when `None`).
    pub fn get(
        &mut self,
        name: Option<char>,
        sync_clipboard: bool,
        clipboard: &mut dyn Clipboard,
    ) -> Result<Option<Register>, String> {
        let name = name.filter(|name| *name != '"');
        Ok(match name {
            None if sync_clipboard => self.read_clipboard(clipboard)?,
            None => self.unnamed.clone(),
            Some('+' | '*') => self.read_clipboard(clipboard)?,
            Some('0') => self.yank.clone(),
            Some(digit @ '1'..='9') => self.numbered.get(digit as usize - '1' as usize).cloned(),
            Some('-') => self.small_delete.clone(),
            Some(letter @ ('a'..='z' | 'A'..='Z')) => {
                self.named.get(&letter.to_ascii_lowercase()).cloned()
            }
            Some('.') => self
                .last_inserted
                .clone()
                .map(|text| Register::new(text, RegisterKind::Char)),
            Some('/') => self
                .last_search
                .clone()
                .map(|text| Register::new(text, RegisterKind::Char)),
            Some(':') => self
                .last_command
                .clone()
                .map(|text| Register::new(text, RegisterKind::Char)),
            Some('_') => None,
            Some(other) => return Err(format!("E354: Invalid register name: '{other}'")),
        })
    }

    fn read_clipboard(
        &mut self,
        clipboard: &mut dyn Clipboard,
    ) -> Result<Option<Register>, String> {
        let Some(text) = clipboard.read()? else {
            return Ok(None);
        };
        let text = text.replace("\r\n", "\n");
        if let Some(written) = &self.clipboard_written
            && clipboard_text(written).replace("\r\n", "\n") == text
        {
            return Ok(Some(written.clone()));
        }
        let kind = if text.ends_with('\n') {
            RegisterKind::Line
        } else {
            RegisterKind::Char
        };
        Ok(Some(Register::new(text, kind)))
    }

    /// Text of a register for macros and the expression of `@`.
    pub fn named_text(&self, name: char) -> Option<String> {
        self.named
            .get(&name.to_ascii_lowercase())
            .map(|register| register.text.clone())
    }

    /// Set a register from recorded keys (`q`).
    pub fn set_recorded(&mut self, name: char, text: String) {
        let register = Register::new(text, RegisterKind::Char);
        if name.is_ascii_uppercase() {
            let lower = name.to_ascii_lowercase();
            let appended = match self.named.remove(&lower) {
                Some(previous) => append(previous, register),
                None => register,
            };
            self.named.insert(lower, appended);
        } else {
            self.set(name, register);
        }
    }
}

/// Clipboard text for a register: line registers end with a line break.
fn clipboard_text(register: &Register) -> String {
    let mut text = register.text.clone();
    if register.kind == RegisterKind::Line && !text.ends_with('\n') {
        text.push('\n');
    }
    if cfg!(windows) {
        text = text.replace('\n', "\r\n");
    }
    text
}

/// `"Ayw`: append to a register; a line register stays line-wise.
fn append(previous: Register, next: Register) -> Register {
    match (previous.kind, next.kind) {
        (RegisterKind::Char, RegisterKind::Char) => {
            Register::new(previous.text + &next.text, RegisterKind::Char)
        }
        _ => {
            let mut text = previous.text;
            if !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&next.text);
            if !text.ends_with('\n') {
                text.push('\n');
            }
            Register::new(text, RegisterKind::Line)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Memory {
        text: Option<String>,
        fail: bool,
    }

    impl Clipboard for Memory {
        fn read(&mut self) -> Result<Option<String>, String> {
            if self.fail {
                return Err("clipboard unavailable".into());
            }
            Ok(self.text.clone())
        }
        fn write(&mut self, text: &str) -> Result<(), String> {
            if self.fail {
                return Err("clipboard unavailable".into());
            }
            self.text = Some(text.to_owned());
            Ok(())
        }
    }

    fn line(text: &str) -> Register {
        Register::new(text, RegisterKind::Line)
    }

    fn chars(text: &str) -> Register {
        Register::new(text, RegisterKind::Char)
    }

    #[test]
    fn yanks_deletes_and_named_registers_fill_the_right_slots() {
        let mut registers = Registers::default();
        let mut clipboard = Memory::default();
        let mut store = |registers: &mut Registers, name, register, how| {
            registers
                .store(name, register, how, false, &mut clipboard)
                .unwrap();
        };
        store(&mut registers, None, chars("yanked"), Store::Yank);
        store(&mut registers, None, line("deleted line\n"), Store::Delete);
        store(&mut registers, None, chars("x"), Store::Delete);
        store(&mut registers, Some('a'), chars("one"), Store::Yank);
        store(&mut registers, Some('A'), chars(" two"), Store::Yank);
        store(&mut registers, Some('_'), chars("gone"), Store::Delete);
        let mut get = |name| registers.get(name, false, &mut Memory::default()).unwrap();
        assert_eq!(get(None), Some(chars("one two")));
        assert_eq!(get(Some('0')), Some(chars("yanked")));
        assert_eq!(get(Some('1')), Some(line("deleted line\n")));
        assert_eq!(get(Some('-')), Some(chars("x")));
        assert_eq!(get(Some('a')), Some(chars("one two")));
        assert_eq!(get(Some('b')), None);
    }

    #[test]
    fn the_clipboard_keeps_types_it_wrote_and_infers_others() {
        let mut registers = Registers::default();
        let mut clipboard = Memory::default();
        registers
            .store(
                Some('+'),
                Register::new("a\nb", RegisterKind::Block),
                Store::Yank,
                false,
                &mut clipboard,
            )
            .unwrap();
        assert_eq!(
            registers
                .get(Some('+'), false, &mut clipboard)
                .unwrap()
                .unwrap()
                .kind,
            RegisterKind::Block
        );
        clipboard.text = Some("from elsewhere\r\n".into());
        assert_eq!(
            registers.get(Some('*'), false, &mut clipboard).unwrap(),
            Some(line("from elsewhere\n"))
        );
        // With clipboard sync, the unnamed register is the clipboard.
        registers
            .store(None, chars("synced"), Store::Yank, true, &mut clipboard)
            .unwrap();
        assert_eq!(
            registers.get(None, true, &mut clipboard).unwrap(),
            Some(chars("synced"))
        );
    }

    #[test]
    fn a_failing_clipboard_still_fills_internal_registers() {
        let mut registers = Registers::default();
        let mut clipboard = Memory {
            fail: true,
            ..Default::default()
        };
        assert!(
            registers
                .store(None, chars("kept"), Store::Yank, true, &mut clipboard)
                .is_err()
        );
        assert_eq!(
            registers.get(Some('0'), false, &mut clipboard).unwrap(),
            Some(chars("kept"))
        );
        assert!(registers.get(Some('+'), false, &mut clipboard).is_err());
    }
}
