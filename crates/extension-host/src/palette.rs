//! What an extension asks the command palette to show: bounded, ordered and read by
//! the application after a round. Answers travel back through a bounded queue the
//! guest polls, so an idle guest costs nothing.
use std::collections::VecDeque;

use crate::bindings::loog::palette::picker::{Choice, Entry, Event, View};

const MAX_CHOICES: usize = 1000;
const MAX_DEPTH: u8 = 32;
const MAX_ENTRIES: usize = 64;
const MAX_TAGS: usize = 4;
const MAX_TEXT: usize = 200;
const MAX_NOTE: usize = 200;
const MAX_QUEUE: usize = 64;
const MAX_FIELD_TEXT: usize = 4096;
const MAX_COMMANDS: usize = 64;

/// One request to the application, in the order the guest made it.
#[derive(Clone, Debug)]
pub enum Command {
    Open { session: u64, view: View },
    Update { session: u64, view: View },
    Close { session: u64 },
    Entries(Vec<Entry>),
    PickFile { request: u64, title: String },
}

#[derive(Default)]
pub(super) struct Palette {
    next: u64,
    open: Option<u64>,
    commands: Vec<Command>,
    events: VecDeque<Event>,
}

fn line(text: &str, limit: usize) -> bool {
    !text.is_empty() && text.len() <= limit && !text.contains(['\0', '\n', '\r'])
}

fn optional(text: &Option<String>, limit: usize) -> bool {
    text.as_ref()
        .is_none_or(|text| text.len() <= limit && !text.contains(['\0', '\n', '\r']))
}

fn choice_valid(choice: &Choice) -> bool {
    line(&choice.id, MAX_TEXT)
        && line(&choice.title, MAX_TEXT)
        && optional(&choice.detail, MAX_TEXT)
        && optional(&choice.group, MAX_TEXT)
        && optional(&choice.trailing, MAX_TEXT)
        && optional(&choice.action, MAX_TEXT)
        && choice.depth <= MAX_DEPTH
}

fn view_valid(view: &View) -> Result<(), String> {
    if view.tags.len() > MAX_TAGS || !view.tags.iter().all(|tag| line(tag, MAX_TEXT)) {
        return Err("palette tags are at most four single lines".into());
    }
    if view.placeholder.len() > MAX_TEXT || view.placeholder.contains(['\0', '\n', '\r']) {
        return Err("a palette placeholder is one short line".into());
    }
    if view.choices.len() > MAX_CHOICES || !view.choices.iter().all(choice_valid) {
        return Err("palette choices exceed their budget".into());
    }
    if !optional(&view.note, MAX_NOTE) {
        return Err("a palette note is one short line".into());
    }
    if view.text.as_ref().is_some_and(|text| text.len() > MAX_FIELD_TEXT || text.contains(['\0', '\n', '\r'])) {
        return Err("palette text is one line within its budget".into());
    }
    if !optional(&view.selected, MAX_TEXT) {
        return Err("a palette selection is one row id".into());
    }
    Ok(())
}

impl Palette {
    fn push(&mut self, command: Command) -> Result<(), String> {
        if self.commands.len() >= MAX_COMMANDS {
            return Err("too many palette requests in one round".into());
        }
        self.commands.push(command);
        Ok(())
    }

    pub fn open(&mut self, view: View) -> Result<u64, String> {
        view_valid(&view)?;
        let session = self.next.checked_add(1).ok_or("palette sessions exhausted")?;
        self.push(Command::Open { session, view })?;
        self.next = session;
        self.open = Some(session);
        Ok(session)
    }

    pub fn update(&mut self, session: u64, view: View) -> Result<(), String> {
        view_valid(&view)?;
        if self.open != Some(session) {
            return Err("that palette session is not open".into());
        }
        self.push(Command::Update { session, view })
    }

    pub fn close(&mut self, session: u64) -> Result<(), String> {
        if self.open != Some(session) {
            return Ok(());
        }
        self.open = None;
        self.push(Command::Close { session })
    }

    pub fn publish_entries(&mut self, entries: Vec<Entry>) -> Result<(), String> {
        if entries.len() > MAX_ENTRIES
            || !entries.iter().all(|entry| {
                line(&entry.id, MAX_TEXT)
                    && line(&entry.title, MAX_TEXT)
                    && optional(&entry.detail, MAX_TEXT)
                    && line(&entry.category, MAX_TEXT)
            })
        {
            return Err("palette entries exceed their budget".into());
        }
        self.push(Command::Entries(entries))
    }

    pub fn pick_file(&mut self, title: String) -> Result<u64, String> {
        if title.len() > MAX_TEXT || title.contains(['\0', '\n', '\r']) {
            return Err("a file dialog title is one short line".into());
        }
        let request = self.next.checked_add(1).ok_or("palette requests exhausted")?;
        self.push(Command::PickFile { request, title })?;
        self.next = request;
        Ok(request)
    }

