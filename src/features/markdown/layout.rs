//! Native-text-independent layout planning. The host supplies whole-paragraph
//! shaping; words are never placed or shaped independently.
use crate::features::markdown::model::*;
use std::ops::Range;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct InlineStyle {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    /// Inline code in a page, drawn on a tinted chip to stand out from prose.
    pub chip: bool,
    pub heading: u8,
    pub link: Option<NodeId>,
    pub foreground: Option<u32>,
    /// Quieter text, such as the module path above a signature.
    pub muted: bool,
    /// Text inside a quotation: dimmer than body text, brighter than `muted`.
    pub quote: bool,
    /// Extra room after this glyph, where Han text meets Latin text or digits.
    pub spaced: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyleSpan {
    pub bytes: Range<usize>,
    pub style: InlineStyle,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScalarIndex(pub usize);
/// Built for one bounded paragraph; no global UTF-8/scalar/UTF-16 conflation.
#[derive(Clone, Debug)]
pub struct ParagraphIndex {
    bytes: Vec<usize>,
    utf16: Vec<usize>,
}
impl ParagraphIndex {
    pub fn new(text: &str) -> Self {
        let mut bytes = Vec::new();
        let mut utf16 = Vec::new();
        let mut units = 0;
        for (byte, c) in text.char_indices() {
            bytes.push(byte);
            utf16.push(units);
            units += c.len_utf16();
        }
        bytes.push(text.len());
        utf16.push(units);
        Self { bytes, utf16 }
    }
    pub fn scalar(&self, byte: usize) -> Option<ScalarIndex> {
        self.bytes.binary_search(&byte).ok().map(ScalarIndex)
    }
    pub fn byte(&self, scalar: ScalarIndex) -> Option<usize> {
        self.bytes.get(scalar.0).copied()
    }
    pub fn utf16(&self, scalar: ScalarIndex) -> Option<usize> {
        self.utf16.get(scalar.0).copied()
    }
    pub fn scalar_from_utf16(&self, units: usize) -> Option<ScalarIndex> {
        self.utf16.binary_search(&units).ok().map(ScalarIndex)
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.len() <= 1
    }
    pub fn len(&self) -> usize {
        self.bytes.len().saturating_sub(1)
    }
}
#[derive(Clone, Debug)]
pub struct LineSummary {
    pub scalars: Range<ScalarIndex>,
    pub top: f32,
    pub baseline: f32,
    pub height: f32,
    pub width: f32,
}
#[derive(Clone, Debug)]
pub struct ParagraphSummary {
    pub width: f32,
    pub height: f32,
    pub lines: Vec<LineSummary>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextFailure {
    Unsupported,
    ParagraphBudget,
    GeometryBudget,
    Cancelled,
}
pub struct ParagraphRequest<'a> {
    pub text: &'a str,
    pub spans: &'a [StyleSpan],
    pub width: f32,
    pub alignment: ColumnAlignment,
    pub wrap: bool,
    pub tab_columns: u8,
    pub font_generation: u64,
    pub scale: f32,
    pub line_height: f32,
}
#[derive(Clone, Copy, Debug)]
pub struct TextRect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}
#[derive(Clone, Debug)]
pub struct GraphemeGeometry {
    pub bytes: Range<usize>,
    pub rect: TextRect,
}
/// MD-024 contract: summary and geometry are separate work. A native adapter must
/// use the same styled paragraph for measure, paint and hit testing.
pub trait ParagraphBackend {
    fn summary(&mut self, request: &ParagraphRequest<'_>) -> Result<ParagraphSummary, TextFailure>;
    fn geometry(
        &mut self,
        request: &ParagraphRequest<'_>,
        scalars: Range<ScalarIndex>,
        limit: usize,
    ) -> Result<Vec<GraphemeGeometry>, TextFailure>;
}
#[derive(Clone, Copy, Debug)]
pub struct LayoutStyle {
    pub width: f32,
    pub line_height: f32,
    pub gap: f32,
    pub indent: f32,
    pub padding: f32,
    pub minimum_cell_width: f32,
    pub font_generation: u64,
    pub scale: f32,
    /// A hover or completion card: flat code blocks without copy buttons,
    /// compact headings and a tighter hairline.
    pub card: bool,
}
impl Default for LayoutStyle {
    fn default() -> Self {
        Self {
            width: 640.0,
            line_height: 22.0,
            gap: 12.0,
            indent: 24.0,
            padding: 12.0,
            minimum_cell_width: 96.0,
            font_generation: 0,
            scale: 1.0,
            card: false,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct LayoutLimits {
    pub paragraph_bytes: usize,
    pub paragraph_scalars: usize,
    pub paragraphs: usize,
    pub lines: usize,
    pub visible_geometry: usize,
}
impl Default for LayoutLimits {
    fn default() -> Self {
        Self {
            paragraph_bytes: 64 * 1024,
            paragraph_scalars: 32_768,
            paragraphs: 20_000,
            lines: 100_000,
            visible_geometry: 8192,
        }
    }
}
#[derive(Clone, Debug)]
pub struct LayoutFailure {
    pub node: NodeId,
    pub reason: TextFailure,
}
#[derive(Clone, Debug)]
pub struct PlacedParagraph {
    pub node: NodeId,
    pub display: DisplayRange,
    pub source: SourceRange,
    pub x: f32,
    pub y: f32,
    pub viewport_width: f32,
    pub wrap: bool,
    pub alignment: ColumnAlignment,
    pub spans: Vec<StyleSpan>,
    pub index: ParagraphIndex,
    pub summary: ParagraphSummary,
}
impl PlacedParagraph {
    pub fn request<'a>(
        &'a self,
        document: &'a MarkdownDocument,
        style: LayoutStyle,
    ) -> ParagraphRequest<'a> {
        ParagraphRequest {
            text: document.text(self.display.clone()).unwrap_or(""),
            spans: &self.spans,
            width: self.viewport_width,
            alignment: self.alignment,
            wrap: self.wrap,
            tab_columns: 4,
            font_generation: style.font_generation,
            scale: style.scale,
            line_height: style.line_height,
        }
    }
}
#[derive(Clone, Debug)]
pub enum Decoration {
    Fallback {
        node: NodeId,
        rect: TextRect,
    },
    Rule {
        y: f32,
        left: f32,
        right: f32,
    },
    Quote {
        rect: TextRect,
    },
    Code {
        node: NodeId,
        rect: TextRect,
        extent: f32,
    },
    ListMarker {
        node: NodeId,
        display: DisplayRange,
        x: f32,
        y: f32,
    },
    TableCell {
        node: NodeId,
        rect: TextRect,
        header: bool,
        /// Every second body row, tinted to guide the eye along wide tables.
        stripe: bool,
        first_column: bool,
        alignment: ColumnAlignment,
    },
}
#[derive(Clone, Debug)]
pub struct FlowBox {
    pub node: NodeId,
    pub source: SourceRange,
    pub top: f32,
    pub height: f32,
    pub paragraphs: Range<usize>,
}
#[derive(Clone, Copy, Debug)]
pub struct ImageSize {
    pub width: f32,
    pub height: f32,
}
/// What the host knows about an image. An image with no entry is still loading
/// and takes no room until it arrives.
#[derive(Clone, Copy, Debug)]
pub enum ImageState {
    Loaded(ImageSize),
    /// Could not be loaded; it keeps a box that shows the reason.
    Failed,
}
#[derive(Clone, Debug)]
pub struct ImageBox {
    pub node: NodeId,
    pub rect: TextRect,
    pub display: DisplayRange,
}
#[derive(Clone, Debug)]
pub struct ScrollBlock {
    pub node: NodeId,
    pub rect: TextRect,
    pub extent: f32,
}
#[derive(Clone, Debug)]
pub struct DocumentLayout {
    pub document: Arc<MarkdownDocument>,
    pub style: LayoutStyle,
    pub paragraphs: Vec<PlacedParagraph>,
    pub boxes: Vec<FlowBox>,
    pub decorations: Vec<Decoration>,
    pub images: Vec<ImageBox>,
    pub horizontal: Vec<ScrollBlock>,
    pub source_boxes: Vec<usize>,
    pub width: f32,
    pub height: f32,
}
impl DocumentLayout {
    pub fn scroll_container(&self, paragraph: usize) -> Option<NodeId> {
        self.node_scroll_container(self.paragraphs.get(paragraph)?.node)
    }
    pub fn node_scroll_container(&self, id: NodeId) -> Option<NodeId> {
        let mut node = Some(id);
        while let Some(id) = node {
            let value = self.document.node(id)?;
            if matches!(
                value.kind,
                NodeKind::CodeBlock { .. }
                    | NodeKind::Table { .. }
                    | NodeKind::Html { block: true }
            ) {
                return Some(id);
            }
            node = value.parent;
        }
        None
    }
    pub fn max_scroll(&self, node: NodeId) -> f32 {
        self.horizontal
            .iter()
            .find(|block| block.node == node)
            .map_or(0.0, |block| {
                (block.extent - (block.rect.right - block.rect.left)).max(0.0)
            })
    }

    pub fn visible_boxes(&self, top: f32, height: f32, overscan: f32) -> Range<usize> {
        let start = self
            .boxes
            .partition_point(|b| b.top + b.height <= (top - overscan).max(0.0));
        let end = self
            .boxes
            .partition_point(|b| b.top < top + height + overscan.max(0.0));
        start..end.max(start)
    }
    pub fn retained_bytes(&self) -> usize {
        self.paragraphs.capacity() * std::mem::size_of::<PlacedParagraph>()
            + self.boxes.capacity() * std::mem::size_of::<FlowBox>()
            + self.images.capacity() * std::mem::size_of::<ImageBox>()
            + self.horizontal.capacity() * std::mem::size_of::<ScrollBlock>()
            + self.source_boxes.capacity() * std::mem::size_of::<usize>()
            + self.decorations.capacity() * std::mem::size_of::<Decoration>()
            + self
                .paragraphs
                .iter()
                .map(|p| {
                    p.spans.capacity() * std::mem::size_of::<StyleSpan>()
                        + p.summary.lines.capacity() * std::mem::size_of::<LineSummary>()
                        + p.index.bytes.capacity() * std::mem::size_of::<usize>()
                        + p.index.utf16.capacity() * std::mem::size_of::<usize>()
                })
                .sum::<usize>()
    }
}
fn children_reversed(document: &MarkdownDocument, id: NodeId) -> Vec<NodeId> {
    let mut ids = document.children(id).collect::<Vec<_>>();
    ids.reverse();
    ids
}
/// The module path a hover puts above a signature, e.g. `std::process::Command`:
/// the first block, one line, no spaces.
fn is_path_block(document: &MarkdownDocument, id: NodeId) -> bool {
    let Some(node) = document.node(id) else {
        return false;
    };
    let text = document.text(node.display.clone()).unwrap_or("").trim();
    matches!(node.kind, NodeKind::CodeBlock { .. })
        && document.node(NodeId(0)).and_then(|root| root.first_child) == Some(id)
        && text.contains("::")
        && !text.chars().any(char::is_whitespace)
}
fn spans(
    document: &MarkdownDocument,
    id: NodeId,
    range: &DisplayRange,
    card: bool,
) -> Vec<StyleSpan> {
    let mut base = InlineStyle::default();
    let mut parent = document.node(id).and_then(|node| node.parent);
    while let Some(id) = parent {
        let node = &document.nodes[id.0];
        if matches!(node.kind, NodeKind::TableRow { header: true }) {
            base.bold = true;
        }
        // A page quotes in a quieter colour; a card keeps its own palette.
        if !card && matches!(node.kind, NodeKind::Quote) {
            base.quote = true;
        }
        parent = node.parent;
    }
    let mut spans = Vec::new();
    let mut pending = vec![(id, base)];
    while let Some((id, mut style)) = pending.pop() {
        let Some(node) = document.node(id) else {
            continue;
        };
        match node.kind {
            NodeKind::Heading { level } => {
                // Cards keep every heading just above body size.
                style.heading = if card { 4 } else { level };
                style.bold = true;
            }
            NodeKind::Strong => style.bold = true,
            NodeKind::Emphasis => style.italic = true,
            NodeKind::Strikethrough => style.strike = true,
            NodeKind::Code | NodeKind::CodeBlock { .. } | NodeKind::Html { block: true } => {
                style.code = true;
                style.chip = !card && matches!(node.kind, NodeKind::Code);
                style.muted = card && is_path_block(document, id);
            }
            NodeKind::Link { .. } => style.link = Some(id),
            _ => {}
        }
        if node.first_child.is_none() {
            let start = node.display.start.0.max(range.start.0);
            let end = node.display.end.0.min(range.end.0);
            if start < end {
                spans.push(StyleSpan {
                    bytes: start - range.start.0..end - range.start.0,
                    style,
                });
            }
        } else {
            for child in children_reversed(document, id) {
                pending.push((child, style));
            }
        }
    }
    if card {
        spans
    } else {
        separate_scripts(document.text(range.clone()).unwrap_or(""), spans)
    }
}

fn is_han(c: char) -> bool {
    matches!(c, '㐀'..='䶿' | '一'..='鿿' | '豈'..='﫿')
}
fn is_latin(c: char) -> bool {
    c.is_ascii_alphanumeric()
}
/// Opens a quarter-em gap where Han text meets Latin letters or digits, as
/// typesetters do between the scripts. Only the glyph before the boundary gets
/// extra advance, so the display text, offsets and selection are unchanged.
pub(crate) fn separate_scripts(text: &str, spans: Vec<StyleSpan>) -> Vec<StyleSpan> {
    let mut out = Vec::with_capacity(spans.len());
    for span in spans {
        let Some(slice) = text.get(span.bytes.clone()) else {
            out.push(span);
            continue;
        };
        if span.style.code {
            out.push(span);
            continue;
        }
        let mut chars = slice.char_indices().peekable();
        let mut from = 0;
        while let Some((at, current)) = chars.next() {
            let Some(&(_, next)) = chars.peek() else {
                break;
            };
            if (is_han(current) && is_latin(next)) || (is_latin(current) && is_han(next)) {
                let end = at + current.len_utf8();
                if from < at {
                    out.push(StyleSpan {
                        bytes: span.bytes.start + from..span.bytes.start + at,
                        style: span.style,
                    });
                }
                out.push(StyleSpan {
                    bytes: span.bytes.start + at..span.bytes.start + end,
                    style: InlineStyle {
                        spaced: true,
                        ..span.style
                    },
                });
                from = end;
            }
        }
        if from == 0 {
            out.push(span);
        } else if from < slice.len() {
            out.push(StyleSpan {
                bytes: span.bytes.start + from..span.bytes.end,
                style: span.style,
            });
        }
    }
    out
}
struct Placer<'a, B> {
    out: DocumentLayout,
    backend: &'a mut B,
    limits: LayoutLimits,
    images: &'a std::collections::HashMap<NodeId, ImageState>,
    lines: usize,
}
impl<B: ParagraphBackend> Placer<'_, B> {
    fn paragraph(
        &mut self,
        id: NodeId,
        range: DisplayRange,
        x: f32,
        y: f32,
        width: f32,
        wrap: bool,
        alignment: ColumnAlignment,
    ) -> Result<usize, LayoutFailure> {
        let fail = |reason| LayoutFailure { node: id, reason };
        let text = self.out.document.text(range.clone()).unwrap_or("");
        if text.len() > self.limits.paragraph_bytes
            || self.out.paragraphs.len() >= self.limits.paragraphs
        {
            return Err(fail(TextFailure::ParagraphBudget));
        }
        let index = ParagraphIndex::new(text);
        if index.len() > self.limits.paragraph_scalars {
            return Err(fail(TextFailure::ParagraphBudget));
        }
        let spans = spans(&self.out.document, id, &range, self.out.style.card);
        let style = self.out.style;
        let request = ParagraphRequest {
            text,
            spans: &spans,
            width: width.max(1.0),
            alignment,
            wrap,
            tab_columns: 4,
            font_generation: style.font_generation,
            scale: style.scale,
            line_height: style.line_height,
        };
        let summary = self.backend.summary(&request).map_err(fail)?;
        if !summary.width.is_finite()
            || !summary.height.is_finite()
            || summary.height < 0.0
            || summary.lines.iter().any(|l| {
                !l.top.is_finite()
                    || !l.height.is_finite()
                    || l.height <= 0.0
                    || l.scalars.end.0 > index.len()
            })
        {
            return Err(fail(TextFailure::Unsupported));
        }
        self.lines = self.lines.saturating_add(summary.lines.len());
        if self.lines > self.limits.lines {
            return Err(fail(TextFailure::ParagraphBudget));
        }
        let paragraph = self.out.paragraphs.len();
        self.out.paragraphs.push(PlacedParagraph {
            node: id,
            display: range.clone(),
            source: {
                let start = self
                    .out
                    .document
                    .source_at(range.start)
                    .map_or(self.out.document.nodes[id.0].source.start, |range| {
                        range.start
                    });
                let end_byte = self.out.document.display_text[..range.end.0]
                    .char_indices()
                    .next_back()
                    .map_or(range.start.0, |(byte, _)| byte);
                let end = self
                    .out
                    .document
                    .source_at(DisplayByte(end_byte))
                    .map_or(self.out.document.nodes[id.0].source.end, |range| range.end);
                start..end.max(start)
            },
            x,
            y,
            viewport_width: width.max(1.0),
            wrap,
            alignment,
            spans,
            index,
            summary,
        });
        Ok(paragraph)
    }
    /// A wrapped text run between images; returns the y below it.
    fn text_block(
        &mut self,
        id: NodeId,
        range: DisplayRange,
        x: f32,
        y: f32,
        width: f32,
        alignment: ColumnAlignment,
    ) -> Result<f32, LayoutFailure> {
        let begin = self.out.paragraphs.len();
        let index = self.paragraph(id, range, x, y, width, true, alignment)?;
        let height = self.out.paragraphs[index]
            .summary
            .height
            .max(self.out.style.line_height);
        self.out.boxes.push(FlowBox {
            node: id,
            source: self.out.paragraphs[index].source.clone(),
            top: y,
            height,
            paragraphs: begin..begin + 1,
        });
        Ok(y + height + self.out.style.gap)
    }
    /// Places `row` left to right, wrapping when a line is full, and aligns each
    /// line inside `width`. Returns the y below the last line and empties `row`.
    fn image_row(
        &mut self,
        row: &mut Vec<(NodeId, f32, f32, DisplayRange)>,
        x: f32,
        mut y: f32,
        width: f32,
        block_alignment: ColumnAlignment,
    ) -> f32 {
        const SPACING: f32 = 4.0;
        let mut start = 0;
        while start < row.len() {
            let mut end = start;
            let mut used = 0.0;
            while end < row.len() {
                let next = if end > start {
                    SPACING + row[end].1
                } else {
                    row[end].1
                };
                if end > start && used + next > width {
                    break;
                }
                used += next;
                end += 1;
            }
            let height = row[start..end].iter().map(|r| r.2).fold(0.0, f32::max);
            let alignment = row[start..end]
                .iter()
                .map(|r| self.out.document.alignment(r.0))
                .find(|a| !matches!(a, ColumnAlignment::Left | ColumnAlignment::None))
                .unwrap_or(block_alignment);
            let mut left = x + match alignment {
                ColumnAlignment::Center => ((width - used) / 2.0).max(0.0),
                ColumnAlignment::Right => (width - used).max(0.0),
                _ => 0.0,
            };
            for (image, w, h, range) in &row[start..end] {
                let at = self.out.paragraphs.len();
                self.out.images.push(ImageBox {
                    node: *image,
                    rect: TextRect {
                        left,
                        top: y,
                        right: left + w,
                        bottom: y + h,
                    },
                    display: range.clone(),
                });
                self.out.boxes.push(FlowBox {
                    node: *image,
                    source: self.out.document.nodes[image.0].source.clone(),
                    top: y,
                    height: *h,
                    paragraphs: at..at,
                });
                left += w + SPACING;
            }
            y += height + self.out.style.gap;
            start = end;
        }
        row.clear();
        y
    }
    fn table(&mut self, id: NodeId, x: f32, mut y: f32, width: f32) -> Result<f32, LayoutFailure> {
        let table_top = y;
        let columns = match &self.out.document.nodes[id.0].kind {
            NodeKind::Table { columns } => columns.clone(),
            _ => return Ok(y),
        };
        let count = columns.len().max(1);
        let cell_width = (width / count as f32).max(self.out.style.minimum_cell_width);
        let padding = self.out.style.padding;
        let rows = self.out.document.children(id).collect::<Vec<_>>();
        // Cells need less air above and below than beside their text.
        let pad_y = (padding * 0.6).round();
        let mut body_rows = 0usize;
        for row in rows {
            let begin = self.out.paragraphs.len();
            let mut height = self.out.style.line_height + pad_y * 2.0;
            let header = matches!(
                self.out.document.nodes[row.0].kind,
                NodeKind::TableRow { header: true }
            );
            let stripe = !header && body_rows % 2 == 1;
            body_rows += usize::from(!header);
            let cells = self.out.document.children(row).collect::<Vec<_>>();
            for (column, cell) in cells.into_iter().enumerate() {
                let range = self.out.document.nodes[cell.0].display.clone();
                let paragraph = self.paragraph(
                    cell,
                    range,
                    x + column as f32 * cell_width + padding,
                    y + pad_y,
                    cell_width - padding * 2.0,
                    true,
                    columns
                        .get(column)
                        .copied()
                        .unwrap_or(ColumnAlignment::None),
                )?;
                height = height.max(self.out.paragraphs[paragraph].summary.height + pad_y * 2.0);
                self.out.decorations.push(Decoration::TableCell {
                    node: cell,
                    rect: TextRect {
                        left: x + column as f32 * cell_width,
                        top: y,
                        right: x + (column + 1) as f32 * cell_width,
                        bottom: 0.0,
                    },
                    header,
                    stripe,
                    first_column: column == 0,
                    alignment: columns
                        .get(column)
                        .copied()
                        .unwrap_or(ColumnAlignment::None),
                });
            }
            for decoration in self.out.decorations.iter_mut().rev().take(count) {
                if let Decoration::TableCell { rect, .. } = decoration {
                    if rect.top == y {
                        rect.bottom = y + height;
                    }
                }
            }
            self.out.boxes.push(FlowBox {
                node: row,
                source: self.out.document.nodes[row.0].source.clone(),
                top: y,
                height,
                paragraphs: begin..self.out.paragraphs.len(),
            });
            y += height;
        }
        self.out.horizontal.push(ScrollBlock {
            node: id,
            rect: TextRect {
                left: x,
                top: table_top,
                right: x + width,
                bottom: y,
            },
            extent: count as f32 * cell_width,
        });
        self.out.width = self.out.width.max(x + count as f32 * cell_width);
        Ok(y + self.out.style.gap)
    }
}

/// Produces structural flow and native paragraph summaries. On any limit the
/// caller gets an explicit fallback outcome; the document is never truncated.
pub fn layout_document(
    document: Arc<MarkdownDocument>,
    style: LayoutStyle,
    limits: LayoutLimits,
    backend: &mut impl ParagraphBackend,
    cancelled: impl FnMut() -> bool,
) -> Result<DocumentLayout, LayoutFailure> {
    layout_with_images(
        document,
        style,
        limits,
        backend,
        cancelled,
        &std::collections::HashMap::new(),
    )
}

pub fn layout_with_images(
    document: Arc<MarkdownDocument>,
    style: LayoutStyle,
    limits: LayoutLimits,
    backend: &mut impl ParagraphBackend,
    mut cancelled: impl FnMut() -> bool,
    images: &std::collections::HashMap<NodeId, ImageState>,
) -> Result<DocumentLayout, LayoutFailure> {
    let mut style = style;
    style.width = style.width.max(1.0);
    style.line_height = style.line_height.max(1.0);
    if !style.width.is_finite() || !style.line_height.is_finite() || !style.scale.is_finite() {
        return Err(LayoutFailure {
            node: NodeId(0),
            reason: TextFailure::Unsupported,
        });
    }
    let mut placer = Placer {
        out: DocumentLayout {
            document,
            style,
            paragraphs: Vec::new(),
            boxes: Vec::new(),
            decorations: Vec::new(),
            images: Vec::new(),
            horizontal: Vec::new(),
            source_boxes: Vec::new(),
            width: style.width,
            height: 0.0,
        },
        backend,
        limits,
        images,
        lines: 0,
    };
    let mut pending = vec![(NodeId(0), 0usize, 0usize)];
    // A page keeps its first line close to the top edge; a card has no padding.
    let mut y = if style.card { style.padding } else { 4.0 };
    while let Some((id, indent, quote)) = pending.pop() {
        if cancelled() {
            return Err(LayoutFailure {
                node: id,
                reason: TextFailure::Cancelled,
            });
        }
        let node = placer.out.document.nodes[id.0].clone();
        let x = style.padding + (indent + quote) as f32 * style.indent;
        let width = (style.width - x - style.padding).max(1.0);
        // A single pathological block must not make the remaining document
        // disappear or enter unbounded native shaping. Keep its canonical text
        // for logical copy and provide an explicit, observable source fallback.
        let block_text = placer.out.document.text(node.display.clone()).unwrap_or("");
        let oversized = block_text.len() > limits.paragraph_bytes
            || block_text.chars().count() > limits.paragraph_scalars;
        let oversized_cell = matches!(node.kind, NodeKind::Table { .. })
            && placer.out.document.children(id).any(|row| {
                placer.out.document.children(row).any(|cell| {
                    let text = placer
                        .out
                        .document
                        .text(placer.out.document.nodes[cell.0].display.clone())
                        .unwrap_or("");
                    text.len() > limits.paragraph_bytes
                        || text.chars().count() > limits.paragraph_scalars
                })
            });
        let tight = matches!(node.kind, NodeKind::ListItem { .. })
            && placer
                .out
                .document
                .children(id)
                .all(|child| !placer.out.document.nodes[child.0].kind.is_block());
        if oversized_cell
            || (oversized
                && (tight
                    || matches!(
                        node.kind,
                        NodeKind::Paragraph
                            | NodeKind::Heading { .. }
                            | NodeKind::CodeBlock { .. }
                            | NodeKind::Html { block: true }
                            | NodeKind::Text
                    )))
        {
            let height = style.line_height * 3.0 + style.padding * 2.0;
            placer.out.decorations.push(Decoration::Fallback {
                node: id,
                rect: TextRect {
                    left: x,
                    top: y,
                    right: x + width,
                    bottom: y + height,
                },
            });
            placer.out.boxes.push(FlowBox {
                node: id,
                source: node.source,
                top: y,
                height,
                paragraphs: placer.out.paragraphs.len()..placer.out.paragraphs.len(),
            });
            y += height + style.gap;
            continue;
        }
        match node.kind {
            NodeKind::Table { .. } => {
                y = placer.table(id, x, y, width)?;
                continue;
            }
            NodeKind::ThematicBreak => {
                // A card keeps its hairline close: about ten pixels of air either side.
                let height = if style.card { 3.0 } else { style.line_height };
                placer.out.decorations.push(Decoration::Rule {
                    y: if style.card {
                        y + 2.0
                    } else {
                        y + style.line_height * 0.5
                    },
                    left: x,
                    right: x + width,
                });
                placer.out.boxes.push(FlowBox {
                    node: id,
                    source: node.source,
                    top: y,
                    height,
                    paragraphs: placer.out.paragraphs.len()..placer.out.paragraphs.len(),
                });
                y += height
                    + if style.card {
                        style.gap + 2.0
                    } else {
                        style.gap
                    };
                continue;
            }
            NodeKind::ListItem { .. } => {
                let first = node
                    .first_child
                    .and_then(|child| placer.out.document.node(child))
                    .map_or(node.display.end, |child| child.display.start);
                placer.out.decorations.push(Decoration::ListMarker {
                    node: id,
                    display: node.display.start..first,
                    x: (x - style.indent).max(style.padding),
                    y,
                });
            }
            _ => {}
        }
        let tight_item = matches!(node.kind, NodeKind::ListItem { .. })
            && placer
                .out
                .document
                .children(id)
                .all(|child| !placer.out.document.nodes[child.0].kind.is_block());
        let literal_root = matches!(node.kind, NodeKind::Text) && node.parent == Some(NodeId(0));
        let leaf_block = matches!(
            node.kind,
            NodeKind::Paragraph
                | NodeKind::Heading { .. }
                | NodeKind::CodeBlock { .. }
                | NodeKind::Html { block: true }
        ) || tight_item
            || literal_root;
        if leaf_block {
            let code = matches!(
                node.kind,
                NodeKind::CodeBlock { .. } | NodeKind::Html { block: true }
            );
            // A card draws code straight on its own surface, with no block around it.
            let boxed = code && !style.card;
            let pad = if boxed { style.padding } else { 0.0 };
            // Vertical padding is tighter than horizontal; the copy button floats over the corner and takes no height.
            let pad_y = if boxed { 10.0 } else { 0.0 };
            let heading = matches!(node.kind, NodeKind::Heading { .. });
            // A heading in a card sits closer to what it introduces than to what precedes it.
            if style.card && heading && y > 0.0 {
                y += 4.0;
            } else if heading && y > 0.0 {
                // A page heading opens a section: more air above than below,
                // growing with its rank.
                y += match node.kind {
                    NodeKind::Heading { level: 1 } => 18.0,
                    NodeKind::Heading { level: 2 } => 16.0,
                    NodeKind::Heading { level: 3 } => 10.0,
                    _ => 6.0,
                };
            }
            let start = placer.out.paragraphs.len();
            let mut range = node.display;
            if tight_item {
                range.start = node
                    .first_child
                    .and_then(|child| placer.out.document.node(child))
                    .map_or(range.end, |child| child.display.start);
            }
            // Images are flow objects. Split inline text at their canonical alt
            // ranges; copy/mapping remains in the unmodified logical document.
            let block_alignment = placer.out.document.alignment(id);
            let mut image_nodes = Vec::new();
            let mut visit = vec![id];
            while let Some(child) = visit.pop() {
                let item = &placer.out.document.nodes[child.0];
                if matches!(item.kind, NodeKind::Image { .. }) {
                    image_nodes.push(child);
                } else {
                    visit.extend(placer.out.document.children(child));
                }
            }
            image_nodes.sort_by_key(|image| placer.out.document.nodes[image.0].display.start);
            if !code && !image_nodes.is_empty() {
                let mut cursor = range.start;
                // Images with only white space between them share a line, like
                // inline images in a browser, and wrap when the line is full.
                let mut row: Vec<(NodeId, f32, f32, DisplayRange)> = Vec::new();
                for image in image_nodes {
                    let image_range = placer.out.document.nodes[image.0].display.clone();
                    if cursor < image_range.start
                        && placer
                            .out
                            .document
                            .text(cursor..image_range.start)
                            .is_some_and(|text| !text.trim().is_empty())
                    {
                        y = placer.image_row(&mut row, x, y, width, block_alignment);
                        y = placer.text_block(
                            id,
                            cursor..image_range.start,
                            x,
                            y,
                            width,
                            block_alignment,
                        )?;
                    }
                    let size = match placer.images.get(&image).copied() {
                        Some(ImageState::Loaded(size)) => size,
                        Some(ImageState::Failed) => ImageSize {
                            width: width.min(240.0),
                            height: 64.0,
                        },
                        None => {
                            // Still loading: no room is made for it. A zero-size
                            // box at its place in the flow lets the host see
                            // when it comes into view and start loading it.
                            let at = placer.out.paragraphs.len();
                            placer.out.images.push(ImageBox {
                                node: image,
                                rect: TextRect {
                                    left: x,
                                    top: y,
                                    right: x,
                                    bottom: y,
                                },
                                display: image_range.clone(),
                            });
                            placer.out.boxes.push(FlowBox {
                                node: image,
                                source: placer.out.document.nodes[image.0].source.clone(),
                                top: y,
                                height: 0.0,
                                paragraphs: at..at,
                            });
                            cursor = image_range.end;
                            continue;
                        }
                    };
                    let factor = (width / size.width.max(1.0)).min(1.0);
                    row.push((
                        image,
                        size.width.max(1.0) * factor,
                        size.height.max(1.0) * factor,
                        image_range.clone(),
                    ));
                    cursor = image_range.end;
                }
                y = placer.image_row(&mut row, x, y, width, block_alignment);
                if cursor < range.end
                    && placer
                        .out
                        .document
                        .text(cursor..range.end)
                        .is_some_and(|text| !text.trim().is_empty())
                {
                    y = placer.text_block(id, cursor..range.end, x, y, width, block_alignment)?;
                }
                continue;
            }
            let index = placer.paragraph(
                id,
                range,
                x + pad,
                y + pad_y,
                (width - pad * 2.0).max(1.0),
                !code,
                block_alignment,
            )?;
            let summary = &placer.out.paragraphs[index].summary;
            // A fence's closing newline shapes an empty last line; it is not content.
            let content = match summary.lines.last() {
                Some(last) if code && summary.lines.len() > 1 && last.width <= 0.0 => last.top,
                _ => summary.height,
            };
            let mut height = content.max(style.line_height) + pad_y * 2.0;
            // Level 1 and 2 headings carry a hairline underneath, as on GitHub.
            if !style.card && matches!(node.kind, NodeKind::Heading { level: 1 | 2 }) {
                placer.out.decorations.push(Decoration::Rule {
                    y: y + height + 3.0,
                    left: x,
                    right: x + width,
                });
                height += 8.0;
            }
            if code {
                placer.out.horizontal.push(ScrollBlock {
                    node: id,
                    rect: TextRect {
                        left: x,
                        top: y,
                        right: x + width,
                        bottom: y + height,
                    },
                    extent: placer.out.paragraphs[index].summary.width + pad * 2.0,
                });
                if !style.card {
                    placer.out.decorations.push(Decoration::Code {
                        node: id,
                        rect: TextRect {
                            left: x,
                            top: y,
                            right: x + width,
                            bottom: y + height,
                        },
                        extent: placer.out.paragraphs[index].summary.width + pad * 2.0,
                    });
                }
            }
            if quote > 0 {
                placer.out.decorations.push(Decoration::Quote {
                    rect: TextRect {
                        left: x - style.indent * 0.5,
                        top: y,
                        right: x - style.indent * 0.5 + 3.0,
                        bottom: y + height,
                    },
                });
            }
            placer.out.boxes.push(FlowBox {
                node: id,
                source: placer.out.paragraphs[index].source.clone(),
                top: y,
                height,
                paragraphs: start..placer.out.paragraphs.len(),
            });
            y += height
                + if style.card && heading {
                    style.gap - 2.0
                } else {
                    style.gap
                };
        } else {
            let indent = indent + usize::from(matches!(node.kind, NodeKind::List { .. }));
            let quote = quote + usize::from(matches!(node.kind, NodeKind::Quote));
            for child in children_reversed(&placer.out.document, id) {
                pending.push((child, indent, quote));
            }
        }
    }
    // The first block also owns the room above it. A reading position inside
    // that room then maps back to itself, instead of snapping to the block's
    // text and hiding the margin.
    if let Some(first) = placer
        .out
        .boxes
        .iter_mut()
        .min_by(|a, b| a.top.total_cmp(&b.top))
    {
        first.height += first.top;
        first.top = 0.0;
    }
    placer.out.source_boxes = (0..placer.out.boxes.len()).collect();
    placer.out.source_boxes.sort_by_key(|index| {
        (
            placer.out.boxes[*index].source.start,
            placer.out.boxes[*index].source.end,
        )
    });
    placer.out.height = y + style.padding;
    Ok(placer.out)
}
