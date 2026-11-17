//! Snapshot package artwork on load; decode it once when the UI requests it.
use crate::domain::media::Image;
use crate::features::extensions::manifest::MAX_ICON_BYTES;
use crate::infrastructure::image_filesystem;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

enum Encoded {
    Svg(String),
    Raster(Vec<u8>),
}
impl Encoded {
    fn decode(self) -> Result<Image, String> {
        match self {
            Self::Svg(source) => image_filesystem::svg(&source, 2.0),
            Self::Raster(bytes) => image_filesystem::decode(&bytes),
        }
    }
}

pub struct Icon {
    source: Mutex<Option<Encoded>>,
    decoded: OnceLock<Result<Arc<Image>, String>>,
}
impl Icon {
    pub fn image(&self) -> &Result<Arc<Image>, String> {
        self.decoded.get_or_init(|| {
            let source = self
                .source
                .lock()
                .unwrap()
                .take()
                .expect("unresolved icon source");
            source.decode().map(Arc::new)
        })
    }
}

pub fn prepare(directory: &Path, relative: &str) -> Result<Icon, String> {
    Ok(Icon {
        source: Mutex::new(Some(read_source(directory, relative)?)),
        decoded: OnceLock::new(),
    })
}

pub fn load(directory: &Path, relative: &str) -> Result<Image, String> {
    read_source(directory, relative)?.decode()
}

fn read_source(directory: &Path, relative: &str) -> Result<Encoded, String> {
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
        String::from_utf8(bytes)
            .map(Encoded::Svg)
            .map_err(|error| error.to_string())
    } else {
        Ok(Encoded::Raster(bytes))
    }
}
