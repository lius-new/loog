//! Compact split diff with central line numbers and live curved connections.
use std::sync::Arc;

use lgui::core::{
    AnimProperty, AnimationBinding, CursorIcon, EventPolicy, PathStyle, Point, PointerButton,
    SemanticRole, Semantics, Stroke, UiElement, UiFocusHandle, UiId, UiPath, UiPathCommand,
    UiRenderContext, clip, path,
};
use lgui::prelude::{Color, Element, UiRect, VisualStyle, panel, text};

use crate::domain::identity::{PaneId, ViewId};
use crate::features::git::DiffTarget;
use crate::features::git::actions::BlockAction;
use crate::features::git::diff_document::DiffDocument;
use crate::features::git::diff_selection::DiffSide;
use crate::features::git::split_layout::{Layout, Section, side_index};
use crate::ui::chrome::diff_binding::{Binding, ChangeFocus, Command};
use crate::ui::diff_editor::{
    NUMBER_W, code_line, line_number, row_background, row_h, split_middle,
};
use crate::ui::git::commands::{Binding as GitBinding, Command as GitCommand};
use crate::ui::theme;

const BRIDGE_W: f32 = 56.0;
// The window schedules a frame for hover animations, including on PointerLeave.
// The preview switches in one frame with no visible fade.
const HOVER_FRAME: AnimationBinding =
    AnimationBinding::new(AnimProperty::Hover, 0.0, 1.0).duration(1.0, 1.0);

#[derive(Clone)]
pub(crate) struct Interaction {
    pub pane: PaneId,
    pub view: ViewId,
    pub state: Binding,
    pub focus: UiFocusHandle,
    pub enabled: bool,
    pub locked: Option<ChangeFocus>,
    pub viewport_height: f32,
    pub max_y: f32,
}

struct ChangeControls<'a> {
    interaction: &'a Interaction,
    context: &'a UiRenderContext<'a>,
    document: &'a Arc<DiffDocument>,
    document_top: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct Columns {
    pub old: UiRect,
    pub new: UiRect,
    pub gutter: UiRect,
    pub numbers: [UiRect; 2],
    pub bridge: UiRect,
}

impl Columns {
    pub fn new(rect: UiRect, ratio: f32) -> Self {
        let middle = split_middle(rect, ratio);
        let width = (NUMBER_W * 2.0 + BRIDGE_W).min(rect.width() * 0.5);
        let left = (middle - width / 2.0).max(rect.left);
        let right = (middle + width / 2.0).min(rect.right);
        let number = NUMBER_W.min((right - left) / 3.0);
        Self {
            old: UiRect::new(rect.left, rect.top, left, rect.bottom),
            new: UiRect::new(right, rect.top, rect.right, rect.bottom),
            gutter: UiRect::new(left, rect.top, right, rect.bottom),
            numbers: [
                UiRect::new(left, rect.top, left + number, rect.bottom),
                UiRect::new(right - number, rect.top, right, rect.bottom),
            ],
            bridge: UiRect::new(left + number, rect.top, right - number, rect.bottom),
        }
    }

    pub fn code(self, side: DiffSide) -> UiRect {
        match side {
            DiffSide::Old => self.old,
            DiffSide::New => self.new,
        }
    }
}

/// File bodies only. The existing Changes renderer owns shared file headings.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render(
    rect: UiRect,
    document: &Arc<DiffDocument>,
    scroll_x: f32,
    scroll_y: f32,
    ratio: f32,
    git: &GitBinding,
    interaction: &Interaction,
    document_top: usize,
) -> Element {
    let document = document.clone();
    let git = git.clone();
    let interaction = interaction.clone();
    Element::new(move |cx| {
        cx.compile(paint(
            rect,
            &Layout::new(&document, None),
            scroll_x,
            scroll_y,
            ratio,
            Some(&git),
            interaction.enabled.then_some(ChangeControls {
                interaction: &interaction,
                context: cx.context,
                document: &document,
                document_top,
            }),
        ))
    })
}

