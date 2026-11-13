//! Bounded content search publications. Paths and display names remain provider data.
use crate::ProviderPath;
use loog_text_core::{
    format::EncodingSettings,
    search::{Match, SearchSpec},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchOptions {
    pub spec: SearchSpec,
    pub encoding: EncodingSettings,
    /// Eligible files are reported without disk matches. The consumer searches
    /// its own immutable buffers, keeping unsaved text out of the provider.
    pub buffers: Vec<ProviderPath>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchFile {
    pub path: ProviderPath,
    pub name: String,
    pub relative: Vec<String>,
    pub matches: Vec<Match>,
    pub content_hash: u64,
    pub buffer: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchProgress {
    pub searched: usize,
    pub skipped_binary: usize,
    pub skipped_encoding: usize,
    pub skipped_large: usize,
    pub errors: usize,
    pub first_error: Option<String>,
    pub limit_hit: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchPage {
    pub files: Vec<SearchFile>,
    /// Cumulative counters for this search handle, not a delta.
    pub progress: SearchProgress,
    pub complete: bool,
}
