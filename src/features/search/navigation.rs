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
    // A text too large to hold is not read: the match is checked against its own line, and the
    // text returned is empty, as the document is opened (or already is) in place.
    let large: Option<Box<dyn super::stream::ByteSource + '_>> = match buffer {
        Some(buffer) => buffer
            .pieces
            .clone()
            .map(|pieces| Box::new(pieces) as Box<dyn super::stream::ByteSource>),
        None => match intent.file.path.local_path() {
            Some(path) => source.open_large(path).map_err(OpenError::Read)?,
            None => None,
        },
    };
    if let Some(large) = large {
        let range = super::stream::locate(large.as_ref(), found).ok_or_else(|| {
            OpenError::Changed("File changed; searching again to update this result.".to_owned())
        })?;
        return Ok((Arc::new(String::new()), range));
    }
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
