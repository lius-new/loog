// Syntax spans from Tree-sitter trees. No theme, GUI state or worker handles live here.
#[cfg(test)]
pub(crate) mod baseline;
#[cfg(test)]
pub(crate) mod bench_support;
mod grammars;
pub(crate) use self::grammars::{EmptyResources, Resources};
mod injections;
mod layers;
pub(crate) mod presentation;
#[cfg(test)]
mod presentation_tests;
mod queries;
mod treesitter;
mod window;
pub(crate) use self::treesitter::MAX_DOCUMENT_BYTES;
pub(crate) use self::treesitter::{TreeIndex, TreeStats};
pub(crate) use self::window::{Freshness, Source, Window, WindowTokens};
use crate::features::highlighting::HighlightId;
use std::ops::Range;

/// Check downloaded resources with the same query compilers used by the worker.
pub(crate) fn validate_resources(resources: &dyn Resources, grammar: &str) -> Result<(), String> {
    queries::compile(resources, grammar)?;
    injections::compile(resources, grammar)?;
    Ok(())
}

/// A run of text and the highlight class it is drawn with; `None` is plain text.
/// Every source of highlighting produces these, so the display does not care
/// which one made them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TokenSpan {
    pub range: Range<usize>,
    pub class: Option<HighlightId>,
}

pub(crate) use self::treesitter::SnippetHighlighter;
#[cfg(test)]
pub(crate) use self::treesitter::highlight_with;
