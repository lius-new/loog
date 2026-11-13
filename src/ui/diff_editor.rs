//! Read-only inline and side-by-side diff editor.

use loog_workspace_api::ResourceRef;

use lgui::core::{
    Color, CursorIcon, EventPolicy, IconStyle, SemanticRole, Semantics, UiElement, UiFocusHandle,
    UiId, WheelUnit, clip, precompiled,
};
use lgui::prelude::{Element, TextAlign, UiRect, VisualStyle, panel, text};

use crate::domain::pane_layout::PaneId;
use crate::features::git::DiffTarget;
use crate::features::git::actions::BlockAction;
use crate::features::git::diff_document::{ChangesLayout, DiffDocument, DiffRow, DiffRowKind};
use crate::ui::chrome::diff_binding::{Binding, Command};
use crate::ui::diff_selection::Geometry;
use crate::ui::diff_split::Interaction as SplitInteraction;
use crate::ui::editor::view::{self as editor_view, SCROLLBAR_SIZE, ScrollMetrics};
use crate::ui::fonts;
use crate::ui::git::commands::{Binding as GitBinding, Command as GitCommand};
use crate::ui::theme;

pub(crate) const TOOLBAR_H: f32 = 34.0;
pub(crate) const NUMBER_W: f32 = 44.0;
const MARKER_W: f32 = 20.0;
const BUTTON_H: f32 = 22.0;
const BUTTON_GAP: f32 = 4.0;

/// Height of one diff row; rows grow with the editor font size.
pub(crate) fn row_h() -> f32 {
    crate::features::git::diff_document::row_height_for(crate::ui::fonts::editor().size)
}

