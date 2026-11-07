//! Output channels: bounded, append-only logs that the application, extensions
//! and extension processes write to and the Output panel reads.
//!
//! Channels are data. A writer names its channel and gives it a title; nothing
//! here knows what an extension or a language service is. Every channel is a
//! ring buffer, so a chatty source can never grow memory without bound.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Most lines one channel keeps.
pub(crate) const MAX_LINES: usize = 10_000;
/// Most bytes of text one channel keeps.
pub(crate) const MAX_BYTES: usize = 4 * 1024 * 1024;
/// Longest single line; the rest is cut and marked.
pub(crate) const MAX_LINE: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    pub const ALL: [Self; 5] = [
        Self::Trace,
        Self::Debug,
        Self::Info,
        Self::Warn,
        Self::Error,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Trace => "TRACE",
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct ChannelId(pub Arc<str>);

impl ChannelId {
    pub fn new(id: impl AsRef<str>) -> Self {
        Self(Arc::from(id.as_ref()))
    }
}

/// Where a channel is listed. Display grouping only; it carries no behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Group {
    Application,
    Extension,
    Process,
}

impl Group {
    pub fn label(self) -> &'static str {
        match self {
            Self::Application => "Application",
            Self::Extension => "Extensions",
            Self::Process => "Processes",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Line {
    /// Never reused within a channel, so a reader can ask for what is new.
    pub seq: u64,
    /// Time since the hub was created.
    pub at: Duration,
    pub level: Level,
    pub text: Arc<str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChannelInfo {
    pub id: ChannelId,
    pub title: String,
    pub group: Group,
    /// An error arrived that nobody has looked at yet.
    pub unread_error: bool,
    pub len: usize,
}

struct Channel {
    title: String,
    group: Group,
    lines: VecDeque<Line>,
    bytes: usize,
    next_seq: u64,
    unread_error: bool,
}

impl Channel {
    fn push(&mut self, at: Duration, level: Level, text: &str) {
        let text = clip(text);
        self.bytes += text.len();
        self.lines.push_back(Line {
            seq: self.next_seq,
            at,
            level,
            text: Arc::from(text.as_str()),
        });
        self.next_seq += 1;
        if level >= Level::Error {
            self.unread_error = true;
        }
        while self.lines.len() > MAX_LINES || self.bytes > MAX_BYTES {
            let Some(old) = self.lines.pop_front() else {
                break;
            };
            self.bytes -= old.text.len();
        }
    }
}

fn clip(text: &str) -> String {
    let text = text.trim_end_matches(['\r', '\n']);
    if text.len() <= MAX_LINE {
        return text.to_owned();
    }
    let mut end = MAX_LINE;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [cut]", &text[..end])
}

struct Inner {
    start: Instant,
    /// Insertion order, so the list in the panel is stable.
    order: Vec<ChannelId>,
    channels: std::collections::HashMap<ChannelId, Channel>,
    /// Bumped by every change; a reader compares it to skip redundant work.
    revision: u64,
}

/// Asks the window for a frame.
type Wake = Arc<dyn Fn() + Send + Sync>;

/// All channels. Cloning shares the same logs.
#[derive(Clone)]
pub(crate) struct LogHub {
    inner: Arc<Mutex<Inner>>,
    wake: Arc<Mutex<Option<Wake>>>,
}

impl Default for LogHub {
    fn default() -> Self {
        Self::new()
    }
}

impl LogHub {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                start: Instant::now(),
                order: Vec::new(),
                channels: Default::default(),
                revision: 0,
            })),
            wake: Arc::new(Mutex::new(None)),
        }
    }

    /// Asks the window for a frame after a write, so a visible panel follows
    /// its channel without polling.
    pub fn on_change(&self, wake: impl Fn() + Send + Sync + 'static) {
        *self.wake.lock().unwrap() = Some(Arc::new(wake));
    }

    fn touch(&self) {
        let wake = self.wake.lock().unwrap().clone();
        if let Some(wake) = wake {
            wake();
        }
    }

    /// Creates the channel if it is new and keeps its title current.
    pub fn declare(&self, id: &ChannelId, title: &str, group: Group) {
        let mut inner = self.inner.lock().unwrap();
        let known = inner.channels.contains_key(id);
        let channel = inner.channels.entry(id.clone()).or_insert_with(|| Channel {
            title: title.to_owned(),
            group,
            lines: VecDeque::new(),
            bytes: 0,
            next_seq: 0,
            unread_error: false,
        });
        if known {
            channel.title = title.to_owned();
            channel.group = group;
        } else {
            inner.order.push(id.clone());
        }
        inner.revision += 1;
    }

    /// Appends one line, splitting on newlines. A channel that was never
    /// declared is created with its id as title.
    pub fn append(&self, id: &ChannelId, level: Level, text: &str) {
        {
            let mut inner = self.inner.lock().unwrap();
            if !inner.channels.contains_key(id) {
                drop(inner);
                self.declare(id, &id.0, Group::Application);
                inner = self.inner.lock().unwrap();
            }
            let at = inner.start.elapsed();
            let channel = inner.channels.get_mut(id).expect("declared above");
            for line in text.split('\n') {
                channel.push(at, level, line);
            }
            inner.revision += 1;
        }
        self.touch();
    }

    pub fn clear(&self, id: &ChannelId) {
        {
            let mut inner = self.inner.lock().unwrap();
            if let Some(channel) = inner.channels.get_mut(id) {
                channel.lines.clear();
                channel.bytes = 0;
                channel.unread_error = false;
            }
            inner.revision += 1;
        }
        self.touch();
    }

    /// The reader has seen this channel's errors.
    pub fn mark_seen(&self, id: &ChannelId) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(channel) = inner.channels.get_mut(id)
            && channel.unread_error
        {
            channel.unread_error = false;
            inner.revision += 1;
        }
    }

    pub fn revision(&self) -> u64 {
        self.inner.lock().unwrap().revision
    }

    /// Channels in display order: application first, then extensions, then
    /// processes, each in the order they were created.
    pub fn channels(&self) -> Vec<ChannelInfo> {
        let inner = self.inner.lock().unwrap();
        let mut list = inner
            .order
            .iter()
            .filter_map(|id| {
                let channel = inner.channels.get(id)?;
                Some(ChannelInfo {
                    id: id.clone(),
                    title: channel.title.clone(),
                    group: channel.group,
                    unread_error: channel.unread_error,
                    len: channel.lines.len(),
                })
            })
            .collect::<Vec<_>>();
        list.sort_by_key(|info| info.group);
        list
    }

    /// Lines at or above `min` that contain `needle` (case-insensitive; empty
    /// matches all), oldest first.
    pub fn lines(&self, id: &ChannelId, min: Level, needle: &str) -> Vec<Line> {
        let inner = self.inner.lock().unwrap();
        let Some(channel) = inner.channels.get(id) else {
            return Vec::new();
        };
        let needle = needle.to_lowercase();
        channel
            .lines
            .iter()
            .filter(|line| line.level >= min)
            .filter(|line| needle.is_empty() || line.text.to_lowercase().contains(&needle))
            .cloned()
            .collect()
    }

    /// A window of `lines`, so a long channel is never copied whole: how many
    /// lines match in all, and the `count` that start at match number `first`.
    pub fn window(
        &self,
        id: &ChannelId,
        min: Level,
        needle: &str,
        first: usize,
        count: usize,
    ) -> (usize, Vec<Line>) {
        let inner = self.inner.lock().unwrap();
        let Some(channel) = inner.channels.get(id) else {
            return (0, Vec::new());
        };
        let needle = needle.to_lowercase();
        let mut total = 0;
        let mut shown = Vec::new();
        for line in channel.lines.iter().filter(|line| line.level >= min) {
            if !needle.is_empty() && !line.text.to_lowercase().contains(&needle) {
                continue;
            }
            if total >= first && shown.len() < count {
                shown.push(line.clone());
            }
            total += 1;
        }
        (total, shown)
    }
}

