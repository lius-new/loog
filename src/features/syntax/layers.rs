//! Embedded languages as layers over a document's text.
//!
//! After a document is parsed, the injection query of its grammar finds the
//! regions written in other languages (depth 1). A region is only *parsed* when a
//! window being coloured reaches it, so a long Markdown file costs what its
//! visible part costs. Parsing a layer finds the regions inside it (depth 2) in the
//! same way, down to [`MAX_DEPTH`].
//!
//! Limits, so that no document can make the editor do unbounded work:
//!
//! * [`MAX_DEPTH`] levels of embedding; deeper regions keep their host's colours.
//! * [`MAX_REGIONS`] regions are remembered per document.
//! * At most [`MAX_LIVE_LAYERS`] parsed layers and [`MAX_INJECTED_BYTES`] of
//!   embedded text are kept at once; a window needing more draws the rest as its
//!   host. Layers a window does not reach are dropped first.
//!
//! A `combined` injection (all of Markdown's inline text) is split into groups of
//! at most [`CHUNK_RANGES`] ranges or [`CHUNK_BYTES`] bytes: a parse with tens of
//! thousands of included ranges is far slower than the same text in small groups.
//!
//! A layer whose language, ranges and text are unchanged after an edit keeps its
//! tree; one that moved or changed is parsed again when next needed.
use super::injections::{self, InjectionQuery};
use super::treesitter::{TreeStats, parse};
use super::{Resources, injections::Region};
use crate::domain::language::Registry;
use std::{
    collections::HashMap,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
use tree_sitter::{Parser, Range as SitterRange, Tree};

/// How deep languages may be embedded in each other (HTML in Markdown is 1,
/// JavaScript in that HTML is 2).
pub(crate) const MAX_DEPTH: u8 = 4;
/// Regions remembered per document.
pub(crate) const MAX_REGIONS: usize = 10_000;
/// Parsed layers kept at once per document.
pub(crate) const MAX_LIVE_LAYERS: usize = 64;
/// Bytes of embedded text in the parsed layers of one document.
pub(crate) const MAX_INJECTED_BYTES: usize = 4 * 1024 * 1024;
const CHUNK_RANGES: usize = 32;
const CHUNK_BYTES: usize = 16 * 1024;

#[derive(Clone)]
pub(crate) struct Layer {
    pub grammar: Arc<str>,
    pub depth: u8,
    pub ranges: Vec<SitterRange>,
    /// First and last byte the layer covers.
    pub bounds: Range<usize>,
    /// Bytes of text inside `ranges`.
    size: usize,
    /// A hash of that text, so an unchanged region keeps its tree.
    content: u64,
    pub tree: Option<Tree>,
    /// Its own embedded regions have been looked for.
    expanded: bool,
}

#[derive(Default)]
pub(crate) struct Layers {
    pub list: Vec<Layer>,
    /// The layers of the previous version of the document, whose trees may still
    /// be valid for regions that did not change.
    pool: Vec<Layer>,
}

/// What building layers needs from the index that owns the caches.
pub(crate) struct Tools<'a> {
    pub resources: &'a dyn Resources,
    pub parsers: &'a mut HashMap<Arc<str>, Parser>,
    pub injection_queries: &'a mut HashMap<Arc<str>, Option<Arc<InjectionQuery>>>,
    pub stats: &'a mut TreeStats,
    pub cancel: &'a AtomicBool,
}

/// The grammar for a language as an embedding names it: a language name or
/// alias (`rs`, `js`), or the name of a grammar that no language uses.
pub(crate) fn resolve_grammar(name: &str) -> Option<Arc<str>> {
    let registry = Registry::builtin();
    if let Some(language) = registry.language_by_name(name) {
        return registry.default_resolved(language).grammar().cloned();
    }
    registry.grammar_by_name(name)
}

fn content_hash(text: &str, ranges: &[SitterRange]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for range in ranges {
        range.start_byte.hash(&mut hasher);
        text.as_bytes()[range.start_byte..range.end_byte].hash(&mut hasher);
    }
    hasher.finish()
}