/// Render the diff shown by `pane` into `rect`.
pub fn render(
    pane: PaneId,
    rect: UiRect,
    state: Binding,
    git_commands: GitBinding,
    surface_id: UiId,
    editor_focus: UiFocusHandle,
    scrollbars: crate::ui::editor_binding::Binding,
) -> Element {
    let app = state.snapshot();
    let Some(view) = app.workspace.pane(pane).and_then(|pane| pane.active()) else {
        return panel(rect, VisualStyle::filled(theme::c().bg));
    };
    let Some(record) = app.workspace.diff_record(view) else {
        return panel(rect, VisualStyle::filled(theme::c().bg));
    };
    let document = &record.content;

    let split = app.split;
    let collapsed = app.workspace.collapsed_diffs(pane);
    let overview = document
        .sections
        .as_ref()
        .map(|_| document.changes_layout(split, &collapsed));
    let starts = overview.as_ref().map_or_else(
        || document.change_starts(split),
        |layout| layout.starts.clone(),
    );
    let source_label = document.source_label();
    let absolute_path = document.sections.is_none().then(|| document.resource());
    let path_label = document.sections.as_ref().map_or_else(
        || document.path_label(),
        |_| {
            let files = document.changed_file_count();
            match &document.revision {
                Some(revision) => format!(
                    "Commit {} · {files} files",
                    crate::features::git::diff_document::short_oid(revision)
                ),
                None => format!("All Changes · {files} files"),
            }
        },
    );
    let additions = document.additions();
    let deletions = document.deletions();
    let (stored_x, stored_y) = app.workspace.scroll(pane);
    // The pinned file heading covers the top rows of the all-files view.
    let heading_inset = if overview.is_some() {
        file_heading_h()
    } else {
        0.0
    };
    let current_line = ((stored_y + heading_inset) / row_h()).floor() as usize;
    let current_change = current_change_number(&starts, current_line);

    let body_top = rect.top + TOOLBAR_H;
    let body_rect = UiRect::new(rect.left, body_top, rect.right, rect.bottom);
    let row_count = if let Some(overview) = &overview {
        overview.row_count
    } else if split {
        document.split_rows.len()
    } else {
        document.rows.len()
    };
    let metrics = scroll_metrics(
        body_rect,
        split,
        row_count,
        document.longest_line(split),
        app.editor.diff_split.ratio,
    );
    let max_y = metrics.max_y;
    let max_x = metrics.max_x;
    let scroll_x = stored_x.clamp(0.0, max_x);
    let scroll_y = stored_y.clamp(0.0, max_y);

    let wheel_state = state.clone();
    let mut root = Element::new(move |cx| {
        UiElement::panel(surface_id, rect, VisualStyle::filled(theme::c().bg))
            .semantics(Semantics::new(SemanticRole::Group).name("Changes view"))
            .children(cx.children)
    })
    .event_policy(EventPolicy::INTERACTIVE)
    .on_wheel(move |cx, delta| {
        let (step_x, step_y) = match delta.unit {
            WheelUnit::Lines => (delta.x * row_h() * 3.0, delta.y * row_h() * 3.0),
            WheelUnit::Pixels => (delta.x, delta.y),
        };
        wheel_state.run(Command::Wheel(pane, step_x, step_y, max_x, max_y));
        cx.stop_propagation();
    });
    let pointer_state = state.clone();
    let pointer_focus = editor_focus.clone();
    root = root.on_pointer_down(move |_cx, _pointer| {
        pointer_state.run(Command::Activate(pane));
        pointer_focus.focus();
    });
    if app
        .editor
        .change_focus
        .as_ref()
        .is_some_and(|focus| focus.view == view)
    {
        let dismiss = state.clone();
        root = root.on_outside_pointer(move |_, _, _| {
            dismiss.run(Command::DismissChangeFocus(view));
        });
    }
    let select_state = state.clone();
    let split_columns = super::diff_split::Columns::new(body_rect, app.editor.diff_split.ratio);
    let mirrored_scrollbar = split && document.sections.is_none();
    root = root.on_pointer_down_with_button(move |_cx, pointer, button| {
        let point = pointer.point;
        let over_scrollbar = (max_y > 0.0 && point.x >= body_rect.right - SCROLLBAR_SIZE)
            || (mirrored_scrollbar
                && max_y > 0.0
                && point.x >= split_columns.old.right - SCROLLBAR_SIZE
                && point.x < split_columns.old.right)
            || (max_x > 0.0 && point.y >= body_rect.bottom - SCROLLBAR_SIZE);
        if button == lgui::core::PointerButton::Left && point.y >= body_rect.top && !over_scrollbar
        {
            select_state.run(Command::SelectStart(pane, body_rect, (point.x, point.y)));
        }
    });
    let drag_state = state.clone();
    root = root.on_pointer_drag(move |_cx, pointer| {
        drag_state.run(Command::SelectDrag(
            pane,
            body_rect,
            (pointer.point.x, pointer.point.y),
        ));
    });
    let end_state = state.clone();
    root = root.on_pointer_up(move |_cx, _pointer| {
        end_state.run(Command::SelectEnd);
    });
    let focus_state = state.clone();
    let blur_state = state.clone();
    root = root
        .on_focus(move |_| {
            focus_state.run(Command::Focus(true));
        })
        .on_blur(move |_| {
            blur_state.run(Command::Focus(false));
        });

    root = root.child(toolbar(
        pane,
        UiRect::new(rect.left, rect.top, rect.right, rect.top + TOOLBAR_H),
        state.clone(),
        editor_focus.clone(),
        path_label,
        additions,
        deletions,
        document.loading.is_some(),
        split,
        app.change_highlight,
        starts.clone(),
        current_change,
        absolute_path,
        document
            .sections
            .as_ref()
            .filter(|sections| !sections.is_empty())
            .map(|sections| {
                sections
                    .iter()
                    .all(|section| collapsed.contains(&section.request()))
            }),
    ));

    if row_count == 0 {
        if overview.is_some() {
            return root.child(text(
                UiRect::new(
                    body_rect.left + 24.0,
                    body_rect.top + 24.0,
                    body_rect.right - 24.0,
                    body_rect.top + 64.0,
                ),
                "No changes in this repository.",
                theme::mono(theme::c().text_dim, theme::UI_SIZE),
            ));
        }
        let label = format!("{} — {source_label}", document.path_label());
        let message = if document.binary {
            format!("Binary file changed: {label}")
        } else if document.target == DiffTarget::HeadToWorktree {
            // Only unmerged files are compared with HEAD. An empty diff means
            // the text is the current side's, not that nothing is left to do.
            format!("Matches the current version, still unresolved: {label}")
        } else {
            format!("No textual changes: {label}")
        };
        return root.child(text(
            UiRect::new(
                body_rect.left + 24.0,
                body_rect.top + 24.0,
                body_rect.right - 24.0,
                body_rect.top + 64.0,
            ),
            message,
            theme::mono(theme::c().text_dim, theme::UI_SIZE),
        ));
    }

    // Rows run under the overlay scrollbars, whose tracks are transparent.
    let interaction = SplitInteraction {
        pane,
        view,
        state: state.clone(),
        focus: editor_focus.clone(),
        enabled: app.change_highlight,
        locked: app.editor.change_focus,
        viewport_height: body_rect.height(),
        max_y,
    };
    root = if let Some(overview) = &overview {
        root.child(render_changes(
            pane,
            body_rect,
            document,
            overview,
            split,
            scroll_x,
            scroll_y,
            app.editor.diff_split.ratio,
            &state,
            &git_commands,
            editor_focus,
            &interaction,
        ))
    } else if split {
        root.child(super::diff_split::render(
            body_rect,
            document,
            scroll_x,
            scroll_y,
            app.editor.diff_split.ratio,
            &git_commands,
            &interaction,
            0,
        ))
    } else {
        root.child(render_inline(body_rect, &document.rows, scroll_x, scroll_y))
    };

    if let Some(selection) = app
        .editor
        .selection
        .filter(|selection| selection.pane == pane && !selection.is_empty())
    {
        root = root.child(crate::ui::diff_selection::layer(
            document,
            overview.as_ref(),
            Geometry {
                body: body_rect,
                split,
                ratio: app.editor.diff_split.ratio,
                scroll_x,
                scroll_y,
            },
            &selection,
        ));
    }

    if !split && !document.blocks.is_empty() {
        root = root.child(block_buttons(
            body_rect,
            document,
            &starts,
            scroll_y,
            &state,
            &git_commands,
        ));
    }

    if split {
        let columns = super::diff_split::Columns::new(body_rect, app.editor.diff_split.ratio);
        let sash_offset = columns.gutter.width() / 2.0;
        // A file heading spans the whole Changes view; the resize divider
        // belongs only to the code below it.
        let spans = overview.as_ref().map_or_else(
            || vec![(body_rect.top, body_rect.bottom)],
            |layout| {
                layout
                    .sections
                    .iter()
                    .filter_map(|item| {
                        let top = body_rect.top + item.body_top as f32 * row_h() - scroll_y;
                        let bottom = top + item.body_rows as f32 * row_h();
                        let top = top.max(body_rect.top);
                        let bottom = bottom.min(body_rect.bottom);
                        (bottom > top).then_some((top, bottom))
                    })
                    .collect()
            },
        );
        for (index, span) in spans.into_iter().enumerate() {
            let down = state.clone();
            let moving = state.clone();
            let up = state.clone();
            root = root.child(crate::ui::pane_sash::horizontal(
                format!("git-split-{}-{index}", pane.get()),
                columns.gutter.left,
                span,
                app.editor.diff_split.drag == Some(pane),
                move || {
                    down.run(Command::SplitStart(pane));
                },
                move |x| {
                    moving.run(Command::SplitDrag(pane, body_rect, x + sash_offset));
                },
                move || {
                    up.run(Command::SplitEnd);
                },
            ));
        }
    }

    let emphasized = app.editor.editor_hovered == Some(pane)
        || (app.workspace.active_pane() == pane
            && (app.editor.editor_vertical_scrollbar_dragging
                || app.editor.editor_horizontal_scrollbar_dragging));
    if !split && document.sections.is_none() {
        let marks = document.inline_change_marks();
        if !marks.is_empty() && marks.len() <= 10_000 {
            root = root.child(editor_view::overview_ruler(
                body_rect,
                metrics,
                max_x > 0.0,
                row_h(),
                marks.iter().map(|&(first, lines, change)| {
                    (first, lines, editor_view::change_color(change))
                }),
            ));
        }
    }
    if max_y > 0.0 {
        if split && document.sections.is_none() {
            let columns = super::diff_split::Columns::new(body_rect, app.editor.diff_split.ratio);
            root = root.child(
                editor_view::vertical_scrollbar(
                    pane,
                    columns.old,
                    metrics,
                    scroll_y,
                    max_x > 0.0,
                    emphasized,
                    scrollbars.clone(),
                )
                .key(format!("diff-old-scrollbar-{}", pane.get())),
            );
        }
        root = root.child(editor_view::vertical_scrollbar(
            pane,
            body_rect,
            metrics,
            scroll_y,
            max_x > 0.0,
            emphasized,
            scrollbars.clone(),
        ));
    }
    if max_x > 0.0 {
        root = root.child(editor_view::horizontal_scrollbar(
            pane,
            body_rect,
            code_left(body_rect, split),
            metrics,
            scroll_x,
            max_y > 0.0,
            emphasized,
            scrollbars.clone(),
        ));
    }
    if max_x > 0.0 && max_y > 0.0 {
        root = root.child(editor_view::scrollbar_corner(pane, body_rect));
    }

    root
}