static HUB: OnceLock<LogHub> = OnceLock::new();

/// The application-wide hub.
pub(crate) fn hub() -> &'static LogHub {
    HUB.get_or_init(LogHub::new)
}

/// The channel for the application itself.
pub(crate) fn app_channel() -> ChannelId {
    ChannelId::new("app")
}

/// Writes to the application channel.
pub(crate) fn app_log(level: Level, text: &str) {
    let id = app_channel();
    hub().declare(&id, "Application", Group::Application);
    hub().append(&id, level, text);
}

/// The channel an extension's lifecycle and own messages go to.
pub(crate) fn extension_channel(extension: &str) -> ChannelId {
    ChannelId::new(format!("ext:{extension}"))
}

/// Writes to an extension's channel, creating it titled with the extension's id.
pub(crate) fn extension_log(extension: &str, level: Level, text: &str) {
    let id = extension_channel(extension);
    hub().declare(&id, extension, Group::Extension);
    hub().append(&id, level, text);
}

/// Formats a time since start as `mm:ss.mmm`.
pub(crate) fn format_at(at: Duration) -> String {
    let ms = at.as_millis();
    format!("{:02}:{:02}.{:03}", ms / 60_000, ms / 1000 % 60, ms % 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub_with(id: &str) -> (LogHub, ChannelId) {
        let hub = LogHub::new();
        let id = ChannelId::new(id);
        hub.declare(&id, "Test", Group::Extension);
        (hub, id)
    }

    #[test]
    fn keeps_only_the_newest_lines() {
        let (hub, id) = hub_with("a");
        for n in 0..MAX_LINES + 5 {
            hub.append(&id, Level::Info, &format!("line {n}"));
        }
        let lines = hub.lines(&id, Level::Trace, "");
        assert_eq!(lines.len(), MAX_LINES);
        assert_eq!(&*lines[0].text, "line 5");
        assert_eq!(lines[0].seq, 5);
    }

    #[test]
    fn byte_cap_drops_oldest_first() {
        let (hub, id) = hub_with("a");
        let chunk = "x".repeat(MAX_LINE);
        for _ in 0..(MAX_BYTES / MAX_LINE) + 10 {
            hub.append(&id, Level::Info, &chunk);
        }
        let lines = hub.lines(&id, Level::Trace, "");
        assert!(lines.iter().map(|l| l.text.len()).sum::<usize>() <= MAX_BYTES);
        assert!(lines.len() >= MAX_BYTES / MAX_LINE - 1);
    }

    #[test]
    fn long_lines_are_cut_on_a_char_boundary() {
        let (hub, id) = hub_with("a");
        hub.append(&id, Level::Info, &"é".repeat(MAX_LINE));
        let line = &hub.lines(&id, Level::Trace, "")[0];
        assert!(line.text.ends_with("[cut]"));
        assert!(line.text.len() <= MAX_LINE + "… [cut]".len());
    }

    #[test]
    fn splits_multiline_text_and_filters() {
        let (hub, id) = hub_with("a");
        hub.append(&id, Level::Info, "one\ntwo\r\n");
        hub.append(&id, Level::Error, "Boom");
        assert_eq!(hub.lines(&id, Level::Trace, "").len(), 4);
        assert_eq!(hub.lines(&id, Level::Warn, "").len(), 1);
        assert_eq!(hub.lines(&id, Level::Trace, "TW").len(), 1);
    }

    #[test]
    fn unread_error_is_set_by_errors_and_cleared_when_seen() {
        let (hub, id) = hub_with("a");
        hub.append(&id, Level::Warn, "careful");
        assert!(!hub.channels()[0].unread_error);
        hub.append(&id, Level::Error, "failed");
        assert!(hub.channels()[0].unread_error);
        hub.mark_seen(&id);
        assert!(!hub.channels()[0].unread_error);
    }

    #[test]
    fn clear_releases_lines_and_unread_state() {
        let (hub, id) = hub_with("a");
        hub.append(&id, Level::Error, "failed");
        hub.clear(&id);
        assert!(hub.lines(&id, Level::Trace, "").is_empty());
        assert!(!hub.channels()[0].unread_error);
        hub.append(&id, Level::Info, "again");
        assert_eq!(hub.lines(&id, Level::Trace, "")[0].seq, 1);
    }

    #[test]
    fn window_counts_all_matches_but_copies_only_the_slice() {
        let (hub, id) = hub_with("a");
        for n in 0..10 {
            hub.append(&id, Level::Info, &format!("row {n}"));
        }
        hub.append(&id, Level::Debug, "hidden");
        let (total, shown) = hub.window(&id, Level::Info, "", 3, 4);
        assert_eq!(total, 10);
        assert_eq!(
            shown.iter().map(|l| l.text.to_string()).collect::<Vec<_>>(),
            ["row 3", "row 4", "row 5", "row 6"]
        );
        assert_eq!(hub.window(&id, Level::Info, "ROW 9", 0, 5).0, 1);
        assert_eq!(
            hub.window(&ChannelId::new("none"), Level::Info, "", 0, 5).0,
            0
        );
    }

    #[test]
    fn channels_list_by_group_then_creation_order() {
        let hub = LogHub::new();
        for (id, group) in [
            ("p", Group::Process),
            ("e1", Group::Extension),
            ("app", Group::Application),
            ("e2", Group::Extension),
        ] {
            hub.declare(&ChannelId::new(id), id, group);
        }
        let ids = hub
            .channels()
            .into_iter()
            .map(|c| c.id.0.to_string())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["app", "e1", "e2", "p"]);
    }

    #[test]
    fn writes_wake_the_window() {
        let (hub, id) = hub_with("a");
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = count.clone();
        hub.on_change(move || {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        hub.append(&id, Level::Info, "x");
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn formats_time_since_start() {
        assert_eq!(format_at(Duration::from_millis(61_005)), "01:01.005");
    }
}
