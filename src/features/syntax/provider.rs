//! What the syntax worker needs from whatever colours code.
//!
//! The worker schedules, cancels and caches; it does not care how spans come to
//! be. Today the engine is the Tree-sitter index in this module's neighbours; this
//! trait is the seam where another one fits, such as an engine hosted by an
//! extension. An engine is owned by the worker's thread and works one request at a
//! time. Languages no engine covers never reach it: the worker colours them with
//! the built-in highlighter in `generic`.
use super::{Source, TreeIndex, TreeStats, Window, WindowTokens};
use crate::domain::identity::FileId;
use crate::domain::language::ResolvedLanguage;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub(crate) trait SpanProvider: Send {
    /// Spans that tile each of `windows` of `source`, in order. `None` when the
    /// work was cancelled or the windows do not fit the text. `cancel` may be set
    /// at any moment; stop soon after it is.
    fn prepare(
        &mut self,
        id: FileId,
        source: Source,
        language: ResolvedLanguage,
        windows: &[Window],
        cancel: &AtomicBool,
    ) -> Option<Vec<WindowTokens>>;

    /// Foldable ranges of the document as `prepare` last saw `source`, taken from
    /// its structure and never from colour spans; `None` when unknown.
    fn folding(
        &mut self,
        id: FileId,
        source: &Source,
        cancel: &AtomicBool,
    ) -> Option<Arc<[Range<usize>]>>;

    /// Drop what is kept for a document.
    fn forget(&mut self, id: FileId);

    /// Drop what is kept for every document not in `open`.
    fn retain(&mut self, open: &[FileId]);

    fn stats(&self) -> TreeStats;
}

impl SpanProvider for TreeIndex {
    fn prepare(
        &mut self,
        id: FileId,
        source: Source,
        language: ResolvedLanguage,
        windows: &[Window],
        cancel: &AtomicBool,
    ) -> Option<Vec<WindowTokens>> {
        TreeIndex::prepare(self, id, source, language, windows, cancel)
    }

    fn folding(
        &mut self,
        id: FileId,
        source: &Source,
        cancel: &AtomicBool,
    ) -> Option<Arc<[Range<usize>]>> {
        TreeIndex::folding(self, id, source, cancel)
    }

    fn forget(&mut self, id: FileId) {
        TreeIndex::forget(self, id)
    }

    fn retain(&mut self, open: &[FileId]) {
        TreeIndex::retain(self, open)
    }

    fn stats(&self) -> TreeStats {
        self.stats
    }
}
