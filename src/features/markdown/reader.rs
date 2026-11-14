//! Controlled logical reading state. Copying and selection do not depend on the
//! elements painted in the current viewport.
use crate::features::markdown::layout::{DocumentLayout, GraphemeGeometry};
use crate::features::markdown::model::*;
use std::ops::Range;
use std::sync::Arc;
use unicode_segmentation::GraphemeCursor;

pub fn grapheme_floor(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    let mut cursor = GraphemeCursor::new(at, text.len(), true);
    if cursor.is_boundary(text, 0).unwrap_or(false) {
        at
    } else {
        cursor.prev_boundary(text, 0).ok().flatten().unwrap_or(0)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogicalSelection {
    pub revision: u64,
    pub anchor: DisplayByte,
    pub head: DisplayByte,
}
impl LogicalSelection {
    pub fn range(self, document: &MarkdownDocument, revision: u64) -> Option<DisplayRange> {
        if self.revision != revision {
            return None;
        }
        let a = DisplayByte(grapheme_floor(&document.display_text, self.anchor.0));
        let b = DisplayByte(grapheme_floor(&document.display_text, self.head.0));
        Some(a.min(b)..a.max(b))
    }
    pub fn copy(self, document: &MarkdownDocument, revision: u64) -> Option<&str> {
        document.text(self.range(document, revision)?)
    }
    pub fn rebase(
        self,
        previous: &MarkdownDocument,
        next: &MarkdownDocument,
        revision: u64,
        mut map_source: impl FnMut(SourceByte, bool) -> Option<SourceByte>,
    ) -> Option<Self> {
        let anchor = map_source(previous.source_at(self.anchor)?.start, false)?;
        let head = map_source(previous.source_at(self.head)?.start, true)?;
        Some(Self {
            revision,
            anchor: next.display_at(anchor, false)?,
            head: next.display_at(head, true)?,
        })
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ReadingAnchor {
    pub source: SourceByte,
    pub fraction: f32,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub top: f32,
    pub height: f32,
}
impl Viewport {
    pub fn new(height: f32) -> Self {
        Self {
            top: 0.0,
            height: if height.is_finite() {
                height.max(1.0)
            } else {
                1.0
            },
        }
    }
    pub fn scroll_to(&mut self, layout: &DocumentLayout, top: f32) {
        self.top = if top.is_finite() {
            top.clamp(0.0, (layout.height - self.height).max(0.0))
        } else {
            0.0
        };
    }
    pub fn visible(&self, layout: &DocumentLayout, overscan: f32) -> Range<usize> {
        layout.visible_boxes(self.top, self.height, overscan)
    }
    pub fn anchor(&self, layout: &DocumentLayout) -> ReadingAnchor {
        let index = layout
            .boxes
            .partition_point(|b| b.top + b.height <= self.top)
            .min(layout.boxes.len().saturating_sub(1));
        layout
            .boxes
            .get(index)
            .map_or_else(ReadingAnchor::default, |b| ReadingAnchor {
                source: b.source.start,
                fraction: ((self.top - b.top) / b.height.max(1.0)).clamp(0.0, 1.0),
            })
    }
    pub fn restore(&mut self, layout: &DocumentLayout, anchor: ReadingAnchor) {
        let end = layout
            .source_boxes
            .partition_point(|index| layout.boxes[*index].source.start <= anchor.source);
        let block = layout
            .source_boxes
            .get(end.saturating_sub(1))
            .and_then(|index| layout.boxes.get(*index))
            .or_else(|| layout.boxes.first());
        let top = block.map_or(0.0, |b| {
            b.top
                + b.height
                    * if anchor.fraction.is_finite() {
                        anchor.fraction.clamp(0.0, 1.0)
                    } else {
                        0.0
                    }
        });
        self.scroll_to(layout, top);
    }
    pub fn reflow(&mut self, previous: &DocumentLayout, next: &DocumentLayout, height: f32) {
        let anchor = self.anchor(previous);
        self.height = if height.is_finite() {
            height.max(1.0)
        } else {
            1.0
        };
        self.restore(next, anchor);
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkIntent {
    Web,
    LocalDocument,
    Fragment,
    Blocked,
}
pub fn classify_link(destination: &str) -> LinkIntent {
    if destination.contains(['\0', '\r', '\n']) {
        return LinkIntent::Blocked;
    }
    let value = destination.trim();
    if value.starts_with('#') {
        return LinkIntent::Fragment;
    }
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        return LinkIntent::Web;
    }
    let windows_path = value.as_bytes().get(1) == Some(&b':')
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic);
    if !windows_path
        && value
            .split('/')
            .next()
            .is_some_and(|first| first.contains(':'))
    {
        return LinkIntent::Blocked;
    }
    if value.is_empty() {
        LinkIntent::Blocked
    } else {
        LinkIntent::LocalDocument
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollAxis {
    Vertical,
    Horizontal(NodeId),
}
#[derive(Clone, Copy, Debug)]
pub struct ScrollbarDrag {
    pub axis: ScrollAxis,
    pub grab: f32,
}
/// Transient state of one reading surface; documents own persisted anchors.
#[derive(Clone, Debug, Default)]
pub struct InteractionState {
    pub scrollbar: Option<ScrollbarDrag>,
    pub selection: Option<LogicalSelection>,
    pub pressed: Option<(u64, DisplayByte, Option<NodeId>)>,
    pub dragging: bool,
    /// Source scroll and reader byte as of the last source/preview sync.
    pub synced: Option<(f32, usize)>,
    pub fragment: Option<String>,
    pub drag_point: Option<(f32, f32)>,
    pub revision: u64,
    pub horizontal: std::collections::HashMap<NodeId, f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameKey {
    pub parse: u64,
    pub layout: u64,
}
#[derive(Clone)]
pub struct ParagraphGeometry {
    pub paragraph: usize,
    pub glyphs: Arc<[GraphemeGeometry]>,
}
#[derive(Clone)]
pub struct ReaderFrame {
    pub scrollbar_dragging: bool,
    pub key: FrameKey,
    pub document: Arc<MarkdownDocument>,
    pub layout: Arc<DocumentLayout>,
    pub viewport: Viewport,
    pub selection: Option<LogicalSelection>,
    pub geometry: Arc<[ParagraphGeometry]>,
    pub geometry_key: FrameKey,
    pub horizontal: std::collections::HashMap<NodeId, f32>,
    pub focused: bool,
    pub extend_selection: bool,
}
#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub display: DisplayByte,
    pub link: Option<NodeId>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Left,
    Right,
    Up,
    Down,
    Start,
    End,
}
#[derive(Clone, Debug)]
pub enum ReaderEvent {
    ScrollbarDown {
        key: FrameKey,
        axis: ScrollAxis,
        grab: f32,
        position: Option<f32>,
    },
    ScrollbarDrag {
        key: FrameKey,
        axis: ScrollAxis,
        position: f32,
        travel: f32,
    },
    ScrollbarUp {
        key: FrameKey,
        axis: ScrollAxis,
    },
    Focus(bool),
    /// Ctrl+wheel: change the preview's reading size by this many steps.
    Zoom(i32),
    HorizontalScroll {
        key: FrameKey,
        node: NodeId,
        delta: f32,
    },
    Scroll {
        key: FrameKey,
        delta: f32,
    },
    PointerDown {
        key: FrameKey,
        hit: Option<Hit>,
        extend: bool,
    },
    PointerDrag {
        key: FrameKey,
        hit: Option<Hit>,
        point: (f32, f32),
    },
    PointerUp {
        key: FrameKey,
        hit: Option<Hit>,
    },
    Move {
        key: FrameKey,
        direction: Move,
        extend: bool,
    },
    Copy {
        key: FrameKey,
    },
    SelectAll {
        key: FrameKey,
    },
    ClearSelection {
        key: FrameKey,
    },
    CopyNode {
        key: FrameKey,
        node: NodeId,
    },
    FocusLink {
        key: FrameKey,
        node: NodeId,
    },
    OpenLink {
        key: FrameKey,
        node: NodeId,
    },
}

#[derive(Default)]
pub struct Effects {
    pub focus: Option<bool>,
    pub link: Option<NodeId>,
    pub copy: Option<String>,
    pub selection_changed: bool,
}
/// The same interaction transaction is used by file and memory hosts. Platform
/// clipboard/navigation effects are returned as values and performed by hosts.
pub fn reduce(
    state: &mut InteractionState,
    frame: &ReaderFrame,
    event: ReaderEvent,
) -> (Viewport, Effects) {
    let mut viewport = frame.viewport;
    let mut effects = Effects::default();
    let key = frame.key;
    let document = &frame.document;
    let before = state.selection;
    if state.revision != key.parse {
        state.selection = None;
        state.pressed = None;
        state.drag_point = None;
        state.scrollbar = None;
        state.horizontal.clear();
        state.revision = key.parse;
    }
    match event {
        ReaderEvent::ScrollbarDown {
            key: expected,
            axis,
            grab,
            position,
        } if expected == key && grab.is_finite() => {
            cancel_gesture(state);
            state.scrollbar = Some(ScrollbarDrag { axis, grab });
            if let Some(position) = position.filter(|p| p.is_finite()) {
                scroll_axis(state, &frame.layout, &mut viewport, axis, position);
            }
        }
        ReaderEvent::ScrollbarDrag {
            key: expected,
            axis,
            position,
            travel,
        } if expected == key && position.is_finite() && travel.is_finite() => {
            if let Some(gesture) = state.scrollbar.filter(|g| g.axis == axis) {
                let max = match axis {
                    ScrollAxis::Vertical => (frame.layout.height - viewport.height).max(0.0),
                    ScrollAxis::Horizontal(node) => frame.layout.max_scroll(node),
                };
                let value = if travel > 0.0 {
                    ((position - gesture.grab) / travel).clamp(0.0, 1.0) * max
                } else {
                    0.0
                };
                scroll_axis(state, &frame.layout, &mut viewport, axis, value);
            }
        }
        ReaderEvent::ScrollbarUp { .. } => state.scrollbar = None,
        ReaderEvent::Zoom(_) => {}
        ReaderEvent::Focus(value) => {
            effects.focus = Some(value);
            if !value {
                state.pressed = None;
                state.drag_point = None;
                state.dragging = false;
                state.scrollbar = None;
            }
        }
        ReaderEvent::HorizontalScroll {
            key: expected,
            node,
            delta,
        } if expected == key && delta.is_finite() => {
            let value = state.horizontal.entry(node).or_default();
            *value = (*value + delta).clamp(0.0, frame.layout.max_scroll(node));
        }
        ReaderEvent::Scroll {
            key: expected,
            delta,
        } if expected == key => viewport.scroll_to(&frame.layout, viewport.top + delta),
        ReaderEvent::PointerDown {
            key: expected,
            hit: Some(hit),
            extend,
        } if expected == key => {
            state.scrollbar = None;
            let anchor = if extend {
                state
                    .selection
                    .map_or(hit.display, |selection| selection.anchor)
            } else {
                hit.display
            };
            state.selection = Some(LogicalSelection {
                revision: key.parse,
                anchor,
                head: hit.display,
            });
            state.pressed = Some((key.parse, hit.display, hit.link));
            state.dragging = false;
            state.drag_point = None;
        }
        ReaderEvent::PointerDrag {
            key: expected,
            hit,
            point,
        } if expected == key && state.pressed.is_some() => {
            state.drag_point = Some(point);
            if let (Some(hit), Some(selection)) = (hit, state.selection.as_mut()) {
                selection.head = hit.display;
                state.dragging |= selection.head != selection.anchor;
            }
            let (delta, horizontal) = drag_scroll_delta(&frame.layout, viewport, point);
            if let Some((node, delta)) = horizontal
                && !frame.geometry.is_empty()
            {
                let offset = state.horizontal.entry(node).or_default();
                *offset = (*offset + delta).clamp(0.0, frame.layout.max_scroll(node));
            }
            if delta != 0.0 || horizontal.is_some() {
                viewport.scroll_to(&frame.layout, viewport.top + delta);
                if let Some(hit) = hit_at(
                    &frame.layout,
                    &frame.geometry,
                    &state.horizontal,
                    viewport,
                    (point.0, point.1.clamp(0.0, viewport.height)),
                ) && let Some(selection) = state.selection.as_mut()
                {
                    selection.head = hit.display;
                    state.dragging = true;
                }
            }
        }
        ReaderEvent::PointerUp { key: expected, hit } if expected == key => {
            if !state.dragging {
                if let (Some((version, _, Some(link))), Some(hit)) = (state.pressed, hit) {
                    if version == key.parse && hit.link == Some(link) {
                        effects.link = Some(link);
                    }
                }
            }
            cancel_gesture(state);
        }
        ReaderEvent::PointerUp { .. } => cancel_gesture(state),
        ReaderEvent::Copy { key: expected } if expected == key => {
            effects.copy = state
                .selection
                .and_then(|selection| selection.copy(document, key.parse))
                .map(str::to_owned)
        }
        ReaderEvent::SelectAll { key: expected } if expected == key => {
            state.selection = Some(LogicalSelection {
                revision: key.parse,
                anchor: DisplayByte(0),
                head: DisplayByte(document.display_text.len()),
            })
        }
        ReaderEvent::ClearSelection { key: expected } if expected == key => {
            state.selection = None;
            cancel_gesture(state);
        }
        ReaderEvent::Move {
            key: expected,
            direction,
            extend,
        } if expected == key => {
            let old = state
                .selection
                .map_or(DisplayByte(0), |selection| selection.head);
            let head = if matches!(direction, Move::Up | Move::Down) {
                vertical_head(frame, old, direction == Move::Down)
                    .unwrap_or_else(|| logical_head(document, old, direction))
            } else {
                logical_head(document, old, direction)
            };
            state.selection = Some(LogicalSelection {
                revision: key.parse,
                anchor: if extend {
                    state.selection.map_or(old, |selection| selection.anchor)
                } else {
                    head
                },
                head,
            });
            match direction {
                Move::Start => viewport.scroll_to(&frame.layout, 0.0),
                Move::End => viewport.scroll_to(&frame.layout, frame.layout.height),
                _ => reveal(&frame.layout, head, &mut viewport),
            }
        }
        ReaderEvent::CopyNode {
            key: expected,
            node,
        } if expected == key => {
            effects.copy = document
                .node(node)
                .filter(|node| {
                    matches!(
                        node.kind,
                        NodeKind::CodeBlock { .. } | NodeKind::Html { block: true }
                    )
                })
                .and_then(|node| document.text(node.display.clone()))
                .map(str::to_owned);
        }
        ReaderEvent::FocusLink {
            key: expected,
            node,
        } if expected == key => {
            if let Some(link) = document
                .node(node)
                .filter(|node| matches!(node.kind, NodeKind::Link { .. }))
            {
                state.selection = Some(LogicalSelection {
                    revision: key.parse,
                    anchor: link.display.start,
                    head: link.display.end,
                });
                reveal(&frame.layout, link.display.start, &mut viewport);
            }
        }
        ReaderEvent::OpenLink {
            key: expected,
            node,
        } if expected == key => effects.link = Some(node),
        _ => {}
    }
    effects.selection_changed = before != state.selection;
    (viewport, effects)
}
pub fn logical_head(document: &MarkdownDocument, old: DisplayByte, direction: Move) -> DisplayByte {
    use unicode_segmentation::UnicodeSegmentation;
    let text = &document.display_text;
    let old = grapheme_floor(text, old.0);
    DisplayByte(match direction {
        Move::Start => 0,
        Move::End => text.len(),
        Move::Left => text[..old]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(byte, _)| byte),
        Move::Right => text[old..]
            .grapheme_indices(true)
            .nth(1)
            .map_or(text.len(), |(byte, _)| old + byte),
        Move::Up => text[..old]
            .rfind('\n')
            .and_then(|end| text[..end].rfind('\n'))
            .map_or(0, |byte| byte + 1),
        Move::Down => text[old..]
            .find('\n')
            .map_or(text.len(), |byte| old + byte + 1),
    })
}
fn reveal(layout: &DocumentLayout, head: DisplayByte, viewport: &mut Viewport) {
    let index = layout
        .paragraphs
        .partition_point(|paragraph| paragraph.display.start <= head)
        .saturating_sub(1);
    if let Some(paragraph) = layout.paragraphs.get(index) {
        let byte = head
            .0
            .saturating_sub(paragraph.display.start.0)
            .min(paragraph.display.end.0 - paragraph.display.start.0);
        if let Some(scalar) = paragraph.index.scalar(byte) {
            if let Some(line) = paragraph
                .summary
                .lines
                .iter()
                .find(|line| line.scalars.start <= scalar && scalar <= line.scalars.end)
            {
                let top = paragraph.y + line.top;
                if top < viewport.top {
                    viewport.scroll_to(layout, top);
                } else if top + line.height > viewport.top + viewport.height {
                    viewport.scroll_to(layout, top + line.height - viewport.height);
                }
                return;
            }
        }
    }
    if let Some(source) = layout.document.source_at(head) {
        viewport.restore(
            layout,
            ReadingAnchor {
                source: source.start,
                fraction: 0.0,
            },
        );
    }
}
fn vertical_head(frame: &ReaderFrame, head: DisplayByte, down: bool) -> Option<DisplayByte> {
    let mut current = None;
    for geometry in frame.geometry.iter() {
        let paragraph = frame.layout.paragraphs.get(geometry.paragraph)?;
        for glyph in geometry.glyphs.iter() {
            let start = paragraph.display.start.0 + glyph.bytes.start;
            let end = paragraph.display.start.0 + glyph.bytes.end;
            if start <= head.0 && head.0 < end {
                current = Some((paragraph.x + glyph.rect.left, paragraph.y + glyph.rect.top));
                break;
            }
        }
        if current.is_some() {
            break;
        }
    }
    let (x, y) = current?;
    let mut nearest = None;
    let mut distance = f32::INFINITY;
    for geometry in frame.geometry.iter() {
        let paragraph = frame.layout.paragraphs.get(geometry.paragraph)?;
        for glyph in geometry.glyphs.iter() {
            let top = paragraph.y + glyph.rect.top;
            if (down && top <= y + 0.5) || (!down && top >= y - 0.5) {
                continue;
            }
            let dx = (paragraph.x + glyph.rect.left - x).abs();
            let score = (top - y).abs() * 10_000.0 + dx;
            if score < distance {
                distance = score;
                nearest = Some(DisplayByte(paragraph.display.start.0 + glyph.bytes.start));
            }
        }
    }
    nearest
}
fn scroll_axis(
    state: &mut InteractionState,
    layout: &DocumentLayout,
    viewport: &mut Viewport,
    axis: ScrollAxis,
    position: f32,
) {
    match axis {
        ScrollAxis::Vertical => viewport.scroll_to(layout, position),
        ScrollAxis::Horizontal(node) => {
            state
                .horizontal
                .insert(node, position.clamp(0.0, layout.max_scroll(node)));
        }
    }
}
/// Edge motion uses the scroll block's viewport, including edges inside pane margins.
/// Keep scheduling at a limit until release so a pending native geometry window can settle.
pub fn drag_scroll_delta(
    layout: &DocumentLayout,
    viewport: Viewport,
    point: (f32, f32),
) -> (f32, Option<(NodeId, f32)>) {
    if !point.0.is_finite() || !point.1.is_finite() {
        return (0.0, None);
    }
    let speed = |at: f32, lo: f32, hi: f32| {
        if at < lo {
            -((lo - at) * 0.25).clamp(2.0, 40.0)
        } else if at > hi {
            ((at - hi) * 0.25).clamp(2.0, 40.0)
        } else {
            0.0
        }
    };
    let y = point.1 + viewport.top;
    let horizontal = layout
        .horizontal
        .iter()
        .find(|b| y >= b.rect.top && y <= b.rect.bottom && layout.max_scroll(b.node) > 0.0)
        .and_then(|block| {
            let delta = speed(point.0, block.rect.left, block.rect.right);
            (delta != 0.0).then_some((block.node, delta))
        });
    (speed(point.1, 0.0, viewport.height), horizontal)
}
pub fn hit_at(
    layout: &DocumentLayout,
    geometry: &[ParagraphGeometry],
    horizontal: &std::collections::HashMap<NodeId, f32>,
    viewport: Viewport,
    point: (f32, f32),
) -> Option<Hit> {
    if !point.0.is_finite() || !point.1.is_finite() {
        return None;
    }
    let x = point.0;
    let y = point.1 + viewport.top;
    for image in &layout.images {
        if x >= image.rect.left
            && x <= image.rect.right
            && y >= image.rect.top
            && y <= image.rect.bottom
        {
            let link = layout
                .document
                .node(image.node)
                .and_then(|image| image.parent)
                .filter(|id| matches!(layout.document.nodes[id.0].kind, NodeKind::Link { .. }));
            return Some(Hit {
                display: if x < (image.rect.left + image.rect.right) * 0.5 {
                    image.display.start
                } else {
                    image.display.end
                },
                link,
            });
        }
    }
    let mut nearest = None;
    let mut distance = f32::INFINITY;
    for geometry in geometry {
        let Some(paragraph) = layout.paragraphs.get(geometry.paragraph) else {
            continue;
        };
        let offset = layout
            .scroll_container(geometry.paragraph)
            .and_then(|node| horizontal.get(&node).copied())
            .unwrap_or(0.0);
        for glyph in geometry.glyphs.iter() {
            let left = paragraph.x - offset + glyph.rect.left;
            let right = paragraph.x - offset + glyph.rect.right;
            let top = paragraph.y + glyph.rect.top;
            let bottom = paragraph.y + glyph.rect.bottom;
            let dx = if x < left {
                left - x
            } else if x > right {
                x - right
            } else {
                0.0
            };
            let dy = if y < top {
                top - y
            } else if y > bottom {
                y - bottom
            } else {
                0.0
            };
            let score = dy * dy * 4.0 + dx * dx;
            if score < distance {
                distance = score;
                let local = if x < (left + right) * 0.5 {
                    glyph.bytes.start
                } else {
                    glyph.bytes.end
                };
                let link = paragraph
                    .spans
                    .iter()
                    .find(|span| {
                        span.bytes.start <= glyph.bytes.start && glyph.bytes.start < span.bytes.end
                    })
                    .and_then(|span| span.style.link);
                nearest = Some(Hit {
                    display: DisplayByte(paragraph.display.start.0 + local),
                    link,
                });
            }
        }
    }
    nearest
}

pub fn active_link(
    document: &MarkdownDocument,
    selection: Option<LogicalSelection>,
    revision: u64,
) -> Option<NodeId> {
    let range = selection?.range(document, revision)?;
    document
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| {
            matches!(node.kind, NodeKind::Link { .. })
                && node.display.start <= range.start
                && range.end <= node.display.end
        })
        .map(|(index, _)| NodeId(index))
}
pub fn next_link(
    document: &MarkdownDocument,
    selection: Option<LogicalSelection>,
    revision: u64,
    backward: bool,
) -> Option<NodeId> {
    let range = selection.and_then(|value| value.range(document, revision));
    let mut links = document
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| matches!(node.kind, NodeKind::Link { .. }));
    if backward {
        let at = range.map_or(DisplayByte(document.display_text.len()), |range| {
            range.start
        });
        links
            .rev()
            .find(|(_, node)| node.display.end <= at && node.display.start < at)
            .map(|(index, _)| NodeId(index))
    } else {
        let at = range.map_or(DisplayByte(0), |range| range.end);
        links
            .find(|(_, node)| node.display.start >= at)
            .map(|(index, _)| NodeId(index))
    }
}

impl ReaderEvent {
    pub fn ends_gesture(&self) -> bool {
        matches!(
            self,
            Self::PointerUp { .. } | Self::ScrollbarUp { .. } | Self::Focus(false)
        )
    }
}
pub fn cancel_gesture(state: &mut InteractionState) {
    state.scrollbar = None;
    state.pressed = None;
    state.drag_point = None;
    state.dragging = false;
}

impl ReaderEvent {
    /// A logical hit comes from the frame actually shown to the user. A newer
    /// raster publication of the same parse must not drop its pointer/scroll
    /// transaction. Different documents/parses remain invalid.
    pub fn on_frame(mut self, frame: FrameKey) -> Option<Self> {
        let key = match &mut self {
            Self::Focus(_) | Self::Zoom(_) => return Some(self),
            Self::ScrollbarDown { key, .. }
            | Self::ScrollbarDrag { key, .. }
            | Self::ScrollbarUp { key, .. }
            | Self::HorizontalScroll { key, .. }
            | Self::Scroll { key, .. }
            | Self::PointerDown { key, .. }
            | Self::PointerDrag { key, .. }
            | Self::PointerUp { key, .. }
            | Self::Move { key, .. }
            | Self::Copy { key }
            | Self::SelectAll { key }
            | Self::ClearSelection { key }
            | Self::CopyNode { key, .. }
            | Self::FocusLink { key, .. }
            | Self::OpenLink { key, .. } => key,
        };
        if key.parse != frame.parse {
            return None;
        }
        *key = frame;
        Some(self)
    }
}