pub(crate) fn code_left(rect: UiRect, split: bool) -> f32 {
    rect.left
        + if split {
            0.0
        } else {
            NUMBER_W * 2.0 + MARKER_W
        }
}

pub(crate) fn split_middle(rect: UiRect, ratio: f32) -> f32 {
    crate::ui::pane_sash::split_position(rect.left, rect.right, rect.left + rect.width() * ratio)
}

pub(crate) fn scroll_metrics(
    rect: UiRect,
    split: bool,
    rows: usize,
    longest_line: usize,
    ratio: f32,
) -> ScrollMetrics {
    let content_h = rows as f32 * row_h();
    let content_w = longest_line as f32 * fonts::editor().char_w + theme::CODE_PAD * 2.0;
    let mut viewport_h = rect.height().max(1.0);
    let mut viewport_w = 1.0;
    for _ in 0..3 {
        // Columns span the full width; only the text under the vertical
        // scrollbar must still scroll into view.
        let reserved = if content_h > viewport_h {
            SCROLLBAR_SIZE
        } else {
            0.0
        };
        viewport_w = (if split {
            let columns = super::diff_split::Columns::new(rect, ratio);
            columns.old.width().min(columns.new.width()) - reserved
        } else {
            rect.width() - reserved - NUMBER_W * 2.0 - MARKER_W
        })
        .max(1.0);
        viewport_h = (rect.height()
            - if content_w > viewport_w {
                SCROLLBAR_SIZE
            } else {
                0.0
            })
        .max(1.0);
    }
    ScrollMetrics {
        viewport_w,
        viewport_h,
        content_w: content_w.max(viewport_w),
        content_h,
        max_x: (content_w - viewport_w).max(0.0),
        max_y: (content_h - viewport_h).max(0.0),
    }
}

