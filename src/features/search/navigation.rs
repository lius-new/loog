//! Resolve a navigation intent using an immutable snapshot or injected disk source.
use super::{Matcher, model::NavigationIntent, ports::SearchSourcePort};
use crate::domain::document_snapshot::DocumentSnapshot;
use std::ops::Range;
use std::sync::Arc;

#[derive(Debug, PartialEq, Eq)]
pub enum OpenError {
    Read(String),
    Changed(String),
}

pub fn prepare(
    source: &dyn SearchSourcePort,
    intent: &NavigationIntent,
    buffer: Option<&DocumentSnapshot>,
) -> Result<(Arc<String>, Range<usize>), OpenError> {
    let found =
        intent.file.matches.get(intent.index).ok_or_else(|| {
            OpenError::Changed("Search result is no longer available.".to_owned())
        })?;
    let text = match buffer {
        Some(buffer) => buffer.text.clone(),
        None => Arc::new(
            source
                .read_text(intent.file.path.local_path().ok_or_else(|| {
                    OpenError::Read("This source only reads local resources".into())
                })?)
                .map_err(OpenError::Read)?,
        ),
    };
    let range = Matcher::new(&intent.spec)
        .map_err(OpenError::Changed)?
        .locate(&text, intent.file.content_hash, found)
        .ok_or_else(|| {
            OpenError::Changed("File changed; searching again to update this result.".to_owned())
        })?;
    Ok((text, range))
}