#[cfg(test)]
pub(crate) fn render_document(
    rect: UiRect,
    document: &DiffDocument,
    scroll_x: f32,
    scroll_y: f32,
    ratio: f32,
) -> Element {
    paint(
        rect,
        &Layout::new(document, None),
        scroll_x,
        scroll_y,
        ratio,
        None,
        None,
    )
}

fn paint(
    rect: UiRect,
    layout: &Layout<'_>,
    scroll_x: f32,
    scroll_y: f32,
    ratio: f32,
    git: Option<&GitBinding>,
    controls: Option<ChangeControls<'_>>,
) -> Element {
    let columns = Columns::new(rect, ratio);
    let offsets = [DiffSide::Old, DiffSide::New]
        .map(|side| layout.offset(side, scroll_y / row_h(), rect.height() / row_h()));
    let mut bridges = clip(columns.bridge, 0.0, 0.0);
    let bridge = columns.bridge;
    let mut actions_layer = Element::new(move |cx| {
        UiElement::clip(cx.id, bridge, 0.0, 0.0)
            .hit_rect(UiRect::new(0.0, 0.0, 0.0, 0.0))
            .children(cx.children)
    });
    let mut targets = Vec::new();
    let mut anchors = clip(rect, 0.0, 0.0);
    let mut preview = None;
    let locked = controls.as_ref().and_then(|controls| {
        let locked = controls.interaction.locked.as_ref()?;
        locked
            .is_in(controls.interaction.view, controls.document)
            .then_some(locked.block)
    });
    let first = layout
        .sections
        .partition_point(|item| (item.end[0] as f32) < offsets[0])
        .min(
            layout
                .sections
                .partition_point(|item| (item.end[1] as f32) < offsets[1]),
        );
    for item in layout.sections.iter().skip(first) {
        if item.top[0] as f32 > offsets[0] + rect.height() / row_h()
            && item.top[1] as f32 > offsets[1] + rect.height() / row_h()
        {
            break;
        }
        if item.closed {
            continue;
        }
        let projection = item.document.split_projection();
        let first_change = projection.changes.partition_point(|change| {
            [DiffSide::Old, DiffSide::New].into_iter().all(|side| {
                let index = side_index(side);
                item.body[index] as f32 + projection.position(side, change.end as f32)
                    < offsets[index]
            })
        });
        for (block_index, change) in projection.changes.iter().enumerate().skip(first_change) {
            let ends = [DiffSide::Old, DiffSide::New].map(|side| {
                let index = side_index(side);
                let top = rect.top
                    + (item.body[index] as f32 + projection.position(side, change.start as f32)
                        - offsets[index])
                        * row_h();
                let bottom = rect.top
                    + (item.body[index] as f32 + projection.position(side, change.end as f32)
                        - offsets[index])
                        * row_h();
                (top, bottom)
            });
            if ends[0].0.min(ends[1].0) > rect.bottom {
                break;
            }
            if ends[0].1.max(ends[1].1) < rect.top {
                continue;
            }
            let modified = is_modified(item.document, change);
            let side = if item.document.split_rows[change.start].new_text.is_some() {
                DiffSide::New
            } else {
                DiffSide::Old
            };
            let (background, _, border) = change_colors(modified, side);
            let focused = locked == Some(block_index);
            // A hit changes both code columns, not only the small control under the pointer.
            let paint_bounds = UiRect::new(
                rect.left,
                (ends[0].0.min(ends[1].0) - 1.0).max(rect.top),
                rect.right,
                (ends[0].1.max(ends[1].1) + 1.0).min(rect.bottom),
            );
            let mut hover_targets = Vec::new();
            if let Some(controls) = &controls {
                let reveal_y = controls.reveal_y(rect, ends, change.start);
                for (part, area) in change_areas(columns, ends).into_iter().enumerate() {
                    if area.height() <= 0.0 || area.bottom <= rect.top || area.top >= rect.bottom {
                        continue;
                    }
                    let id = controls.id(block_index, "hit", part);
                    hover_targets.push(id.clone());
                    targets.push(
                        controls
                            .target(id, area, block_index, reveal_y, focused)
                            .paint_bounds(paint_bounds),
                    );
                }
            }
            if let Some(git) = git
                && let Some(&block) = item.document.blocks.get(block_index)
            {
                let actions: &[(BlockAction, &str, &str)] =
                    if item.document.target == DiffTarget::HeadToIndex {
                        &[(BlockAction::Unstage, "←", "Unstage change")]
                    } else {
                        &[
                            (BlockAction::Discard, "←", "Revert change"),
                            (BlockAction::Stage, "→", "Stage change"),
                        ]
                    };
                let top =
                    ((ends[0].0 + ends[0].1 + ends[1].0 + ends[1].1) / 4.0 - 10.0).max(rect.top);
                for (index, &(action, arrow, label)) in actions.iter().enumerate() {
                    let left = columns.bridge.left
                        + (columns.bridge.width() - actions.len() as f32 * 22.0) / 2.0
                        + index as f32 * 22.0;
                    let button = UiRect::new(left, top, left + 20.0, top + 20.0);
                    let bordered =
                        theme::bordered(button, theme::c().surface, theme::c().border, 3.0, 1.0);
                    let control = if let Some(controls) = &controls {
                        let id = controls.id(block_index, "action", index);
                        hover_targets.push(id.clone());
                        Element::new(move |cx| {
                            UiElement::panel(id, button, VisualStyle::default())
                                .children(cx.children)
                        })
                        .animation(HOVER_FRAME)
                        .paint_bounds(paint_bounds.union(button))
                        .child(bordered)
                    } else {
                        bordered
                    };
                    let commands = git.clone();
                    let root = item.document.repository_root.clone();
                    let file = item.document.path.clone();
                    let target = item.document.target;
                    actions_layer = actions_layer.child(
                        control
                            .key(format!("diff-block-{}-{block_index}-{index}", item.index))
                            .semantics(Semantics::new(SemanticRole::Button).name(label))
                            .event_policy(EventPolicy::INTERACTIVE)
                            .cursor(CursorIcon::Pointer)
                            .on_click(move |cx: &mut lgui::core::UiEventContext| {
                                if action == BlockAction::Discard {
                                    commands.dispatch(GitCommand::RequestDiscardBlock {
                                        root: root.clone(),
                                        path: file.clone(),
                                        block,
                                    });
                                } else {
                                    commands.dispatch(GitCommand::ApplyBlock {
                                        root: root.clone(),
                                        path: file.clone(),
                                        target,
                                        block,
                                        action,
                                    });
                                }
                                cx.stop_propagation();
                            })
                            .child(text(
                                button,
                                arrow,
                                theme::mono(theme::c().text_soft, theme::SMALL).centered(),
                            )),
                    );
                }
            }
            let hovered = controls.as_ref().is_some_and(|controls| {
                let cx = controls.context;
                hover_targets
                    .iter()
                    .any(|id| cx.interaction_flags(id).hovered)
                    && hover_targets
                        .iter()
                        .any(|id| cx.animation_value(id, AnimProperty::Hover) > 0.0)
            });
            if hovered {
                preview = Some(block_index);
            }
            if hovered || focused {
                for (index, (top, bottom)) in ends.into_iter().enumerate() {
                    if top == bottom && top >= rect.top && top <= rect.bottom {
                        anchors = empty_side_marker(anchors, columns.numbers[index], top, border);
                    }
                }
            }
            bridges = bridges.child(
                connector(
                    columns.bridge,
                    ends,
                    emphasize(background, border, hovered, focused),
                    border,
                )
                .key(format!("diff-curve-{}-{block_index}", item.index)),
            );
        }
    }
    let mut out = clip(rect, 0.0, 0.0).child(panel(
        columns.gutter,
        VisualStyle::filled(theme::c().sidebar),
    ));
    for side in [DiffSide::Old, DiffSide::New] {
        let index = side_index(side);
        let code = columns.code(side);
        let mut lines = clip(code, 0.0, 0.0);
        let mut numbers = clip(columns.numbers[index], 0.0, 0.0);
        let first = offsets[index].floor() as usize;
        let last = (offsets[index] + rect.height() / row_h()).ceil() as usize;
        let start = layout
            .sections
            .partition_point(|item| item.end[index] < first);
        for item in layout.sections.iter().skip(start) {
            if item.top[index] > last {
                break;
            }
            if item.closed {
                continue;
            }
            let rows = item.document.split_projection().rows(side);
            if rows.is_empty() && item.document.split_rows.is_empty() {
                let top = rect.top + (item.body[index] as f32 - offsets[index]) * row_h();
                let message = layout_message(item);
                lines = lines.child(text(
                    UiRect::new(code.left + 12.0, top, code.right - 12.0, top + row_h()),
                    message,
                    theme::mono(theme::c().text_dim, theme::SMALL),
                ));
            }
            let from = first.saturating_sub(item.body[index]);
            let to = (last + 1).saturating_sub(item.body[index]).min(rows.len());
            for (compact, &shared) in rows.iter().enumerate().take(to).skip(from) {
                let row = &item.document.split_rows[shared];
                let top = rect.top
                    + (item.body[index] as f32 + compact as f32 - offsets[index]) * row_h();
                let row_rect = UiRect::new(code.left, top, code.right, top + row_h());
                if let Some(hunk) = &row.hunk {
                    lines = lines
                        .child(row_background(row_rect, theme::c().surface))
                        .child(code_line(
                            code,
                            code.left + theme::CODE_PAD,
                            top,
                            hunk,
                            theme::c().text_dim,
                        ));
                    continue;
                }
                let (value, number) = match side {
                    DiffSide::Old => (row.old_text.as_deref(), row.old_line),
                    DiffSide::New => (row.new_text.as_deref(), row.new_line),
                };
                let Some(value) = value else {
                    continue;
                };
                let mut number_color = theme::c().text_faint;
                let color = if row.changed {
                    let projection = item.document.split_projection();
                    let block = projection
                        .changes
                        .partition_point(|range| range.end <= shared);
                    let change = &projection.changes[block];
                    let modified = is_modified(item.document, change);
                    let (background, foreground, border) = change_colors(modified, side);
                    let hovered = preview == Some(block);
                    let focused = locked == Some(block);
                    let background = emphasize(background, border, hovered, focused);
                    lines = lines.child(row_background(row_rect, background));
                    if hovered || focused {
                        let gutter = columns.numbers[index];
                        number_color = theme::c().text_bright;
                        numbers = numbers.child(row_background(
                            UiRect::new(gutter.left, top, gutter.right, top + row_h()),
                            mix(theme::c().sidebar, border),
                        ));
                    }
                    foreground
                } else {
                    theme::c().text_soft
                };
                lines = lines.child(code_line(
                    code,
                    code.left + theme::CODE_PAD - scroll_x,
                    top,
                    value,
                    color,
                ));
                numbers = numbers.child(line_number(
                    columns.numbers[index].left,
                    top,
                    number,
                    number_color,
                ));
            }
        }
        out = out.child(lines).child(numbers);
    }
    out = out.child(bridges).child(anchors);
    for x in [
        columns.gutter.left,
        columns.bridge.left,
        columns.bridge.right,
        columns.gutter.right,
    ] {
        out = out.child(rule(x, rect.top, rect.bottom));
    }
    if !targets.is_empty() {
        out = out.child(clip(rect, 0.0, 0.0).children(targets));
    }
    out.child(actions_layer)
}

