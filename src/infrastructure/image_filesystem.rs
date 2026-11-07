//! Bounded image decoding. GIF opening prepares one frame, never the entire animation.
use crate::domain::document::DiskState;
use crate::domain::media::{Frame, Image};
use image::{ImageDecoder, ImageEncoder};
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::Arc;

const FILE_LIMIT: u64 = 32 * 1024 * 1024;
const PIXEL_LIMIT: u64 = 64 * 1024 * 1024;
fn fingerprint(bytes: &[u8]) -> u64 {
    let hash = ring::digest::digest(&ring::digest::SHA256, bytes);
    u64::from_le_bytes(hash.as_ref()[..8].try_into().unwrap())
}
pub(crate) fn limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(PIXEL_LIMIT);
    limits
}
fn png(rgba: &[u8], width: u32, height: u32) -> Result<Arc<Vec<u8>>, String> {
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|e| e.to_string())?;
    Ok(Arc::new(bytes))
}
pub fn load(path: &Path) -> Result<(Image, DiskState), String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let before = file.metadata().map_err(|e| e.to_string())?;
    if before.len() > FILE_LIMIT {
        return Err("Image exceeds the 32 MiB file limit".into());
    }
    let mut bytes = Vec::new();
    file.take(FILE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > FILE_LIMIT {
        return Err("Image exceeds the 32 MiB file limit".into());
    }
    let after = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        return Err("Image changed while loading; retry after it settles".into());
    }
    let image = decode(&bytes)?;
    let disk = DiskState {
        modified_time: after.modified().ok(),
        size: after.len(),
        content_hash: image.identity,
        format: Default::default(),
    };
    Ok((image, disk))
}
pub fn decode(bytes: &[u8]) -> Result<Image, String> {
    let format = image::guess_format(bytes).map_err(|_| "Unrecognized image format".to_owned())?;
    let label = match format {
        image::ImageFormat::Png => "PNG",
        image::ImageFormat::Jpeg => "JPEG",
        image::ImageFormat::WebP => "WebP",
        image::ImageFormat::Gif => "GIF",
        image::ImageFormat::Bmp => "BMP",
        image::ImageFormat::Ico => "ICO",
        _ => return Err("Unsupported image format".into()),
    };
    let (mut width, mut height) = image::ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|e| e.to_string())?;
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || u64::from(width) * u64::from(height) * 4 > PIXEL_LIMIT
    {
        return Err("Image dimensions exceed the 64 MiB decode limit".into());
    }
    let mut frames = Vec::new();
    let mut animation = None;
    if format == image::ImageFormat::Gif {
        let source: Arc<[u8]> = Arc::from(bytes);
        let mut stream = crate::infrastructure::gif_stream::Stream::new(source.clone())?;
        frames.push(stream.next_frame()?);
        animation = Some(source);
    } else {
        let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
        reader.limits(limits());
        let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
        let orientation = decoder.orientation().map_err(|e| e.to_string())?;
        let mut decoded = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
        decoded.apply_orientation(orientation);
        let rgba = decoded.into_rgba8();
        width = rgba.width();
        height = rgba.height();
        frames.push(Frame {
            bytes: png(rgba.as_raw(), width, height)?,
            duration_ms: 100,
            sequence: 0,
        });
    }
    if frames.is_empty() {
        return Err("Image has no frames".into());
    }
    Ok(Image {
        width,
        height,
        format: label,
        byte_len: bytes.len() as u64,
        identity: fingerprint(bytes),
        frames,
        animation,
    })
}

