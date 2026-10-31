//! Read the package's own artwork once, when it is loaded or reloaded.
use crate::domain::media::Image;
use crate::features::extensions::manifest::MAX_ICON_BYTES;
use crate::infrastructure::image_filesystem;
use std::io::Read;
use std::path::Path;

pub fn load(directory: &Path, relative: &str) -> Result<Image, String> {
    let base = directory
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let path = base
        .join(relative)
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if !path.starts_with(&base) {
        return Err("the icon is outside the package".to_owned());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .map_err(|error| error.to_string())?
        .take(MAX_ICON_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_ICON_BYTES {
        return Err("the icon exceeds the 256 KiB limit".to_owned());
    }
    if crate::domain::media::is_svg(&path) {
        image_filesystem::svg(
            std::str::from_utf8(&bytes).map_err(|error| error.to_string())?,
            2.0,
        )
    } else {
        image_filesystem::decode(&bytes)
    }
}