#[allow(clippy::too_many_arguments)]
fn toolbar(
    pane: PaneId,
    rect: UiRect,
    state: Binding,
    editor_focus: UiFocusHandle,
    path: String,
    additions: usize,
    deletions: usize,
    loading: bool,
    split: bool,
    change_highlight: bool,
    starts: std::sync::Arc<Vec<usize>>,
    current_change: Option<usize>,
    absolute_path: Option<ResourceRef>,
    all_collapsed: Option<bool>,
) -> Element {
    let mut bar = panel(rect, VisualStyle::filled(theme::c().sidebar));
    bar = bar.child(panel(
        UiRect::new(rect.left, rect.bottom - 1.0, rect.right, rect.bottom),
        VisualStyle::filled(theme::c().border),
    ));

    let compact = rect.width() < 560.0;
    let button_top = rect.top + (rect.height() - BUTTON_H) / 2.0;
    let mut right = rect.right - 8.0;

    if split {
        let toggle_rect = take_button_rect(&mut right, button_top, 24.0);
        let toggle_state = state.clone();
        let mut semantics = Semantics::new(SemanticRole::CheckBox).name("Highlight linked changes");
        semantics.state.checked = Some(change_highlight);
        let (style, icon, color) = if change_highlight {
            (
                VisualStyle::filled(theme::c().active_line),
                "diff-link",
                theme::c().accent,
            )
        } else {
            (
                VisualStyle::default(),
                "diff-link-off",
                theme::c().text_muted,
            )
        };
        bar = bar.child(
            panel(toggle_rect, style.radius(3.0))
                .key("diff-change-highlight")
                .event_policy(EventPolicy::INTERACTIVE)
                .cursor(CursorIcon::Pointer)
                .semantics(semantics)
                .on_click(move || {
                    toggle_state.run(Command::ToggleChangeHighlight);
                })
                .child(crate::ui::drawer::drawer_icon(
                    UiId::owned(format!("diff-change-highlight-icon-{}", pane.get())),
                    icon,
                    UiRect::new(
                        toggle_rect.left + 5.0,
                        toggle_rect.top + 4.0,
                        toggle_rect.right - 5.0,
                        toggle_rect.bottom - 4.0,
                    ),
                    color,
                )),
        );
    }

    if let Some(absolute_path) = absolute_path {
        let open_rect = take_button_rect(&mut right, button_top, if compact { 42.0 } else { 68.0 });
        let open_state = state.clone();
        bar = bar.child(
            toolbar_button(open_rect, if compact { "Open" } else { "Open File" }, false).on_click(
                move || {
                    if open_state.run(Command::OpenResource(absolute_path.clone())) {
                        editor_focus.focus();
                    }
                },
            ),
        );
    }

    if let Some(all_collapsed) = all_collapsed {
        let collapse_rect = take_button_rect(&mut right, button_top, 24.0);
        let collapse_state = state.clone();
        let (label, icon) = if all_collapsed {
            ("Expand All Files", "expand-all")
        } else {
            ("Collapse All Files", "search-collapse")
        };
        bar = bar.child(
            panel(collapse_rect, VisualStyle::default().radius(3.0))
                .key("diff-collapse-all")
                .event_policy(EventPolicy::INTERACTIVE)
                .cursor(CursorIcon::Pointer)
                .semantics(Semantics::new(SemanticRole::Button).name(label))
                .on_click(move || {
                    collapse_state.run(Command::ToggleAll(pane));
                })
                .child(crate::ui::drawer::drawer_icon(
                    "diff-collapse-all-icon",
                    icon,
                    UiRect::new(
                        collapse_rect.left + 5.0,
                        collapse_rect.top + 4.0,
                        collapse_rect.right - 5.0,
                        collapse_rect.bottom - 4.0,
                    ),
                    theme::c().text_dim,
                )),
        );
    }

    let split_rect = take_button_rect(&mut right, button_top, if compact { 28.0 } else { 42.0 });
    let split_state = state.clone();
    bar = bar.child(
        toolbar_button(split_rect, if compact { "S" } else { "Split" }, split).on_click(
            move || {
                split_state.run(Command::Layout(pane, true));
            },
        ),
    );

    let inline_rect = take_button_rect(&mut right, button_top, if compact { 28.0 } else { 46.0 });
    let inline_state = state.clone();
    bar = bar.child(
        toolbar_button(inline_rect, if compact { "I" } else { "Inline" }, !split).on_click(
            move || {
                inline_state.run(Command::Layout(pane, false));
            },
        ),
    );

    let next_rect = take_button_rect(&mut right, button_top, 24.0);
    let next_state = state.clone();
    let next_starts = starts.clone();
    bar = bar.child(toolbar_button(next_rect, "↓", false).on_click(move || {
        next_state.run(Command::Navigate(pane, next_starts.clone(), true));
    }));

    let prev_rect = take_button_rect(&mut right, button_top, 24.0);
    let prev_state = state;
    let previous_starts = starts.clone();
    bar = bar.child(toolbar_button(prev_rect, "↑", false).on_click(move || {
        prev_state.run(Command::Navigate(pane, previous_starts.clone(), false));
    }));

    let position = current_change.map_or_else(
        || format!("0/{}", starts.len()),
        |current| format!("{current}/{}", starts.len()),
    );
    let position_w = 42.0;
    let position_rect = UiRect::new(right - position_w, rect.top, right, rect.bottom);
    right -= position_w + BUTTON_GAP;
    bar = bar.child(text(
        position_rect,
        position,
        theme::mono_right(theme::c().text_faint, theme::SMALL),
    ));

    let stats = if loading {
        "…".into()
    } else {
        format!("+{additions}  −{deletions}")
    };
    let stats_w = if compact { 62.0 } else { 82.0 };
    let stats_rect = UiRect::new(right - stats_w, rect.top, right, rect.bottom);
    right -= stats_w + 8.0;
    bar = bar.child(text(
        stats_rect,
        stats,
        theme::mono_right(theme::c().text_dim, theme::SMALL),
    ));

    let path_rect = UiRect::new(
        rect.left + 12.0,
        rect.top,
        right.max(rect.left + 12.0),
        rect.bottom,
    );
    bar.child(clip(path_rect, 0.0, 0.0).child(text(
        UiRect::new(
            path_rect.left,
            path_rect.top,
            path_rect.left + path.chars().count() as f32 * theme::UI_CHAR_W + 12.0,
            path_rect.bottom,
        ),
        path,
        theme::mono(theme::c().text_soft, theme::UI_SIZE),
    )))
}

