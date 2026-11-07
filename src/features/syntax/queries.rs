//! Compiles a grammar's highlight query and checks it against what Loog
//! supports, so an unsupported query fails loudly instead of being half applied.
//!
//! Supported:
//!
//! * Captures named like a highlight class (`@function.method`) or one of its
//!   ancestors (`@keyword.control` is a `@keyword`). `@escape` and `@label` are
//!   upstream spellings Loog maps to `string.escape` and `type`; `@embedded`
//!   carries no highlight; captures starting with `_` are helpers for predicates.
//! * The text predicates `#eq?`, `#not-eq?`, `#match?`, `#not-match?`,
//!   `#any-of?` and `#not-any-of?`, which Tree-sitter evaluates itself.
//! * `#is-not? local`, accepted because Loog has no scope analysis yet: such a
//!   pattern then applies to every identifier it matches.
//!
//! Anything else (other predicates, `#set!`, unknown captures) is an error.
//!
//! When patterns overlap, the shorter range wins, and for equal ranges the
//! pattern written later in the file. The upstream queries put general patterns
//! first and specific ones after, which is what that rule needs.
use super::Resources;
use crate::features::highlighting::HighlightId;
use tree_sitter::{Language, Query};

const HIGHLIGHTS: &str = "highlights.scm";

/// Upstream capture names Loog spells as another class.
const ALIASES: &[(&str, &str)] = &[("escape", "string.escape"), ("label", "type")];
/// Captures that deliberately carry no highlight.
const IGNORED: &[&str] = &["embedded"];

pub(crate) struct CompiledQuery {
    pub(crate) query: Query,
    /// The class of each capture, by capture index; `None` for helper captures.
    classes: Vec<Option<HighlightId>>,
}

impl CompiledQuery {
    pub(crate) fn class_of(&self, capture: u32) -> Option<HighlightId> {
        self.classes[capture as usize]
    }
}

pub(crate) fn compile(resources: &dyn Resources, grammar: &str) -> Result<CompiledQuery, String> {
    let language = resources.language(grammar)?;
    let source = resources
        .text(grammar, HIGHLIGHTS)?
        .ok_or_else(|| format!("grammar `{grammar}` has no {HIGHLIGHTS}"))?;
    compile_source(&language, &source).map_err(|error| format!("grammar `{grammar}`: {error}"))
}

/// Whether `source` is a `file` query (`highlights.scm` or `injections.scm`) that
/// Loog accepts for `language`, with the same compilers the worker uses.
pub(crate) fn check_query(language: &Language, file: &str, source: &str) -> Result<(), String> {
    match file {
        HIGHLIGHTS => compile_source(language, source).map(|_| ()),
        _ => super::injections::compile_source(language, source).map(|_| ()),
    }
}

pub(crate) fn compile_source(language: &Language, source: &str) -> Result<CompiledQuery, String> {
    let query = Query::new(language, source).map_err(|error| error.to_string())?;
    let classes = query
        .capture_names()
        .iter()
        .map(|name| class_for(name))
        .collect::<Result<Vec<_>, _>>()?;
    for pattern in 0..query.pattern_count() {
        if let Some(predicate) = query.general_predicates(pattern).first() {
            return Err(format!(
                "pattern {pattern}: the predicate #{} is not supported",
                predicate.operator
            ));
        }
        if let Some(setting) = query.property_settings(pattern).first() {
            return Err(format!(
                "pattern {pattern}: #set! {} is not supported",
                setting.key
            ));
        }
        for (property, positive) in query.property_predicates(pattern) {
            let local_only = &*property.key == "local" && property.value.is_none() && !positive;
            if !local_only {
                return Err(format!(
                    "pattern {pattern}: #{} {} is not supported",
                    if *positive { "is?" } else { "is-not?" },
                    property.key
                ));
            }
        }
    }
    Ok(CompiledQuery { query, classes })
}

fn class_for(name: &str) -> Result<Option<HighlightId>, String> {
    if name.starts_with('_') || IGNORED.contains(&name) {
        return Ok(None);
    }
    let resolved = ALIASES
        .iter()
        .find(|(from, _)| *from == name)
        .map_or(name, |(_, to)| *to);
    HighlightId::from_name(resolved)
        .map(Some)
        .ok_or_else(|| format!("the capture @{name} is not a known highlight class"))
}
