// Coalesced viewport work. Parsing and querying never hold the UI lock.
pub(crate) use self::retained::ProvisionalStats;
use self::retained::RetainedSet;
use crate::{
    domain::{
        document::DocumentStamp,
        identity::{FileId, PaneId},
        language::{PLAIN_TEXT, ResolvedLanguage},
    },
    features::syntax::{
        Freshness, Resources, Source, TokenSpan, TreeIndex, TreeStats, Window, WindowTokens,
        generic, provider::SpanProvider,
    },
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

const WINDOW_BUDGET: usize = 4 * 1024 * 1024;

/// The engine Loom ships: Tree-sitter, driven by the grammars extensions supply.
fn tree_sitter_engine(resources: Arc<dyn Resources>) -> Box<dyn SpanProvider> {
    Box::new(TreeIndex::new(resources))
}
// Cache pages are anchored to source lines, independently of caret, scroll,
// viewport width and language. Screen clipping never invalidates known tokens.
const PAGE_BYTES: usize = 1024;

fn pages(source: &Source, window: &Window) -> Vec<Window> {
    if window.range.is_empty() {
        return vec![window.clone()];
    }
    let first = (window.range.start - window.line.start) / PAGE_BYTES;
    let last = (window.range.end - 1 - window.line.start) / PAGE_BYTES;
    (first..=last)
        .map(|page| {
            let floor = |mut offset| {
                while !source.text.is_char_boundary(offset) {
                    offset -= 1;
                }
                offset
            };
            Window {
                line: window.line.clone(),
                range: floor(window.line.start + page * PAGE_BYTES)
                    ..floor((window.line.start + (page + 1) * PAGE_BYTES).min(window.line.end)),
            }
        })
        .filter(|page| page.range.start < window.range.end && page.range.end > window.range.start)
        .collect()
}
#[derive(Clone)]
struct Request {
    stamp: DocumentStamp,
    document: FileId,
    source: Source,
    lang: ResolvedLanguage,
    windows: Vec<Window>,
}

struct Epoch {
    identity: usize,
    version: u64,
    incarnation: u64,
    language_generation: u64,
    lang: ResolvedLanguage,
    cancel: Arc<AtomicBool>,
}
impl Epoch {
    fn matches(&self, request: &Request) -> bool {
        self.identity == request.source.identity()
            && self.version == request.source.version
            && self.incarnation == request.stamp.incarnation
            && self.language_generation == request.stamp.language_generation
            && self.lang == request.lang
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Key {
    document: FileId,
    incarnation: u64,
    language_generation: u64,
    identity: usize,
    version: u64,
    lang: ResolvedLanguage,
    start: usize,
    end: usize,
}
impl Key {
    fn new(request: &Request, window: &Window) -> Self {
        Self {
            document: request.document,
            incarnation: request.stamp.incarnation,
            language_generation: request.stamp.language_generation,
            identity: request.source.identity(),
            version: request.source.version,
            lang: request.lang,
            start: window.range.start,
            end: window.range.end,
        }
    }
}
struct Cached {
    window: WindowTokens,
    used: u64,
    bytes: usize,
}
impl Cached {
    /// The spans cut to `range`.
    fn clipped(&self, range: &std::ops::Range<usize>) -> impl Iterator<Item = TokenSpan> + '_ {
        let range = range.clone();
        self.intersecting(&range).iter().map(move |span| TokenSpan {
            range: span.range.start.max(range.start)..span.range.end.min(range.end),
            class: span.class,
        })
    }

    fn intersecting(&self, range: &std::ops::Range<usize>) -> &[TokenSpan] {
        let first = self
            .window
            .spans
            .partition_point(|span| span.range.end <= range.start);
        let last = self
            .window
            .spans
            .partition_point(|span| span.range.start < range.end);
        &self.window.spans[first..last]
    }
}
type StructuralRanges = Arc<[std::ops::Range<usize>]>;
#[derive(Default)]
struct State {
    structural: HashMap<FileId, (DocumentStamp, StructuralRanges)>,
    targets: HashMap<PaneId, Request>,
    pending: VecDeque<FileId>,
    running: Option<(Request, Vec<Window>, Arc<AtomicBool>)>,
    epochs: HashMap<FileId, Epoch>,
    ready: HashMap<Key, Cached>,
    bytes: usize,
    clock: u64,
    hits: usize,
    evictions: usize,
    prune: Option<Vec<FileId>>,
    forget: Vec<FileId>,
    tree_stats: TreeStats,
    /// The last fully coloured spans of each document, for drawing while a
    /// newer version is parsed.
    retained: RetainedSet,
}
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    stopped: AtomicBool,
}
struct Owner(Arc<Shared>, #[allow(dead_code)] Arc<dyn Fn() + Send + Sync>);
impl Drop for Owner {
    fn drop(&mut self) {
        stop(&self.0);
    }
}
fn stop(shared: &Shared) {
    shared.stopped.store(true, Ordering::Release);
    let state = shared.state.lock().unwrap();
    for epoch in state.epochs.values() {
        epoch.cancel.store(true, Ordering::Release);
    }
    shared.wake.notify_all();
}

#[derive(Clone)]
pub(crate) struct Worker(Arc<Owner>);
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Stats {
    pub tree: TreeStats,
    pub provisional: ProvisionalStats,
    pub cache_bytes: usize,
    pub cache_hits: usize,
    pub evictions: usize,
}
impl Worker {
    #[cfg(test)]
    pub(crate) fn with_resources(
        resources: Arc<dyn crate::features::syntax::Resources>,
        ready: impl Fn(DocumentStamp) + Send + Sync + 'static,
    ) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::default(),
            wake: Condvar::new(),
            stopped: AtomicBool::new(false),
        });
        Self::start_resources(shared, resources, ready, || {}, tree_sitter_engine)
    }
    pub fn new(ready: impl Fn(DocumentStamp) + Send + Sync + 'static) -> Self {
        Self::start(ready, || {})
    }
    /// A worker whose engine is `engine`, built on the worker's own thread from the
    /// language resources. This is how a different engine than Tree-sitter plugs in.
    #[allow(dead_code)]
    pub(crate) fn with_engine(
        engine: impl FnOnce(Arc<dyn Resources>) -> Box<dyn SpanProvider> + Send + 'static,
        ready: impl Fn(DocumentStamp) + Send + Sync + 'static,
    ) -> Self {
        Self::start_with(ready, || {}, engine)
    }
    fn start(
        ready: impl Fn(DocumentStamp) + Send + Sync + 'static,
        before_prepare: impl Fn() + Send + 'static,
    ) -> Self {
        Self::start_with(ready, before_prepare, tree_sitter_engine)
    }
    fn start_with(
        ready: impl Fn(DocumentStamp) + Send + Sync + 'static,
        before_prepare: impl Fn() + Send + 'static,
        engine: impl FnOnce(Arc<dyn Resources>) -> Box<dyn SpanProvider> + Send + 'static,
    ) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            stopped: AtomicBool::new(false),
        });
        let resources = crate::infrastructure::language_packs::resources();
        Self::start_resources(shared, resources, ready, before_prepare, engine)
    }
    fn start_resources(
        shared: Arc<Shared>,
        resources: Arc<dyn Resources>,
        ready: impl Fn(DocumentStamp) + Send + Sync + 'static,
        before_prepare: impl Fn() + Send + 'static,
        engine: impl FnOnce(Arc<dyn Resources>) -> Box<dyn SpanProvider> + Send + 'static,
    ) -> Self {
        let weak = Arc::downgrade(&shared);
        let changed: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let Some(shared) = weak.upgrade() else {
                return;
            };
            if shared.stopped.load(Ordering::Acquire) {
                return;
            }
            let mut state = shared.state.lock().unwrap();
            state.ready.clear();
            state.bytes = 0;
            state.retained = RetainedSet::default();
            for epoch in state.epochs.values_mut() {
                epoch.cancel.store(true, Ordering::Release);
                epoch.cancel = Arc::new(AtomicBool::new(false));
            }
            let documents: Vec<_> = state.targets.values().map(|r| r.document).collect();
            for document in documents {
                if !state.pending.contains(&document) {
                    state.pending.push_back(document);
                }
            }
            shared.wake.notify_all();
        });
        resources.subscribe(changed.clone());
        let background = shared.clone();
        thread::Builder::new()
            .name("loom-syntax".into())
            .spawn(move || {
                let mut trees = engine(resources);
                loop {
                    let job = {
                        let mut state = background.state.lock().unwrap();
                        while state.pending.is_empty()
                            && state.prune.is_none()
                            && state.forget.is_empty()
                            && !background.stopped.load(Ordering::Acquire)
                        {
                            state = background.wake.wait(state).unwrap();
                        }
                        if background.stopped.load(Ordering::Acquire) {
                            break;
                        }
                        if !state.forget.is_empty() {
                            let forgotten = std::mem::take(&mut state.forget);
                            drop(state);
                            for id in forgotten {
                                trees.forget(id);
                            }
                            background.state.lock().unwrap().tree_stats = trees.stats();
                            continue;
                        }
                        if let Some(open) = state.prune.take() {
                            drop(state);
                            trees.retain(&open);
                            let mut state = background.state.lock().unwrap();
                            state.tree_stats = trees.stats();
                            continue;
                        }
                        let document = state.pending.pop_front().unwrap();
                        let Some(epoch) = state.epochs.get(&document) else {
                            continue;
                        };
                        let Some(request) = state
                            .targets
                            .values()
                            .find(|r| r.document == document && epoch.matches(r))
                            .cloned()
                        else {
                            continue;
                        };
                        let mut windows = Vec::new();
                        for target in state
                            .targets
                            .values()
                            .filter(|r| r.document == document && epoch.matches(r))
                        {
                            for window in &target.windows {
                                if !windows.contains(window)
                                    && !state.ready.contains_key(&Key::new(target, window))
                                {
                                    windows.push(window.clone());
                                }
                            }
                        }
                        if windows.is_empty() {
                            continue;
                        }
                        let cancel = epoch.cancel.clone();
                        state.running = Some((request.clone(), windows.clone(), cancel.clone()));
                        (request, windows, cancel)
                    };
                    let (request, windows, cancel) = job;
                    before_prepare();
                    let result = trees.prepare(
                        request.document,
                        request.source.clone(),
                        request.lang,
                        &windows,
                        &cancel,
                    );
                    let structural = trees.folding(request.document, &request.source, &cancel);
                    let mut state = background.state.lock().unwrap();
                    state.running = None;
                    state.tree_stats = trees.stats();
                    if background.stopped.load(Ordering::Acquire) {
                        break;
                    }
                    if cancel.load(Ordering::Acquire)
                        || !state.epochs.get(&request.document).is_some_and(|epoch| {
                            epoch.matches(&request) && Arc::ptr_eq(&epoch.cancel, &cancel)
                        })
                    {
                        continue;
                    }
                    let Some(result) = result else {
                        continue;
                    };
                    if let Some(ranges) = structural {
                        state
                            .structural
                            .insert(request.document, (request.stamp, ranges));
                    } else {
                        state.structural.remove(&request.document);
                    }
                    for (window, tokens) in windows.iter().zip(result) {
                        let key = Key::new(&request, window);
                        let bytes = std::mem::size_of::<Cached>()
                            + tokens.spans.len()
                                * std::mem::size_of::<crate::features::syntax::TokenSpan>();
                        state.clock += 1;
                        let used = state.clock;
                        if let Some(old) = state.ready.insert(
                            key,
                            Cached {
                                window: tokens,
                                used,
                                bytes,
                            },
                        ) {
                            state.bytes -= old.bytes;
                        }
                        state.bytes += bytes;
                    }
                    while state.bytes > WINDOW_BUDGET || state.ready.len() > 2048 {
                        let key = *state
                            .ready
                            .iter()
                            .min_by_key(|(_, entry)| entry.used)
                            .unwrap()
                            .0;
                        state.bytes -= state.ready.remove(&key).unwrap().bytes;
                        state.evictions += 1;
                    }
                    drop(state);
                    ready(request.stamp);
                }
            })
            .expect("could not start syntax worker");
        Self(Arc::new(Owner(shared, changed)))
    }
    pub fn shutdown(&self) {
        stop(&self.0.0);
    }
    pub fn query(
        &self,
        pane: PaneId,
        stamp: DocumentStamp,
        source: Source,
        lang: ResolvedLanguage,
        windows: Vec<Window>,
    ) -> Vec<WindowTokens> {
        let shared = &self.0.0;
        if shared.stopped.load(Ordering::Acquire) {
            return Vec::new();
        }
        // Only request bookkeeping and immutable viewport results share this
        // lock. The scanner owns its index outside it, even during cancellation.
        let mut state = shared.state.lock().unwrap();
        debug_assert_eq!(
            (stamp.version, stamp.snapshot_identity),
            (source.version, source.identity()),
            "the stamp and the source must describe the same text"
        );
        let document = stamp.document;
        if lang.grammar.is_none() || source.text.len() > crate::features::syntax::MAX_DOCUMENT_BYTES
        {
            // Plain rendering needs no source, epoch, or asynchronous viewport
            // request. In particular it must not force a copy on the next edit.
            let previous = state.targets.remove(&pane);
            if let Some(previous) = previous
                && previous.document != document
                && !state
                    .targets
                    .values()
                    .any(|r| r.document == previous.document)
                && let Some(epoch) = state.epochs.get(&previous.document)
            {
                epoch.cancel.store(true, Ordering::Release);
            }
            state.targets.retain(|_, r| r.document != document);
            let old_epoch = state.epochs.remove(&document);
            if let Some(epoch) = &old_epoch {
                epoch.cancel.store(true, Ordering::Release);
            }
            state.pending.retain(|id| *id != document);
            state.ready.retain(|key, _| key.document != document);
            state.structural.remove(&document);
            state.retained.forget(document);
            state.bytes = state.ready.values().map(|entry| entry.bytes).sum();
            // The worker owns parsed trees outside the mutex. Dispose of any
            // older tree there, including a parse cancelled by this query.
            if old_epoch.is_some() && !state.forget.contains(&document) {
                state.forget.push(document);
                shared.wake.notify_one();
            }
            // Text no grammar covers still gets the generic highlighter, which
            // reads one line and keeps nothing between requests.
            let rules = generic::Rules::for_language(lang.definition());
            return windows
                .into_iter()
                .filter(|window| window.range.end <= source.text.len())
                .map(|window| {
                    let spans = if lang.name() == PLAIN_TEXT {
                        if window.range.is_empty() {
                            Vec::new()
                        } else {
                            vec![TokenSpan {
                                range: window.range.clone(),
                                class: None,
                            }]
                        }
                    } else {
                        generic::highlight(&source.text, &window.line, &window.range, &rules)
                    };
                    WindowTokens::fresh(window.range, spans)
                })
                .collect();
        }
        let request = Request {
            stamp,
            document,
            source,
            lang,
            windows: Vec::new(),
        };
        let changed_data = !state
            .epochs
            .get(&document)
            .is_some_and(|epoch| epoch.matches(&request));
        let changed = changed_data
            || state
                .epochs
                .get(&document)
                .is_some_and(|epoch| epoch.cancel.load(Ordering::Relaxed));
        if changed {
            state.structural.remove(&document);
            let old = state.epochs.insert(
                document,
                Epoch {
                    identity: request.source.identity(),
                    version: request.source.version,
                    incarnation: request.stamp.incarnation,
                    language_generation: request.stamp.language_generation,
                    lang,
                    cancel: Arc::new(AtomicBool::new(false)),
                },
            );
            if let Some(old) = &old {
                old.cancel.store(true, Ordering::Release);
            }
            if changed_data {
                // The spans that were ready for the previous version are kept
                // for drawing, unless the language or the document's lifetime
                // changed, when they say nothing about this text.
                let pages: Vec<_> = state
                    .ready
                    .iter()
                    .filter(|(key, _)| key.document == document)
                    .map(|(key, entry)| (key.start..key.end, entry.window.spans.clone()))
                    .collect();
                match old {
                    Some(old)
                        if (old.incarnation, old.language_generation, old.lang)
                            == (
                                request.stamp.incarnation,
                                request.stamp.language_generation,
                                lang,
                            ) =>
                    {
                        if !pages.is_empty() {
                            state.retained.keep(
                                document,
                                old.version,
                                old.identity,
                                old.incarnation,
                                old.language_generation,
                                old.lang,
                                pages,
                            );
                        }
                    }
                    _ => state.retained.forget(document),
                }
                state.ready.retain(|key, _| key.document != document);
                state.bytes = state.ready.values().map(|entry| entry.bytes).sum();
            }
        }
        let mut result = Vec::new();
        let mut missing = false;
        let mut request = request;
        for window in windows {
            if window.range.is_empty() {
                result.push(WindowTokens::fresh(window.range, Vec::new()));
                continue;
            }
            let blocks = pages(&request.source, &window);
            let mut complete = true;
            let mut found = false;
            let mut exact = None;
            let mut spans = Vec::new();
            for block in &blocks {
                if !request.windows.contains(block) {
                    request.windows.push(block.clone());
                }
                state.clock += 1;
                let used = state.clock;
                if let Some(entry) = state.ready.get_mut(&Key::new(&request, block)) {
                    entry.used = used;
                    found = true;
                    if block.range == window.range {
                        exact = Some(entry.window.clone());
                    } else {
                        // Each ready page speaks for its own part of the window, so a
                        // long token seen from one page cannot mask what the next one
                        // found inside it.
                        let own = block.range.start.max(window.range.start)
                            ..block.range.end.min(window.range.end);
                        spans.extend(entry.clipped(&own));
                    }
                    state.hits += 1;
                } else {
                    complete = false;
                }
            }
            missing |= !complete;
            if !complete {
                // A token may span several pages (strings, comments, names).
                // Its full semantic range stays valid beyond the page that
                // first published it, even while the next page is pending.
                state.clock += 1;
                let used = state.clock;
                for (key, entry) in &mut state.ready {
                    if key.document != document
                        || key.incarnation != request.stamp.incarnation
                        || key.language_generation != request.stamp.language_generation
                        || key.identity != request.source.identity()
                        || key.version != request.source.version
                        || key.lang != lang
                    {
                        continue;
                    }
                    let available = entry.intersecting(&window.range);
                    if !available.is_empty() {
                        found = true;
                        spans.extend_from_slice(available);
                        entry.used = used;
                    }
                }
            }
            if let Some(exact) = exact {
                result.push(exact);
            } else if found {
                spans.sort_unstable_by_key(|span| (span.range.start, span.range.end));
                spans.dedup();
                let mut covered = window.range.start;
                for span in &spans {
                    if span.range.start > covered {
                        break;
                    }
                    covered = covered.max(span.range.end);
                }
                complete = covered >= window.range.end;
                result.push(WindowTokens {
                    range: window.range,
                    spans: spans.into(),
                    complete,
                    freshness: Freshness::Fresh,
                });
            } else if let Some(provisional) = state.retained.provisional(
                document,
                &request.source,
                request.stamp.incarnation,
                request.stamp.language_generation,
                lang,
                &window,
            ) {
                // Nothing is ready for this version yet: draw the previous
                // colours, moved across the edits, instead of plain text.
                result.push(provisional);
            }
        }
        let in_flight = state
            .running
            .as_ref()
            .is_some_and(|(running, windows, cancel)| {
                !cancel.load(Ordering::Acquire)
                    && running.document == document
                    && running.source.same(&request.source)
                    && running.lang == lang
                    && request.windows.iter().all(|window| {
                        state.ready.contains_key(&Key::new(&request, window))
                            || windows.contains(window)
                    })
            });
        let previous = state.targets.insert(pane, request);
        if let Some(previous) = previous
            && previous.document != document
            && !state
                .targets
                .values()
                .any(|r| r.document == previous.document)
        {
            if let Some(epoch) = state.epochs.get(&previous.document) {
                epoch.cancel.store(true, Ordering::Release);
            }
        }
        if missing && !in_flight && !state.pending.contains(&document) {
            state.pending.push_front(document);
            shared.wake.notify_one();
        }
        result
    }
    pub fn retain(&self, open: Vec<FileId>) {
        let mut state = self.0.0.state.lock().unwrap();
        state.targets.retain(|_, r| open.contains(&r.document));
        state.epochs.retain(|id, epoch| {
            if open.contains(id) {
                true
            } else {
                epoch.cancel.store(true, Ordering::Release);
                false
            }
        });
        state.ready.retain(|key, _| open.contains(&key.document));
        state.structural.retain(|id, _| open.contains(id));
        state.bytes = state.ready.values().map(|entry| entry.bytes).sum();
        state.retained.retain(&open);
        state.prune = Some(open);
        self.0.0.wake.notify_one();
    }
    pub fn folding(&self, stamp: DocumentStamp) -> Option<Arc<[std::ops::Range<usize>]>> {
        let state = self.0.0.state.lock().unwrap();
        state
            .structural
            .get(&stamp.document)
            .filter(|(current, _)| *current == stamp)
            .map(|(_, ranges)| ranges.clone())
    }
    pub fn stats(&self) -> Stats {
        let state = self.0.0.state.lock().unwrap();
        Stats {
            tree: state.tree_stats,
            provisional: state.retained.stats,
            cache_bytes: state.bytes,
            cache_hits: state.hits,
            evictions: state.evictions,
        }
    }
}