/// Stage, unstage or discard buttons at the right of each change block's
/// first row. `starts` lists those rows in block order.
fn block_buttons(
    rect: UiRect,
    document: &DiffDocument,
    starts: &[usize],
    scroll_y: f32,
    _state: &Binding,
    git_commands: &GitBinding,
) -> Element {
    let actions: &[(BlockAction, &str)] = match document.target {
        DiffTarget::HeadToIndex => &[(BlockAction::Unstage, "Unstage")],
        _ => &[
            (BlockAction::Discard, "Discard"),
            (BlockAction::Stage, "Stage"),
        ],
    };
    let mut layer = clip(rect, 0.0, 0.0);
    for (&row, &block) in starts.iter().zip(&document.blocks) {
        let top = rect.top + row as f32 * row_h() - scroll_y;
        if top + row_h() < rect.top || top > rect.bottom {
            continue;
        }
        let mut right = rect.right - SCROLLBAR_SIZE - 6.0;
        for &(action, label) in actions.iter().rev() {
            let width = super::tabs::measure(label, theme::SMALL, 400) + 14.0;
            let button = UiRect::new(right - width, top + 2.0, right, top + row_h() - 2.0);
            right = button.left - 4.0;
            let click_commands = git_commands.clone();
            let root = document.repository_root.clone();
            let path = document.path.clone();
            let target = document.target;
            let mut style = theme::mono(theme::c().text_soft, theme::SMALL);
            style.align = TextAlign::Center;
            layer = layer.child(
                theme::bordered(button, theme::c().surface, theme::c().border, 3.0, 1.0)
                    .key(format!("diff-block-{row}-{label}"))
                    .event_policy(EventPolicy::INTERACTIVE)
                    .cursor(CursorIcon::Pointer)
                    .on_click(move |cx: &mut lgui::core::UiEventContext| {
                        if action == BlockAction::Discard {
                            click_commands.dispatch(GitCommand::RequestDiscardBlock {
                                root: root.clone(),
                                path: path.clone(),
                                block,
                            });
                        } else {
                            click_commands.dispatch(GitCommand::ApplyBlock {
                                root: root.clone(),
                                path: path.clone(),
                                target,
                                block,
                                action,
                            });
                        }
                        cx.stop_propagation();
                    })
                    .child(text(button, label, style)),
            );
        }
    }
    layer
}

