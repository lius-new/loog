//! Controlled reading surface. Native paragraph painting is supplied through a
//! narrow port; this module owns neither parsing nor text/native resources.
use crate::features::markdown::layout::Decoration;
use crate::ui::components::scrollbar;
use crate::ui::theme;
use lgui::core::{
    CursorIcon, EventPolicy, KeyState, LogicalKey, NamedKey, PathStyle, Point, PointerButton,
    SemanticRole, Semantics, Stroke, UiElement, UiEventKind, UiFocusHandle, UiId, UiPath,
    UiPathCommand, WheelUnit, clip, path,
};
use lgui::prelude::{Element, UiRect, VisualStyle, panel, text};
use std::sync::Arc;

pub use crate::features::markdown::reader::{
    FrameKey, Hit, Move, ParagraphGeometry, ReaderEvent, ReaderFrame,
};

type PaintImage = dyn Fn(&ReaderFrame, &crate::features::markdown::layout::ImageBox, UiRect) -> Element
    + Send
    + Sync;
type PaintParagraph = dyn Fn(&ReaderFrame, usize, UiRect, UiRect) -> Element + Send + Sync;
#[derive(Clone)]
pub struct Binding {
    read: Arc<dyn Fn() -> ReaderFrame + Send + Sync>,
    emit: Arc<dyn Fn(ReaderEvent) + Send + Sync>,
    paint: Arc<PaintParagraph>,
    image: Arc<PaintImage>,
}
impl Binding {
    /// The host keeps the publication/retention envelope alive with this binding.
    /// Paint must use the same styled paragraph request as summary and geometry.
    pub fn new(
        read: impl Fn() -> ReaderFrame + Send + Sync + 'static,
        emit: impl Fn(ReaderEvent) + Send + Sync + 'static,
        paint: impl Fn(&ReaderFrame, usize, UiRect, UiRect) -> Element + Send + Sync + 'static,
    ) -> Self {
        Self {
            read: Arc::new(read),
            emit: Arc::new(emit),
            paint: Arc::new(paint),
            image: Arc::new(|frame, image, rect| {
                text(
                    rect,
                    frame
                        .document
                        .text(image.display.clone())
                        .unwrap_or("Image")
                        .to_owned(),
                    theme::sans(theme::c().text_soft, 14.0),
                )
                .into()
            }),
        }
    }
    pub fn with_images(
        mut self,
        paint: impl Fn(&ReaderFrame, &crate::features::markdown::layout::ImageBox, UiRect) -> Element
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.image = Arc::new(paint);
        self
    }
    pub fn get(&self) -> ReaderFrame {
        (self.read)()
    }
    pub fn send(&self, event: ReaderEvent) {
        (self.emit)(event);
    }
}
fn hit(frame: &ReaderFrame, rect: UiRect, point: (f32, f32)) -> Option<Hit> {
    if frame.geometry_key != frame.key {
        return None;
    }
    crate::features::markdown::reader::hit_at(
        &frame.layout,
        &frame.geometry,
        &frame.horizontal,
        frame.viewport,
        (point.0 - rect.left, point.1 - rect.top),
    )
}
/// The vertical extent of each visual line of a paragraph. Glyph boxes of mixed
/// fonts differ in height and leave gaps between lines, so every band runs down
/// to the top of the next one.
fn line_bands(glyphs: &[crate::features::markdown::layout::GraphemeGeometry]) -> Vec<(f32, f32)> {
    let mut spans: Vec<(f32, f32)> = glyphs
        .iter()
        .map(|glyph| (glyph.rect.top, glyph.rect.bottom))
        .collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut bands: Vec<(f32, f32)> = Vec::new();
    for (top, bottom) in spans {
        match bands.last_mut() {
            // Starting above the middle of the band means the same line.
            Some(last) if top < (last.0 + last.1) / 2.0 => last.1 = last.1.max(bottom),
            _ => bands.push((top, bottom)),
        }
    }
    for index in 1..bands.len() {
        bands[index - 1].1 = bands[index].0.max(bands[index - 1].0);
    }
    bands
}

/// The band holding `glyph`.
fn band_of(
    bands: &[(f32, f32)],
    glyph: crate::features::markdown::layout::TextRect,
) -> Option<usize> {
    let middle = (glyph.top + glyph.bottom) / 2.0;
    bands
        .iter()
        .position(|band| middle < band.1)
        .or(bands.len().checked_sub(1))
}

/// A selection block on whole pixels. Every edge rounds the same way, so the
/// edge two lines share lands on one pixel row; an overlap would be painted
/// twice by the translucent fill and show as a bright line.
fn snapped(run: crate::features::markdown::layout::TextRect, left: f32, top: f32) -> UiRect {
    UiRect::new(
        (left + run.left).round(),
        (top + run.top).round(),
        (left + run.right).round(),
        (top + run.bottom).round(),
    )
}

