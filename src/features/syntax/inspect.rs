//! Explains how one place in a document is highlighted: which captures cover it,
//! which one wins, the class it gives, and the query file and line it came from.
//!
//! This is for people writing or tuning highlight queries. It parses the whole
//! text with the document's own grammar, so it runs on demand, never while
//! drawing. Embedded languages are not looked into; the answer is for the
//! document's own grammar.
use super::{Resources, queries};
use crate::features::highlighting::HighlightId;
use std::ops::Range;
use streaming_iterator::StreamingIterator;
use tree_sitter::QueryCursor;

/// One capture whose node covers the inspected byte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hit {
    /// The capture's name in the query, such as `keyword.control`.
    pub capture: String,
    /// The class it gives; `None` for helper captures that colour nothing.
    pub class: Option<HighlightId>,
    pub range: Range<usize>,
    /// The query file and 1-based line of the pattern, when known.
    pub origin: Option<(String, usize)>,
    /// Whether this is the capture that decides the colour.
    pub wins: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Inspection {
    pub grammar: String,
    /// The syntax node at the byte and its ancestors, innermost first.
    pub nodes: Vec<String>,
    pub hits: Vec<Hit>,
}

/// The most ancestors listed.
const DEPTH: usize = 5;

pub(crate) fn inspect(
    resources: &dyn Resources,
    grammar: &str,
    text: &str,
    offset: usize,
) -> Result<Inspection, String> {
    if offset > text.len() || !text.is_char_boundary(offset) {
        return Err("the position is not inside the text".to_owned());
    }
    let mut parser = resources.parser(grammar)?;
    let tree = parser
        .parse(text, None)
        .ok_or_else(|| format!("{grammar} could not parse this document"))?;
    let compiled = queries::compile(resources, grammar)?;
    let end = (offset + 1).min(text.len());
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(offset..end);
    let mut matches = cursor.matches(&compiled.query, tree.root_node(), text.as_bytes());
    let mut found: Vec<(usize, Hit)> = Vec::new();
    while let Some(found_match) = matches.next() {
        for capture in found_match.captures() {
            let range = capture.node.byte_range();
            if !(range.start <= offset && offset < range.end.max(range.start + 1)) {
                continue;
            }
            let pattern = found_match.pattern_index;
            found.push((
                pattern,
                Hit {
                    capture: compiled.query.capture_names()[capture.index as usize].to_owned(),
                    class: compiled.class_of(capture.index),
                    range,
                    origin: resources.origin(
                        grammar,
                        "highlights.scm",
                        compiled.query.start_byte_for_pattern(pattern),
                    ),
                    wins: false,
                },
            ));
        }
    }
    // The highlighter's rule: the shorter range wins, and for equal ranges the
    // pattern written later.
    let winner = found
        .iter()
        .enumerate()
        .filter(|(_, (_, hit))| hit.class.is_some())
        .min_by_key(|(at, (pattern, hit))| (hit.range.len(), std::cmp::Reverse((*pattern, *at))))
        .map(|(at, _)| at);
    if let Some(at) = winner {
        found[at].1.wins = true;
    }
    let mut nodes = Vec::new();
    let mut node = tree
        .root_node()
        .descendant_for_byte_range(offset, end)
        .or_else(|| Some(tree.root_node()));
    while let Some(current) = node {
        if nodes.len() == DEPTH {
            break;
        }
        nodes.push(current.kind().to_owned());
        node = current.parent();
    }
    Ok(Inspection {
        grammar: grammar.to_owned(),
        nodes,
        hits: found.into_iter().map(|(_, hit)| hit).collect(),
    })
}

/// A short, readable report. `colour` shows the theme's colour for a class.
pub(crate) fn describe(inspection: &Inspection, colour: impl Fn(HighlightId) -> String) -> String {
    let mut out = format!("{}: {}", inspection.grammar, inspection.nodes.join(" < "));
    if inspection.hits.is_empty() {
        out.push_str("\nNo query colours this. Add a pattern for it.");
        return out;
    }
    for hit in &inspection.hits {
        let class = match hit.class {
            Some(class) => format!("{} {}", class.name(), colour(class)),
            None => "no colour (helper capture)".to_owned(),
        };
        let place = match &hit.origin {
            Some((file, line)) => format!("{file}:{line}"),
            None => "unknown file".to_owned(),
        };
        out.push_str(&format!(
            "\n{} @{} -> {class} ({place})",
            if hit.wins { ">" } else { " " },
            hit.capture
        ));
    }
    out.push_str("\n> marks the capture that decides the colour.");
    out
}
