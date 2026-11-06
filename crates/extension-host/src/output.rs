//! What an extension writes to its Output channels: bounded, and handed to the
//! application in batches after a round.
use std::collections::BTreeMap;

use crate::OutputLevel;

/// Most channels one extension declares.
const MAX_CHANNELS: usize = 8;
const MAX_KEY: usize = 64;
const MAX_NAME: usize = 64;
/// Longest text of one `append`.
const MAX_TEXT: usize = 8 * 1024;
/// Lines held back between two hand-overs; further lines are refused, not cut.
const MAX_PENDING: usize = 512;

/// One thing the extension did to a channel, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputChange {
    Declare { key: String, name: String },
    Append { key: String, level: OutputLevel, text: String },
    Clear { key: String },
}

#[derive(Default)]
pub struct Channels {
    names: BTreeMap<String, String>,
    pending: Vec<OutputChange>,
}

fn valid(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.contains('\0')
}

impl Channels {
    pub fn declare(&mut self, key: &str, name: &str) -> Result<(), String> {
        if !valid(key, MAX_KEY) || !valid(name, MAX_NAME) || name.contains(['\n', '\r']) {
            return Err("an output channel needs a key and a one-line name of at most 64 bytes".into());
        }
        if !self.names.contains_key(key) && self.names.len() >= MAX_CHANNELS {
            return Err("too many output channels".into());
        }
        if self.names.get(key).is_some_and(|known| known == name) {
            return Ok(());
        }
        self.room()?;
        self.names.insert(key.to_owned(), name.to_owned());
        self.pending.push(OutputChange::Declare {
            key: key.to_owned(),
            name: name.to_owned(),
        });
        Ok(())
    }

    pub fn append(&mut self, key: &str, level: OutputLevel, text: &str) -> Result<(), String> {
        if !self.names.contains_key(key) {
            return Err("declare an output channel before writing to it".into());
        }
        if text.len() > MAX_TEXT || text.contains('\0') {
            return Err("a line of output is at most 8 KiB".into());
        }
        self.room()?;
        self.pending.push(OutputChange::Append {
            key: key.to_owned(),
            level,
            text: text.to_owned(),
        });
        Ok(())
    }

    pub fn clear(&mut self, key: &str) -> Result<(), String> {
        if !self.names.contains_key(key) {
            return Err("declare an output channel before clearing it".into());
        }
        self.room()?;
        self.pending.push(OutputChange::Clear { key: key.to_owned() });
        Ok(())
    }

    /// A burst beyond the budget is refused with an error the extension can
    /// see, so it can slow down; nothing is dropped silently.
    fn room(&self) -> Result<(), String> {
        if self.pending.len() >= MAX_PENDING {
            Err("output is written faster than it is shown; try again later".into())
        } else {
            Ok(())
        }
    }

    pub fn dirty(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn take(&mut self) -> Option<Vec<OutputChange>> {
        (!self.pending.is_empty()).then(|| std::mem::take(&mut self.pending))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writing_needs_a_declared_channel() {
        let mut channels = Channels::default();
        assert!(channels.append("a", OutputLevel::Info, "x").is_err());
        channels.declare("a", "Alpha").unwrap();
        channels.append("a", OutputLevel::Info, "x").unwrap();
        assert_eq!(channels.take().unwrap().len(), 2);
        assert!(channels.take().is_none());
    }

    #[test]
    fn declaring_the_same_name_again_is_no_change() {
        let mut channels = Channels::default();
        channels.declare("a", "Alpha").unwrap();
        channels.take();
        channels.declare("a", "Alpha").unwrap();
        assert!(!channels.dirty());
        channels.declare("a", "Renamed").unwrap();
        assert!(channels.dirty());
    }

    #[test]
    fn channels_names_and_lines_are_bounded() {
        let mut channels = Channels::default();
        for n in 0..MAX_CHANNELS {
            channels.declare(&format!("c{n}"), "name").unwrap();
        }
        assert!(channels.declare("extra", "name").is_err());
        assert!(channels.declare("c0", "two\nlines").is_err());
        assert!(channels.declare("", "name").is_err());
        let long = "x".repeat(MAX_TEXT + 1);
        assert!(channels.append("c0", OutputLevel::Info, &long).is_err());
        assert!(channels.append("c0", OutputLevel::Info, "a\0b").is_err());
    }

    #[test]
    fn a_burst_past_the_budget_is_refused_until_taken() {
        let mut channels = Channels::default();
        channels.declare("a", "Alpha").unwrap();
        let mut accepted = 1;
        while channels.append("a", OutputLevel::Info, "line").is_ok() {
            accepted += 1;
        }
        assert_eq!(accepted, MAX_PENDING);
        assert_eq!(channels.take().unwrap().len(), MAX_PENDING);
        channels.append("a", OutputLevel::Info, "again").unwrap();
    }

    #[test]
    fn clear_is_kept_in_order_with_appends() {
        let mut channels = Channels::default();
        channels.declare("a", "Alpha").unwrap();
        channels.append("a", OutputLevel::Warn, "before").unwrap();
        channels.clear("a").unwrap();
        let changes = channels.take().unwrap();
        assert!(matches!(changes[2], OutputChange::Clear { .. }));
    }
}
