//! Spans kept from the last version of a document that was fully coloured, so the
//! editor can keep showing them, moved across the edits made since, while the
//! next version is still being parsed.
//!
//! What is kept is only the colour of recently visible text: no tree, no parser
//! and no copy of the document. The edit journal of the current text says how
//! to move it. The result is for drawing only (see `features::syntax::presentation`).
//!
//! The kept pages are carried forward one frame at a time: each frame applies only
//! the edits made since the previous one, so a long burst of typing costs the same
//! per frame as the first keystroke. That is the same arithmetic as carrying the
//! original pages across every edit at once.
use crate::domain::{identity::FileId, language::ResolvedLanguage};
use crate::features::syntax::{
    Freshness, Source, TokenSpan, Window, WindowTokens,
    presentation::{Refused, project_window},
};
use std::{
    collections::HashMap,
    ops::Range,
    sync::Arc,
    time::{Duration, Instant},
};

/// Bytes of retained spans across all documents.
pub(super) const BUDGET: usize = 4 * 1024 * 1024;

type Page = (Range<usize>, Arc<[TokenSpan]>);

struct Retained {
    incarnation: u64,
    language_generation: u64,
    language: ResolvedLanguage,
    /// The version the colours were last fully computed for.
    anchor_version: u64,
    /// The version and snapshot `pages` currently describe.
    version: u64,
    identity: usize,
    pages: Vec<Page>,
    bytes: usize,
    used: u64,
}

/// Counters for acceptance tests and measurements.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ProvisionalStats {
    /// Windows answered from retained spans.
    pub windows: usize,
    /// Projections skipped because the input exceeded a limit.
    pub refused: usize,
    /// Retained spans dropped because the edit journal no longer led to them.
    pub broken_chain: usize,
    pub time: Duration,
}

#[derive(Default)]
pub(super) struct RetainedSet {
    documents: HashMap<FileId, Retained>,
    bytes: usize,
    clock: u64,
    pub stats: ProvisionalStats,
}

fn size(pages: &[Page]) -> usize {
    pages
        .iter()
        .map(|(_, spans)| std::mem::size_of_val(&**spans) + std::mem::size_of::<Page>())
        .sum()
}

impl RetainedSet {
    /// Replaces what is retained for `document` with `pages`, computed for the
    /// text identified by `version` and `identity`.
    #[allow(clippy::too_many_arguments)]
    pub fn keep(
        &mut self,
        document: FileId,
        version: u64,
        identity: usize,
        incarnation: u64,
        language_generation: u64,
        language: ResolvedLanguage,
        pages: Vec<Page>,
    ) {
        self.forget(document);
        self.clock += 1;
        let bytes = size(&pages);
        self.bytes += bytes;
        self.documents.insert(
            document,
            Retained {
                incarnation,
                language_generation,
                language,
                anchor_version: version,
                version,
                identity,
                pages,
                bytes,
                used: self.clock,
            },
        );
        while self.bytes > BUDGET {
            let Some(oldest) = self
                .documents
                .iter()
                .filter(|(id, _)| **id != document)
                .min_by_key(|(_, retained)| retained.used)
                .map(|(id, _)| *id)
            else {
                break;
            };
            self.forget(oldest);
        }
    }

    pub fn forget(&mut self, document: FileId) {
        if let Some(old) = self.documents.remove(&document) {
            self.bytes -= old.bytes;
        }
    }

    pub fn retain(&mut self, open: &[FileId]) {
        let closed: Vec<FileId> = self
            .documents
            .keys()
            .copied()
            .filter(|id| !open.contains(id))
            .collect();
        for id in closed {
            self.forget(id);
        }
    }

    /// The retained spans for `window`, moved to the text of `source`. `None`
    /// when nothing is retained for the window, or for the document's current
    /// language and lifetime, or the edits since cannot be followed.
    pub fn provisional(
        &mut self,
        document: FileId,
        source: &Source,
        incarnation: u64,
        language_generation: u64,
        language: &ResolvedLanguage,
        window: &Window,
    ) -> Option<WindowTokens> {
        let retained = self.documents.get_mut(&document)?;
        if (
            retained.incarnation,
            retained.language_generation,
            &retained.language,
        ) != (incarnation, language_generation, language)
        {
            self.forget(document);
            return None;
        }
        if source.version < retained.version {
            // A view of an older text than the one already followed: say nothing
            // for it, but keep what is kept for the newer one.
            return None;
        }
        self.clock += 1;
        retained.used = self.clock;
        let started = Instant::now();
        if (source.version, source.identity()) != (retained.version, retained.identity) {
            let Some(chain) = source.changes_since_version(retained.version, retained.identity)
            else {
                self.stats.broken_chain += 1;
                self.forget(document);
                return None;
            };
            let mut pages = Vec::with_capacity(retained.pages.len());
            for (range, spans) in &retained.pages {
                match project_window(range.clone(), spans, &chain, source.text.len()) {
                    Ok((range, spans)) => pages.push((range, Arc::from(spans))),
                    Err(Refused::Spans | Refused::Revisions | Refused::Edits) => {
                        self.stats.refused += 1;
                        // Too much to follow in one step: show plain text until the
                        // parse is ready, and do not try again for these pages.
                        self.forget(document);
                        return None;
                    }
                }
            }
            pages.sort_by_key(|(range, _)| range.start);
            let bytes = size(&pages);
            self.bytes = self.bytes - retained.bytes + bytes;
            retained.bytes = bytes;
            retained.pages = pages;
            retained.version = source.version;
            retained.identity = source.identity();
        }
        let spans = assemble(&retained.pages, &window.range)?;
        let freshness = if retained.anchor_version == source.version {
            Freshness::Fresh
        } else {
            Freshness::Provisional
        };
        self.stats.windows += 1;
        self.stats.time += started.elapsed();
        Some(WindowTokens {
            range: window.range.clone(),
            spans: spans.into(),
            complete: false,
            freshness,
        })
    }
}

/// The spans of `window` out of `pages` (sorted by start), with plain text
/// where no page says anything. `None` when no page touches the window.
fn assemble(pages: &[Page], window: &Range<usize>) -> Option<Vec<TokenSpan>> {
    let mut spans: Vec<TokenSpan> = Vec::new();
    let mut cursor = window.start;
    let mut touched = false;
    let mut add = |range: Range<usize>, class| {
        if range.is_empty() {
            return;
        }
        match spans.last_mut() {
            Some(last) if last.class == class && last.range.end == range.start => {
                last.range.end = range.end;
            }
            _ => spans.push(TokenSpan { range, class }),
        }
    };
    for (range, page) in pages {
        if range.end <= cursor || range.start >= window.end {
            continue;
        }
        touched = true;
        for span in page.iter() {
            let start = span.range.start.max(cursor).max(window.start);
            let end = span.range.end.min(window.end);
            if start >= end {
                continue;
            }
            add(cursor..start, None);
            add(start..end, span.class);
            cursor = end;
        }
    }
    if !touched {
        return None;
    }
    add(cursor..window.end, None);
    Some(spans)
}
