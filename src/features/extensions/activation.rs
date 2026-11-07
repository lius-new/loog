//! On-demand activation (design §6.3, §8.1).
//!
//! Activation is decided from the manifest alone, while the instance is
//! dormant. Concurrent triggers of one activation generation coalesce into a
//! single activation with a bounded number of pending triggers.
use super::manifest::{Activation, Manifest};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trigger {
    Command(String),
    /// A document in `language` became relevant.
    Document {
        language: String,
    },
    Event(String),
}

/// Whether `trigger` is a declared reason to wake `manifest`'s behaviour.
pub fn matches(manifest: &Manifest, trigger: &Trigger) -> bool {
    manifest.has_behaviour()
        && manifest
            .activation
            .iter()
            .any(|declared| match (declared, trigger) {
                (Activation::Command { id }, Trigger::Command(requested)) => id == requested,
                (Activation::Document { selector }, Trigger::Document { language }) => {
                    selector.languages.contains(language)
                }
                (Activation::Event { name }, Trigger::Event(fired)) => name == fired,
                _ => false,
            })
}

/// A command call waiting for the instance; each keeps its own captured context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingCommand {
    pub command: String,
    pub call: u64,
}

/// Triggers received while activating. Commands are kept individually (up to a
/// bound); language demand is a level, so only the newest state is kept.
#[derive(Debug)]
pub struct PendingTriggers {
    max_commands: usize,
    commands: Vec<PendingCommand>,
    language: Option<String>,
    events: Vec<String>,
}

impl PendingTriggers {
    pub fn new(max_commands: usize) -> Self {
        Self {
            max_commands,
            commands: Vec::new(),
            language: None,
            events: Vec::new(),
        }
    }

    /// Returns `false` when a command could not be queued (caller reports
    /// `limit-exceeded` to that command only).
    pub fn push_command(&mut self, command: PendingCommand) -> bool {
        if self.commands.len() >= self.max_commands {
            return false;
        }
        self.commands.push(command);
        true
    }

    pub fn set_language(&mut self, language: String) {
        self.language = Some(language);
    }

    /// Repeated identical events collapse; the list stays tiny by construction.
    pub fn push_event(&mut self, name: String) {
        if !self.events.contains(&name) {
            self.events.push(name);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty() && self.language.is_none() && self.events.is_empty()
    }

    /// Hand everything over once activation succeeds, in arrival order for commands.
    pub fn drain(&mut self) -> (Vec<PendingCommand>, Option<String>, Vec<String>) {
        (
            std::mem::take(&mut self.commands),
            self.language.take(),
            std::mem::take(&mut self.events),
        )
    }

    /// Activation failed: every waiting command is reported with the reason.
    pub fn fail_all(&mut self) -> Vec<PendingCommand> {
        self.language = None;
        self.events.clear();
        std::mem::take(&mut self.commands)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest::parse(
            r#"{
                "schema": 1, "id": "example.lang", "version": "0.1.0",
                "api": "loog:extension@0.1.0", "component": "x.wasm",
                "activation": [
                    { "kind": "command", "id": "example.lang.run" },
                    { "kind": "document", "selector": { "languages": ["rust"] } },
                    { "kind": "event", "name": "document.save" }
                ],
                "contributes": { "commands": [{ "id": "example.lang.run", "title": "Run" }] }
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn only_declared_triggers_wake_the_extension() {
        let m = manifest();
        assert!(matches(&m, &Trigger::Command("example.lang.run".into())));
        assert!(!matches(&m, &Trigger::Command("example.lang.other".into())));
        assert!(matches(
            &m,
            &Trigger::Document {
                language: "rust".into()
            }
        ));
        assert!(!matches(
            &m,
            &Trigger::Document {
                language: "go".into()
            }
        ));
        assert!(matches(&m, &Trigger::Event("document.save".into())));
        assert!(!matches(&m, &Trigger::Event("document.open".into())));
    }

    #[test]
    fn resource_only_packages_never_activate() {
        let m =
            Manifest::parse(r#"{ "schema": 1, "id": "acme.night", "version": "1.0.0" }"#).unwrap();
        assert!(!matches(&m, &Trigger::Event("document.save".into())));
    }

    #[test]
    fn pending_commands_are_bounded_and_keep_order() {
        let mut pending = PendingTriggers::new(2);
        assert!(pending.push_command(PendingCommand {
            command: "a".into(),
            call: 1
        }));
        assert!(pending.push_command(PendingCommand {
            command: "a".into(),
            call: 2
        }));
        assert!(!pending.push_command(PendingCommand {
            command: "a".into(),
            call: 3
        }));
        let (commands, _, _) = pending.drain();
        assert_eq!(commands.iter().map(|c| c.call).collect::<Vec<_>>(), [1, 2]);
        assert!(pending.is_empty());
    }

    #[test]
    fn language_demand_keeps_only_the_latest_and_events_collapse() {
        let mut pending = PendingTriggers::new(4);
        pending.set_language("rust".into());
        pending.set_language("go".into());
        pending.push_event("document.save".into());
        pending.push_event("document.save".into());
        let (_, language, events) = pending.drain();
        assert_eq!(language.as_deref(), Some("go"));
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn failed_activation_releases_every_waiting_command() {
        let mut pending = PendingTriggers::new(4);
        pending.push_command(PendingCommand {
            command: "a".into(),
            call: 9,
        });
        pending.set_language("rust".into());
        assert_eq!(pending.fail_all().len(), 1);
        assert!(pending.is_empty());
    }
}