/// Build only visible file sections; the existing inline/split renderers
/// further limit each visible section to the rows inside the viewport.
#[allow(clippy::too_many_arguments)]
/// Height of a file heading in the all-files view, which stays pinned at the top
/// while its file's rows scroll beneath it.
pub(crate) fn file_heading_h() -> f32 {
    row_h() * 2.0
}

fn render_changes(
    pane: PaneId,
    rect: UiRect,
    overview: &DiffDocument,
    layout: &ChangesLayout,
    split: bool,
    scroll_x: f32,
    scroll_y: f32,
    ratio: f32,
    state: &Binding,
    git_commands: &GitBinding,
    editor_focus: UiFocusHandle,
    interaction: &SplitInteraction,
) -> Element {
    let sections = overview.sections.as_ref().unwrap();
    let mut body = clip(rect, 0.0, 0.0);
    // The heading of the file under the top edge, drawn last so it covers rows.
    let mut pinned = None;
    let first = layout
        .sections
        .partition_point(|item| (item.body_top + item.body_rows) as f32 * row_h() < scroll_y);
    for item in layout.sections.iter().skip(first) {
        let top = rect.top + item.top as f32 * row_h() - scroll_y;
        if top - row_h() > rect.bottom {
            break;
        }
        let section = &sections[item.index];
        let document = &section.document;
        let body_end = rect.top + (item.body_top + item.body_rows) as f32 * row_h() - scroll_y;
        // A file whose heading has scrolled off keeps it at the top edge until
        // its last row passes, when the next file's heading pushes it out.
        let is_pinned = item.body_rows > 0 && top < rect.top && body_end > rect.top;
        let top = if is_pinned {
            rect.top.min(body_end - file_heading_h())
        } else {
            top
        };
        let header = UiRect::new(rect.left, top, rect.right, top + file_heading_h());
        let toggle_state = state.clone();
        let request = section.request();
        let toggle_request = request.clone();
        let collapsed = item.body_rows == 0;
        let compact = rect.width() < 360.0;
        let (changes_w, file_w) = if compact { (66.0, 44.0) } else { (100.0, 76.0) };
        let file_rect = UiRect::new(
            rect.right - SCROLLBAR_SIZE - file_w - 8.0,
            top + 9.0,
            rect.right - SCROLLBAR_SIZE - 8.0,
            top + 31.0,
        );
        let action_rect = UiRect::new(
            file_rect.left - BUTTON_GAP - changes_w,
            top + 9.0,
            file_rect.left - BUTTON_GAP,
            top + 31.0,
        );
        let stats_right = action_rect.left - 10.0;
        let stats_w = if rect.width() < 460.0 { 0.0 } else { 96.0 };
        let label_rect = UiRect::new(
            rect.left + 32.0,
            top,
            (stats_right - stats_w).max(rect.left + 32.0),
            header.bottom,
        );
        let path = super::tabs::ellipsize(
            &super::display_path(&document.path),
            (label_rect.width() - 6.0).max(0.0),
            theme::UI_SIZE,
            400,
        );
        let mut file_header = panel(header, VisualStyle::filled(theme::c().sidebar))
            .key(format!("changes-header-{}", item.index))
            .event_policy(EventPolicy::INTERACTIVE)
            .semantics(Semantics::new(SemanticRole::Button).name(format!(
                "{} {}",
                if collapsed { "Expand" } else { "Collapse" },
                document.path.display()
            )))
            .cursor(CursorIcon::Pointer)
            .on_click(move || {
                toggle_state.run(Command::Toggle(pane, toggle_request.clone()));
            })
            .child(panel(
                UiRect::new(
                    header.left,
                    header.bottom - 1.0,
                    header.right,
                    header.bottom,
                ),
                VisualStyle::filled(theme::c().border),
            ))
            .child(text(
                label_rect,
                path,
                theme::mono(theme::c().text, theme::UI_SIZE),
            ));
        file_header = file_header.child(precompiled(
            UiElement::icon(
                UiId::owned(format!("changes-chevron-{}", item.index)),
                theme::icon_rect(UiRect::new(
                    rect.left + 8.0,
                    top + 12.0,
                    rect.left + 24.0,
                    top + 28.0,
                )),
                if collapsed {
                    "chevron-right"
                } else {
                    "chevron-down"
                },
            )
            .icon_style(IconStyle::new(theme::c().text_dim)),
        ));
        if stats_w > 0.0 {
            let stats = if section.loading {
                "…".into()
            } else if document.binary {
                "Binary".into()
            } else {
                format!("+{}  −{}", document.additions(), document.deletions())
            };
            file_header = file_header.child(text(
                UiRect::new(stats_right - stats_w, top, stats_right, header.bottom),
                stats,
                theme::mono_right(theme::c().text_dim, theme::SMALL),
            ));
        }
        let open_commands = git_commands.clone();
        let focus = editor_focus.clone();
        let file_commands = git_commands.clone();
        let file_focus = editor_focus.clone();
        let file_path = request.repository_root.join(&request.path);
        let untracked = section.group == crate::features::git::ChangeGroup::Untracked;
        let revision = overview.revision.clone();
        file_header = file_header.child(
            toolbar_button(
                action_rect,
                if rect.width() < 360.0 {
                    "Changes"
                } else {
                    "Open Changes"
                },
                false,
            )
            .semantics(
                Semantics::new(SemanticRole::Button)
                    .name(format!("Open Changes for {}", document.path.display())),
            )
            .on_click(move |cx: &mut lgui::core::UiEventContext| {
                cx.stop_propagation();
                match &revision {
                    Some(revision) => {
                        open_commands.dispatch(GitCommand::OpenCommitDiff {
                            root: request.repository_root.clone(),
                            oid: revision.clone(),
                            path: request.path.clone(),
                        });
                    }
                    None => {
                        open_commands.dispatch(GitCommand::OpenDiff {
                            request: request.clone(),
                            untracked,
                        });
                    }
                }
                focus.focus();
            }),
        );
        file_header = file_header.child(
            toolbar_button(file_rect, if compact { "File" } else { "Open File" }, false)
                .semantics(
                    Semantics::new(SemanticRole::Button)
                        .name(format!("Open File {}", document.path.display())),
                )
                .on_click(move |cx: &mut lgui::core::UiEventContext| {
                    cx.stop_propagation();
                    if file_commands.dispatch(GitCommand::OpenFile {
                        path: file_path.clone(),
                    }) {
                        file_focus.focus();
                    }
                }),
        );
        if is_pinned {
            pinned = Some(file_header);
        } else {
            body = body.child(file_header);
        }
        if collapsed {
            continue;
        }
        let body_top = rect.top + item.body_top as f32 * row_h() - scroll_y;
        let body_bottom = body_top + item.body_rows as f32 * row_h();
        let visible = UiRect::new(
            rect.left,
            body_top.max(rect.top),
            rect.right,
            body_bottom.min(rect.bottom),
        );
        if visible.bottom <= visible.top {
            continue;
        }
        let local_y = visible.top - body_top;
        let mut file_body =
            panel(visible, VisualStyle::default()).key(format!("changes-body-{}", item.index));
        if document.rows.is_empty() {
            let message = section.error.clone().unwrap_or_else(|| {
                if section.loading {
                    "Loading changes…".into()
                } else if document.binary {
                    "Binary file changed.".into()
                } else if section.group == crate::features::git::ChangeGroup::Conflicts {
                    "Matches the current version. Mark it as resolved to finish.".into()
                } else {
                    "No textual changes.".into()
                }
            });
            file_body = file_body.child(text(
                UiRect::new(rect.left + 32.0, body_top, rect.right - 12.0, body_bottom),
                message,
                theme::mono(theme::c().text_dim, theme::SMALL),
            ));
        } else {
            file_body = file_body.child(if split {
                super::diff_split::render(
                    visible,
                    document,
                    scroll_x,
                    local_y,
                    ratio,
                    git_commands,
                    interaction,
                    item.body_top,
                )
            } else {
                render_inline(visible, &document.rows, scroll_x, local_y)
            });
            if !split && !document.blocks.is_empty() {
                file_body = file_body.child(block_buttons(
                    visible,
                    document,
                    &document.change_starts(false),
                    local_y,
                    state,
                    git_commands,
                ));
            }
        }
        body = body.child(file_body);
    }
    if let Some(pinned) = pinned {
        body = body.child(pinned);
    }
    body
}

