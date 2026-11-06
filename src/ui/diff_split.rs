//! Compact split diff with central line numbers and live curved connections.
use lgui::core::{
    CursorIcon, EventPolicy, PathStyle, Point, SemanticRole, Semantics, Stroke, UiPath,
    UiPathCommand, clip, path,
};
use lgui::prelude::{Color, Element, UiRect, VisualStyle, panel, text};

use crate::features::git::DiffTarget;
use crate::features::git::actions::BlockAction;
use crate::features::git::diff_document::DiffDocument;
use crate::features::git::diff_selection::DiffSide;
use crate::features::git::split_layout::{Layout, Section, side_index};
use crate::ui::diff_editor::{
    NUMBER_W, code_line, line_number, row_background, row_h, split_middle,
};
use crate::ui::git::commands::{Binding as GitBinding, Command as GitCommand};
use crate::ui::theme;

const BRIDGE_W: f32 = 56.0;

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
pub(crate) fn render(
    rect: UiRect,
    document: &DiffDocument,
    scroll_x: f32,
    scroll_y: f32,
    ratio: f32,
    git: &GitBinding,
) -> Element {
    paint(
        rect,
        &Layout::new(document, None),
        scroll_x,
        scroll_y,
        ratio,
        Some(git),
    )
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
    )
}

fn paint(
    rect: UiRect,
    layout: &Layout<'_>,
    scroll_x: f32,
    scroll_y: f32,
    ratio: f32,
    git: Option<&GitBinding>,
) -> Element {
    let columns = Columns::new(rect, ratio);
    let offsets = [DiffSide::Old, DiffSide::New]
        .map(|side| layout.offset(side, scroll_y / row_h(), rect.height() / row_h()));
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
                let color = if row.changed {
                    let projection = item.document.split_projection();
                    let change = &projection.changes[projection
                        .changes
                        .partition_point(|range| range.end <= shared)];
                    let modified = is_modified(item.document, change);
                    let (background, foreground, _) = change_colors(modified, side);
                    lines = lines.child(row_background(row_rect, background));
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
                numbers = numbers.child(line_number(columns.numbers[index].left, top, number));
            }
        }
        out = out.child(lines).child(numbers);
    }
    let mut bridges = clip(columns.bridge, 0.0, 0.0);
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
                (top, bottom.max(top + 1.0))
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
            bridges = bridges.child(
                connector(columns.bridge, ends, background, border)
                    .key(format!("diff-curve-{}-{block_index}", item.index)),
            );
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
                    let commands = git.clone();
                    let root = item.document.repository_root.clone();
                    let file = item.document.path.clone();
                    let target = item.document.target;
                    bridges = bridges.child(
                        theme::bordered(button, theme::c().surface, theme::c().border, 3.0, 1.0)
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
        }
    }
    out = out.child(bridges);
    for x in [
        columns.gutter.left,
        columns.bridge.left,
        columns.bridge.right,
        columns.gutter.right,
    ] {
        out = out.child(row_background(
            UiRect::new(x, rect.top, x + 1.0, rect.bottom),
            theme::c().border,
        ));
    }
    out
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
    let c1 = left + rect.width() * 0.3;
    let c2 = left + rect.width() * 0.7;
    let [(top1, bottom1), (top2, bottom2)] = ends;
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
