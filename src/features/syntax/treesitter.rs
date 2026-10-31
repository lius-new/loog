//! Per-document syntax trees and the highlight spans read from them.
//!
//! The worker owns one [`TreeIndex`] and calls it with the same shape as the
//! former line index: a document, its text snapshot and the windows to colour.
//! A tree always describes a whole document, so a comment that starts above the
//! window is still a comment inside it; only the query is limited to the window.
//!
//! The tree is kept up to date from the text's edit journal when it is complete
//! and still follows from the tree's version; otherwise the document is parsed
//! again. Editing happens on a private copy and replaces the stored tree only
//! when the new parse finished, so a cancelled parse leaves the old state intact.
use super::injections::InjectionQuery;
use super::layers::{Layers, Tools};
use super::queries::{self, CompiledQuery};
use super::{Source, TokenSpan, Window, WindowTokens};
use crate::domain::{
    identity::FileId,
    language::{GrammarId, Registry, ResolvedLanguage},
};
use crate::features::highlighting::HighlightId;
use loom_editor_core::text::{BytePoint, RevisionEdit};
use std::{
    cmp::Reverse,
    collections::HashMap,
    ops::{ControlFlow, Range},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use streaming_iterator::StreamingIterator;
use tree_sitter::{InputEdit, ParseOptions, ParseState, Parser, Point, QueryCursor, Tree};

/// Documents larger than this are shown as plain text rather than parsed.
pub(crate) const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
/// Trees kept at once; the least recently used one is dropped first.
const MAX_TREES: usize = 16;
/// Captures read for one window; a window needing more is drawn partly plain.
const MAX_CAPTURES: usize = 200_000;
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TreeStats {
    pub full_parses: usize,
    pub incremental_parses: usize,
    pub parsed_bytes: usize,
    pub parse_time: Duration,
    pub query_runs: usize,
    pub query_time: Duration,
    pub queries_compiled: usize,
    pub query_compile_time: Duration,
    pub plain_fallbacks: usize,
    pub trees: usize,
    /// Embedded regions parsed from scratch, reused from the previous parse, and
    /// left out because a limit was reached.
    pub layers: usize,
    pub layer_parses: usize,
    pub layer_reuses: usize,
    pub layer_overflows: usize,
}

struct DocTree {
    folds: Option<Arc<[Range<usize>]>>,
    tree: Tree,
    /// Embedded languages, parsed as windows reach them.
    layers: Layers,
    source: Source,
    grammar: GrammarId,
    used: u64,
}

pub(crate) struct TreeIndex {
    parsers: HashMap<GrammarId, Parser>,
    /// Parsers for embedded regions, which carry included ranges and so are never
    /// shared with whole-document parses.
    layer_parsers: HashMap<&'static str, Parser>,
    /// `None` remembers a query that failed to compile, so it is not retried.
    queries: HashMap<&'static str, Option<Arc<CompiledQuery>>>,
    injection_queries: HashMap<&'static str, Option<Arc<InjectionQuery>>>,
    docs: HashMap<FileId, DocTree>,
    clock: u64,
    pub stats: TreeStats,
    resources: Arc<dyn super::Resources>,
    resource_generation: u64,
}

impl Default for TreeIndex {
    fn default() -> Self {
        Self::new(Arc::new(super::EmptyResources))
    }
}

impl TreeIndex {
    pub(crate) fn forget(&mut self, id: FileId) {
        self.docs.remove(&id);
        self.stats.trees = self.docs.len();
    }

    pub(crate) fn new(resources: Arc<dyn super::Resources>) -> Self {
        let resource_generation = resources.generation();
        Self {
            parsers: HashMap::new(),
            layer_parsers: HashMap::new(),
            queries: HashMap::new(),
            injection_queries: HashMap::new(),
            docs: HashMap::new(),
            clock: 0,
            stats: TreeStats::default(),
            resources,
            resource_generation,
        }
    }
    /// Structural ranges come from the current syntax tree, never highlight spans.
    pub(crate) fn folding(
        &mut self,
        id: FileId,
        source: &Source,
        cancel: &AtomicBool,
    ) -> Option<Arc<[Range<usize>]>> {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let doc = self.docs.get_mut(&id)?;
        if doc.source.version != source.version
            || doc.source.identity() != source.identity()
            || source.text.len() > 1024 * 1024
        {
            return None;
        }
        if let Some(ranges) = &doc.folds {
            return Some(ranges.clone());
        }
        let mut cursor = doc.tree.walk();
        let mut ranges = Vec::new();
        let mut visited = 0usize;
        loop {
            let node = cursor.node();
            visited += 1;
            if visited > 250_000 || visited.is_multiple_of(256) && cancel.load(Ordering::Relaxed) {
                return None;
            }
            let kind = node.kind();
            let structural = [
                "function",
                "method",
                "class",
                "struct",
                "enum",
                "impl",
                "block",
                "body",
                "object",
                "array",
                "element",
                "namespace",
                "declaration_list",
            ]
            .iter()
            .any(|name| kind.contains(name));
            if visited > 1
                && node.is_named()
                && !node.is_error()
                && !node.is_missing()
                && structural
                && node.end_position().row > node.start_position().row
            {
                ranges.push(node.start_byte()..node.end_byte());
                if ranges.len() > 8192 {
                    return None;
                }
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    drop(cursor);
                    ranges.sort_by_key(|r| (r.start, r.end));
                    ranges.dedup();
                    let ranges: Arc<[Range<usize>]> = ranges.into();
                    doc.folds = Some(ranges.clone());
                    return Some(ranges);
                }
            }
        }
    }

    pub(crate) fn retain(&mut self, open: &[FileId]) {
        self.docs.retain(|id, _| open.contains(id));
        self.stats.trees = self.docs.len();
    }

    /// Spans for each window, or `None` when the work was cancelled or `windows`
    /// do not fit the text.
    pub(crate) fn prepare(
        &mut self,
        id: FileId,
        source: Source,
        language: ResolvedLanguage,
        windows: &[Window],
        cancel: &AtomicBool,
    ) -> Option<Vec<WindowTokens>> {
        let generation = self.resources.generation();
        if generation != self.resource_generation {
            self.parsers.clear();
            self.layer_parsers.clear();
            self.queries.clear();
            self.injection_queries.clear();
            self.docs.clear();
            self.stats.trees = 0;
            self.resource_generation = generation;
        }
        self.clock += 1;
        if windows
            .iter()
            .any(|window| window.range.end > source.text.len())
        {
            return None;
        }
        // Without a grammar the text is plain.
        let Some(grammar) = language.grammar else {
            self.docs.remove(&id);
            return Some(windows.iter().map(plain).collect());
        };
        if source.text.len() > MAX_DOCUMENT_BYTES {
            self.docs.remove(&id);
            self.stats.plain_fallbacks += 1;
            return Some(windows.iter().map(plain).collect());
        }
        let Some(query) = self.query_by_name(Registry::builtin().grammar_name(grammar)) else {
            self.stats.plain_fallbacks += 1;
            return Some(windows.iter().map(plain).collect());
        };
        let current = self
            .docs
            .get(&id)
            .is_some_and(|doc| doc.grammar == grammar && doc.source.same(&source));
        if !current {
            match self.reparse(id, &source, grammar, cancel) {
                Ok(()) => {}
                Err(true) => return None,
                Err(false) => {
                    self.docs.remove(&id);
                    self.stats.plain_fallbacks += 1;
                    return Some(windows.iter().map(plain).collect());
                }
            }
        }
        // Parse the embedded languages the windows reach.
        {
            let doc = self.docs.get_mut(&id).expect("parsed or current");
            let reached: Vec<Range<usize>> = windows.iter().map(|w| w.range.clone()).collect();
            let mut tools = Tools {
                resources: self.resources.as_ref(),
                parsers: &mut self.layer_parsers,
                injection_queries: &mut self.injection_queries,
                stats: &mut self.stats,
                cancel,
            };
            doc.layers.ensure(&mut tools, &reached, &doc.source.text)?;
        }
        // The queries of the embedded languages, in the order of the layers.
        let layer_names: Vec<Option<&'static str>> = self.docs[&id]
            .layers
            .list
            .iter()
            .map(|layer| layer.tree.as_ref().map(|_| layer.grammar))
            .collect();
        let layer_queries: Vec<Option<Arc<CompiledQuery>>> = layer_names
            .into_iter()
            .map(|name| name.and_then(|name| self.query_by_name(name)))
            .collect();
        let doc = self.docs.get_mut(&id).expect("parsed or current");
        doc.used = self.clock;
        let started = Instant::now();
        let mut result = Vec::with_capacity(windows.len());
        let mut sources: Vec<HighlightSource<'_>> = vec![HighlightSource {
            tree: &doc.tree,
            query: &query,
            depth: 0,
            bounds: 0..usize::MAX,
        }];
        for (layer, query) in doc.layers.list.iter().zip(&layer_queries) {
            if let (Some(query), Some(tree)) = (query, &layer.tree) {
                sources.push(HighlightSource {
                    tree,
                    query,
                    depth: layer.depth,
                    bounds: layer.bounds.clone(),
                });
            }
        }
        for window in windows {
            let spans =
                highlight_sources(&sources, &doc.source.text, window.range.clone(), cancel)?;
            self.stats.query_runs += 1;
            result.push(WindowTokens::fresh(window.range.clone(), spans));
        }
        self.stats.query_time += started.elapsed();
        self.evict(id);
        Some(result)
    }

    fn query_by_name(&mut self, name: &'static str) -> Option<Arc<CompiledQuery>> {
        let stats = &mut self.stats;
        self.queries
            .entry(name)
            .or_insert_with(|| {
                let started = Instant::now();
                let compiled = queries::compile(self.resources.as_ref(), name);
                if let Err(error) = &compiled
                    && !self.resources.pending(name)
                {
                    self.resources.report(error);
                }
                stats.queries_compiled += 1;
                stats.query_compile_time += started.elapsed();
                compiled.ok().map(Arc::new)
            })
            .clone()
    }

    /// Brings the tree up to date. Err(true) is cancellation; Err(false) means
    /// resources cannot execute and the document should be shown as plain text.
    fn reparse(
        &mut self,
        id: FileId,
        source: &Source,
        grammar: GrammarId,
        cancel: &AtomicBool,
    ) -> Result<(), bool> {
        let name = Registry::builtin().grammar_name(grammar);
        if let std::collections::hash_map::Entry::Vacant(entry) = self.parsers.entry(grammar) {
            let parser = self.resources.parser(name).map_err(|error| {
                self.resources.report(&error);
                false
            })?;
            entry.insert(parser);
        }
        let parser = self.parsers.get_mut(&grammar).expect("initialized parser");
        let started = Instant::now();
        let previous = self.docs.get(&id).filter(|doc| doc.grammar == grammar);
        let edited = previous.and_then(|doc| {
            let revisions = source.changes_since(&doc.source)?;
            let mut tree = doc.tree.clone();
            for revision in &revisions {
                // One revision's edits all describe the text before it, so they
                // are applied from the last to the first.
                for edit in revision.edits.iter().rev() {
                    tree.edit(&input_edit(edit));
                }
            }
            Some(tree)
        });
        let incremental = edited.is_some();
        let Some(tree) = parse(parser, &source.text, edited.as_ref(), cancel) else {
            return Err(cancel.load(Ordering::Relaxed));
        };
        let mut tools = Tools {
            resources: self.resources.as_ref(),
            parsers: &mut self.layer_parsers,
            injection_queries: &mut self.injection_queries,
            stats: &mut self.stats,
            cancel,
        };
        let Some(layers) = Layers::rebuild(
            self.docs.get(&id).map(|doc| &doc.layers),
            &mut tools,
            name,
            &tree,
            &source.text,
        ) else {
            return Err(true);
        };
        self.stats.parse_time += started.elapsed();
        self.stats.parsed_bytes += source.text.len();
        if incremental {
            self.stats.incremental_parses += 1;
        } else {
            self.stats.full_parses += 1;
        }
        self.docs.insert(
            id,
            DocTree {
                folds: None,
                tree,
                layers,
                source: source.clone(),
                grammar,
                used: self.clock,
            },
        );
        Ok(())
    }

    /// The embedded languages parsed so far and how deep each is, for tests.
    #[cfg(test)]
    pub(crate) fn layer_summary(&self, id: FileId) -> Vec<(&'static str, u8)> {
        self.docs
            .get(&id)
            .map(|doc| doc.layers.parsed())
            .unwrap_or_default()
    }

    fn evict(&mut self, keep: FileId) {
        while self.docs.len() > MAX_TREES {
            let Some(oldest) = self
                .docs
                .iter()
                .filter(|(id, _)| **id != keep)
                .min_by_key(|(_, doc)| doc.used)
                .map(|(id, _)| *id)
            else {
                break;
            };
            self.docs.remove(&oldest);
        }
        self.stats.trees = self.docs.len();
    }
}