/// Selected glyphs merged into one rectangle per stretch of each line. A
/// translucent fill must not overlap itself, or the overlap would look darker.
fn selection_runs(
    bands: &[(f32, f32)],
    glyphs: impl Iterator<Item = crate::features::markdown::layout::TextRect>,
) -> Vec<crate::features::markdown::layout::TextRect> {
    use crate::features::markdown::layout::TextRect;
    let mut lines: Vec<Vec<(f32, f32)>> = vec![Vec::new(); bands.len()];
    for glyph in glyphs {
        if let Some(line) = band_of(bands, glyph) {
            lines[line].push((glyph.left, glyph.right));
        }
    }
    let mut runs = Vec::new();
    for (line, mut spans) in lines.into_iter().enumerate() {
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut merged: Vec<(f32, f32)> = Vec::new();
        for (left, right) in spans {
            match merged.last_mut() {
                // Neighbouring glyphs may sit a sub-pixel apart.
                Some(last) if left <= last.1 + 1.0 => last.1 = last.1.max(right),
                _ => merged.push((left, right)),
            }
        }
        runs.extend(merged.into_iter().map(|(left, right)| TextRect {
            left,
            top: bands[line].0,
            right,
            bottom: bands[line].1,
        }));
    }
    runs
}

/// Split `start..end` into runs that are entirely inside one link or entirely
/// outside any link, so punctuation glued to a link never changes its cursor.
fn link_runs(
    spans: &[crate::features::markdown::layout::StyleSpan],
    start: usize,
    end: usize,
) -> Vec<(
    std::ops::Range<usize>,
    Option<crate::features::markdown::model::NodeId>,
)> {
    let mut runs: Vec<(std::ops::Range<usize>, Option<_>)> = Vec::new();
    let mut push = |range: std::ops::Range<usize>, link| {
        if range.is_empty() {
            return;
        }
        match runs.last_mut() {
            Some((last, previous)) if *previous == link && last.end == range.start => {
                last.end = range.end;
            }
            _ => runs.push((range, link)),
        }
    };
    let mut position = start;
    for span in spans
        .iter()
        .filter(|span| span.bytes.end > start && span.bytes.start < end)
    {
        let from = span.bytes.start.max(position);
        let to = span.bytes.end.min(end);
        push(position..from, None);
        push(from..to, span.style.link);
        position = position.max(to);
    }
    push(position..end, None);
    runs
}
fn translated(
    rect: crate::features::markdown::layout::TextRect,
    viewport: UiRect,
    scroll: f32,
) -> UiRect {
    UiRect::new(
        viewport.left + rect.left,
        viewport.top + rect.top - scroll,
        viewport.left + rect.right,
        viewport.top + rect.bottom - scroll,
    )
}
/// Only visible paragraphs and bounded visible glyph rectangles enter the scene.
/// Selection/copy remains a full-document logical range outside this scene.
pub fn render(id: UiId, rect: UiRect, binding: Binding, focus: UiFocusHandle) -> Element {
    let emphasized = false;
    // A card sits on its own surface, so the reader adds no fill of its own.
    render_in(
        id,
        rect,
        rect,
        emphasized,
        VisualStyle::default(),
        binding,
        focus,
    )
}
/// `area` is the whole pane; `rect` is the (possibly narrower, centered)
/// reading column. Wheel input and the vertical scrollbar use the pane edge.
pub fn render_in(
    id: UiId,
    area: UiRect,
    rect: UiRect,
    emphasized: bool,
    background: VisualStyle,
    binding: Binding,
    focus: UiFocusHandle,
) -> Element {
    let frame = binding.get();
    let semantic_scope = id.to_string();
    let mut root = Element::new(move |cx| {
        UiElement::panel(id, area, background)
            .semantics(Semantics::new(SemanticRole::Group).name("Markdown document"))
            .children(cx.children)
    })
    .event_policy(EventPolicy::INTERACTIVE)
    .cursor(CursorIcon::Default);
    for kind in [
        UiEventKind::Input,
        UiEventKind::CompositionStart,
        UiEventKind::CompositionUpdate,
        UiEventKind::CompositionEnd,
    ] {
        root = root.on_event_capture(kind, |cx, _| {
            cx.prevent_default();
            cx.stop_propagation();
        });
    }
    let mut scrollbar_layer = None;
    let mut horizontal_scrollbars = Vec::new();
    // I-beam only over the reading column, as in the editor; margins and scrollbars keep the arrow.
    // Drawn after the paragraph tiles, which would otherwise cover them.
    let mut overlays: Vec<Element> = Vec::new();
    let mut content = clip(rect, 0.0, 0.0)
        .key("markdown-reading-content")
        .cursor(CursorIcon::Text);
    for image in &frame.layout.images {
        let bounds = translated(image.rect, rect, frame.viewport.top);
        if bounds.bottom > rect.top && bounds.top < rect.bottom {
            content = content.child((binding.image)(&frame, image, bounds));
        }
    }
    for decoration in &frame.layout.decorations {
        match decoration {
            Decoration::Fallback { node, rect: bounds } => {
                let mut bounds = translated(*bounds, rect, frame.viewport.top);
                if let Some(container) = frame.layout.node_scroll_container(*node) {
                    let offset = frame.horizontal.get(&container).copied().unwrap_or(0.0);
                    bounds.left -= offset;
                    bounds.right -= offset;
                }
                if bounds.bottom > rect.top && bounds.top < rect.bottom {
                    content=content.child(panel(bounds,VisualStyle::filled(theme::c().surface)).child(text(bounds,"Large block exceeds the preview budget. Toggle the preview to read the source; Select All / Copy includes its complete text.",theme::sans(theme::c().warning,13.0))));
                }
            }
            Decoration::Rule { y, left, right } => {
                let top = rect.top + y - frame.viewport.top;
                if top >= rect.top && top <= rect.bottom {
                    content = content.child(panel(
                        UiRect::new(rect.left + left, top, rect.left + right, top + 1.0),
                        VisualStyle::filled(theme::c().border),
                    ));
                }
            }
            Decoration::Quote { rect: bounds } => {
                let bounds = translated(*bounds, rect, frame.viewport.top);
                if bounds.bottom > rect.top && bounds.top < rect.bottom {
                    content = content.child(panel(bounds, VisualStyle::filled(theme::c().border)));
                }
            }
            Decoration::Code {
                node, rect: bounds, ..
            } => {
                let bounds = translated(*bounds, rect, frame.viewport.top);
                if bounds.bottom > rect.top && bounds.top < rect.bottom {
                    content = content.child(panel(
                        bounds,
                        VisualStyle::filled(theme::c().surface).radius(4.0),
                    ));
                    let copy = binding.clone();
                    let node = *node;
                    if let crate::features::markdown::model::NodeKind::CodeBlock {
                        language: Some(language),
                        ..
                    } = &frame.document.nodes[node.0].kind
                        && !language.is_empty()
                    {
                        // The language sits left of the copy button.
                        content = content.child(text(
                            UiRect::new(
                                (bounds.right - 36.0 - 120.0).max(bounds.left),
                                bounds.top + 6.0,
                                (bounds.right - 36.0).max(bounds.left),
                                bounds.top + 28.0,
                            ),
                            language.clone(),
                            theme::mono_right(theme::c().text_dim, theme::SMALL),
                        ));
                    }
                    let copied = recently_copied(node);
                    let button = UiRect::new(
                        (bounds.right - 30.0).max(bounds.left),
                        bounds.top + 6.0,
                        bounds.right - 8.0,
                        bounds.top + 28.0,
                    );
                    let glyph = theme::c().text_soft;
                    let (mx, my) = (
                        (button.left + button.right) * 0.5,
                        (button.top + button.bottom) * 0.5,
                    );
                    let mut icon = Vec::new();
                    if copied {
                        let point = |dx: f32, dy: f32| Point::new(mx + dx, my + dy);
                        icon.push(path(
                            button,
                            UiPath::new([
                                UiPathCommand::MoveTo(point(-4.5, 0.0)),
                                UiPathCommand::LineTo(point(-1.5, 3.5)),
                                UiPathCommand::LineTo(point(4.5, -3.5)),
                            ]),
                            PathStyle {
                                fill: None,
                                fill_alpha: 0,
                                stroke: Some(Stroke::new(glyph, 1.6, 255)),
                            },
                        ));
                    } else {
                        // Two overlapping outlined squares on whole pixels, centered as one glyph.
                        let square = |left: f32, top: f32, fill: lgui::prelude::Color| {
                            panel(
                                UiRect::new(left, top, left + 8.0, top + 9.0),
                                VisualStyle::filled(fill)
                                    .radius(2.0)
                                    .stroked(Stroke::new(glyph, 1.4, 255)),
                            )
                        };
                        icon.push(square(mx - 2.0, my - 6.0, theme::c().surface));
                        icon.push(square(mx - 5.0, my - 3.0, theme::c().surface));
                    }
                    overlays.push(
                        Element::new(move |cx| {
                            let flags = cx.context.interaction_flags(&cx.id);
                            let fill = if flags.pressed || flags.hovered {
                                theme::c().active_line
                            } else {
                                theme::c().surface
                            };
                            UiElement::panel(
                                cx.id.clone(),
                                button,
                                VisualStyle::filled(fill).radius(6.0),
                            )
                            .semantics(Semantics::new(SemanticRole::Button).name("Copy code"))
                            .children(cx.children)
                        })
                        .event_policy(EventPolicy {
                            focus: false,
                            ..EventPolicy::INTERACTIVE
                        })
                        .cursor(CursorIcon::Pointer)
                        .children(icon)
                        .on_pointer_down_with_button(|cx, _, _| cx.stop_propagation())
                        .on_click(
                            move |cx: &mut lgui::core::UiEventContext| {
                                let frame = copy.get();
                                copy.send(ReaderEvent::CopyNode {
                                    key: frame.key,
                                    node,
                                });
                                cx.stop_propagation();
                            },
                        ),
                    );
                }
            }
            Decoration::ListMarker {
                node,
                display,
                x,
                y,
            } => {
                let top = rect.top + y - frame.viewport.top;
                if top + frame.layout.style.line_height > rect.top && top < rect.bottom {
                    content = content.child(text(
                        UiRect::new(
                            rect.left + x,
                            top,
                            rect.left + x + frame.layout.style.indent,
                            top + frame.layout.style.line_height,
                        ),
                        match frame.document.nodes[node.0].kind {
                            crate::features::markdown::model::NodeKind::ListItem {
                                task: Some(done),
                                ..
                            } => if done { "☑" } else { "☐" }.to_owned(),
                            _ => frame
                                .document
                                .text(display.clone())
                                .unwrap_or("")
                                .trim()
                                .to_owned(),
                        },
                        theme::sans(theme::c().text_soft, 16.0 * frame.layout.style.scale),
                    ));
                }
            }
            Decoration::TableCell {
                node,
                rect: bounds,
                header,
                stripe,
                first_column,
                ..
            } => {
                let mut bounds = translated(*bounds, rect, frame.viewport.top);
                let offset = frame
                    .layout
                    .node_scroll_container(*node)
                    .and_then(|node| frame.horizontal.get(&node).copied())
                    .unwrap_or(0.0);
                bounds.left -= offset;
                bounds.right -= offset;
                if bounds.bottom > rect.top && bounds.top < rect.bottom {
                    if *stripe {
                        content = content.child(panel(
                            bounds,
                            VisualStyle::filled(theme::c().active_line).alpha(80),
                        ));
                    }
                    if *header {
                        content = content
                            .child(panel(bounds, VisualStyle::filled(theme::c().active_line)))
                            // Cells draw their right and bottom edges; the table's
                            // top edge belongs to the header row.
                            .child(panel(
                                UiRect::new(
                                    bounds.left,
                                    bounds.top,
                                    bounds.right,
                                    bounds.top + 1.0,
                                ),
                                VisualStyle::filled(theme::c().border),
                            ));
                    }
                    if *first_column {
                        content = content.child(panel(
                            UiRect::new(bounds.left, bounds.top, bounds.left + 1.0, bounds.bottom),
                            VisualStyle::filled(theme::c().border),
                        ));
                    }
                    content = content
                        .child(panel(
                            UiRect::new(
                                bounds.right - 1.0,
                                bounds.top,
                                bounds.right,
                                bounds.bottom,
                            ),
                            VisualStyle::filled(theme::c().border),
                        ))
                        .child(panel(
                            UiRect::new(
                                bounds.left,
                                bounds.bottom - 1.0,
                                bounds.right,
                                bounds.bottom,
                            ),
                            VisualStyle::filled(theme::c().border),
                        ));
                }
            }
        }
    }
    if frame.geometry_key == frame.key
        && let Some(selected) = frame
            .selection
            .and_then(|selection| selection.range(&frame.document, frame.key.parse))
    {
        for image in &frame.layout.images {
            if selected.start < image.display.end && image.display.start < selected.end {
                let bounds = translated(image.rect, rect, frame.viewport.top);
                content = content.child(panel(
                    bounds,
                    VisualStyle::filled(theme::c().selection).alpha(110),
                ));
            }
        }
        for geometry in frame.geometry.iter() {
            let Some(paragraph) = frame.layout.paragraphs.get(geometry.paragraph) else {
                continue;
            };
            let bands = line_bands(&geometry.glyphs);
            let chosen = geometry.glyphs.iter().filter(|glyph| {
                let start = paragraph.display.start.0 + glyph.bytes.start;
                let end = paragraph.display.start.0 + glyph.bytes.end;
                start < selected.end.0 && end > selected.start.0
            });
            let left = rect.left + paragraph.x
                - frame
                    .layout
                    .scroll_container(geometry.paragraph)
                    .and_then(|node| frame.horizontal.get(&node).copied())
                    .unwrap_or(0.0);
            let top = rect.top + paragraph.y - frame.viewport.top;
            // One translucent block per line, as in the editor and Changes view.
            for run in selection_runs(&bands, chosen.map(|glyph| glyph.rect)) {
                content = content.child(panel(
                    snapped(run, left, top),
                    VisualStyle::filled(theme::c().accent).alpha(55),
                ));
            }
        }
    }
    for index in frame
        .viewport
        .visible(&frame.layout, frame.layout.style.line_height * 2.0)
    {
        for paragraph in frame.layout.boxes[index].paragraphs.clone() {
            let value = &frame.layout.paragraphs[paragraph];
            let bounds = UiRect::new(
                rect.left + value.x
                    - frame
                        .layout
                        .scroll_container(paragraph)
                        .and_then(|node| frame.horizontal.get(&node).copied())
                        .unwrap_or(0.0),
                rect.top + value.y - frame.viewport.top,
                rect.left + value.x
                    - frame
                        .layout
                        .scroll_container(paragraph)
                        .and_then(|node| frame.horizontal.get(&node).copied())
                        .unwrap_or(0.0)
                    + value.viewport_width,
                rect.top + value.y + value.summary.height - frame.viewport.top,
            );
            content = content.child((binding.paint)(&frame, paragraph, bounds, rect));
        }
    }
    for overlay in overlays {
        content = content.child(overlay);
    }
    // Semantic text uses the same visible glyph bounds as hit testing. These
    // transparent nodes do not shape or paint text a second time.
    if frame.geometry_key == frame.key {
        for geometry in frame.geometry.iter() {
            let Some(paragraph) = frame.layout.paragraphs.get(geometry.paragraph) else {
                continue;
            };
            let value = frame.document.text(paragraph.display.clone()).unwrap_or("");
            let bands = line_bands(&geometry.glyphs);
            let mut offset = 0;
            for word in value.split_whitespace() {
                let Some(found) = value[offset..].find(word) else {
                    continue;
                };
                let word_start = offset + found;
                let word_end = word_start + word.len();
                offset = word_end;
                // A link can start or end inside a word, e.g. `(LICENSE-MIT)`.
                for (range, link) in link_runs(&paragraph.spans, word_start, word_end) {
                    let (start, end) = (range.start, range.end);
                    let from = geometry
                        .glyphs
                        .partition_point(|glyph| glyph.bytes.end <= start);
                    // A run that wraps gets one box per visual line; a single
                    // union box would cover everything between the lines.
                    let mut lines: Vec<Option<UiRect>> = vec![None; bands.len()];
                    let left = rect.left + paragraph.x
                        - frame
                            .layout
                            .scroll_container(geometry.paragraph)
                            .and_then(|node| frame.horizontal.get(&node).copied())
                            .unwrap_or(0.0);
                    let top = rect.top + paragraph.y - frame.viewport.top;
                    for glyph in geometry.glyphs[from..]
                        .iter()
                        .take_while(|glyph| glyph.bytes.start < end)
                    {
                        let Some(band) = band_of(&bands, glyph.rect) else {
                            continue;
                        };
                        let b = glyph.rect;
                        let slot = &mut lines[band];
                        *slot = Some(match *slot {
                            Some(old) => UiRect::new(
                                old.left.min(left + b.left),
                                old.top,
                                old.right.max(left + b.right),
                                old.bottom,
                            ),
                            None => UiRect::new(
                                left + b.left,
                                top + bands[band].0,
                                left + b.right,
                                top + bands[band].1,
                            ),
                        });
                    }
                    let linked = link
                        .and_then(|node| frame.document.node(node))
                        .and_then(|node| match &node.kind {
                            crate::features::markdown::model::NodeKind::Link {
                                destination,
                                ..
                            } => Some(destination.clone()),
                            _ => None,
                        });
                    let word = value[start..end].to_owned();
                    let is_link = linked.is_some();
                    for (line, bounds) in lines
                        .into_iter()
                        .enumerate()
                        .filter_map(|(i, b)| b.map(|b| (i, b)))
                    {
                        if bounds.bottom <= rect.top || bounds.top >= rect.bottom {
                            continue;
                        }
                        let semantics = match linked.clone() {
                            Some(url) => Semantics::new(SemanticRole::Link)
                                .name(word.clone())
                                .value(word.clone())
                                .description(url),
                            None => Semantics::new(SemanticRole::Text).value(word.clone()),
                        };
                        let semantic_id = UiId::owned(format!(
                            "{semantic_scope}-text-{}-{start}-{line}",
                            geometry.paragraph
                        ));
                        let semantic = Element::new(move |_| {
                            UiElement::panel(semantic_id, bounds, VisualStyle::default())
                                .semantics(semantics)
                        });
                        content = content.child(if is_link {
                            semantic.cursor(CursorIcon::Pointer)
                        } else {
                            semantic
                        });
                    }
                }
            }
        }
    }
    for block in &frame.layout.horizontal {
        let max = frame.layout.max_scroll(block.node);
        if max <= 0.0 {
            continue;
        }
        let bounds = translated(block.rect, rect, frame.viewport.top);
        if bounds.bottom < rect.top || bounds.top > rect.bottom {
            continue;
        }
        let node = block.node;
        let wheel = binding.clone();
        let surface = panel(bounds, VisualStyle::default())
            .event_policy(EventPolicy {
                focus: false,
                ..EventPolicy::INTERACTIVE
            })
            .on_wheel(move |cx, delta| {
                let frame = wheel.get();
                if delta.x.abs() > delta.y.abs() || frame.extend_selection {
                    let factor = if delta.unit == WheelUnit::Lines {
                        40.0
                    } else {
                        1.0
                    };
                    let delta = if delta.x != 0.0 { -delta.x } else { -delta.y };
                    wheel.send(ReaderEvent::HorizontalScroll {
                        key: frame.key,
                        node,
                        delta: delta * factor,
                    });
                    cx.stop_propagation();
                }
            });
        content = content.child(surface);
        let track = UiRect::new(
            bounds.left,
            bounds.bottom - scrollbar::SIZE,
            bounds.right,
            bounds.bottom,
        );
        let current = frame
            .horizontal
            .get(&node)
            .copied()
            .unwrap_or(0.0)
            .clamp(0.0, max);
        let visible = UiRect::new(
            track.left.max(rect.left),
            track.top.max(rect.top),
            track.right.min(rect.right),
            track.bottom.min(rect.bottom),
        );
        if visible.width() > 0.0 && visible.height() > 0.0 {
            horizontal_scrollbars.push(
                clip(visible, 0.0, 0.0)
                    .key(format!("markdown-scrollbar-clip-{}", node.0))
                    .child(axis_scrollbar(
                        crate::features::markdown::reader::ScrollAxis::Horizontal(node),
                        track,
                        block.extent,
                        track.width(),
                        current,
                        max,
                        emphasized,
                        binding.clone(),
                        focus.clone(),
                    )),
            );
        }
    }
    if frame.layout.height > frame.viewport.height {
        let max = (frame.layout.height - frame.viewport.height).max(1.0);
        let track = UiRect::new(
            area.right - scrollbar::SIZE,
            area.top,
            area.right,
            area.bottom,
        );
        scrollbar_layer = Some(axis_scrollbar(
            crate::features::markdown::reader::ScrollAxis::Vertical,
            track,
            frame.layout.height,
            track.height(),
            frame.viewport.top,
            max,
            emphasized,
            binding.clone(),
            focus.clone(),
        ));
    }
    root = root.child(content);
    for scrollbar in horizontal_scrollbars {
        root = root.child(scrollbar);
    }
    if let Some(layer) = scrollbar_layer {
        root = root.child(layer);
    }
    if frame.scrollbar_dragging {
        // Default cursors inherit from ancestors in lgui. A passive foreground
        // surface keeps the arrow while dragging across text or links, without
        // replacing the captured scrollbar's event target.
        root = root.child(
            panel(area, VisualStyle::default())
                .key("markdown-scrollbar-cursor")
                .cursor(CursorIcon::Default),
        );
    }
    let down = binding.clone();
    let down_focus = focus.clone();
    root = root.on_pointer_down_with_button(move |cx, pointer, button| {
        if button != PointerButton::Left {
            return;
        }
        down_focus.focus();
        let frame = down.get();
        down.send(ReaderEvent::PointerDown {
            key: frame.key,
            hit: hit(&frame, rect, (pointer.point.x, pointer.point.y)),
            extend: frame.extend_selection,
        });
        cx.stop_propagation();
    });
    let drag = binding.clone();
    root = root.on_pointer_drag(move |cx, pointer| {
        let frame = drag.get();
        let point = (pointer.point.x, pointer.point.y);
        drag.send(ReaderEvent::PointerDrag {
            key: frame.key,
            hit: hit(&frame, rect, point),
            point: (point.0 - rect.left, point.1 - rect.top),
        });
        cx.stop_propagation();
    });
    let up = binding.clone();
    root = root.on_pointer_up(move |_, pointer| {
        let frame = up.get();
        up.send(ReaderEvent::PointerUp {
            key: frame.key,
            hit: hit(&frame, rect, (pointer.point.x, pointer.point.y)),
        });
    });
    let wheel = binding.clone();
    root = root.on_wheel(move |cx, delta| {
        let frame = wheel.get();
        let amount = match delta.unit {
            WheelUnit::Lines => -delta.y * frame.layout.style.line_height * 3.0,
            WheelUnit::Pixels => -delta.y,
        };
        wheel.send(ReaderEvent::Scroll {
            key: frame.key,
            delta: amount,
        });
        cx.stop_propagation();
    });
    let keyboard = binding.clone();
    root = root.on_key_down(move |cx, event| {
        if event.state != KeyState::Down {
            return;
        }
        let frame = keyboard.get();
        let command = event.modifiers.ctrl() || event.modifiers.meta();
        let action = match &event.key {
            LogicalKey::Character(value) if command && value.eq_ignore_ascii_case("c") => {
                Some(ReaderEvent::Copy { key: frame.key })
            }
            LogicalKey::Character(value) if command && value.eq_ignore_ascii_case("a") => {
                Some(ReaderEvent::SelectAll { key: frame.key })
            }
            LogicalKey::Named(NamedKey::Tab) => crate::features::markdown::reader::next_link(
                &frame.document,
                frame.selection,
                frame.key.parse,
                event.modifiers.shift(),
            )
            .map(|node| ReaderEvent::FocusLink {
                key: frame.key,
                node,
            }),
            LogicalKey::Named(NamedKey::Enter) => crate::features::markdown::reader::active_link(
                &frame.document,
                frame.selection,
                frame.key.parse,
            )
            .map(|node| ReaderEvent::OpenLink {
                key: frame.key,
                node,
            }),
            LogicalKey::Named(NamedKey::Escape) => {
                Some(ReaderEvent::ClearSelection { key: frame.key })
            }
            LogicalKey::Named(NamedKey::PageDown) => Some(ReaderEvent::Scroll {
                key: frame.key,
                delta: frame.viewport.height * 0.9,
            }),
            LogicalKey::Named(NamedKey::PageUp) => Some(ReaderEvent::Scroll {
                key: frame.key,
                delta: -frame.viewport.height * 0.9,
            }),
            LogicalKey::Named(key) => {
                let direction = match key {
                    NamedKey::ArrowLeft => Some(Move::Left),
                    NamedKey::ArrowRight => Some(Move::Right),
                    NamedKey::ArrowUp => Some(Move::Up),
                    NamedKey::ArrowDown => Some(Move::Down),
                    NamedKey::Home => Some(Move::Start),
                    NamedKey::End => Some(Move::End),
                    _ => None,
                };
                direction.map(|direction| ReaderEvent::Move {
                    key: frame.key,
                    direction,
                    extend: event.modifiers.shift(),
                })
            }
            _ => None,
        };
        if let Some(action) = action {
            keyboard.send(action);
            cx.prevent_default();
            cx.stop_propagation();
        } else if !matches!(event.key, LogicalKey::Named(NamedKey::Tab)) {
            cx.prevent_default();
            cx.stop_propagation();
        }
    });
    // Reading input and composition are intercepted above, never sent to text edits.
    let focused = binding.clone();
    root = root.on_focus(move |_| focused.send(ReaderEvent::Focus(true)));
    root.on_blur(move |_| binding.send(ReaderEvent::Focus(false)))
}
/// Each scrollbar has stable keyed identity and view-owned gesture state.
/// Raster-window publications may replace every paragraph while it is held.
#[allow(clippy::too_many_arguments)]
fn axis_scrollbar(
    axis: crate::features::markdown::reader::ScrollAxis,
    track: UiRect,
    content: f32,
    viewport: f32,
    scroll: f32,
    max: f32,
    emphasized: bool,
    binding: Binding,
    focus: UiFocusHandle,
) -> Element {
    use crate::features::markdown::reader::ScrollAxis;
    let vertical = axis == ScrollAxis::Vertical;
    let (start, length) = if vertical {
        (track.top, track.height())
    } else {
        (track.left, track.width())
    };
    let thumb = (length * viewport / content.max(1.0))
        .max(scrollbar::MIN_THUMB_LENGTH)
        .min(length);
    let travel = (length - thumb).max(0.0);
    let thumb_start = start + travel * (scroll / max.max(1.0)).clamp(0.0, 1.0);
    let pick = move |x: f32, y: f32| if vertical { y } else { x };
    let policy = EventPolicy {
        focus: false,
        ..EventPolicy::INTERACTIVE
    };
    let (down, drag, up) = (binding.clone(), binding.clone(), binding.clone());
    let track_focus = focus.clone();
    let track_hit = scrollbar::track(track)
        .key("track")
        .event_policy(policy)
        .on_pointer_down_with_button(move |cx, pointer, button| {
            if button != PointerButton::Left {
                return;
            }
            track_focus.focus();
            let frame = down.get();
            let position = if travel > 0.0 {
                ((pick(pointer.point.x, pointer.point.y) - start - thumb * 0.5) / travel)
                    .clamp(0.0, 1.0)
                    * max
            } else {
                0.0
            };
            down.send(ReaderEvent::ScrollbarDown {
                key: frame.key,
                axis,
                grab: thumb * 0.5,
                position: Some(position),
            });
            cx.stop_propagation();
        })
        .on_pointer_drag(move |cx, pointer| {
            drag.send(ReaderEvent::ScrollbarDrag {
                key: drag.get().key,
                axis,
                position: pick(pointer.point.x, pointer.point.y) - start,
                travel,
            });
            cx.stop_propagation();
        })
        .on_pointer_up(move |cx, _| {
            up.send(ReaderEvent::ScrollbarUp {
                key: up.get().key,
                axis,
            });
            cx.stop_propagation();
        });
    let rect = if vertical {
        UiRect::new(
            track.left + scrollbar::INSET,
            thumb_start,
            track.right - scrollbar::INSET,
            thumb_start + thumb,
        )
    } else {
        UiRect::new(
            thumb_start,
            track.top + scrollbar::INSET,
            thumb_start + thumb,
            track.bottom - scrollbar::INSET,
        )
    };
    let (down, drag, up) = (binding.clone(), binding.clone(), binding);
    let thumb_hit = scrollbar::thumb(rect, if emphasized { 0xb8 } else { 0x68 })
        .key("thumb")
        .semantics(Semantics::new(SemanticRole::Group).name(if vertical {
            "Markdown vertical scrollbar thumb"
        } else {
            "Markdown horizontal scrollbar thumb"
        }))
        .event_policy(policy)
        .on_pointer_down_with_button(move |cx, pointer, button| {
            if button != PointerButton::Left {
                return;
            }
            focus.focus();
            down.send(ReaderEvent::ScrollbarDown {
                key: down.get().key,
                axis,
                grab: pick(pointer.point.x, pointer.point.y) - thumb_start,
                position: None,
            });
            cx.stop_propagation();
        })
        .on_pointer_drag(move |cx, pointer| {
            drag.send(ReaderEvent::ScrollbarDrag {
                key: drag.get().key,
                axis,
                position: pick(pointer.point.x, pointer.point.y) - start,
                travel,
            });
            cx.stop_propagation();
        })
        .on_pointer_up(move |cx, _| {
            up.send(ReaderEvent::ScrollbarUp {
                key: up.get().key,
                axis,
            });
            cx.stop_propagation();
        });
    let key = match axis {
        ScrollAxis::Vertical => "markdown-vertical-scrollbar".to_owned(),
        ScrollAxis::Horizontal(node) => format!("markdown-horizontal-scrollbar-{}", node.0),
    };
    lgui::core::group(track)
        .key(key)
        .child(track_hit)
        .child(thumb_hit)
}

