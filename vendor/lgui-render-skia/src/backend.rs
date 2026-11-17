use std::{
    cell::RefCell,
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use lgui_assets::backend::render_resources;
use lgui_core::core::{
    BlurEdgeMode, BlurStyle, Color, CompositingLayerBackground, ImageFit, LayerTransform,
    PathStyle, PhysicalRect, RasterCachePolicy, Scene, ScenePrimitive, StaticLayerBackground,
    StaticLayerSource, Stroke, TextAlign, TextStyle, UiImageSource, UiPath, UiPathCommand, UiRect,
    VisualStyle,
};
use lgui_render_api::{FrameInfo, GraphicsPreference, MemoryPressure};
use skia_safe::textlayout::{
    FontCollection, Paragraph, ParagraphBuilder, ParagraphStyle, RectHeightStyle, RectWidthStyle,
    TextAlign as SkTextAlign, TextDirection as SkTextDirection, TextStyle as SkTextStyle,
    TypefaceFontProvider,
};
use skia_safe::{
    AlphaType, BlendMode, Canvas, Color as SkColor, Color4f, ColorType, Data, FontMgr, FontStyle,
    Image, ImageInfo, Paint, PaintStyle, Path, PathBuilder, RRect, Rect, SamplingOptions, Surface,
    TileMode, surfaces,
};
use unicode_bidi::BidiInfo;
use unicode_segmentation::UnicodeSegmentation;

mod cache;
mod painter;
mod primitives;
mod rasterization;
mod software;
mod support;
mod text;

pub use cache::{SkiaCache, SkiaCacheStats};
pub use rasterization::TextRasterization;
pub use software::SkiaSoftwareSurface;
#[cfg(any(
    feature = "renderer-skia-gl",
    feature = "renderer-skia-vulkan",
    feature = "renderer-skia-metal"
))]
pub use software::paint_scene_damage;
pub use support::probe_skia_support;
#[cfg(any(
    feature = "renderer-skia-gl",
    feature = "renderer-skia-vulkan",
    feature = "renderer-skia-metal"
))]
pub use support::{cpu_cache_budget, gpu_cache_budget, with_gpu_cache_usage};
pub use text::skia_text_system_handle;

use painter::*;
use primitives::*;
use text::*;

#[path = "backend/backend_test.rs"]
#[cfg(test)]
mod tests;

#[path = "backend/rasterization_test.rs"]
#[cfg(test)]
mod rasterization_tests;

#[path = "backend/sampling_test.rs"]
#[cfg(test)]
mod sampling_tests;
