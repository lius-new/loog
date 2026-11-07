//! The text snapshot a result belongs to, the windows asked for and the spans
//! returned for them.
use super::TokenSpan;
use loog_editor_core::text::{TextBuffer, TextRevision};
use std::{collections::VecDeque, ops::Range, sync::Arc};

/// An immutable view of a document's text together with the journal that says
/// how earlier versions led to it.
#[derive(Clone)]
pub(crate) struct Source {
    pub text: Arc<String>,
    pub version: u64,
    revisions: Arc<VecDeque<TextRevision>>,
}

impl Source {
    pub fn new(buffer: &TextBuffer) -> Self {
        Self {
            text: buffer.text_snapshot(),
            version: buffer.version(),
            revisions: buffer.revision_log(),
        }
    }

    pub fn same(&self, other: &Self) -> bool {
        self.version == other.version && Arc::ptr_eq(&self.text, &other.text)
    }

    pub fn identity(&self) -> usize {
        Arc::as_ptr(&self.text) as usize
    }

    /// The edits that lead from `old` to this text, or `None` when the journal
    /// no longer holds an unbroken chain between them.
    pub(crate) fn changes_since(&self, old: &Self) -> Option<Vec<TextRevision>> {
        self.changes_since_version(old.version, old.identity())
    }

    /// Like `changes_since`, for a text that is only known by its version and
    /// snapshot identity, so it need not be kept alive.
    pub(crate) fn changes_since_version(
        &self,
        version: u64,
        identity: usize,
    ) -> Option<Vec<TextRevision>> {
        let mut current = (version, identity);
        let mut changes = Vec::new();
        for revision in self.revisions.iter().filter(|r| r.after_version > version) {
            if (revision.before_version, revision.before_snapshot) != current {
                return None;
            }
            current = (revision.after_version, revision.after_snapshot);
            changes.push(revision.clone());
        }
        (current == (self.version, self.identity())).then_some(changes)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Window {
    pub line: Range<usize>,
    pub range: Range<usize>,
}

/// Whether spans were computed for the version they are shown with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Freshness {
    /// Read from the syntax of exactly this version of the text.
    Fresh,
    /// Carried over from an earlier version across the recorded edits, to be
    /// drawn until the parser catches up. Never a parse result.
    Provisional,
}

#[derive(Clone, Debug)]
pub(crate) struct WindowTokens {
    pub range: Range<usize>,
    pub spans: Arc<[TokenSpan]>,
    /// Known spans remain useful while another part of this range is pending.
    pub complete: bool,
    pub freshness: Freshness,
}

impl WindowTokens {
    pub(crate) fn fresh(range: Range<usize>, spans: impl Into<Arc<[TokenSpan]>>) -> Self {
        Self {
            range,
            spans: spans.into(),
            complete: true,
            freshness: Freshness::Fresh,
        }
    }
}
