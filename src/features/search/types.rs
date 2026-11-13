//! Request/result values; no UI, runtime, or filesystem ownership.
use loog_workspace_api::ResourceRef;
use std::path::PathBuf;
use std::sync::Arc;

pub use loog_text_core::search::{Match, SearchSpec};

pub use crate::domain::document_snapshot::DocumentSnapshot as BufferSnapshot;

#[derive(Clone, Debug)]
pub struct FileMatches {
    pub path: ResourceRef,
    pub root: ResourceRef,
    pub provider: Option<ProviderLabels>,
    pub matches: Vec<Match>,
    pub content_hash: u64,
}

/// Display components supplied by a provider, never parsed as a client path.
#[derive(Clone, Debug)]
pub struct ProviderLabels {
    pub name: String,
    pub relative: Vec<String>,
    pub root_name: String,
}
impl FileMatches {
    pub fn name(&self) -> std::borrow::Cow<'_, str> {
        match &self.provider {
            Some(labels) => std::borrow::Cow::Borrowed(&labels.name),
            None => self
                .path
                .local_path()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy())
                .unwrap_or_else(|| "Resource".into()),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SearchResults {
    pub files: Vec<Arc<FileMatches>>,
    pub matches: usize,
    pub searched: usize,
    pub skipped: usize,
    pub skipped_binary: usize,
    pub skipped_encoding: usize,
    pub skipped_large: usize,
    pub errors: usize,
    pub first_error: Option<String>,
    pub limit_hit: bool,
    pub cancelled: bool,
    pub complete: bool,
}

pub struct SearchRequest {
    pub spec: SearchSpec,
    pub roots: Vec<PathBuf>,
    pub buffers: Vec<BufferSnapshot>,
}

pub use self::SearchRequest as Request;
pub use self::SearchResults as Results;
