pub use api::session::{Command, Event, Reply};
pub use loog_workspace_api as api;
use std::collections::VecDeque;

#[derive(Clone, Debug)]
pub struct Submitted {
    pub ticket: u64,
    pub command: Command,
    _credit: crate::PublicationCredit,
}

const MAX_PAYLOAD: usize = 1024 * 1024;
const MAX_QUEUE_BYTES: usize = 16 * 1024 * 1024;
const MAX_QUEUE_ITEMS: usize = 256;

#[derive(Default)]
pub(super) struct Mailbox {
    next: u64,
    submissions: Vec<Submitted>,
    submission_bytes: usize,
    events: VecDeque<String>,
    event_bytes: usize,
    budget: crate::PublicationBudget,
}
impl Mailbox {
    #[cfg(test)]
    pub fn submit(&mut self, text: String) -> Result<u64, String> {
        self.submit_with_policy(text, false)
    }
    pub fn submit_with_policy(&mut self, text: String, saving: bool) -> Result<u64, String> {
        if text.len() > MAX_PAYLOAD
            || self.submissions.len() >= MAX_QUEUE_ITEMS
            || self.submission_bytes + text.len() > MAX_QUEUE_BYTES
        {
            return Err("workspace submission budget exceeded".into());
        }
        let command = serde_json::from_str::<Command>(&text)
            .map_err(|e| format!("invalid workspace command: {e}"))?;
        if saving
            && !matches!(&command,
                Command::Request { operation, .. } if !operation.changes_state()
            )
            && !matches!(
                &command,
                Command::Roots { .. }
                    | Command::ResolveSettings { .. }
                    | Command::Documents { .. }
                    | Command::ReadDocument { .. }
                    | Command::Cancel { .. }
                    | Command::CancelWorkspaceEdit { .. }
                    | Command::Complete { .. }
                    | Command::Notify { .. }
            )
        {
            return Err("workspace side effects are unavailable during save participation".into());
        }
        let credit = self
            .budget
            .reserve(text.len())
            .ok_or("workspace publication budget exceeded")?;
        let ticket = self
            .next
            .checked_add(1)
            .ok_or("workspace ticket space exhausted")?;
        self.next = ticket;
        self.submission_bytes += text.len();
        self.submissions.push(Submitted {
            ticket,
            command,
            _credit: credit,
        });
        Ok(ticket)
    }
    pub fn take(&mut self) -> Vec<Submitted> {
        self.submission_bytes = 0;
        std::mem::take(&mut self.submissions)
    }
    pub fn deliver(&mut self, event: &Event) -> Result<(), String> {
        let text = serde_json::to_string(event).map_err(|e| e.to_string())?;
        // The router bounds the inner Delivery; the outer session tag and
        // ticket need a small additional, explicitly bounded envelope budget.
        if text.len() > MAX_PAYLOAD + 1024
            || self.events.len() >= MAX_QUEUE_ITEMS
            || self.event_bytes + text.len() > MAX_QUEUE_BYTES
        {
            return Err("workspace event budget exceeded".into());
        }
        self.event_bytes += text.len();
        self.events.push_back(text);
        Ok(())
    }
    pub fn poll(&mut self) -> Vec<String> {
        // One guest round consumes a bounded batch; remaining events trigger
        // another executor round, rather than a guest busy-loop.
        let count = self.events.len().min(32);
        let values: Vec<_> = self.events.drain(..count).collect();
        self.event_bytes -= values.iter().map(String::len).sum::<usize>();
        values
    }
    pub fn has_events(&self) -> bool {
        !self.events.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn submission_is_deferred_and_malformed_input_does_not_consume_an_id() {
        let mut mailbox = Mailbox::default();
        assert!(
            mailbox
                .submit(r#"{"kind":"grant","scope":1}"#.into())
                .is_err()
        );
        assert_eq!(
            mailbox
                .submit(r#"{"kind":"cancel","request":4}"#.into())
                .unwrap(),
            1
        );
        assert!(mailbox.poll().is_empty());
        let submitted = mailbox.take();
        assert_eq!(submitted.len(), 1);
        assert!(matches!(
            submitted[0].command,
            Command::Cancel { request: 4 }
        ));
        assert!(mailbox.take().is_empty());
    }
    #[test]
    fn event_budget_is_atomic_and_polling_returns_capacity() {
        let mut mailbox = Mailbox::default();
        let event = Event::Reply {
            ticket: 1,
            result: Ok(Reply::Done),
        };
        for _ in 0..MAX_QUEUE_ITEMS {
            mailbox.deliver(&event).unwrap();
        }
        assert!(mailbox.deliver(&event).is_err());
        assert_eq!(mailbox.poll().len(), 32);
        mailbox.deliver(&event).unwrap();
        assert!(mailbox.has_events());
    }

    #[test]
    fn save_participants_can_read_and_complete_but_cannot_start_side_effects() {
        let mut mailbox = Mailbox::default();
        let read = r#"{"kind":"request","scope":1,"operation":{"kind":"read","path":"/a","offset":0,"limit":4},"timeout_ms":100}"#;
        let write = r#"{"kind":"request","scope":1,"operation":{"kind":"write","path":"/a","bytes":[],"expected_revision":null},"timeout_ms":100}"#;
        assert!(mailbox.submit_with_policy(write.into(), true).is_err());
        let edit = r#"{"kind":"apply_workspace_edit","origin":{"document":1,"lifecycle":1,"version":1},"title":"Server edit","documents":[]}"#;
        assert!(mailbox.submit_with_policy(edit.into(), true).is_err());
        assert_eq!(mailbox.submit_with_policy(read.into(), true).unwrap(), 1);
        assert_eq!(mailbox.take().len(), 1);
        assert!(
            mailbox
                .submit_with_policy(
                    r#"{"kind":"cancel_workspace_edit","ticket":1}"#.into(),
                    true
                )
                .is_ok()
        );
        assert!(mailbox.submit_with_policy(edit.into(), false).is_ok());
    }
}
