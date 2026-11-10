//! Immutable media publications and independent, renderer-free view positions.
use std::path::Path;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Frame {
    pub bytes: Arc<Vec<u8>>,
    pub duration_ms: u64,
    pub sequence: u64,
}

#[derive(Clone, Debug)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub format: &'static str,
    pub byte_len: u64,
    pub identity: u64,
    pub frames: Vec<Frame>,
    /// Compressed GIF source; playback owns a decoder and only a few frames.
    pub animation: Option<Arc<[u8]>>,
}

/// Artwork already filtered to fit a small display surface.
#[derive(Clone, Debug)]
pub(crate) struct Thumbnail {
    pub bytes: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Background {
    #[default]
    Checkerboard,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    /// None follows the viewport; a manual scale remains fixed across resizes.
    pub scale: Option<f32>,
    pub x: f32,
    pub y: f32,
    pub background: Background,
}
impl Default for Position {
    fn default() -> Self {
        Self {
            scale: None,
            x: 0.0,
            y: 0.0,
            background: Background::Checkerboard,
        }
    }
}
impl Position {
    pub fn scale_for(self, width: f32, height: f32, image: &Image) -> f32 {
        self.scale
            .unwrap_or_else(|| {
                ((width - 48.0).max(1.0) / image.width.max(1) as f32)
                    .min((height - 48.0).max(1.0) / image.height.max(1) as f32)
                    .min(1.0)
            })
            .clamp(0.00001, 16.0)
    }
    pub fn zoom(&mut self, scale: Option<f32>) {
        self.scale = scale.filter(|s| s.is_finite()).map(|s| s.clamp(0.05, 16.0));
        self.x = 0.0;
        self.y = 0.0;
    }
}
pub fn is_svg(path: &Path) -> bool {
    path.extension()
        .and_then(|v| v.to_str())
        .is_some_and(|v| v.eq_ignore_ascii_case("svg"))
}
pub fn is_raster(path: &Path) -> bool {
    path.extension().and_then(|v| v.to_str()).is_some_and(|v| {
        ["png", "jpg", "jpeg", "webp", "gif", "bmp", "ico"]
            .iter()
            .any(|ext| v.eq_ignore_ascii_case(ext))
    })
}
