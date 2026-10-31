//! Preserve Markdown source and its identity until the package is reloaded.
use crate::features::extensions::manifest::MAX_DOCUMENT_BYTES;
use crate::features::markdown::content::Markup;
use std::{io::Read, path::Path};

pub fn load(directory: &Path, relative: &str) -> Result<Markup, String> {
    let base = directory
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let path = base
        .join(relative)
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if !path.starts_with(base) {
        return Err("the document is outside the package".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(MAX_DOCUMENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err("the document exceeds the 512 KiB limit".into());
    }
    Ok(Markup::markdown(
        String::from_utf8(bytes).map_err(|error| error.to_string())?,
    ))
}