impl Tools<'_> {
    fn injection_query(&mut self, grammar: &Arc<str>) -> Option<Arc<InjectionQuery>> {
        self.injection_queries
            .entry(grammar.clone())
            .or_insert_with(|| {
                let result = injections::compile(self.resources, grammar);
                if let Err(error) = &result {
                    self.resources.report(error);
                }
                result.ok().flatten().map(Arc::new)
            })
            .clone()
    }

    /// The embedded regions of `tree`, as unparsed layers one level deeper.
    /// `None` when cancelled.
    fn discover(
        &mut self,
        grammar: &Arc<str>,
        tree: &Tree,
        text: &str,
        depth: u8,
        room: usize,
    ) -> Option<Vec<Layer>> {
        let Some(query) = self.injection_query(grammar) else {
            return Some(Vec::new());
        };
        let mut layers = Vec::new();
        for region in injections::find(tree, text, &query, self.cancel)? {
            let Some(inner) = resolve_grammar(&region.language) else {
                continue; // an unknown language keeps its host's colours
            };
            for chunk in split(region) {
                if layers.len() >= room {
                    self.stats.layer_overflows += 1;
                    return Some(layers);
                }
                let size = chunk.iter().map(|r| r.end_byte - r.start_byte).sum();
                layers.push(Layer {
                    grammar: inner.clone(),
                    depth,
                    bounds: chunk[0].start_byte..chunk[chunk.len() - 1].end_byte,
                    size,
                    content: content_hash(text, &chunk),
                    ranges: chunk,
                    tree: None,
                    expanded: false,
                });
            }
        }
        Some(layers)
    }

    /// `Err(true)` when cancelled, `Err(false)` when the region cannot be parsed.
    fn parse(
        &mut self,
        grammar: &Arc<str>,
        ranges: &[SitterRange],
        text: &str,
    ) -> Result<Tree, bool> {
        if let std::collections::hash_map::Entry::Vacant(entry) =
            self.parsers.entry(grammar.clone())
        {
            let parser = self.resources.parser(grammar).map_err(|error| {
                if !self.resources.pending(grammar) {
                    self.resources.report(&error);
                }
                false
            })?;
            entry.insert(parser);
        }
        let parser = self.parsers.get_mut(grammar).expect("initialized parser");
        parser.set_included_ranges(ranges).map_err(|_| false)?;
        let started = Instant::now();
        let tree = parse(parser, text, None, self.cancel)
            .ok_or_else(|| self.cancel.load(Ordering::Relaxed))?;
        self.stats.layer_parses += 1;
        self.stats.parse_time += started.elapsed();
        Ok(tree)
    }
}

/// Splits a combined region into groups a parser handles quickly.
fn split(region: Region) -> Vec<Vec<SitterRange>> {
    let mut groups: Vec<Vec<SitterRange>> = Vec::new();
    let mut bytes = 0;
    for range in region.ranges {
        let size = range.end_byte - range.start_byte;
        match groups.last_mut() {
            Some(group) if group.len() < CHUNK_RANGES && bytes + size <= CHUNK_BYTES => {
                group.push(range);
                bytes += size;
            }
            _ => {
                groups.push(vec![range]);
                bytes = size;
            }
        }
    }
    groups
}

fn reaches(bounds: &Range<usize>, windows: &[Range<usize>]) -> bool {
    windows
        .iter()
        .any(|w| bounds.start < w.end && bounds.end > w.start)
}

impl Layers {
    /// Starts the layers of a freshly parsed document: finds its depth-1 regions
    /// and keeps the previous version's layers to take unchanged trees from.
    /// `None` when cancelled.
    pub fn rebuild(
        previous: Option<&Layers>,
        tools: &mut Tools<'_>,
        grammar: &Arc<str>,
        tree: &Tree,
        text: &str,
    ) -> Option<Self> {
        // Only parsed layers are worth keeping; cloning a tree shares its nodes.
        let pool: Vec<Layer> = previous
            .into_iter()
            .flat_map(|p| p.list.iter().chain(&p.pool))
            .filter(|layer| layer.tree.is_some())
            .cloned()
            .collect();
        let list = tools.discover(grammar, tree, text, 1, MAX_REGIONS)?;
        tools.stats.layers = list.len();
        Some(Self { list, pool })
    }