impl ChangeControls<'_> {
    fn id(&self, block: usize, kind: &str, part: usize) -> UiId {
        UiId::owned(format!(
            "split-change-{:?}-{:p}-{block}-{kind}-{part}",
            self.interaction.view,
            Arc::as_ptr(self.document),
        ))
    }

    fn reveal_y(&self, rect: UiRect, ends: [(f32, f32); 2], start: usize) -> Option<f32> {
        // Both columns use the same shared-row anchor at the upper third.
        // Moving that anchor to the change start brings its compact rows into phase.
        ends.into_iter()
            .any(|(top, bottom)| top >= rect.bottom || (top < rect.top && bottom <= rect.top))
            .then(|| {
                (((self.document_top + start) as f32 * row_h())
                    - self.interaction.viewport_height / 3.0)
                    .clamp(0.0, self.interaction.max_y)
            })
    }

    fn target(
        &self,
        id: UiId,
        rect: UiRect,
        block: usize,
        reveal_y: Option<f32>,
        focused: bool,
    ) -> Element {
        let state = self.interaction.state.clone();
        let editor_focus = self.interaction.focus.clone();
        let pane = self.interaction.pane;
        let focus = ChangeFocus {
            view: self.interaction.view,
            document: Arc::downgrade(self.document),
            block,
        };
        Element::new(move |_| UiElement::panel(id, rect, VisualStyle::default()))
            .event_policy(EventPolicy {
                focus: false,
                ..EventPolicy::INTERACTIVE
            })
            .cursor(CursorIcon::Pointer)
            .animation(HOVER_FRAME)
            .semantics(Semantics::new(SemanticRole::Button).name(if focused {
                "Clear change focus"
            } else {
                "Focus change"
            }))
            .on_pointer_down_with_button(|cx, _, button| {
                if button == PointerButton::Left {
                    cx.stop_propagation();
                }
            })
            .on_click(move |cx: &mut lgui::core::UiEventContext| {
                state.run(Command::FocusChange(pane, focus.clone(), reveal_y));
                editor_focus.focus();
                cx.stop_propagation();
            })
    }
}