#[path = "syntax_worker_retained.rs"]
mod retained;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::language;
    use crate::features::highlighting::HighlightId;
    use loom_editor_core::text::TextBuffer;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn plain_queries_release_sources_and_return_complete_windows_immediately() {
        let notifications = Arc::new(AtomicUsize::new(0));
        let count = notifications.clone();
        let worker = Worker::new(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
        });
        for (text, lang) in [
            (
                "small plain text".into(),
                language::from_path(std::path::Path::new("file.txt")),
            ),
            (
                "x".repeat(crate::features::syntax::MAX_DOCUMENT_BYTES + 1),
                language::builtin("javascript"),
            ),
        ] {
            let buffer = TextBuffer::new(text);
            let snapshot = buffer.text_snapshot();
            let document = FileId::new(1);
            let stamp = DocumentStamp::new(document, 1, 0, &buffer);
            let result = worker.query(
                PaneId::new(1),
                stamp,
                Source::new(&buffer),
                lang,
                vec![Window {
                    line: 0..buffer.len(),
                    range: 1..8,
                }],
            );
            assert_eq!(Arc::strong_count(&snapshot), 2);
            assert!(result[0].complete);
            assert_eq!(
                result[0].spans[0],
                TokenSpan {
                    range: 1..8,
                    class: None
                }
            );
            let state = worker.0.0.state.lock().unwrap();
            assert!(state.targets.is_empty());
            assert!(state.epochs.is_empty());
        }
        assert_eq!(notifications.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn text_without_a_grammar_gets_the_generic_highlighter_but_plain_text_does_not() {
        let worker = Worker::new(|_| {});
        // Past the size a grammar is used for, a known language still gets colour.
        let code = format!(
            "const a = 1; // c\n{}",
            "x".repeat(crate::features::syntax::MAX_DOCUMENT_BYTES)
        );
        let first = code.find('\n').unwrap();
        let query = |text: String, lang| {
            let buffer = TextBuffer::new(text);
            let stamp = DocumentStamp::new(FileId::new(1), 1, 0, &buffer);
            worker.query(
                PaneId::new(1),
                stamp,
                Source::new(&buffer),
                lang,
                vec![Window {
                    line: 0..first,
                    range: 0..first,
                }],
            )
        };
        let coloured = query(code.clone(), language::builtin("javascript"));
        let classes: Vec<_> = coloured[0].spans.iter().map(|s| s.class).collect();
        assert!(classes.contains(&Some(HighlightId::KEYWORD)), "{classes:?}");
        assert!(classes.contains(&Some(HighlightId::COMMENT)), "{classes:?}");
        assert!(coloured[0].complete);

        let plain = query(code, language::from_path(std::path::Path::new("a.txt")));
        assert_eq!(plain[0].spans.len(), 1);
        assert_eq!(plain[0].spans[0].class, None);
    }

    #[test]
    fn another_engine_colours_a_language_in_place_of_tree_sitter() {
        use std::{sync::mpsc, time::Duration};
        /// Colours every window as a keyword, and notes what it was asked.
        struct Everything(Arc<Mutex<Vec<FileId>>>);
        impl SpanProvider for Everything {
            fn prepare(
                &mut self,
                id: FileId,
                _source: Source,
                _language: ResolvedLanguage,
                windows: &[Window],
                _cancel: &AtomicBool,
            ) -> Option<Vec<WindowTokens>> {
                self.0.lock().unwrap().push(id);
                Some(
                    windows
                        .iter()
                        .map(|window| {
                            WindowTokens::fresh(
                                window.range.clone(),
                                vec![TokenSpan {
                                    range: window.range.clone(),
                                    class: Some(HighlightId::KEYWORD),
                                }],
                            )
                        })
                        .collect(),
                )
            }
            fn folding(
                &mut self,
                _id: FileId,
                _source: &Source,
                _cancel: &AtomicBool,
            ) -> Option<Arc<[std::ops::Range<usize>]>> {
                None
            }
            fn forget(&mut self, _id: FileId) {}
            fn retain(&mut self, _open: &[FileId]) {}
            fn stats(&self) -> TreeStats {
                TreeStats::default()
            }
        }
        let asked = Arc::new(Mutex::new(Vec::new()));
        let record = asked.clone();
        let (done, finished) = mpsc::channel();
        let worker = Worker::with_engine(
            move |_resources| Box::new(Everything(record)),
            move |stamp| {
                let _ = done.send(stamp);
            },
        );
        let buffer = TextBuffer::new("plain words");
        let document = FileId::new(7);
        let stamp = DocumentStamp::new(document, 1, 0, &buffer);
        let query = || {
            worker.query(
                PaneId::new(1),
                stamp,
                Source::new(&buffer),
                language::builtin("javascript"),
                vec![Window {
                    line: 0..buffer.len(),
                    range: 0..buffer.len(),
                }],
            )
        };
        assert!(query().iter().all(|tokens| !tokens.complete));
        finished.recv_timeout(Duration::from_secs(5)).unwrap();
        let coloured = query();
        assert!(coloured[0].complete);
        assert_eq!(coloured[0].spans[0].class, Some(HighlightId::KEYWORD));
        assert_eq!(*asked.lock().unwrap(), [document]);
    }

    #[test]
    fn switching_to_plain_cancels_and_releases_an_in_flight_source() {
        use std::{
            sync::mpsc,
            time::{Duration, Instant},
        };
        let (entered, receiving) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let worker = Worker::start(
            |_| {},
            move || {
                entered.send(()).unwrap();
                gate.recv().unwrap();
            },
        );
        let buffer = TextBuffer::new("const value=123;");
        let snapshot = buffer.text_snapshot();
        let document = FileId::new(1);
        let stamp = DocumentStamp::new(document, 1, 0, &buffer);
        let windows = vec![Window {
            line: 0..buffer.len(),
            range: 0..buffer.len(),
        }];
        worker.query(
            PaneId::new(1),
            stamp,
            Source::new(&buffer),
            language::builtin("javascript"),
            windows.clone(),
        );
        receiving.recv_timeout(Duration::from_secs(5)).unwrap();
        let result = worker.query(
            PaneId::new(1),
            stamp,
            Source::new(&buffer),
            language::from_path(std::path::Path::new("file.txt")),
            windows,
        );
        assert!(result[0].complete);
        assert!(
            worker
                .0
                .0
                .state
                .lock()
                .unwrap()
                .running
                .as_ref()
                .unwrap()
                .2
                .load(Ordering::Acquire)
        );
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Arc::strong_count(&snapshot) != 2 {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(worker.0.0.state.lock().unwrap().targets.is_empty());
    }
}
