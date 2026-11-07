//! Bounded file/network image loading. Hosts supply the remote policy explicitly.
use crate::features::markdown::resources::{Address, resolve};
use std::{
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub path: PathBuf,
    pub length: u64,
    pub modified: Option<SystemTime>,
}
pub fn stamp(path: &Path) -> Option<FileStamp> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(FileStamp {
        path: path.to_owned(),
        length: metadata.len(),
        modified: metadata.modified().ok(),
    })
}
#[derive(Clone, Debug)]
pub struct LoadedImage {
    pub width: u32,
    pub height: u32,
    pub encoded: Arc<Vec<u8>>,
    pub local: Option<FileStamp>,
}
pub fn network_path(path: &Path) -> bool {
    let value = path.to_string_lossy();
    value.starts_with("\\\\") || value.starts_with("//")
}
pub fn load(base: Option<&Path>, destination: &str, remote: bool) -> Result<LoadedImage, String> {
    let address = resolve(base, destination).ok_or("Unsupported image address")?;
    let mut bytes = Vec::new();
    let mut local = None;
    match address {
        Address::Local { path, .. } => {
            if !remote && network_path(&path) {
                return Err("Network images are not loaded in this view".into());
            }
            let before = stamp(&path);
            let file = std::fs::File::open(&path).map_err(|error| error.to_string())?;
            if file.metadata().map_err(|error| error.to_string())?.len() > 8 * 1024 * 1024 {
                return Err("Image exceeds the 8 MiB encoded limit".into());
            }
            file.take(8 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            let after = stamp(&path);
            if before != after {
                return Err("Image changed while loading; retry after it settles".into());
            }
            local = after;
        }
        Address::Web(url) => {
            if !remote {
                return Err("Remote images are not loaded in this view".into());
            }
            let agent = ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(3))
                .timeout_read(Duration::from_secs(5))
                .timeout(Duration::from_secs(8))
                .build();
            let response = agent.get(&url).call().map_err(|error| error.to_string())?;
            response
                .into_reader()
                .take(8 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
        }
        Address::Anchor(_) => return Err("An anchor is not an image".into()),
    }
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("Image exceeds the 8 MiB encoded limit".into());
    }
    if is_svg(&bytes) {
        return rasterize_svg(&bytes, local);
    }
    let format = image::guess_format(&bytes)
        .map_err(|_| "Unsupported image format; PNG and JPEG are supported")?;
    if !matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg) {
        return Err("Unsupported image format; PNG and JPEG are supported".into());
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(&bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(16 * 1024 * 1024);
    reader.limits(limits);
    let (width, height) = image::ImageReader::with_format(Cursor::new(&bytes), format)
        .into_dimensions()
        .map_err(|error| error.to_string())?;
    if width == 0
        || height == 0
        || width > 4096
        || height > 4096
        || (width as u64) * (height as u64) * 4 > 16 * 1024 * 1024
    {
        return Err("Image dimensions exceed the decode budget".into());
    }
    // Validate decoding under limits before handing encoded data to the GUI loader.
    reader.decode().map_err(|error| error.to_string())?;
    Ok(LoadedImage {
        width,
        height,
        encoded: Arc::new(bytes),
        local,
    })
}
fn is_svg(bytes: &[u8]) -> bool {
    String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]).contains("<svg")
}
/// Draws an SVG once into a PNG so the GUI loader sees an ordinary image. The
/// layout size is the SVG's own; the pixels are twice that for sharp scaling.
/// External resources are never read, and sizes stay within the decode budget.
fn rasterize_svg(bytes: &[u8], local: Option<FileStamp>) -> Result<LoadedImage, String> {
    static OPTIONS: std::sync::OnceLock<resvg::usvg::Options<'static>> = std::sync::OnceLock::new();
    let options = OPTIONS.get_or_init(|| {
        let mut options = resvg::usvg::Options::default();
        options.fontdb_mut().load_system_fonts();
        options
    });
    let tree = resvg::usvg::Tree::from_data(bytes, options).map_err(|error| error.to_string())?;
    let size = tree.size();
    let (width, height) = (size.width().ceil(), size.height().ceil());
    if !(width >= 1.0 && height >= 1.0 && width <= 4096.0 && height <= 4096.0) {
        return Err("Image dimensions exceed the decode budget".into());
    }
    let mut scale = 2.0_f32;
    let fits = |scale: f32| {
        let (w, h) = (
            (width * scale).ceil() as u64,
            (height * scale).ceil() as u64,
        );
        w <= 4096 && h <= 4096 && w * h * 4 <= 16 * 1024 * 1024
    };
    if !fits(scale) {
        scale = 1.0;
        if !fits(scale) {
            return Err("Image dimensions exceed the decode budget".into());
        }
    }
    let (pixel_width, pixel_height) = (
        (width * scale).ceil() as u32,
        (height * scale).ceil() as u32,
    );
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pixel_width, pixel_height)
        .ok_or("Image dimensions exceed the decode budget")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(
            pixel_width as f32 / size.width(),
            pixel_height as f32 / size.height(),
        ),
        &mut pixmap.as_mut(),
    );
    let mut rgba = Vec::with_capacity(pixmap.pixels().len() * 4);
    for pixel in pixmap.pixels() {
        let color = pixel.demultiply();
        rgba.extend_from_slice(&[color.red(), color.green(), color.blue(), color.alpha()]);
    }
    let mut encoded = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut encoded),
        &rgba,
        pixel_width,
        pixel_height,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|error| error.to_string())?;
    Ok(LoadedImage {
        width: width as u32,
        height: height as u32,
        encoded: Arc::new(encoded),
        local,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_and_unsupported_inputs_fail_without_loading() {
        assert!(
            load(None, "https://example.com/image.png", false)
                .unwrap_err()
                .contains("not loaded")
        );
        assert!(load(None, "data:image/png,a", true).is_err());
    }

    #[test]
    fn svg_badges_rasterize_at_their_own_size_without_reading_other_files() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("loog-svg-{unique}"));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("badge.svg");
        std::fs::write(
            &path,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="90" height="20">
<rect width="45" height="20" fill="#555"/><rect x="45" width="45" height="20" fill="#4c1"/>
<text x="22" y="14" fill="#fff" font-size="11">CI</text>
<image href="file:///etc/passwd" width="10" height="10"/></svg>"##,
        )
        .unwrap();
        let loaded = load(None, path.to_str().unwrap(), false).unwrap();
        assert_eq!((loaded.width, loaded.height), (90, 20));
        let (w, h) = image::ImageReader::with_format(
            Cursor::new(loaded.encoded.as_slice()),
            image::ImageFormat::Png,
        )
        .into_dimensions()
        .unwrap();
        assert_eq!((w, h), (180, 40));
        assert!(load(None, directory.join("missing.svg").to_str().unwrap(), false).is_err());
        let _ = std::fs::remove_dir_all(directory);
    }
}
