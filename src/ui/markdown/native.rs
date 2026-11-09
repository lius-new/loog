//! Project-owned native paragraph adapter. Skia stays behind the reader port.
//! Paragraphs never cross threads: a layout worker owns this bounded cache.
use crate::features::markdown::layout::*;
use crate::features::markdown::model::ColumnAlignment;
use skia_safe::{
    self as sk,
    textlayout::{
        FontCollection, Paragraph, ParagraphBuilder, ParagraphStyle, PlaceholderAlignment,
        PlaceholderStyle, RectHeightStyle, RectWidthStyle, TextAlign, TextBaseline, TextDecoration,
        TextStyle,
    },
};
use std::{collections::VecDeque, ops::Range, sync::Arc};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const FAMILIES: &[&str] = crate::ui::fonts::SANS_FAMILIES;
const MONO: &[&str] = &[
    "Cascadia Mono",
    "Consolas",
    "Menlo",
    "DejaVu Sans Mono",
    "monospace",
];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ink {
    pub rendering: lgui::text::TextRenderingOptions,
    pub text: u32,
    pub link: u32,
    pub code: u32,
    pub muted: u32,
    pub quote: u32,
    /// Fill behind inline code.
    pub chip: u32,
    pub syntax: [u32; crate::features::highlighting::HighlightId::COUNT],
}
impl Default for Ink {
    fn default() -> Self {
        Self {
            rendering: crate::ui::text_rendering::defaults(),
            text: 0xffd4d4d4,
            link: 0xff70a5ff,
            code: 0xffd4d4d4,
            muted: 0xff9a9a9a,
            quote: 0xffb4b4b4,
            chip: 0xff2a2a2a,
            syntax: [0xffd4d4d4; crate::features::highlighting::HighlightId::COUNT],
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
struct Key {
    text: String,
    spans: Vec<StyleSpan>,
    width: f32,
    wrap: bool,
    alignment: ColumnAlignment,
    tabs: u8,
    font: u64,
    scale: f32,
    line: f32,
    ink: Ink,
}
/// Halfway between two `0xRRGGBB` colours, fully opaque.
fn mix(a: u32, b: u32) -> u32 {
    let channel = |shift: u32| (((a >> shift) & 0xff) + ((b >> shift) & 0xff)) / 2;
    0xff000000 | channel(16) << 16 | channel(8) << 8 | channel(0)
}
impl Ink {
    pub fn current() -> Self {
        let mut out = Self {
            rendering: crate::ui::theme::text_rendering(crate::ui::theme::current()),
            text: 0xff000000 | crate::ui::theme::c().text.0,
            link: 0xff000000 | crate::ui::theme::c().accent.0,
            code: 0xff000000 | crate::ui::theme::c().text_bright.0,
            muted: 0xff000000 | crate::ui::theme::c().text_dim.0,
            quote: mix(
                crate::ui::theme::c().text.0,
                crate::ui::theme::c().text_dim.0,
            ),
            chip: 0xff000000 | crate::ui::theme::c().active_line.0,
            ..Default::default()
        };
        for class in crate::features::highlighting::HighlightId::all() {
            out.syntax[class.index()] = 0xff000000 | crate::ui::theme::syntax_color(class).0;
        }
        out
    }
}
impl Key {
    fn new(r: &ParagraphRequest<'_>, ink: Ink) -> Self {
        Self {
            text: r.text.to_owned(),
            spans: r.spans.to_vec(),
            width: r.width,
            wrap: r.wrap,
            alignment: r.alignment,
            tabs: r.tab_columns,
            font: r.font_generation,
            scale: r.scale,
            line: r.line_height,
            ink,
        }
    }
}
struct Cached {
    key: Key,
    paragraph: Paragraph,
    summary: ParagraphSummary,
    bytes: usize,
}
struct CachedTile {
    key: Key,
    bounds: [u32; 5],
    png: std::sync::Weak<Vec<u8>>,
    bytes: usize,
}
pub struct NativeBackend {
    fonts: FontCollection,
    cache: VecDeque<Cached>,
    tiles: VecDeque<CachedTile>,
    tile_bytes: usize,
    spaces: VecDeque<(u64, u32, f32)>,
    bytes: usize,
    pub ink: Ink,
}
impl Default for NativeBackend {
    fn default() -> Self {
        let mut fonts = FontCollection::new();
        fonts.set_default_font_manager(sk::FontMgr::default(), None);
        Self {
            fonts,
            cache: VecDeque::new(),
            tiles: VecDeque::new(),
            tile_bytes: 0,
            spaces: VecDeque::new(),
            bytes: 0,
            ink: Ink::default(),
        }
    }
}
fn text_style(style: InlineStyle, r: &ParagraphRequest<'_>, ink: Ink) -> TextStyle {
    let mut out = TextStyle::new();
    // These paragraphs are painted into transparent tiles. Use the same font
    // policy as the screen renderer, with its transparent-target fallback.
    lgui::render_skia::TextRasterization::Grayscale.configure_with_options(&mut out, ink.rendering);
    let size = if style.heading > 0 {
        match style.heading {
            1 => 32.0,
            2 => 26.0,
            3 => 21.0,
            4 => 18.0,
            _ => 16.0,
        }
    } else {
        16.0
    };
    out.set_font_size(size * r.scale)
        .set_font_families(if style.code { MONO } else { FAMILIES })
        .set_color(sk::Color::from(style.foreground.unwrap_or_else(|| {
            if style.link.is_some() {
                ink.link
            } else if style.muted {
                ink.muted
            } else if style.quote {
                ink.quote
            } else if style.code {
                ink.code
            } else {
                ink.text
            }
        })));
    if style.chip {
        let mut background = sk::Paint::default();
        background.set_color(sk::Color::from(ink.chip));
        out.set_background_paint(&background);
    }
    if style.spaced {
        out.set_letter_spacing(size * r.scale * 0.25);
    }
    out.set_font_style(sk::FontStyle::new(
        if style.bold {
            sk::font_style::Weight::BOLD
        } else {
            sk::font_style::Weight::NORMAL
        },
        sk::font_style::Width::NORMAL,
        if style.italic {
            sk::font_style::Slant::Italic
        } else {
            sk::font_style::Slant::Upright
        },
    ));
    // Without the override Skia ignores the multiplier and uses each font's own
    // spacing, so the line height setting would have no effect. A large
    // heading keeps a tighter multiplier than body text.
    let multiplier = r.line_height / (16.0 * r.scale).max(1.0);
    out.set_height(if style.heading > 0 {
        multiplier.min(1.35)
    } else {
        multiplier
    })
    .set_height_override(true);
    let mut decoration = TextDecoration::NO_DECORATION;
    if style.strike {
        decoration |= TextDecoration::LINE_THROUGH;
    }
    if style.link.is_some() {
        decoration |= TextDecoration::UNDERLINE;
    }
    out.set_decoration_type(decoration);
    out
}
impl NativeBackend {
    fn tab_space(&mut self, r: &ParagraphRequest<'_>) -> Result<f32, TextFailure> {
        if !r.text.contains('\t') {
            return Ok(0.0);
        }
        if let Some((_, _, space)) = self
            .spaces
            .iter()
            .find(|(font, scale, _)| *font == r.font_generation && *scale == r.scale.to_bits())
        {
            return Ok(*space);
        }
        let manager = sk::FontMgr::default();
        let face = MONO
            .iter()
            .find_map(|family| manager.match_family_style(family, sk::FontStyle::normal()))
            .ok_or(TextFailure::Unsupported)?;
        let space = sk::Font::new(face, 16.0 * r.scale).measure_str(" ", None).0;
        if !space.is_finite() || space <= 0.0 {
            return Err(TextFailure::Unsupported);
        }
        if self.spaces.len() >= 8 {
            self.spaces.pop_front();
        }
        self.spaces
            .push_back((r.font_generation, r.scale.to_bits(), space));
        Ok(space)
    }
    fn get(&mut self, r: &ParagraphRequest<'_>) -> Result<&Cached, TextFailure> {
        if !r.width.is_finite()
            || r.width <= 0.0
            || !r.scale.is_finite()
            || r.scale <= 0.0
            || r.text.len() > 16 * 1024
        {
            return Err(TextFailure::ParagraphBudget);
        }
        let key = Key::new(r, self.ink);
        if let Some(index) = self.cache.iter().position(|entry| entry.key == key) {
            let entry = self.cache.remove(index).unwrap();
            self.cache.push_back(entry);
            return Ok(self.cache.back().unwrap());
        }
        let mut style = ParagraphStyle::new();
        style.set_text_style(&text_style(InlineStyle::default(), r, self.ink));
        style.set_text_align(match r.alignment {
            ColumnAlignment::Center => TextAlign::Center,
            ColumnAlignment::Right => TextAlign::Right,
            _ => TextAlign::Left,
        });
        let mut builder = ParagraphBuilder::new(&style, self.fonts.clone());
        // The parser supplies disjoint, ordered leaf spans. Fill synthetic gaps with body style.
        let mut offset = 0;
        let mut column = 0usize;
        let space = self.tab_space(r)?;
        for span in r.spans {
            if span.bytes.start < offset
                || span.bytes.end > r.text.len()
                || !r.text.is_char_boundary(span.bytes.start)
                || !r.text.is_char_boundary(span.bytes.end)
            {
                return Err(TextFailure::Unsupported);
            }
            if offset < span.bytes.start {
                add_text(
                    &mut builder,
                    &r.text[offset..span.bytes.start],
                    r.tab_columns,
                    &mut column,
                    space,
                    r.line_height,
                );
            }
            builder.push_style(&text_style(span.style, r, self.ink));
            add_text(
                &mut builder,
                &r.text[span.bytes.clone()],
                r.tab_columns,
                &mut column,
                space,
                r.line_height,
            );
            builder.pop();
            offset = span.bytes.end;
        }
        if offset < r.text.len() {
            add_text(
                &mut builder,
                &r.text[offset..],
                r.tab_columns,
                &mut column,
                space,
                r.line_height,
            );
        }
        let mut paragraph = builder.build();
        paragraph.layout(if r.wrap { r.width } else { 1_000_000.0 });
        // SkParagraph line indexes and getRectsForRange use UTF-16 units.
        let index = ParagraphIndex::new(r.text);
        let lines = paragraph
            .get_line_metrics()
            .iter()
            .map(|line| LineSummary {
                scalars: index
                    .scalar_from_utf16(line.start_index)
                    .unwrap_or(ScalarIndex(0))
                    ..index
                        .scalar_from_utf16(line.end_including_newline)
                        .unwrap_or(ScalarIndex(index.len())),
                top: (line.baseline - line.ascent) as f32,
                baseline: line.baseline as f32,
                height: line.height as f32,
                width: line.width as f32,
            })
            .collect::<Vec<_>>();
        let summary = ParagraphSummary {
            width: paragraph.longest_line(),
            height: paragraph.height(),
            lines,
        };
        let bytes = r.text.len() * 32 + summary.lines.len() * 128;
        while self.cache.len() >= 64 || self.bytes + bytes > 12 * 1024 * 1024 {
            let Some(old) = self.cache.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(old.bytes);
        }
        self.bytes += bytes;
        self.cache.push_back(Cached {
            key,
            paragraph,
            summary,
            bytes,
        });
        Ok(self.cache.back().unwrap())
    }
    /// Rasterize a clipped, transparent paragraph tile; it uses precisely the same
    /// native paragraph as summary/geometry. No shaping occurs on the UI thread.
    pub fn tile(
        &mut self,
        r: &ParagraphRequest<'_>,
        left: f32,
        top: f32,
        width: f32,
        height: f32,
        density: f32,
    ) -> Result<Arc<Vec<u8>>, TextFailure> {
        if !density.is_finite() || density <= 0.0 {
            return Err(TextFailure::Unsupported);
        }
        let tile_key = Key::new(r, self.ink);
        let bounds = [
            left.to_bits(),
            top.to_bits(),
            width.to_bits(),
            height.to_bits(),
            density.to_bits(),
        ];
        if let Some(index) = self.tiles.iter().position(|tile| {
            tile.bounds == bounds && tile.key == tile_key && tile.png.strong_count() > 0
        }) {
            let tile = self.tiles.remove(index).unwrap();
            let png = tile.png.upgrade().unwrap();
            self.tiles.push_back(tile);
            return Ok(png);
        }
        let w = (width * density).ceil() as i32;
        let h = (height * density).ceil() as i32;
        if w <= 0 || h <= 0 || (w as usize).saturating_mul(h as usize) > 4 * 1024 * 1024 {
            return Err(TextFailure::GeometryBudget);
        }
        let props = lgui::render_skia::TextRasterization::Grayscale
            .surface_props_with_options(self.ink.rendering);
        let cached = self.get(r)?;
        let info = sk::ImageInfo::new_n32_premul((w, h), None);
        let mut surface =
            sk::surfaces::raster(&info, None, Some(&props)).ok_or(TextFailure::Unsupported)?;
        let canvas = surface.canvas();
        canvas.clear(sk::Color::TRANSPARENT);
        // Rasterize directly at the requested physical density, including <1.
        // The UI presents this tile at exactly its native pixel dimensions.
        canvas.scale((density, density));
        canvas.translate((-left, -top));
        cached.paragraph.paint(canvas, (0.0, 0.0));
        let data = surface
            .image_snapshot()
            .encode(None, sk::EncodedImageFormat::PNG, None)
            .ok_or(TextFailure::Unsupported)?;
        let png = Arc::new(data.as_bytes().to_vec());
        let bytes = tile_key.text.capacity()
            + tile_key.spans.capacity() * std::mem::size_of::<StyleSpan>()
            + std::mem::size_of::<CachedTile>();
        while self.tiles.len() >= 128 || self.tile_bytes + bytes > 2 * 1024 * 1024 {
            let Some(old) = self.tiles.pop_front() else {
                break;
            };
            self.tile_bytes = self.tile_bytes.saturating_sub(old.bytes);
        }
        if bytes <= 2 * 1024 * 1024 {
            self.tiles.push_back(CachedTile {
                key: tile_key,
                bounds,
                png: Arc::downgrade(&png),
                bytes,
            });
            self.tile_bytes += bytes;
        }
        Ok(png)
    }
    pub fn visible_scalars(
        &mut self,
        r: &ParagraphRequest<'_>,
        left: f32,
        right: f32,
        y: f32,
    ) -> Result<Range<ScalarIndex>, TextFailure> {
        let index = ParagraphIndex::new(r.text);
        let paragraph = &self.get(r)?.paragraph;
        let a = paragraph
            .get_glyph_position_at_coordinate((left, y))
            .position as usize;
        let b = paragraph
            .get_glyph_position_at_coordinate((right, y))
            .position as usize;
        let a = index.scalar_from_utf16(a).unwrap_or(ScalarIndex(0)).0;
        let b = index
            .scalar_from_utf16(b)
            .unwrap_or(ScalarIndex(index.len()))
            .0;
        let start = a.min(b).saturating_sub(2);
        let end = (a.max(b) + 2).min(index.len());
        let start = crate::features::markdown::reader::grapheme_floor(
            r.text,
            index.byte(ScalarIndex(start)).unwrap(),
        );
        let byte = index.byte(ScalarIndex(end)).unwrap();
        let floor = crate::features::markdown::reader::grapheme_floor(r.text, byte);
        let end = if floor < byte {
            floor + r.text[floor..].graphemes(true).next().map_or(0, str::len)
        } else {
            floor
        };
        Ok(index.scalar(start).unwrap()..index.scalar(end).unwrap())
    }
    pub fn clear(&mut self) {
        self.cache.clear();
        self.tiles.clear();
        self.tile_bytes = 0;
        self.spaces.clear();
        self.bytes = 0;
    }
}
// One placeholder replaces each tab: both occupy exactly one UTF-16 unit.
// Width advances to measured monospace tab stops while the model keeps '\t'.
fn add_text(
    builder: &mut ParagraphBuilder,
    text: &str,
    columns: u8,
    column: &mut usize,
    space: f32,
    line_height: f32,
) {
    if space == 0.0 {
        builder.add_text(text);
        return;
    }
    let mut start = 0;
    for (byte, c) in text.grapheme_indices(true) {
        if c == "\t" {
            if start < byte {
                builder.add_text(&text[start..byte]);
            }
            let advance = columns.max(1) as usize - *column % columns.max(1) as usize;
            builder.add_placeholder(&PlaceholderStyle::new(
                space * advance as f32,
                line_height,
                PlaceholderAlignment::Baseline,
                TextBaseline::Alphabetic,
                line_height * 0.8,
            ));
            *column += advance;
            start = byte + 1;
        } else if c == "\n" || c == "\r" || c == "\r\n" {
            *column = 0;
        } else {
            *column += UnicodeWidthStr::width(c);
        }
    }
    if start < text.len() {
        builder.add_text(&text[start..]);
    }
}
impl ParagraphBackend for NativeBackend {
    fn summary(&mut self, r: &ParagraphRequest<'_>) -> Result<ParagraphSummary, TextFailure> {
        Ok(self.get(r)?.summary.clone())
    }
    fn geometry(
        &mut self,
        r: &ParagraphRequest<'_>,
        scalars: Range<ScalarIndex>,
        limit: usize,
    ) -> Result<Vec<GraphemeGeometry>, TextFailure> {
        let index = ParagraphIndex::new(r.text);
        let start = index.byte(scalars.start).ok_or(TextFailure::Unsupported)?;
        let end = index.byte(scalars.end).ok_or(TextFailure::Unsupported)?;
        if start > end || scalars.end.0 - scalars.start.0 > 512 {
            return Err(TextFailure::GeometryBudget);
        }
        let cached = self.get(r)?;
        let mut out = Vec::new();
        for (byte, grapheme) in r.text[start..end].grapheme_indices(true) {
            if out.len() >= limit {
                return Err(TextFailure::GeometryBudget);
            }
            let a = start + byte;
            let b = a + grapheme.len();
            let ua = index
                .utf16(index.scalar(a).ok_or(TextFailure::Unsupported)?)
                .unwrap();
            let ub = index
                .utf16(index.scalar(b).ok_or(TextFailure::Unsupported)?)
                .unwrap();
            for rect in cached.paragraph.get_rects_for_range(
                ua..ub,
                RectHeightStyle::Max,
                RectWidthStyle::Tight,
            ) {
                out.push(GraphemeGeometry {
                    bytes: a..b,
                    rect: TextRect {
                        left: rect.rect.left,
                        top: rect.rect.top,
                        right: rect.rect.right,
                        bottom: rect.rect.bottom,
                    },
                });
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::markdown::{
        model::{ContentFormat, ParseLimits, SyntaxOptions},
        parser::parse_document,
    };
    #[test]
    fn native_shaping_wraps_chinese_and_maps_utf16_geometry() {
        let doc=Arc::new(parse_document(Arc::new("# 中文标题\n\n连续中文没有空格也应折行 English e\u{301} 😀 **bold** *italic* [link](https://example.com).".replace("\\u{301}","\u{301}")),ContentFormat::Markdown,SyntaxOptions::default(),ParseLimits::automatic(),||false).unwrap());
        let mut backend = NativeBackend::default();
        let narrow = layout_document(
            doc.clone(),
            LayoutStyle {
                width: 180.0,
                ..Default::default()
            },
            LayoutLimits::default(),
            &mut backend,
            || false,
        )
        .unwrap();
        let wide = layout_document(
            doc,
            LayoutStyle {
                width: 600.0,
                ..Default::default()
            },
            LayoutLimits::default(),
            &mut backend,
            || false,
        )
        .unwrap();
        assert!(narrow.height > wide.height);
        for p in &narrow.paragraphs {
            let request = p.request(&narrow.document, narrow.style);
            let glyphs = backend
                .geometry(&request, ScalarIndex(0)..ScalarIndex(p.index.len()), 512)
                .unwrap();
            assert!(!glyphs.is_empty());
            assert!(
                glyphs
                    .iter()
                    .all(|g| request.text.is_char_boundary(g.bytes.start)
                        && request.text.is_char_boundary(g.bytes.end))
            );
            let tile = backend
                .tile(
                    &request,
                    0.0,
                    0.0,
                    request.width,
                    p.summary.height.min(256.0),
                    1.0,
                )
                .unwrap();
            assert_eq!(&tile[..8], b"\x89PNG\r\n\x1a\n");
        }
    }
}

#[cfg(test)]
mod coordinate_tests {
    use super::*;
    #[test]
    fn application_raster_profile_invalidates_tiles_without_changing_layout() {
        let request = ParagraphRequest {
            text: "English 中文 00 0689",
            spans: &[],
            width: 320.0,
            alignment: ColumnAlignment::Left,
            wrap: true,
            tab_columns: 4,
            font_generation: 0,
            scale: 1.0,
            line_height: 22.0,
        };
        let mut native = NativeBackend::default();
        let before = native.summary(&request).unwrap();
        let a = native
            .tile(&request, 0.0, 0.0, 320.0, before.height, 0.8)
            .unwrap();
        native.ink.rendering = native.ink.rendering.with_grayscale_gamma(1.2).unwrap();
        let after = native.summary(&request).unwrap();
        let b = native
            .tile(&request, 0.0, 0.0, 320.0, after.height, 0.8)
            .unwrap();
        assert_eq!(before.width, after.width);
        assert_eq!(before.height, after.height);
        assert_ne!(a.as_slice(), b.as_slice());
        let cached = native
            .tile(&request, 0.0, 0.0, 320.0, after.height, 0.8)
            .unwrap();
        assert!(Arc::ptr_eq(&b, &cached));
    }
    #[test]
    fn tabs_surrogates_and_far_horizontal_geometry_use_one_native_paragraph() {
        let text = format!("中\t😀 e\u{301} {}END", "x".repeat(2000));
        let spans = [StyleSpan {
            bytes: 0..text.len(),
            style: InlineStyle {
                code: true,
                ..Default::default()
            },
        }];
        let request = ParagraphRequest {
            text: &text,
            spans: &spans,
            width: 260.0,
            alignment: ColumnAlignment::Left,
            wrap: false,
            tab_columns: 4,
            font_generation: 0,
            scale: 1.0,
            line_height: 22.0,
        };
        let mut native = NativeBackend::default();
        let summary = native.summary(&request).unwrap();
        assert!(summary.width > 10_000.0);
        let visible = native
            .visible_scalars(&request, summary.width - 200.0, summary.width, 12.0)
            .unwrap();
        assert!(visible.start.0 > 1500);
        let glyphs = native.geometry(&request, visible, 512).unwrap();
        assert!(glyphs.iter().any(|g| text[g.bytes.clone()].contains('D')));
        let prefix = native
            .geometry(&request, ScalarIndex(0)..ScalarIndex(8), 512)
            .unwrap();
        let emoji = prefix
            .iter()
            .find(|g| &text[g.bytes.clone()] == "😀")
            .unwrap();
        assert!(emoji.rect.left > 15.0);
        let a = native
            .tile(&request, 0.0, 0.0, 260.0, summary.height, 1.0)
            .unwrap();
        let b = native
            .tile(&request, 0.0, 0.0, 260.0, summary.height, 1.0)
            .unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        // The 80% interface scale must not rasterize at 100% and shrink later.
        for density in [0.8, 1.25, 1.5] {
            let tile = native
                .tile(&request, 0.0, 0.0, 260.0, summary.height, density)
                .unwrap();
            let decoded = sk::Image::from_encoded(sk::Data::new_copy(tile.as_slice())).unwrap();
            assert_eq!(decoded.width(), (260.0 * density).ceil() as i32);
            assert_eq!(decoded.height(), (summary.height * density).ceil() as i32);
            assert!(!Arc::ptr_eq(&a, &tile));
        }
    }
}
