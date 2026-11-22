//! Disk observations use original bytes, including encoding-only changes.
use crate::domain::document::{DiskRead, DiskState};
use crate::domain::encoding::TextFormat;
use std::path::Path;
pub(crate) fn capture(path: &Path, contents: &str) -> DiskState {
    super::document_filesystem::read_encoded(
        path,
        None,
        super::document_filesystem::MAX_EDITABLE_FILE_BYTES,
    )
    .map(|(_, state)| state)
    .unwrap_or_else(|_| DiskState::from_contents(contents))
}
pub(crate) fn read_with_encoding(path: &Path, explicit: Option<TextFormat>) -> DiskRead {
    match super::document_filesystem::read_encoded(
        path,
        explicit,
        super::document_filesystem::MAX_EDITABLE_FILE_BYTES,
    ) {
        Ok((decoded, state)) => DiskRead::Found {
            contents: decoded.text,
            state,
            bytes: (!decoded.bytes.is_empty()).then_some(decoded.bytes),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DiskRead::Missing,
        Err(e) => DiskRead::Failed(e.to_string()),
    }
}
pub(crate) fn read(path: &Path) -> DiskRead {
    read_with_encoding(path, None)
}
pub(crate) fn read_contents(path: &Path) -> std::io::Result<String> {
    super::document_filesystem::read_diff_file(path)
}

pub(crate) fn capture_format(path: &Path, contents: &str, format: TextFormat) -> DiskState {
    match std::fs::read(path) {
        Ok(bytes) => super::document_filesystem::state(path, &bytes, format),
        Err(_) => {
            let mut state = DiskState::from_contents(contents);
            state.format = format;
            state
        }
    }
}
