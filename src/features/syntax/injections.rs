//! Finds the parts of a document that are written in another language.
//!
//! A grammar may ship `injections.scm`. Each match names the text to hand to
//! another grammar (`@injection.content`) and the language, either with
//! `#set! injection.language "html"` or by capturing the node whose text names it
//! (`@injection.language`, such as a code fence's info string).
//! Helper captures beginning with `_` may be used by text predicates; they do
//! not contribute a language or an injected range.
//!
//! `@injection.shebang` names the language through a script's interpreter, which Loog
//! does not read. A pattern that uses it is skipped; the other patterns of the query
//! still apply.
//!
//! Supported properties, all others are errors:
//!
//! * `injection.language` — a fixed language name.
//! * `injection.combined` — all matches of the pattern for one language are parsed
//!   together as a single document with several included ranges.
//! * `injection.include-children` — keep the content node's named child nodes in
//!   the range. Without it they are cut out, as Tree-sitter's own highlighter
//!   does, so that for example the `> ` prefixes inside a quoted fence are not
//!   code.
use super::Resources;
use std::sync::atomic::{AtomicBool, Ordering};
use tree_sitter::{CaptureQuantifier, Node, Point, Query, QueryCursor, Range, Tree};

use streaming_iterator::StreamingIterator;

pub(crate) struct InjectionQuery {
    query: Query,
    content: u32,
    language: Option<u32>,
    patterns: Vec<PatternRules>,
}

#[derive(Clone, Default)]
struct PatternRules {
    language: Option<Box<str>>,
    combined: bool,
    include_children: bool,
    /// The pattern relies on `@injection.shebang`, which is not supported.
    skipped: bool,
}

/// One embedded stretch: the language as written, and where it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    pub language: String,
    pub ranges: Vec<Range>,
}

/// `None` when the grammar has no injection query.
pub(crate) fn compile(
    resources: &dyn Resources,
    grammar: &str,
) -> Result<Option<InjectionQuery>, String> {
    let Some(source) = resources.text(grammar, "injections.scm")? else {
        return Ok(None);
    };
    let language = resources.language(grammar)?;
    compile_source(&language, &source)
        .map(Some)
        .map_err(|error| format!("grammar `{grammar}` injections: {error}"))
}

pub(crate) fn compile_source(
    language: &tree_sitter::Language,
    source: &str,
) -> Result<InjectionQuery, String> {
    let query = Query::new(language, source).map_err(|error| error.to_string())?;
    let mut content = None;
    let mut language_capture = None;
    let mut shebang = None;
    for (index, name) in query.capture_names().iter().enumerate() {
        match *name {
            "injection.content" => content = Some(index as u32),
            "injection.language" => language_capture = Some(index as u32),
            "injection.shebang" => shebang = Some(index),
            helper if helper.starts_with('_') => {}
            other => return Err(format!("the capture @{other} is not supported")),
        }
    }
    let content = content.ok_or("the query has no @injection.content capture")?;
    let mut patterns = Vec::with_capacity(query.pattern_count());
    for pattern in 0..query.pattern_count() {
        if let Some(predicate) = query.general_predicates(pattern).first() {
            return Err(format!(
                "pattern {pattern}: the predicate #{} is not supported",
                predicate.operator
            ));
        }
        if let Some((property, _)) = query.property_predicates(pattern).first() {
            return Err(format!(
                "pattern {pattern}: the property predicate {} is not supported",
                property.key
            ));
        }
        let mut rules = PatternRules::default();
        for setting in query.property_settings(pattern) {
            match (&*setting.key, setting.value.as_deref()) {
                ("injection.language", Some(name)) => rules.language = Some(name.into()),
                ("injection.combined", None) => rules.combined = true,
                ("injection.include-children", None) => rules.include_children = true,
                (key, _) => {
                    return Err(format!("pattern {pattern}: #set! {key} is not supported"));
                }
            }
        }
        let uses_shebang = shebang.is_some_and(|capture| {
            query
                .capture_quantifiers(pattern)
                .get(capture)
                .is_some_and(|quantifier| *quantifier != CaptureQuantifier::Zero)
        });
        if uses_shebang {
            rules.skipped = true;
            patterns.push(rules);
            continue;
        }
        if rules.language.is_none() && language_capture.is_none() {
            return Err(format!("pattern {pattern} names no language"));
        }
        patterns.push(rules);
    }
    Ok(InjectionQuery {
        query,
        content,
        language: language_capture,
        patterns,
    })
}