    /// Parses the layers any of `windows` reaches (and the layers inside them),
    /// within the limits. `None` when cancelled.
    pub fn ensure(
        &mut self,
        tools: &mut Tools<'_>,
        windows: &[Range<usize>],
        text: &str,
    ) -> Option<()> {
        if self.list.is_empty() {
            return Some(());
        }
        self.drop_unreached(windows);
        loop {
            let mut progressed = false;
            let mut at = 0;
            while at < self.list.len() {
                if !reaches(&self.list[at].bounds, windows) {
                    at += 1;
                    continue;
                }
                if self.list[at].tree.is_none() && !self.start(at, tools, text)? {
                    at += 1;
                    continue;
                }
                let layer = &self.list[at];
                if !layer.expanded && layer.depth < MAX_DEPTH {
                    let (grammar, depth) = (&layer.grammar, layer.depth + 1);
                    let tree = layer.tree.clone().expect("parsed above");
                    let room = MAX_REGIONS.saturating_sub(self.list.len());
                    let inner = tools.discover(grammar, &tree, text, depth, room)?;
                    self.list[at].expanded = true;
                    if !inner.is_empty() {
                        self.list.extend(inner);
                        progressed = true;
                    }
                } else {
                    self.list[at].expanded = true;
                }
                at += 1;
            }
            if !progressed {
                break;
            }
        }
        tools.stats.layers = self.list.len();
        Some(())
    }

    /// Gives layer `at` a tree: the previous version's if nothing changed,
    /// otherwise a new parse. `Some(false)` when a limit leaves it unparsed.
    fn start(&mut self, at: usize, tools: &mut Tools<'_>, text: &str) -> Option<bool> {
        let layer = &self.list[at];
        let reused = self.pool.iter().position(|old| {
            old.grammar == layer.grammar
                && old.depth == layer.depth
                && old.content == layer.content
                && old.ranges == layer.ranges
        });
        if let Some(found) = reused {
            let old = self.pool.swap_remove(found);
            tools.stats.layer_reuses += 1;
            self.list[at].tree = old.tree;
            return Some(true);
        }
        let live = self.list.iter().filter(|l| l.tree.is_some()).count();
        let bytes: usize = self
            .list
            .iter()
            .filter(|l| l.tree.is_some())
            .map(|l| l.size)
            .sum();
        if live >= MAX_LIVE_LAYERS || bytes + layer.size > MAX_INJECTED_BYTES {
            tools.stats.layer_overflows += 1;
            return Some(false);
        }
        let (grammar, ranges) = (&layer.grammar, layer.ranges.clone());
        match tools.parse(grammar, &ranges, text) {
            Ok(tree) => {
                self.list[at].tree = Some(tree);
                Some(true)
            }
            Err(true) => None,
            Err(false) => Some(false),
        }
    }

    /// Frees the trees of layers no window reaches when many are held, so a long
    /// scroll through a document does not fill the limits.
    fn drop_unreached(&mut self, windows: &[Range<usize>]) {
        let live = self.list.iter().filter(|l| l.tree.is_some()).count();
        let bytes: usize = self
            .list
            .iter()
            .filter(|l| l.tree.is_some())
            .map(|l| l.size)
            .sum();
        if live <= MAX_LIVE_LAYERS / 2 && bytes <= MAX_INJECTED_BYTES / 2 {
            return;
        }
        for layer in &mut self.list {
            if layer.tree.is_some() && !reaches(&layer.bounds, windows) {
                layer.tree = None;
            }
        }
    }

    /// The parsed layers, for tests: grammar and depth.
    #[cfg(test)]
    pub fn parsed(&self) -> Vec<(&str, u8)> {
        self.list
            .iter()
            .filter(|l| l.tree.is_some())
            .map(|l| (l.grammar.as_ref(), l.depth))
            .collect()
    }
}
