//! Every grammar is provided through the demand-loaded Resources adapter.
use std::sync::Arc;
use tree_sitter::{Language, Parser};

/// Languages must remain usable by parsers, trees and queries after this call.
/// Native adapters retain module handles; WASM languages use reference counting.
/// Called by background
/// syntax work; this port has no filesystem or application dependencies.
pub(crate) trait Resources: Send + Sync {
    /// Identity stays fixed; a generation changes when resource bytes become available.
    fn generation(&self) -> u64 {
        0
    }
    /// Pin a resource set while a worker builds parsers and queries.
    fn snapshot(&self) -> Option<Arc<dyn Resources>> {
        None
    }
    fn pending(&self, _grammar: &str) -> bool {
        false
    }
    fn subscribe(&self, _changed: Arc<dyn Fn() + Send + Sync>) {}
    fn language(&self, grammar: &str) -> Result<Language, String>;
    /// The adapter supplies any execution store needed by this language. Each
    /// parser owns its mutable store; trees and queries can share the language.
    fn parser(&self, grammar: &str) -> Result<Parser, String> {
        let language = self.language(grammar)?;
        let mut parser = Parser::new();
        parser.set_language(&language).map_err(|e| e.to_string())?;
        Ok(parser)
    }
    fn text(&self, grammar: &str, file: &str) -> Result<Option<Arc<str>>, String>;
    /// The file and 1-based line that supplied byte `at` of what `text` returned
    /// for `grammar` and `file`, when the adapter keeps track.
    fn origin(&self, _grammar: &str, _file: &str, _at: usize) -> Option<(String, usize)> {
        None
    }
    fn report(&self, _error: &str) {}
}

/// A detached index can still handle plain text without installed resources.
pub(crate) struct EmptyResources;
impl Resources for EmptyResources {
    fn language(&self, grammar: &str) -> Result<Language, String> {
        Err(format!("no language resources for {grammar}"))
    }
    fn text(&self, _grammar: &str, _file: &str) -> Result<Option<Arc<str>>, String> {
        Ok(None)
    }
}