/// A vertical line centred on `x` and snapped like the resize divider drawn
/// over the first gutter edge (`resize_handle`). Both must cover the same
/// physical pixels, or the two lines sit side by side and read as one thick one.
fn rule(x: f32, top: f32, bottom: f32) -> Element {
    Element::new(move |cx| {
        let scale = cx.context.scale();
        let factor = scale.factor();
        let left = scale.physical_value(x - 0.5) as f32 / factor;
        let width = scale.physical_length(1.0) as f32 / factor;
        let top = scale.physical_value(top) as f32 / factor;
        let bottom = scale.physical_value(bottom) as f32 / factor;
        UiElement::panel(
            cx.id,
            UiRect::new(left, top, left + width, bottom),
            VisualStyle::filled(theme::c().border),
        )
    })
}

fn change_areas(columns: Columns, ends: [(f32, f32); 2]) -> Vec<UiRect> {
    let mut areas = Vec::with_capacity(10);
    for (number, (top, bottom)) in columns.numbers.into_iter().zip(ends) {
        // Keep a stable part index when one side has no rows.
        areas.push(UiRect::new(number.left, top, number.right, bottom));
    }
    // Lgui hit regions are rectangular. Narrow strips follow the curve without
    // making the empty corners of a sloped connector clickable.
    const STRIPS: usize = 8;
    let bridge = columns.bridge;
    let edge = |t: f32| {
        let y = t * t * (3.0 - 2.0 * t);
        let [(a, b), (c, d)] = ends;
        (a + (c - a) * y, b + (d - b) * y)
    };
    for strip in 0..STRIPS {
        let from = strip as f32 / STRIPS as f32;
        let to = (strip + 1) as f32 / STRIPS as f32;
        let [(top1, bottom1), (top2, bottom2)] = [edge(from), edge(to)];
        areas.push(UiRect::new(
            bridge.left + bridge.width() * from,
            top1.min(top2) - 2.0,
            bridge.left + bridge.width() * to,
            bottom1.max(bottom2) + 2.0,
        ));
    }
    areas
}