/// Rasterize SVG at the requested device scale, retaining its logical dimensions.
pub fn svg(source: &str, density: f32) -> Result<Image, String> {
    static OPTIONS: std::sync::OnceLock<resvg::usvg::Options<'static>> = std::sync::OnceLock::new();
    let options = OPTIONS.get_or_init(|| {
        let mut options = resvg::usvg::Options::default();
        options.image_href_resolver.resolve_string = Box::new(|_, _| None);
        options.fontdb_mut().load_system_fonts();
        options
    });
    let tree = resvg::usvg::Tree::from_str(source, options).map_err(|e| e.to_string())?;
    let size = tree.size();
    let (width, height) = (size.width().ceil(), size.height().ceil());
    if !(width >= 1.0 && height >= 1.0 && width <= 32768.0 && height <= 32768.0) {
        return Err("SVG dimensions exceed the preview limit".into());
    }
    let density = density
        .clamp(0.05, 16.0)
        .min(8192.0 / width)
        .min(8192.0 / height)
        .min((PIXEL_LIMIT as f32 / (width * height * 4.0)).sqrt());
    let (w, h) = (
        (width * density).floor().max(1.0) as u32,
        (height * density).floor().max(1.0) as u32,
    );
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h).ok_or("Could not allocate SVG preview")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(w as f32 / size.width(), h as f32 / size.height()),
        &mut pixmap.as_mut(),
    );
    let mut rgba = Vec::with_capacity(pixmap.pixels().len() * 4);
    for pixel in pixmap.pixels() {
        let color = pixel.demultiply();
        rgba.extend_from_slice(&[color.red(), color.green(), color.blue(), color.alpha()]);
    }
    Ok(Image {
        width: width as u32,
        height: height as u32,
        format: "SVG",
        byte_len: source.len() as u64,
        identity: fingerprint(source.as_bytes()) ^ u64::from(w) << 32 ^ u64::from(h),
        frames: vec![Frame {
            bytes: png(&rgba, w, h)?,
            duration_ms: 100,
            sequence: 0,
        }],
        animation: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "set LOOG_GIF_BENCH_FILE to measure a local GIF without opening the application"]
    fn gif_open_and_playback_performance() {
        use std::time::{Duration, Instant};
        let path =
            std::env::var_os("LOOG_GIF_BENCH_FILE").expect("LOOG_GIF_BENCH_FILE is required");
        let started = Instant::now();
        let (opened, _) = load(Path::new(&path)).unwrap();
        let open_time = started.elapsed();
        assert_eq!(opened.frames.len(), 1);
        let mut surface =
            skia_safe::surfaces::raster_n32_premul((opened.width as i32, opened.height as i32))
                .unwrap();
        let draw = |surface: &mut skia_safe::Surface, bytes: &[u8]| {
            let image = skia_safe::Image::from_encoded(skia_safe::Data::new_copy(bytes))
                .expect("Skia must accept the uncompressed frame");
            surface.canvas().clear(skia_safe::Color::TRANSPARENT);
            surface.canvas().draw_image(&image, (0.0, 0.0), None);
        };
        draw(&mut surface, &opened.frames[0].bytes);
        let visible_time = started.elapsed();
        let phase = Instant::now();
        std::hint::black_box(fingerprint(&opened.animation.clone().unwrap()));
        let hash_time = phase.elapsed();
        let phase = Instant::now();
        use image::AnimationDecoder;
        let raw =
            image::codecs::gif::GifDecoder::new(Cursor::new(opened.animation.clone().unwrap()))
                .unwrap()
                .into_frames()
                .next()
                .unwrap()
                .unwrap()
                .into_buffer();
        let raw_time = phase.elapsed();
        eprintln!(
            "GIF phase probe: hash={:.2}ms, first raw decode={:.2}ms",
            hash_time.as_secs_f64() * 1000.0,
            raw_time.as_secs_f64() * 1000.0
        );
        std::hint::black_box(raw);
        let mut stream =
            crate::infrastructure::gif_stream::Stream::new(opened.animation.clone().unwrap())
                .unwrap();
        let count: usize = std::env::var("LOOG_GIF_BENCH_FRAMES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(250);
        let mut samples = Vec::with_capacity(count);
        let mut duration_ms = 0;
        for _ in 0..count {
            let started = Instant::now();
            let frame = stream.next_frame().unwrap();
            draw(&mut surface, &frame.bytes);
            samples.push(started.elapsed());
            duration_ms += frame.duration_ms;
        }
        let total: Duration = samples.iter().copied().sum();
        samples.sort_unstable();
        let p95 = samples[(samples.len() - 1) * 95 / 100];
        eprintln!(
            "GIF {}x{}, {} bytes: load={:.2}ms, first frame painted={:.2}ms; {} frames decode+paint={:.2}ms total, avg={:.2}ms, p95={:.2}ms, max={:.2}ms; timeline={}ms; initially retained frames={}",
            opened.width,
            opened.height,
            opened.byte_len,
            open_time.as_secs_f64() * 1000.0,
            visible_time.as_secs_f64() * 1000.0,
            count,
            total.as_secs_f64() * 1000.0,
            total.as_secs_f64() * 1000.0 / count as f64,
            p95.as_secs_f64() * 1000.0,
            samples.last().unwrap().as_secs_f64() * 1000.0,
            duration_ms,
            opened.frames.len()
        );
        assert!(
            visible_time < Duration::from_millis(100),
            "first-frame decode and drawing should be interactive"
        );
        assert!(
            p95 < Duration::from_millis(40),
            "decoding and drawing must stay below this GIF's frame interval"
        );
    }
    #[test]
    fn jpeg_preview_respects_exif_orientation() {
        let mut jpeg = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(16, 12)
            .write_to(&mut jpeg, image::ImageFormat::Jpeg)
            .unwrap();
        // Little-endian TIFF, one SHORT Orientation entry, rotate 90 degrees.
        let exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
        let mut bytes = jpeg.get_ref()[..2].to_vec();
        bytes.extend_from_slice(&[0xff, 0xe1]);
        bytes.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        bytes.extend_from_slice(exif);
        bytes.extend_from_slice(&jpeg.get_ref()[2..]);
        let result = decode(&bytes).unwrap();
        assert_eq!((result.width, result.height), (12, 16));
    }
    #[test]
    fn common_raster_formats_are_validated_and_malformed_files_fail() {
        for format in [
            image::ImageFormat::Png,
            image::ImageFormat::Jpeg,
            image::ImageFormat::WebP,
            image::ImageFormat::Bmp,
            image::ImageFormat::Ico,
        ] {
            let mut bytes = Cursor::new(Vec::new());
            let fixture = if format == image::ImageFormat::Ico {
                // ICO embeds a PNG here, and its decoder requires RGBA pixels.
                let mut rgba = image::RgbaImage::from_pixel(16, 12, image::Rgba([23, 45, 67, 255]));
                rgba.put_pixel(0, 0, image::Rgba([23, 45, 67, 64]));
                image::DynamicImage::ImageRgba8(rgba)
            } else {
                image::DynamicImage::new_rgb8(16, 12)
            };
            fixture
                .write_to(&mut bytes, format)
                .unwrap_or_else(|error| panic!("{format:?} fixture encoding failed: {error}"));
            let result = decode(bytes.get_ref())
                .unwrap_or_else(|error| panic!("{format:?} preview decoding failed: {error}"));
            assert_eq!((result.width, result.height), (16, 12));
            assert!(result.frames[0].bytes.starts_with(b"\x89PNG"));
            if format == image::ImageFormat::Ico {
                let preview = image::load_from_memory(&result.frames[0].bytes)
                    .unwrap()
                    .into_rgba8();
                assert_eq!(*preview.get_pixel(0, 0), image::Rgba([23, 45, 67, 64]));
                assert_eq!(*preview.get_pixel(1, 0), image::Rgba([23, 45, 67, 255]));
            }
        }
        assert!(decode(b"not an image").is_err());
    }
    #[test]
    fn gif_animation_keeps_composited_frames_and_delays() {
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            for delay in [40, 100] {
                encoder
                    .encode_frame(image::Frame::from_parts(
                        image::RgbaImage::from_pixel(2, 2, image::Rgba([delay, 0, 0, 255])),
                        0,
                        0,
                        image::Delay::from_numer_denom_ms(u32::from(delay), 1),
                    ))
                    .unwrap();
            }
        }
        let result = decode(&bytes).unwrap();
        assert_eq!(
            result.frames.len(),
            1,
            "opening must only prepare the first frame"
        );
        let mut stream =
            crate::infrastructure::gif_stream::Stream::new(result.animation.unwrap()).unwrap();
        assert_eq!(stream.next_frame().unwrap().duration_ms, 40);
        assert_eq!(stream.next_frame().unwrap().duration_ms, 100);
        assert_eq!(
            stream.next_frame().unwrap().duration_ms,
            40,
            "loops automatically"
        );
    }
    #[test]
    fn svg_is_rendered_from_unsaved_source_at_requested_density() {
        let source = r##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="20"><rect width="32" height="20" fill="#ff0000"/></svg>"##;
        let result = svg(source, 2.0).unwrap();
        assert_eq!((result.width, result.height), (32, 20));
        let pixels = image::load_from_memory(&result.frames[0].bytes).unwrap();
        assert_eq!((pixels.width(), pixels.height()), (64, 40));
        assert!(svg("<svg>", 1.0).is_err());
    }
}