/// One tree and the query that colours it. Embedded layers cover only part of
/// the text, so `bounds` lets a window skip the ones it cannot touch.
pub(crate) struct HighlightSource<'a> {
    pub tree: &'a Tree,
    pub query: &'a CompiledQuery,
    pub depth: u8,
    pub bounds: Range<usize>,
}

fn point(point: BytePoint) -> Point {
    Point {
        row: point.row,
        column: point.column,
    }
}

/// The Tree-sitter edit for one journal entry. `new_end_byte` comes from the
/// inserted extent, not from the entry's final-text range, because the edits of
/// one revision are applied one after another.
pub(crate) fn input_edit(edit: &RevisionEdit) -> InputEdit {
    InputEdit {
        start_byte: edit.old.start,
        old_end_byte: edit.old.end,
        new_end_byte: edit.old.start + edit.inserted_extent.bytes,
        start_position: point(edit.old_start_point),
        old_end_position: point(edit.old_end_point),
        new_end_position: point(edit.new_end_point()),
    }
}

/// Parses `text`, giving up when `cancel` is set. A cancelled parse is reset so
/// the parser can be used for another document.
pub(crate) fn parse(
    parser: &mut Parser,
    text: &str,
    old: Option<&Tree>,
    cancel: &AtomicBool,
) -> Option<Tree> {
    let bytes = text.as_bytes();
    let mut progress = |_: &ParseState| {
        if cancel.load(Ordering::Relaxed) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let tree = parser.parse_with_options(
        &mut |offset, _| {
            if offset < bytes.len() {
                &bytes[offset..]
            } else {
                &[][..]
            }
        },
        old,
        Some(ParseOptions::new().progress_callback(&mut progress)),
    );
    if tree.is_none() {
        parser.reset();
    }
    tree
}

fn plain(window: &Window) -> WindowTokens {
    let spans = if window.range.is_empty() {
        Vec::new()
    } else {
        vec![TokenSpan {
            range: window.range.clone(),
            class: None,
        }]
    };
    WindowTokens::fresh(window.range.clone(), spans)
}

pub(crate) struct Capture {
    pub(crate) range: Range<usize>,
    pub(crate) class: HighlightId,
    /// How deeply embedded the language that made this capture is; the deeper
    /// capture wins over a shallower one covering the same text.
    pub(crate) depth: u8,
    /// (pattern, position within the query's results): later wins a tie.
    pub(crate) rank: (usize, usize),
}

/// The spans for `window`, tiling it completely: text no pattern colours is a
/// span with no class. A token that starts before or ends after the window keeps
/// its full range, so the result may reach past the window on either side.
/// `None` when cancelled.
pub(crate) fn highlight(
    tree: &Tree,
    text: &str,
    query: &CompiledQuery,
    window: Range<usize>,
    cancel: &AtomicBool,
) -> Option<Vec<TokenSpan>> {
    let source = HighlightSource {
        tree,
        query,
        depth: 0,
        bounds: 0..usize::MAX,
    };
    highlight_sources(&[source], text, window, cancel)
}

/// Like [`highlight`] for a document with embedded languages: the captures of
/// every layer that reaches the window are resolved together.
pub(crate) fn highlight_sources(
    sources: &[HighlightSource<'_>],
    text: &str,
    window: Range<usize>,
    cancel: &AtomicBool,
) -> Option<Vec<TokenSpan>> {
    if window.is_empty() {
        return Some(Vec::new());
    }
    let mut captures = Vec::new();
    for source in sources {
        if source.bounds.end <= window.start || source.bounds.start >= window.end {
            continue;
        }
        collect_captures(source, text, &window, cancel, &mut captures)?;
        if captures.len() >= MAX_CAPTURES {
            break;
        }
    }
    Some(spans_for(&window, captures))
}

fn collect_captures(
    source: &HighlightSource<'_>,
    text: &str,
    window: &Range<usize>,
    cancel: &AtomicBool,
    captures: &mut Vec<Capture>,
) -> Option<()> {
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(window.clone());
    let mut matches = cursor.matches(
        &source.query.query,
        source.tree.root_node(),
        text.as_bytes(),
    );
    let mut seen = 0usize;
    while let Some(found) = matches.next() {
        seen += 1;
        if seen.is_multiple_of(1024) && cancel.load(Ordering::Relaxed) {
            return None;
        }
        for capture in found.captures() {
            let Some(class) = source.query.class_of(capture.index) else {
                continue;
            };
            // The capture keeps its whole extent even where it leaves the window,
            // so a long token stays known to the pages beside this one.
            let range = capture.node.byte_range();
            if range.start < range.end {
                captures.push(Capture {
                    range,
                    class,
                    depth: source.depth,
                    rank: (found.pattern_index, captures.len()),
                });
            }
        }
        if captures.len() >= MAX_CAPTURES {
            break;
        }
    }
    Some(())
}

fn spans_for(window: &Range<usize>, captures: Vec<Capture>) -> Vec<TokenSpan> {
    let start = captures
        .iter()
        .map(|c| c.range.start)
        .fold(window.start, usize::min);
    let end = captures
        .iter()
        .map(|c| c.range.end)
        .fold(window.end, usize::max);
    let mut spans = tile(start..end, captures);
    // Tokens keep their full range, but nothing is claimed about text the query
    // did not look at: spans that miss the window are dropped, and the plain
    // stretches between captures are cut to the window.
    spans.retain(|span| span.range.end > window.start && span.range.start < window.end);
    for span in &mut spans {
        if span.class.is_none() {
            span.range = span.range.start.max(window.start)..span.range.end.min(window.end);
        }
    }
    spans
}

/// Resolves overlapping captures into consecutive spans. At each point the
/// shortest capture wins, and among equal ranges the later pattern.
pub(crate) fn tile(window: Range<usize>, mut captures: Vec<Capture>) -> Vec<TokenSpan> {
    let mut cuts = vec![window.start, window.end];
    for capture in &captures {
        cuts.push(capture.range.start);
        cuts.push(capture.range.end);
    }
    cuts.sort_unstable();
    cuts.dedup();
    captures.sort_by_key(|capture| capture.range.start);

    let mut spans: Vec<TokenSpan> = Vec::new();
    let mut active: Vec<usize> = Vec::new();
    let mut next = 0;
    for pair in cuts.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        while next < captures.len() && captures[next].range.start <= from {
            active.push(next);
            next += 1;
        }
        active.retain(|&index| captures[index].range.end > from);
        let class = active
            .iter()
            .copied()
            .min_by_key(|&index| {
                (
                    Reverse(captures[index].depth),
                    captures[index].range.len(),
                    Reverse(captures[index].rank),
                )
            })
            .map(|index| captures[index].class);
        match spans.last_mut() {
            Some(last) if last.class == class => last.range.end = to,
            _ => spans.push(TokenSpan {
                range: from..to,
                class,
            }),
        }
    }
    spans
}

/// The spans of `text` for the grammar `grammar` of `resources`, tiling the whole
/// text, or why the grammar cannot be used (not installed, module or query
/// rejected). It is the same work the editor does for a document, without the
/// document: it names the grammar directly, so it also checks a language pack
/// before any file is associated with it.
#[cfg(test)]
pub(crate) fn highlight_with(
    resources: &dyn super::Resources,
    grammar: &str,
    text: &str,
) -> Result<Vec<TokenSpan>, String> {
    let cancel = AtomicBool::new(false);
    let mut parser = resources.parser(grammar)?;
    let query = queries::compile(resources, grammar)?;
    let tree = parse(&mut parser, text, None, &cancel).ok_or("the text could not be parsed")?;
    highlight(&tree, text, &query, 0..text.len(), &cancel).ok_or_else(|| "cancelled".to_owned())
}

/// Bounded snippets reuse the grammar/query resources without inventing file or
/// workspace identities. Only visible fenced blocks enter this adapter.
pub(crate) struct SnippetHighlighter {
    resources: Arc<dyn super::Resources>,
    generation: u64,
    parser: Option<(String, Parser)>,
    queries: HashMap<String, Option<CompiledQuery>>,
    cached: std::collections::VecDeque<(String, String, Vec<TokenSpan>)>,
}
impl SnippetHighlighter {
    pub fn new(resources: Arc<dyn super::Resources>) -> Self {
        let generation = resources.generation();
        Self {
            resources,
            generation,
            parser: None,
            queries: HashMap::new(),
            cached: std::collections::VecDeque::new(),
        }
    }
    pub fn generation(&self) -> u64 {
        self.resources.generation()
    }
    pub fn highlight(&mut self, language: &str, text: &str, cancel: &AtomicBool) -> Vec<TokenSpan> {
        if text.len() > 16 * 1024 || cancel.load(Ordering::Acquire) {
            return Vec::new();
        }
        let generation = self.resources.generation();
        if generation != self.generation {
            self.cached.clear();
            self.queries.clear();
            self.parser = None;
            self.generation = generation;
        }
        let registry = Registry::builtin();
        let name = language
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let resolved = registry
            .language_by_name(&name)
            .map(|id| registry.default_resolved(id))
            .unwrap_or_else(|| {
                registry.resolve_path(std::path::Path::new(&format!("snippet.{name}")))
            });
        let Some(grammar) = resolved.grammar else {
            return Vec::new();
        };
        let grammar = registry.grammar_name(grammar).to_owned();
        if let Some(index) = self
            .cached
            .iter()
            .position(|(g, source, _)| g == &grammar && source == text)
        {
            let entry = self.cached.remove(index).unwrap();
            let spans = entry.2.clone();
            self.cached.push_back(entry);
            return spans;
        }
        if self
            .parser
            .as_ref()
            .is_none_or(|(name, _)| *name != grammar)
        {
            let Ok(parser) = self.resources.parser(&grammar) else {
                return Vec::new();
            };
            self.parser = Some((grammar.clone(), parser));
        }
        if !self.queries.contains_key(&grammar) {
            if self.queries.len() >= 8 {
                self.queries.clear();
            }
            self.queries.insert(
                grammar.clone(),
                queries::compile(self.resources.as_ref(), &grammar).ok(),
            );
        }
        let Some(query) = self.queries.get(&grammar).and_then(Option::as_ref) else {
            return Vec::new();
        };
        let Some(tree) = parse(&mut self.parser.as_mut().unwrap().1, text, None, cancel) else {
            return Vec::new();
        };
        let spans = highlight(&tree, text, query, 0..text.len(), cancel).unwrap_or_default();
        if self.cached.len() >= 16 {
            self.cached.pop_front();
        }
        self.cached
            .push_back((grammar, text.to_owned(), spans.clone()));
        spans
    }
}