fn empty_side_marker(mut layer: Element, number: UiRect, y: f32, color: Color) -> Element {
    let y = y.clamp(number.top, (number.bottom - 1.0).max(number.top));
    let mut x = number.left + 2.0;
    while x < number.right - 2.0 {
        layer = layer.child(row_background(
            UiRect::new(x, y, (x + 4.0).min(number.right - 2.0), y + 1.0),
            color,
        ));
        x += 7.0;
    }
    layer
}

fn is_modified(document: &DiffDocument, change: &std::ops::Range<usize>) -> bool {
    let projection = document.split_projection();
    [DiffSide::Old, DiffSide::New].into_iter().all(|side| {
        projection.position(side, change.end as f32)
            > projection.position(side, change.start as f32)
    })
}

fn layout_message(item: &Section<'_>) -> String {
    if let Some(error) = item.error {
        error.to_owned()
    } else if item.document.binary {
        "Binary file changed.".into()
    } else if item.document.target == DiffTarget::HeadToWorktree {
        "Matches the current version. Mark it as resolved to finish.".into()
    } else {
        "No textual changes.".into()
    }
}

fn mix(base: Color, accent: Color) -> Color {
    let channel =
        |shift: u32| (((base.0 >> shift) & 255u32) * 4 + ((accent.0 >> shift) & 255u32)) / 5;
    Color(channel(16) << 16 | channel(8) << 8 | channel(0))
}