fn take_button_rect(right: &mut f32, top: f32, width: f32) -> UiRect {
    let rect = UiRect::new(*right - width, top, *right, top + BUTTON_H);
    *right -= width + BUTTON_GAP;
    rect
}

fn toolbar_button(rect: UiRect, label: &'static str, active: bool) -> Element {
    let mut style = theme::mono(
        if active {
            theme::c().text
        } else {
            theme::c().text_dim
        },
        theme::SMALL,
    );
    style.align = TextAlign::Center;
    panel(
        rect,
        if active {
            VisualStyle::filled(theme::c().active_line).radius(3.0)
        } else {
            VisualStyle::default().radius(3.0)
        },
    )
    .event_policy(EventPolicy::INTERACTIVE)
    .cursor(CursorIcon::Pointer)
    .child(text(rect, label, style))
}

pub(crate) fn render_inline(
    rect: UiRect,
    rows: &[DiffRow],
    scroll_x: f32,
    scroll_y: f32,
) -> Element {
    let text_left = rect.left + NUMBER_W * 2.0 + MARKER_W;
    let text_viewport = UiRect::new(text_left, rect.top, rect.right, rect.bottom);
    let start = (scroll_y / row_h()).floor() as usize;
    let end = (((scroll_y + rect.height()) / row_h()).ceil() as usize + 1).min(rows.len());
    let mut body = clip(rect, 0.0, 0.0);

    for (index, row) in rows.iter().enumerate().take(end).skip(start) {
        let top = rect.top + index as f32 * row_h() - scroll_y;
        let row_rect = UiRect::new(rect.left, top, rect.right, top + row_h());
        let (background, foreground, marker) = match row.kind {
            DiffRowKind::Context => (None, theme::c().text_soft, ""),
            DiffRowKind::Addition => (Some(theme::c().diff.add_bg), theme::c().diff.add_fg, "+"),
            DiffRowKind::Deletion => (Some(theme::c().diff.del_bg), theme::c().diff.del_fg, "−"),
            DiffRowKind::Hunk => (Some(theme::c().surface), theme::c().text_dim, ""),
        };
        if let Some(background) = background {
            body = body.child(row_background(row_rect, background));
        }
        if row.kind == DiffRowKind::Hunk {
            body = body.child(text(
                UiRect::new(rect.left + 10.0, top, rect.right - 10.0, top + row_h()),
                row.text.clone(),
                theme::mono(theme::c().text_dim, theme::SMALL),
            ));
            continue;
        }

        body = body
            .child(line_number(
                rect.left,
                top,
                row.old_line,
                theme::c().text_faint,
            ))
            .child(line_number(
                rect.left + NUMBER_W,
                top,
                row.new_line,
                theme::c().text_faint,
            ))
            .child(text(
                UiRect::new(rect.left + NUMBER_W * 2.0, top, text_left, top + row_h()),
                marker,
                fonts::editor().style(foreground),
            ))
            .child(code_line(
                text_viewport,
                text_left + theme::CODE_PAD - scroll_x,
                top,
                &row.text,
                foreground,
            ));
    }

    body.child(vertical_rule(rect.left + NUMBER_W, rect))
        .child(vertical_rule(text_left, rect))
}