/// The language a fence-style name stands for: the leading run of letters,
/// digits and `_ + # -`, lowercased. ```` ```rust,ignore ```` is `rust`, and
/// `{.python}` is not a name at all.
pub(crate) fn language_name(text: &str) -> Option<String> {
    let end = text
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '#' | '-')))
        .unwrap_or(text.len());
    let name = &text[..end];
    (!name.is_empty()).then(|| name.to_ascii_lowercase())
}

/// The ranges of `node` without those of its named children. Unnamed children are
/// tokens of the node's own text (the block grammar of Markdown exposes the
/// brackets of a code line as such), not other constructs to leave out.
fn without_children(node: Node<'_>) -> Vec<Range> {
    let mut ranges = Vec::new();
    let mut cursor = node.walk();
    let mut at = (node.start_byte(), node.start_position());
    for child in node.children(&mut cursor).filter(Node::is_named) {
        push(
            &mut ranges,
            at,
            (child.start_byte(), child.start_position()),
        );
        at = (child.end_byte(), child.end_position());
    }
    push(&mut ranges, at, (node.end_byte(), node.end_position()));
    ranges
}

fn push(ranges: &mut Vec<Range>, from: (usize, Point), to: (usize, Point)) {
    if to.0 > from.0 {
        ranges.push(Range {
            start_byte: from.0,
            end_byte: to.0,
            start_point: from.1,
            end_point: to.1,
        });
    }
}

/// Every embedded region of the document, in document order. `None` when
/// cancelled.
pub(crate) fn find(
    tree: &Tree,
    text: &str,
    query: &InjectionQuery,
    cancel: &AtomicBool,
) -> Option<Vec<Region>> {
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query.query, tree.root_node(), text.as_bytes());
    let mut regions: Vec<Region> = Vec::new();
    // Combined regions grow while later matches are read: (pattern, language) to index.
    let mut combined: Vec<((usize, String), usize)> = Vec::new();
    let mut seen = 0usize;
    while let Some(found) = matches.next() {
        seen += 1;
        if seen.is_multiple_of(256) && cancel.load(Ordering::Relaxed) {
            return None;
        }
        let rules = &query.patterns[found.pattern_index];
        if rules.skipped {
            continue;
        }
        let language = match &rules.language {
            Some(fixed) => Some(fixed.to_string()),
            None => found
                .captures()
                .iter()
                .find(|c| Some(c.index) == query.language)
                .and_then(|c| text.get(c.node.byte_range()))
                .and_then(language_name),
        };
        let Some(language) = language else { continue };
        let mut ranges = Vec::new();
        for capture in found.captures().iter().filter(|c| c.index == query.content) {
            if rules.include_children {
                ranges.push(capture.node.range());
            } else {
                ranges.extend(without_children(capture.node));
            }
        }
        if ranges.is_empty() {
            continue;
        }
        if rules.combined {
            let key = (found.pattern_index, language.clone());
            match combined.iter().find(|(k, _)| *k == key) {
                Some((_, at)) => regions[*at].ranges.extend(ranges),
                None => {
                    combined.push((key, regions.len()));
                    regions.push(Region { language, ranges });
                }
            }
        } else {
            regions.push(Region { language, ranges });
        }
    }
    // The parser needs its ranges sorted and apart.
    for region in &mut regions {
        region.ranges.sort_by_key(|r| (r.start_byte, r.end_byte));
        region.ranges.dedup();
        let mut kept: Vec<Range> = Vec::with_capacity(region.ranges.len());
        for range in region.ranges.drain(..) {
            if kept
                .last()
                .is_none_or(|last| last.end_byte <= range.start_byte)
            {
                kept.push(range);
            }
        }
        region.ranges = kept;
    }
    Some(regions)
}