    pub fn take(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.commands)
    }

    /// Queue an answer for the guest. A session that is no longer the open one gets none.
    pub fn deliver(&mut self, event: Event) -> Result<(), String> {
        let current = match &event {
            Event::Input(input) => Some(input.session),
            Event::Accepted(accepted) | Event::Complete(accepted) => Some(accepted.session),
            Event::Cancelled(session) => Some(*session),
            Event::Entry(_) | Event::File(_) => None,
        };
        if let Some(session) = current {
            let cancelled = matches!(event, Event::Cancelled(_));
            if self.open != Some(session) {
                return Ok(());
            }
            if cancelled {
                self.open = None;
            }
        }
        let too_long = match &event {
            Event::Input(input) => input.text.len() > MAX_FIELD_TEXT,
            Event::Accepted(accepted) | Event::Complete(accepted) => accepted.text.len() > MAX_FIELD_TEXT,
            _ => false,
        };
        if too_long {
            return Err("palette text exceeds its budget".into());
        }
        // Typing outruns the guest: of a run of unread text only the newest matters.
        if let Event::Input(next) = &event
            && let Some(Event::Input(last)) = self.events.back_mut()
            && last.session == next.session
        {
            *last = next.clone();
            return Ok(());
        }
        if self.events.len() >= MAX_QUEUE {
            return Err("palette event budget exceeded".into());
        }
        self.events.push_back(event);
        Ok(())
    }

    pub fn poll(&mut self) -> Vec<Event> {
        let count = self.events.len().min(32);
        self.events.drain(..count).collect()
    }

    pub fn has_events(&self) -> bool {
        !self.events.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::loog::palette::picker::{Accepted, Activity, Input, Mode, Trigger};

    fn view() -> View {
        View {
            tags: vec!["one".into()],
            placeholder: "type".into(),
            mode: Mode::Choose,
            choices: vec![],
            activity: Activity::Idle,
            note: None,
            text: None,
            selected: None,
        }
    }

    fn row(id: &str) -> Choice {
        Choice {
            id: id.into(),
            title: id.into(),
            detail: None,
            group: None,
            trailing: None,
            depth: 0,
            expanded: None,
            icon: None,
            action: None,
            dim: false,
        }
    }

    #[test]
    fn a_session_is_open_until_it_is_closed_or_cancelled() {
        let mut palette = Palette::default();
        let session = palette.open(view()).unwrap();
        palette.update(session, view()).unwrap();
        assert!(palette.update(session + 1, view()).is_err());
        palette.close(session).unwrap();
        assert!(palette.update(session, view()).is_err());
        assert_eq!(palette.take().len(), 3);
    }

    #[test]
    fn answers_for_a_closed_session_are_dropped() {
        let mut palette = Palette::default();
        let session = palette.open(view()).unwrap();
        palette
            .deliver(Event::Accepted(Accepted {
                session,
                choice: None,
                text: "a".into(),
                trigger: Trigger::Enter,
            }))
            .unwrap();
        palette.close(session).unwrap();
        palette.deliver(Event::Cancelled(session)).unwrap();
        assert_eq!(palette.poll().len(), 1);
    }

    #[test]
    fn a_burst_of_typing_keeps_only_the_newest_text_and_an_answer_ends_the_run() {
        let mut palette = Palette::default();
        let session = palette.open(view()).unwrap();
        let typed = |text: &str| Event::Input(Input { session, text: text.into() });
        for text in ["/", "/h", "/ho"] {
            palette.deliver(typed(text)).unwrap();
        }
        palette
            .deliver(Event::Accepted(Accepted { session, choice: None, text: "/ho".into(), trigger: Trigger::Enter }))
            .unwrap();
        palette.deliver(typed("/hom")).unwrap();
        let events = palette.poll();
        assert_eq!(events.len(), 3);
        assert!(matches!(&events[0], Event::Input(input) if input.text == "/ho"));
        assert!(matches!(&events[2], Event::Input(input) if input.text == "/hom"));
        assert!(palette.deliver(typed(&"x".repeat(MAX_FIELD_TEXT + 1))).is_err());
    }

    #[test]
    fn tree_rows_and_field_text_are_bounded() {
        let mut palette = Palette::default();
        let mut deep = row("a");
        deep.depth = MAX_DEPTH + 1;
        let mut bad = view();
        bad.choices = vec![deep];
        assert!(palette.open(bad).is_err());
        let mut hint = row("b");
        hint.action = Some("two\nlines".into());
        let mut bad = view();
        bad.choices = vec![hint];
        assert!(palette.open(bad).is_err());
        let mut bad = view();
        bad.text = Some("a\nb".into());
        assert!(palette.open(bad).is_err());
        let mut good = view();
        good.mode = Mode::Browse;
        good.text = Some("/home/liu/".into());
        good.choices = (0..MAX_CHOICES).map(|index| row(&index.to_string())).collect();
        palette.open(good).unwrap();
        let mut many = view();
        many.choices = (0..=MAX_CHOICES).map(|index| row(&index.to_string())).collect();
        assert!(palette.open(many).is_err());
    }

    #[test]
    fn views_and_entries_are_bounded() {
        let mut palette = Palette::default();
        let mut bad = view();
        bad.tags = vec!["x".into(); MAX_TAGS + 1];
        assert!(palette.open(bad).is_err());
        let mut bad = view();
        bad.note = Some("two\nlines".into());
        assert!(palette.open(bad).is_err());
        let entry = |id: usize| Entry {
            id: id.to_string(),
            title: "t".into(),
            detail: None,
            category: "c".into(),
        };
        assert!(
            palette
                .publish_entries((0..=MAX_ENTRIES).map(entry).collect())
                .is_err()
        );
        palette
            .publish_entries((0..MAX_ENTRIES).map(entry).collect())
            .unwrap();
    }

    #[test]
    fn a_cancelled_session_cannot_be_updated() {
        let mut palette = Palette::default();
        let session = palette.open(view()).unwrap();
        palette.deliver(Event::Cancelled(session)).unwrap();
        assert!(palette.update(session, view()).is_err());
    }
}