#[cfg(test)]
pub(crate) fn render_split(
    rect: UiRect,
    document: &DiffDocument,
    scroll_x: f32,
    scroll_y: f32,
    ratio: f32,
) -> Element {
    super::diff_split::render_document(rect, document, scroll_x, scroll_y, ratio)
}

pub(super) fn row_background(rect: UiRect, color: lgui::core::Color) -> Element {
    Element::new(move |cx| {
        let scale = cx.context.scale();
        // Adjacent fills must share a physical pixel edge, or antialiasing
        // exposes the surface beneath them as a horizontal seam.
        let rect = scale.logical_rect(scale.physical_rect(rect));
        UiElement::panel(cx.id, rect, VisualStyle::filled(color))
    })
}

pub(super) fn line_number(left: f32, top: f32, number: Option<usize>, color: Color) -> Element {
    text(
        UiRect::new(left, top, left + NUMBER_W - 8.0, top + row_h()),
        number.map(|line| line.to_string()).unwrap_or_default(),
        theme::mono_right(color, theme::SMALL),
    )
    .into()
}

pub(super) fn code_line(
    viewport: UiRect,
    left: f32,
    top: f32,
    value: &str,
    color: lgui::core::Color,
) -> Element {
    let (display, _, columns) = editor_view::expand_tabs(value, 0);
    let width = (columns as f32 * fonts::editor().char_w + 16.0).max(viewport.width());
    clip(viewport, 0.0, 0.0).child(text(
        UiRect::new(left, top, left + width, top + row_h()),
        display,
        fonts::editor().style(color),
    ))
}

fn vertical_rule(x: f32, rect: UiRect) -> Element {
    panel(
        UiRect::new(x - 1.0, rect.top, x, rect.bottom),
        VisualStyle::filled(theme::c().border),
    )
}

pub(crate) fn current_change_number(starts: &[usize], current_line: usize) -> Option<usize> {
    if starts.is_empty() {
        return None;
    }
    Some(
        starts
            .iter()
            .rposition(|start| *start <= current_line)
            .map_or(1, |index| index + 1),
    )
}