fn emphasize(base: Color, accent: Color, hovered: bool, focused: bool) -> Color {
    if focused {
        mix(mix(base, accent), accent)
    } else if hovered {
        mix(base, accent)
    } else {
        base
    }
}

fn change_colors(modified: bool, side: DiffSide) -> (Color, Color, Color) {
    let colors = theme::c();
    if modified {
        (
            mix(colors.bg, colors.diff.mod_mark),
            colors.text_soft,
            colors.diff.mod_mark,
        )
    } else {
        match side {
            DiffSide::Old => (colors.diff.del_bg, colors.diff.del_fg, colors.diff.del_mark),
            DiffSide::New => (colors.diff.add_bg, colors.diff.add_fg, colors.diff.add_mark),
        }
    }
}

fn connector(rect: UiRect, ends: [(f32, f32); 2], fill: Color, border: Color) -> Element {
    let left = rect.left;
    let right = rect.right;
    // With equally spaced x controls the curve can be sampled by x directly.
    let c1 = left + rect.width() / 3.0;
    let c2 = right - rect.width() / 3.0;
    let [(top1, bottom1), (top2, bottom2)] = ends.map(|(top, bottom)| (top, bottom.max(top + 1.0)));
    path(
        UiRect::new(left, top1.min(top2), right, bottom1.max(bottom2)),
        UiPath::new([
            UiPathCommand::MoveTo(Point::new(left, top1)),
            UiPathCommand::CubicTo {
                control1: Point::new(c1, top1),
                control2: Point::new(c2, top2),
                to: Point::new(right, top2),
            },
            UiPathCommand::LineTo(Point::new(right, bottom2)),
            UiPathCommand::CubicTo {
                control1: Point::new(c2, bottom2),
                control2: Point::new(c1, bottom1),
                to: Point::new(left, bottom1),
            },
            UiPathCommand::Close,
        ]),
        PathStyle {
            fill: Some(fill),
            fill_alpha: 255,
            stroke: Some(Stroke::new(border, 1.0, 100)),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lgui::application::AppView;
    use lgui::core::UiScale;
    use lgui::prelude::group;
    use lgui::session::UiSession;

    fn physical_span(session: &UiSession, scale: UiScale) -> (i32, i32) {
        let node = session
            .tree()
            .nodes()
            .iter()
            .find(|n| n.style.fill == Some(theme::c().border))
            .unwrap();
        let rect = scale.physical_rect(node.layout_rect);
        (rect.left, rect.right)
    }

    #[test]
    fn gutter_rules_cover_the_same_pixels_as_the_resize_divider_over_them() {
        let viewport = UiRect::new(0.0, 0.0, 200.0, 40.0);
        for factor in [1.0, 1.25, 1.5, 1.75, 2.0] {
            let scale = UiScale::new(factor);
            for x in [10.0, 10.3, 10.5, 10.7, 77.4, 120.9] {
                let view: AppView = Arc::new(move |_| group(viewport).child(rule(x, 0.0, 40.0)));
                let mut session = UiSession::new();
                session.render_view(&view, viewport, scale);
                // The divider's line: centred on x, snapped by its left edge.
                let left = scale.physical_value(x - 0.5);
                let expected = (left, left + scale.physical_length(1.0));
                assert_eq!(
                    physical_span(&session, scale),
                    expected,
                    "x {x} at {factor}"
                );
            }
        }
    }
}