/// How long a code block's copy button reads "Copied" after a successful copy.
pub const COPIED_FOR: std::time::Duration = std::time::Duration::from_millis(1400);
thread_local! {
    // Keep the node and timestamp together, within the rendering context.
    static COPIED: std::cell::Cell<Option<(crate::features::markdown::model::NodeId, std::time::Instant)>> = const { std::cell::Cell::new(None) };
}
pub fn mark_copied(node: crate::features::markdown::model::NodeId) {
    COPIED.set(Some((node, std::time::Instant::now())));
}
fn recently_copied(node: crate::features::markdown::model::NodeId) -> bool {
    recently_copied_at(node, std::time::Instant::now())
}
fn recently_copied_at(
    node: crate::features::markdown::model::NodeId,
    now: std::time::Instant,
) -> bool {
    COPIED.get().is_some_and(|(copied, at)| {
        copied == node && now.saturating_duration_since(at) < COPIED_FOR
    })
}

/// Narrow value-only inspection for cross-thread application tests.
#[cfg(test)]
pub(crate) fn context_feedback() -> Option<crate::features::markdown::model::NodeId> {
    COPIED.get().map(|(node, _)| node)
}
#[cfg(test)]
pub(crate) fn set_context_feedback(node: crate::features::markdown::model::NodeId) {
    mark_copied(node);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_feedback_expires_without_sleeping() {
        use crate::features::markdown::model::NodeId;
        let at = std::time::Instant::now();
        COPIED.set(Some((NodeId(7), at)));
        assert!(recently_copied_at(NodeId(7), at));
        assert!(!recently_copied_at(NodeId(8), at));
        assert!(recently_copied_at(
            NodeId(7),
            at + COPIED_FOR - std::time::Duration::from_nanos(1)
        ));
        assert!(!recently_copied_at(NodeId(7), at + COPIED_FOR));
    }

    use crate::features::markdown::layout::{GraphemeGeometry, TextRect};

    fn glyph(top: f32, bottom: f32) -> GraphemeGeometry {
        GraphemeGeometry {
            bytes: 0..1,
            rect: TextRect {
                left: 0.0,
                top,
                right: 8.0,
                bottom,
            },
        }
    }

    #[test]
    fn selection_bands_close_the_gaps_between_lines_of_mixed_fonts() {
        // Line one has a short code glyph; line two starts 3px below it.
        let glyphs = [
            glyph(0.0, 20.0),
            glyph(1.0, 17.0),
            glyph(23.0, 43.0),
            glyph(24.0, 40.0),
        ];
        let bands = line_bands(&glyphs);
        assert_eq!(bands, [(0.0, 23.0), (23.0, 43.0)]);
        // Every glyph lands in its own line, and the code glyphs fill it.
        assert_eq!(band_of(&bands, glyphs[1].rect), Some(0));
        assert_eq!(band_of(&bands, glyphs[3].rect), Some(1));
    }

    #[test]
    fn slightly_overlapping_line_boxes_stay_separate_lines() {
        let bands = line_bands(&[glyph(0.0, 20.5), glyph(20.0, 40.0)]);
        assert_eq!(bands, [(0.0, 20.0), (20.0, 40.0)]);
    }

    #[test]
    fn blocks_of_adjacent_lines_touch_without_overlapping_at_any_offset() {
        let line = |top: f32, bottom: f32| TextRect {
            left: 0.0,
            top,
            right: 100.0,
            bottom,
        };
        for step in 0..40 {
            // Paragraphs start at fractional pixels as the view scrolls.
            let offset = 12.0 + step as f32 * 0.137;
            let first = snapped(line(0.0, 20.3), 3.2, offset);
            let second = snapped(line(20.3, 41.7), 3.2, offset);
            assert_eq!(first.bottom, second.top, "offset {offset}");
            assert!(first.top < first.bottom && second.top < second.bottom);
        }
    }

    #[test]
    fn selected_glyphs_merge_into_one_block_per_line_stretch() {
        let at = |left: f32, top: f32, bottom: f32| TextRect {
            left,
            top,
            right: left + 8.0,
            bottom,
        };
        let glyphs = [glyph(0.0, 20.0), glyph(23.0, 43.0)];
        let bands = line_bands(&glyphs);
        let chosen = [
            at(0.0, 0.0, 20.0),
            at(8.0, 1.0, 17.0),
            at(16.4, 0.0, 20.0),
            // A gap of unselected glyphs, then the second line.
            at(48.0, 0.0, 20.0),
            at(0.0, 23.0, 43.0),
        ];
        let runs = selection_runs(&bands, chosen.into_iter());
        let spans = runs
            .iter()
            .map(|run| (run.left, run.top, run.right, run.bottom))
            .collect::<Vec<_>>();
        assert_eq!(
            spans,
            [
                (0.0, 0.0, 24.4, 23.0),
                (48.0, 0.0, 56.0, 23.0),
                (0.0, 23.0, 8.0, 43.0)
            ]
        );
    }
}
