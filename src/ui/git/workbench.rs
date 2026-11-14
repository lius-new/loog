//! The Git panel below the editor: commit history, stashes, worktrees and
//! tags as tabs over one scrolling list. Source Control keeps the changes and
//! the commit form; this panel holds what is browsed rather than edited.

use lgui::core::{
    CursorIcon, EventPolicy, PhysicalRect, PointerButton, SemanticRole, Semantics, UiElement,
    UiEventKind, UiEventPayload, UiFocusHandle, WheelUnit, clip,
};
use lgui::prelude::{Element, UiRect, VisualStyle, panel, text};

use crate::ui::git::commands::{Binding, Command, Snapshot};
use crate::ui::git::history_detail::Subject;
use crate::ui::git::panel::{
    GitHover, HISTORY, HistoryRow, ROW_H, STASHES, ScrollRegion, TAGS, WORKTREES, history_row,
    history_rows_matching, scroll_region, stash_row, tag_row, visible_rows, visible_worktree_rows,
    worktree_row,
};
use crate::ui::git::state::HistoryPane;
use crate::ui::git::{history_branches, history_detail};
use crate::ui::theme;

pub(crate) const HEADER_H: f32 = theme::TABS_H;
const DEFAULT_H: f32 = 340.0;
const MIN_H: f32 = 120.0;
const TAB_PAD: f32 = 12.0;
const TAB_GAP: f32 = 2.0;
/// Room that stays for the editor above the panel.
const EDITOR_MIN_H: f32 = 160.0;
/// Narrowest panel that still makes room for the commit detail pane.
const DETAIL_MIN_W: f32 = 720.0;
/// Narrowest history list that shows its author and time columns.
const COLUMNS_MIN_W: f32 = 420.0;
const COLUMNS_H: f32 = 22.0;
const LIST_MIN_W: f32 = 240.0;
const BRANCHES_MIN_PANE_W: f32 = 148.0;
const BRANCHES_MAX_PANE_W: f32 = 390.0;
const DETAIL_MIN_PANE_W: f32 = 180.0;
const DETAIL_MAX_PANE_W: f32 = 480.0;
/// Narrowest panel that also shows the branch list.
pub(crate) const BRANCHES_MIN_W: f32 = 900.0;

/// Reserve the bottom header even while collapsed. Expand its content only
/// when there is enough room to preserve the editor above it.
pub(crate) fn split(
    region: UiRect,
    open: bool,
    preferred: Option<f32>,
) -> (UiRect, Option<UiRect>) {
    let max = region.height() - EDITOR_MIN_H;
    if region.height() < HEADER_H {
        return (region, None);
    }
    let height = if open && max >= MIN_H {
        preferred.unwrap_or(DEFAULT_H).clamp(MIN_H, max)
    } else {
        HEADER_H
    };
    let top = region.bottom - height;
    (
        UiRect::new(region.left, region.top, region.right, top),
        Some(UiRect::new(region.left, top, region.right, region.bottom)),
    )
}

pub fn render(
    rect: UiRect,
    state: Binding,
    store: Snapshot,
    editor_focus: UiFocusHandle,
) -> Element {
    let app = state.get();
    let git = store.get();
    let mut root = Element::new(move |cx| {
        let scale = cx.context.scale();
        // The background must end on the same pixel as its separators.
        let painted = scale.logical_rect(scale.physical_rect(rect));
        UiElement::panel(cx.id, painted, VisualStyle::filled(theme::c().sidebar))
            .children(cx.children)
    })
    .key("git-panel");
    // Wide enough for the navigator, which reaches stashes, tags and worktrees.
    let wide = rect.width() >= BRANCHES_MIN_W;
    let panel_tab = if wide { HISTORY } else { app.ui.panel_tab };
    let header = UiRect::new(rect.left, rect.top, rect.right, rect.top + HEADER_H);
    let body = UiRect::new(rect.left, header.bottom, rect.right, rect.bottom);
    let expanded = app.ui.panel_open && rect.height() > HEADER_H;
    root = root.child(crate::ui::components::divider::top(header));

    let tabs = [
        ("History", HISTORY, None),
        (
            "Stashes",
            STASHES,
            git.active().map(|repository| repository.stashes.len()),
        ),
        (
            "Worktrees",
            WORKTREES,
            git.active().map(|repository| repository.worktrees.len()),
        ),
        (
            "Tags",
            TAGS,
            git.active().map(|repository| repository.tags.len()),
        ),
    ]
    .into_iter()
    .filter(|(_, tab, _)| !wide || *tab == HISTORY)
    .map(|(title, tab, count)| {
        let label = match count {
            Some(count) if count > 0 => format!("{title} {count}"),
            _ => title.to_owned(),
        };
        let width = crate::ui::tabs::measure(&label, theme::SMALL, 400) + TAB_PAD * 2.0 + 6.0;
        (tab, label, width)
    })
    .collect::<Vec<_>>();
    // The collapse button owns a fixed slot. Clip both painting and hit testing
    // before it, leaving room for scroll buttons only when tabs overflow.
    let tabs_right = (rect.right - 36.0).max(rect.left);
    let tabs_left = rect.left.min(tabs_right);
    let available = (tabs_right - tabs_left).max(0.0);
    let natural_width = tabs
        .iter()
        .map(|(_, _, width)| width + TAB_GAP)
        .sum::<f32>()
        - TAB_GAP;
    let overflow = natural_width > available;
    let arrow_width = if overflow {
        20.0_f32.min(available / 2.0)
    } else {
        0.0
    };
    let viewport = UiRect::new(
        tabs_left + arrow_width,
        header.top,
        tabs_right - arrow_width,
        header.bottom,
    );
    let mut x = viewport.left;
    let tabs = tabs
        .into_iter()
        .map(|(tab, label, width)| {
            let width = width.min(viewport.width());
            let rect = UiRect::new(x, header.top, x + width, header.bottom);
            x += width + TAB_GAP;
            (tab, label, rect)
        })
        .collect::<Vec<_>>();
    let max_scroll = (x - TAB_GAP - viewport.right).max(0.0);
    let selected_scroll = tabs
        .iter()
        .find(|(tab, _, _)| *tab == panel_tab)
        .map_or(0.0, |(_, _, rect)| {
            (rect.right - viewport.right).clamp(0.0, max_scroll)
        });
    let scroll = app
        .ui
        .panel_tab_scroll
        .unwrap_or(selected_scroll)
        .clamp(0.0, max_scroll);
    let mut layer = clip(viewport, -scroll, 0.0);
    let mut active_tab = None;
    for (tab, label, tab_rect) in tabs {
        if expanded && panel_tab == tab {
            active_tab = Some((tab, tab_rect.translate(-scroll, 0.0)));
        }
        layer = layer.child(tab_button(
            tab_rect,
            tab,
            label,
            expanded && panel_tab == tab,
            state.clone(),
        ));
    }
    let wheel_state = state.clone();
    root = root.child(
        panel(viewport, VisualStyle::default())
            .key("git-panel-tabs")
            .child(layer)
            .on_wheel(move |cx, delta| {
                let delta_axis = if delta.x.abs() > delta.y.abs() {
                    delta.x
                } else {
                    delta.y
                };
                let step = match delta.unit {
                    WheelUnit::Lines => delta_axis * 48.0,
                    WheelUnit::Pixels => delta_axis,
                };
                wheel_state.update(|app| {
                    app.ui.panel_tab_scroll = Some((scroll - step).clamp(0.0, max_scroll))
                });
                cx.stop_propagation();
            }),
    );
    if overflow {
        root = root
            .child(tab_scroll_button(
                UiRect::new(tabs_left, header.top, viewport.left, header.bottom),
                false,
                scroll > 0.0,
                (scroll - viewport.width()).max(0.0),
                state.clone(),
            ))
            .child(tab_scroll_button(
                UiRect::new(viewport.right, header.top, tabs_right, header.bottom),
                true,
                scroll < max_scroll,
                (scroll + viewport.width()).min(max_scroll),
                state.clone(),
            ));
    }
    root = root.child(collapse_button(
        UiRect::new(
            rect.right - 30.0,
            header.top,
            rect.right - 6.0,
            header.bottom,
        ),
        state.clone(),
        expanded,
    ));
    if !expanded {
        // There is no body to separate. The next region owns the shared edge.
        return root;
    }
    let header_bottom = header_bottom_rule(header, viewport, active_tab);

    root.child(render_body(
        body,
        panel_tab,
        state.clone(),
        store,
        editor_focus,
    ))
    .child(resize_handle(rect, state))
    .child(header_bottom)
}

/// Existing history content hosted under the shared bottom-panel header.
pub fn render_content(
    rect: UiRect,
    state: Binding,
    store: Snapshot,
    editor_focus: UiFocusHandle,
) -> Element {
    render_body(rect, HISTORY, state, store, editor_focus)
}

fn render_body(
    rect: UiRect,
    panel_tab: u8,
    state: Binding,
    store: Snapshot,
    editor_focus: UiFocusHandle,
) -> Element {
    let app = state.get();
    let git = store.get();
    let body = rect;
    let mut root = Element::new(move |cx| {
        let scale = cx.context.scale();
        let painted = scale.logical_rect(scale.physical_rect(rect));
        UiElement::panel(cx.id, painted, VisualStyle::filled(theme::c().sidebar))
            .hit_rect(rect)
            .children(cx.children)
    })
    .key("git-panel-content");
    let Some(repository) = git.active() else {
        return root.child(text(
            UiRect::new(
                body.left + 14.0,
                body.top + 10.0,
                body.right - 14.0,
                body.top + 10.0 + ROW_H,
            ),
            "Open a Git repository to browse its history.",
            theme::mono(theme::c().text_ghost, theme::SMALL),
        ));
    };

    let history = app
        .model
        .history()
        .filter(|history| history.repository == repository.worktree_root);
    let history_tab = panel_tab == HISTORY;
    let mut commit_rows = if history_tab {
        history_rows_matching(
            history,
            app.model.history_commit(),
            app.ui.history_query.text(),
            app.model.history_path().is_some() || !app.ui.history_query.text().trim().is_empty(),
        )
    } else {
        Vec::new()
    };
    // The detail pane lists a selected commit's files, so the list keeps commits only.
    let panel_body = body;
    let show_detail = history_tab && body.width() >= DETAIL_MIN_W;
    let selected = commit_rows.iter().find_map(|row| match row {
        HistoryRow::Commit(commit, true) => Some(commit.clone()),
        _ => None,
    });
    if show_detail
        && commit_rows
            .iter()
            .any(|row| matches!(row, HistoryRow::Commit(..)))
    {
        commit_rows.retain(|row| matches!(row, HistoryRow::Commit(..)));
    }
    let branches = app
        .ui
        .history_branches
        .as_ref()
        .filter(|(root, _)| *root == repository.worktree_root)
        .map(|(_, branches)| branches.as_slice())
        .filter(|branches| !branches.is_empty());
    let show_branches = history_tab && body.width() >= BRANCHES_MIN_W && branches.is_some();
    // The dragged widths, kept clear of limits that would squeeze the list.
    let spare = (body.width() - LIST_MIN_W).max(0.0);
    let mut detail_w = if show_detail {
        app.ui
            .history_detail_width
            .unwrap_or(history_detail::WIDTH)
            .clamp(DETAIL_MIN_PANE_W, DETAIL_MAX_PANE_W)
    } else {
        0.0
    };
    let mut branches_w = if show_branches {
        app.ui
            .history_branches_width
            .unwrap_or(history_branches::WIDTH)
            .clamp(BRANCHES_MIN_PANE_W, BRANCHES_MAX_PANE_W)
    } else {
        0.0
    };
    detail_w = detail_w.min((spare - branches_w).max(0.0));
    branches_w = branches_w.min((spare - detail_w).max(0.0));
    let list_left = body.left + branches_w;
    let list_right = body.right - detail_w;
    let columns = history_tab && list_right - list_left >= COLUMNS_MIN_W;
    let body = if history_tab {
        UiRect::new(
            list_left,
            body.top + if columns { COLUMNS_H } else { 0.0 },
            list_right,
            body.bottom,
        )
    } else {
        body
    };
    let row_count = match panel_tab {
        STASHES => repository.stashes.len().max(1),
        TAGS => repository.tags.len().max(1),
        WORKTREES => repository.worktrees.len() * 2,
        _ => commit_rows.len(),
    };
    let rows_top = body.top + 4.0;
    let content_bottom = rows_top + row_count as f32 * ROW_H + 2.0;
    let max_scroll = (content_bottom - body.bottom).max(0.0);
    let scroll = app.ui.details_scroll.clamp(0.0, max_scroll);
    let (view_top, view_bottom) = (body.top + scroll, body.bottom + scroll);

    let mut content = Vec::new();
    let mut hover_targets = Vec::new();
    match panel_tab {
        STASHES => {
            let count = repository.stashes.len();
            if count == 0 {
                content.push(empty_row(rows_top, rect, "No stashes."));
            }
            for index in visible_rows(rows_top, view_top, view_bottom, count) {
                let stash = &repository.stashes[index];
                let top = rows_top + index as f32 * ROW_H;
                let row_rect = UiRect::new(rect.left, top, rect.right, top + ROW_H);
                let hover = GitHover::Stash(stash.oid.clone());
                let hovered = app.ui.hover.as_ref() == Some(&hover);
                hover_targets.push((row_rect, hover));
                content.push(stash_row(
                    row_rect,
                    stash,
                    hovered,
                    state.clone(),
                    store.clone(),
                ));
            }
        }
        WORKTREES => {
            let count = repository.worktrees.len();
            for index in visible_worktree_rows(rows_top, view_top, view_bottom, count) {
                let worktree = &repository.worktrees[index];
                let top = rows_top + index as f32 * ROW_H * 2.0;
                let row_rect = UiRect::new(rect.left, top, rect.right, top + ROW_H * 2.0);
                let hover = GitHover::Worktree(worktree.path.clone());
                let hovered = app.ui.hover.as_ref() == Some(&hover);
                hover_targets.push((row_rect, hover));
                content.push(worktree_row(
                    row_rect,
                    worktree,
                    repository,
                    index == 0,
                    hovered,
                    git.operation.is_none(),
                    state.clone(),
                    store.clone(),
                ));
            }
        }
        TAGS => {
            let count = repository.tags.len();
            if count == 0 {
                content.push(empty_row(rows_top, rect, "No tags yet."));
            }
            for index in visible_rows(rows_top, view_top, view_bottom, count) {
                let tag = &repository.tags[index];
                let top = rows_top + index as f32 * ROW_H;
                let row_rect = UiRect::new(rect.left, top, rect.right, top + ROW_H);
                let hover = GitHover::Tag(tag.name.clone());
                let hovered = app.ui.hover.as_ref() == Some(&hover);
                hover_targets.push((row_rect, hover));
                content.push(tag_row(
                    row_rect,
                    tag,
                    hovered,
                    &repository.worktree_root,
                    &state,
                ));
            }
        }
        _ => {
            let graph = crate::ui::git::history_graph::Graph::new(&commit_rows);
            for index in visible_rows(rows_top, view_top, view_bottom, commit_rows.len()) {
                let top = rows_top + index as f32 * ROW_H;
                let row_rect = UiRect::new(body.left, top, body.right, top + ROW_H);
                let row = &commit_rows[index];
                let hover = match row {
                    HistoryRow::Commit(commit, _) => Some(GitHover::Commit(commit.oid.clone())),
                    HistoryRow::File { oid, path, .. } => {
                        Some(GitHover::CommitFile(oid.clone(), path.clone()))
                    }
                    HistoryRow::Message(_) => None,
                };
                let hovered = hover.is_some() && app.ui.hover == hover;
                if let Some(hover) = hover {
                    hover_targets.push((row_rect, hover));
                }
                content.push(history_row(
                    row_rect,
                    row,
                    hovered,
                    columns,
                    graph.render(index, row_rect, row, hovered),
                    &repository.worktree_root,
                    &state,
                    &editor_focus,
                ));
            }
        }
    }

    // Only the scrollable region under the pointer shows its scrollbar.
    let stash = app
        .ui
        .history_stash
        .as_ref()
        .and_then(|oid| repository.stashes.iter().find(|stash| stash.oid == *oid));
    let detail_subject = stash
        .map(Subject::Stash)
        .or(selected.as_ref().map(Subject::Commit));
    let detail_details = detail_subject.and_then(|subject| app.model.commit_detail(subject.oid()));
    let detail_rect = UiRect::new(list_right, panel_body.top, rect.right, rect.bottom);
    let detail_files = detail_subject
        .filter(|_| show_detail)
        .map(|_| history_detail::files_rect(detail_rect, app.ui.history_summary_height));
    let detail_summary = detail_subject
        .filter(|_| show_detail)
        .map(|_| history_detail::summary_rect(detail_rect, app.ui.history_summary_height));
    let navigator_body = show_branches.then(|| {
        history_branches::body_rect(UiRect::new(
            panel_body.left,
            panel_body.top,
            list_left,
            panel_body.bottom,
        ))
    });
    // One hit test per move finds the hovered row, as in Source Control.
    let hover_state = state.clone();
    let hover_root = repository.worktree_root.clone();
    root = root.on_event_capture(UiEventKind::PointerMove, move |_cx, payload| {
        if let UiEventPayload::PointerMove { pointer } = payload {
            let point = pointer.point;
            let target = if body.contains(point) {
                hover_targets.iter().find_map(|(rect, hover)| {
                    let visible = UiRect::new(
                        rect.left,
                        rect.top - scroll,
                        rect.right,
                        rect.bottom - scroll,
                    );
                    visible.contains(point).then_some((visible, hover))
                })
            } else {
                None
            };
            let next = target.as_ref().map(|(_, hover)| (*hover).clone());
            let region = if body.contains(point) {
                Some(ScrollRegion::Details)
            } else if navigator_body.is_some_and(|rect| rect.contains(point)) {
                Some(ScrollRegion::Navigator)
            } else if detail_files.is_some_and(|rect| rect.contains(point)) {
                Some(ScrollRegion::CommitFiles)
            } else if detail_summary.is_some_and(|rect| rect.contains(point)) {
                Some(ScrollRegion::CommitSummary)
            } else {
                None
            };
            let mut entered = false;
            hover_state.try_update(|app| {
                let changed = app.ui.hover != next;
                if changed {
                    entered = true;
                    app.ui.clear_hover_cards();
                }
                app.ui.hover = next;
                let region_changed = app.ui.scroll_hover != region;
                app.ui.scroll_hover = region;
                changed || region_changed
            });
            if entered && let Some((anchor, hover)) = target {
                match hover {
                    GitHover::Commit(oid) => {
                        hover_state.dispatch(Command::HoverCommit {
                            root: hover_root.clone(),
                            oid: oid.clone(),
                            anchor,
                        });
                    }
                    GitHover::Tag(name) => {
                        hover_state.dispatch(Command::HoverTag {
                            root: hover_root.clone(),
                            name: name.clone(),
                            anchor,
                        });
                    }
                    _ => {}
                }
            }
        }
    });
    if columns {
        root = root.child(columns_header(UiRect::new(
            body.left,
            panel_body.top,
            body.right,
            body.top,
        )));
    }
    root = root.child(scroll_region(
        body,
        content,
        content_bottom,
        scroll,
        max_scroll,
        state.clone(),
        ScrollRegion::Details,
    ));
    if let Some(branches) = branches.filter(|_| show_branches) {
        let scope = app
            .ui
            .history_scope
            .as_ref()
            .filter(|(root, _)| *root == repository.worktree_root)
            .map(|(_, name)| name.as_str());
        root = root.child(history_branches::render(
            UiRect::new(
                panel_body.left,
                panel_body.top,
                list_left,
                panel_body.bottom,
            ),
            repository,
            branches,
            scope,
            app.ui.history_stash.as_deref(),
            selected
                .as_ref()
                .map(|commit| commit.oid.as_str())
                .or(repository.head_oid.as_deref()),
            app.ui.history_nav,
            &app.ui.history_collapsed,
            app.ui.history_branch_scroll,
            &state,
            &store,
        ));
    }
    if show_detail {
        let files = app.model.history_commit().map(|expanded| &expanded.files);
        let none = None;
        root = root.child(history_detail::render(
            detail_rect,
            detail_subject.map(|subject| (subject, files.unwrap_or(&none))),
            detail_details,
            &repository.worktree_root,
            &state,
            &editor_focus,
            app.ui.history_files_scroll,
            &app.ui.history_collapsed,
        ));
    }
    if show_branches {
        root = root.child(pane_handle(
            HistoryPane::Branches,
            list_left,
            panel_body,
            app.ui.history_pane_resize,
            state.clone(),
        ));
    }
    if show_detail {
        root = root.child(pane_handle(
            HistoryPane::Detail,
            list_right,
            panel_body,
            app.ui.history_pane_resize,
            state.clone(),
        ));
    }
    root
}

/// A vertical strip on a history pane's edge that drags its width.
fn pane_handle(
    pane: HistoryPane,
    x: f32,
    body: UiRect,
    resizing: Option<(HistoryPane, f32)>,
    state: Binding,
) -> Element {
    let active = resizing.is_some_and(|(dragged, _)| dragged == pane);
    let (left, right) = (body.left, body.right);
    let drag_start = state.clone();
    let drag_move = state.clone();
    let drag_end = state;
    crate::ui::components::resize_handle::render(
        match pane {
            HistoryPane::Branches => "git-history-branches-resize",
            HistoryPane::Detail => "git-history-detail-resize",
        },
        UiRect::new(x - 3.0, body.top, x + 3.0, body.bottom),
        UiRect::new(x, body.top, x + 1.0, body.bottom),
        CursorIcon::ResizeHorizontal,
        active,
    )
    .on_pointer_down_with_button(move |_cx, pointer, button| {
        if button == PointerButton::Left {
            drag_start.update(|app| {
                app.ui.history_pane_resize = Some((pane, pointer.point.x - x));
                app.ui.hover = None;
                app.ui.clear_hover_cards();
            });
        }
    })
    .on_pointer_drag(move |_cx, pointer| {
        drag_move.update(|app| {
            if let Some((dragged, offset)) = app.ui.history_pane_resize
                && dragged == pane
            {
                let edge = pointer.point.x - offset;
                match pane {
                    HistoryPane::Branches => app.ui.history_branches_width = Some(edge - left),
                    HistoryPane::Detail => app.ui.history_detail_width = Some(right - edge),
                }
            }
        });
    })
    .on_pointer_up(move |_cx, _pointer| {
        drag_end.update(|app| app.ui.history_pane_resize = None);
    })
}

/// Column titles above the history list.
fn columns_header(rect: UiRect) -> Element {
    let style = theme::mono(theme::c().text_ghost, theme::SMALL);
    let author_left = crate::ui::git::panel::author_left(rect.right);
    panel(rect, VisualStyle::default())
        .child(text(
            UiRect::new(rect.left + 28.0, rect.top, author_left, rect.bottom),
            "Message",
            style,
        ))
        .child(text(
            UiRect::new(
                author_left,
                rect.top,
                author_left + crate::ui::git::panel::AUTHOR_W,
                rect.bottom,
            ),
            "Author",
            style,
        ))
        .child(text(
            UiRect::new(
                rect.right - 12.0 - crate::ui::git::panel::TIME_W,
                rect.top,
                rect.right - 12.0,
                rect.bottom,
            ),
            "Time",
            theme::mono_right(theme::c().text_ghost, theme::SMALL),
        ))
        .child(crate::ui::components::divider::bottom(rect))
}

fn empty_row(top: f32, rect: UiRect, message: &str) -> Element {
    text(
        UiRect::new(rect.left + 14.0, top, rect.right - 12.0, top + ROW_H),
        message.to_owned(),
        theme::mono(theme::c().text_ghost, theme::SMALL),
    )
    .into()
}

// Paint after the body so fractional child clips cannot wash out the border.
// Decorations have no hit area, leaving the tabs' click targets and cursors intact.
fn header_bottom_rule(
    header: UiRect,
    viewport: UiRect,
    active_tab: Option<(u8, UiRect)>,
) -> Element {
    Element::new(move |cx| {
        let scale = cx.context.scale();
        let no_hit = UiRect::new(header.left, header.bottom, header.left, header.bottom);
        let mut rule = cx
            .compile(crate::ui::components::divider::bottom(header))
            .hit_rect(no_hit);
        if let Some((tab, rect)) = active_tab {
            // Clip horizontally without antialiasing the underline's vertical edges.
            let left = scale.physical_value(rect.left.max(viewport.left));
            let right = scale.physical_value(rect.right.min(viewport.right));
            if right > left {
                let bottom = scale.physical_value(header.bottom);
                let underline = scale.logical_rect(PhysicalRect::new(
                    left,
                    bottom - scale.physical_length(2.0),
                    right,
                    bottom,
                ));
                rule = rule.child(
                    UiElement::panel(
                        lgui::core::UiId::owned(format!("git-panel-tab-underline-{tab}")),
                        underline,
                        VisualStyle::filled(theme::c().accent),
                    )
                    .hit_rect(no_hit),
                );
            }
        }
        rule
    })
}

fn tab_button(rect: UiRect, tab: u8, label: String, active: bool, state: Binding) -> Element {
    Element::new(move |cx| {
        let c = theme::c();
        let hovered = cx.context.interaction_flags(&cx.id).hovered;
        let fill = if hovered && !active {
            VisualStyle::filled(c.active_line).radius(3.0)
        } else {
            VisualStyle::default().radius(3.0)
        };
        let color = if active { c.text } else { c.text_muted };
        let style = if active {
            theme::mono_bold(color, theme::SMALL)
        } else {
            theme::mono(color, theme::SMALL)
        };
        UiElement::panel(
            cx.id,
            UiRect::new(rect.left, rect.top + 3.0, rect.right, rect.bottom - 3.0),
            fill,
        )
        .hit_rect(rect)
        .child(UiElement::text(
            lgui::core::UiId::owned(format!("git-panel-tab-label-{tab}")),
            UiRect::new(
                rect.left + TAB_PAD,
                rect.top,
                rect.right - TAB_PAD + 6.0,
                rect.bottom,
            ),
            crate::ui::tabs::ellipsize(
                &label,
                (rect.width() - TAB_PAD * 2.0 + 6.0).max(0.0),
                theme::SMALL,
                if active { 700 } else { 400 },
            ),
            style,
        ))
    })
    .key(format!("git-panel-tab-{tab}"))
    .event_policy(EventPolicy::INTERACTIVE)
    .semantics({
        let mut semantics = Semantics::new(SemanticRole::Button).name(match tab {
            HISTORY => "History",
            STASHES => "Stashes",
            WORKTREES => "Worktrees",
            _ => "Tags",
        });
        semantics.state.selected = active;
        semantics.state.expanded = Some(active);
        semantics
    })
    .cursor(CursorIcon::Pointer)
    .on_click(move || {
        state.update(move |app| {
            if app.ui.panel_open && app.ui.panel_tab == tab {
                app.ui.set_panel(false);
            } else {
                app.ui.select_panel_tab(tab);
            }
        });
    })
}

fn tab_scroll_button(
    rect: UiRect,
    forward: bool,
    enabled: bool,
    scroll: f32,
    state: Binding,
) -> Element {
    panel(rect, VisualStyle::default().radius(3.0))
        .key(if forward {
            "git-panel-tabs-next"
        } else {
            "git-panel-tabs-previous"
        })
        .event_policy(EventPolicy::INTERACTIVE)
        .semantics({
            let mut semantics = Semantics::new(SemanticRole::Button).name(if forward {
                "Next Git tabs"
            } else {
                "Previous Git tabs"
            });
            semantics.state.disabled = !enabled;
            semantics
        })
        .cursor(if enabled {
            CursorIcon::Pointer
        } else {
            CursorIcon::Default
        })
        .on_click(move || {
            if enabled {
                state.update(|app| app.ui.panel_tab_scroll = Some(scroll));
            }
        })
        .child(crate::ui::drawer::drawer_icon(
            if forward {
                "git-panel-tabs-next-icon"
            } else {
                "git-panel-tabs-previous-icon"
            },
            if forward {
                "chevron-right"
            } else {
                "chevron-left"
            },
            theme::icon_rect(rect),
            if enabled {
                theme::c().text_muted
            } else {
                theme::c().text_ghost
            },
        ))
}

fn collapse_button(rect: UiRect, state: Binding, expanded: bool) -> Element {
    let icon = theme::icon_rect(UiRect::new(
        rect.left,
        rect.top + 2.0,
        rect.right,
        rect.bottom - 2.0,
    ));
    panel(rect, VisualStyle::default().radius(3.0))
        .key("git-panel-collapse")
        .event_policy(EventPolicy::INTERACTIVE)
        .semantics({
            let mut semantics = Semantics::new(SemanticRole::Button).name(if expanded {
                "Collapse Git panel"
            } else {
                "Expand Git panel"
            });
            semantics.state.expanded = Some(expanded);
            semantics
        })
        .cursor(CursorIcon::Pointer)
        .on_click(move || state.update(|app| app.ui.set_panel(!app.ui.panel_open)))
        .child(crate::ui::drawer::drawer_icon(
            "git-panel-collapse-icon",
            if expanded {
                "chevron-down"
            } else {
                "chevron-up"
            },
            icon,
            theme::c().text_faint,
        ))
}

fn resize_handle(rect: UiRect, state: Binding) -> Element {
    let active = state.get().ui.panel_resize_offset.is_some();
    let bottom = rect.bottom;
    let top = rect.top;
    let drag_start = state.clone();
    let drag_move = state.clone();
    let drag_end = state;
    crate::ui::components::resize_handle::render(
        "git-panel-resize-handle",
        UiRect::new(rect.left, rect.top - 4.0, rect.right, rect.top + 4.0),
        UiRect::new(rect.left, rect.top, rect.right, rect.top + 1.0),
        CursorIcon::ResizeVertical,
        active,
    )
    .on_pointer_down_with_button(move |_cx, pointer, button| {
        if button == PointerButton::Left {
            drag_start.update(|app| {
                app.ui.panel_resize_offset = Some(pointer.point.y - top);
                app.ui.hover = None;
                app.ui.clear_hover_cards();
            });
        }
    })
    .on_pointer_drag(move |_cx, pointer| {
        drag_move.update(|app| {
            if let Some(offset) = app.ui.panel_resize_offset {
                app.ui.panel_height = Some(bottom - pointer.point.y + offset);
            }
        });
    })
    .on_pointer_up(move |_cx, _pointer| {
        drag_end.update(|app| app.ui.panel_resize_offset = None);
    })
}

pub const HISTORY_QUERY_ID: &str = "git-history-query";

/// Scope switch, search box and commit count that sit in the Commits header,
/// right of the Terminal / Commits switch.
pub fn history_toolbar(
    rect: UiRect,
    state: Binding,
    store: Snapshot,
    focus: UiFocusHandle,
) -> Element {
    use crate::ui::components::input::{AccessInputOptions, InputBinding, InputStyle};
    use crate::ui::components::text::SingleLineText;
    use crate::ui::tabs::measure_sans as measure;
    use lgui::core::UiId;
    use std::sync::Arc;

    let app = state.get();
    let active_file = state.frame().active_file.filter(|path| {
        store
            .get()
            .active()
            .is_some_and(|repository| path.starts_with(&repository.worktree_root))
    });
    let file_scope = app.ui.history_file_scope && active_file.is_some();
    let commits = app
        .model
        .history()
        .and_then(|history| history.commits.as_ref().ok());
    let shown = commits.map_or(0, |commits| {
        commits
            .iter()
            .filter(|commit| {
                crate::ui::git::panel::commit_matches(commit, app.ui.history_query.text().trim())
            })
            .count()
    });
    let count = if commits.is_none() {
        String::new()
    } else {
        let more = if app.model.history_can_load_more() {
            "+"
        } else {
            ""
        };
        format!(
            "{shown}{more} {}",
            if shown == 1 && more.is_empty() {
                "commit"
            } else {
                "commits"
            }
        )
    };

    let size = theme::UI_SIZE;
    let height = 22.0;
    let top = rect.top + (rect.height() - height) / 2.0;
    let file_label = active_file
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|name| {
            let name = name.to_string_lossy();
            let clipped = name.chars().take(18).collect::<String>();
            if clipped.len() < name.len() {
                format!("File · {clipped}…")
            } else {
                format!("File · {name}")
            }
        })
        .unwrap_or_else(|| "File".to_owned());
    let label_width = |label: &str| {
        SingleLineText::new(
            label.to_owned(),
            UiRect::new(0.0, 0.0, 10_000.0, size),
            theme::sans(theme::c().text, size),
        )
        .width()
    };
    let branch_w = label_width("Branch") + 20.0;
    let file_w = label_width(&file_label) + 20.0;
    let track = UiRect::new(
        rect.left,
        top,
        (rect.left + 6.0 + branch_w + file_w + 2.0).min(rect.right),
        top + height,
    );
    if track.right - track.left < branch_w + file_w {
        // Too narrow for the toolbar; the commit list alone remains.
        return lgui::prelude::group(rect);
    }
    let mut root = lgui::prelude::group(rect)
        .key("history-toolbar")
        .child(panel(
            track,
            VisualStyle::filled(theme::c().active_line).radius(4.0),
        ));
    let mut left = track.left + 3.0;
    for (id, label, width, active, enabled, to_file) in [
        (
            "history-scope-branch",
            "Branch".to_owned(),
            branch_w,
            !file_scope,
            true,
            false,
        ),
        (
            "history-scope-file",
            file_label,
            file_w,
            file_scope,
            active_file.is_some(),
            true,
        ),
    ] {
        let slot = UiRect::new(left, track.top + 2.0, left + width, track.bottom - 2.0);
        left = slot.right + 2.0;
        let mut semantics = Semantics::new(SemanticRole::Button).name(label.clone());
        semantics.state.selected = active;
        semantics.state.disabled = !enabled;
        let color = if !enabled {
            theme::c().text_ghost
        } else if active {
            theme::c().text
        } else {
            theme::c().text_muted
        };
        let style = if active {
            VisualStyle::filled(theme::c().surface).radius(3.0)
        } else {
            VisualStyle::default()
        };
        let click = state.clone();
        let mut button = panel(slot, style).key(id).semantics(semantics);
        if enabled {
            button = button
                .event_policy(EventPolicy::INTERACTIVE)
                .cursor(CursorIcon::Pointer)
                .on_click(move || {
                    click.update(|app| {
                        app.ui.history_file_scope = to_file;
                        app.ui.details_scroll = 0.0;
                    });
                });
        }
        let label_rect = UiRect::new(slot.left + 10.0, slot.top, slot.right, slot.bottom);
        root = root.child(button.child(text(label_rect, label, theme::sans(color, size))));
    }

    let count_w = if count.is_empty() {
        0.0
    } else {
        measure(&count, theme::SMALL, 400) + 8.0
    };
    let input_left = track.right + 8.0;
    let input_right = (input_left + 240.0).min(rect.right - count_w - 8.0);
    if input_right - input_left >= 80.0 {
        let field = UiRect::new(input_left, top, input_right, top + height);
        let searching = app.ui.history_query.focused;
        let reset = state.clone();
        let box_ = theme::bordered(
            field,
            theme::c().bg,
            if searching {
                theme::c().accent
            } else {
                theme::c().border
            },
            4.0,
            1.0,
        )
        .child(crate::ui::drawer::drawer_icon(
            "history-search-icon",
            "search",
            UiRect::new(
                field.left + 6.0,
                field.top + 4.0,
                field.left + 20.0,
                field.bottom - 4.0,
            ),
            theme::c().text_ghost,
        ));
        root = root
            .child(box_)
            .child(crate::ui::components::input::render_access(
                UiRect::new(
                    field.left + 20.0,
                    field.top,
                    field.right - 2.0,
                    field.bottom,
                ),
                UiId::new(HISTORY_QUERY_ID),
                focus,
                InputBinding::new_access(
                    state.panel.clone(),
                    |app| &app.ui.history_query,
                    |app| {
                        app.ui.details_scroll = 0.0;
                        &mut app.ui.history_query
                    },
                ),
                AccessInputOptions {
                    label: "Search commits",
                    placeholder: "Search commits, authors, hashes…",
                    style: InputStyle {
                        text: theme::sans(theme::c().text, 12.0),
                        placeholder: theme::sans(theme::c().text_ghost, 12.0),
                        caret: theme::c().text,
                        selection: theme::c().accent,
                    },
                    on_submit: None,
                    on_cancel: Some(Arc::new(move || {
                        reset.update(|app| {
                            app.ui.history_query.clear();
                            app.ui.details_scroll = 0.0;
                        })
                    })),
                    on_blur: None,
                },
            ));
        if count_w > 0.0 {
            root = root.child(text(
                UiRect::new(field.right + 8.0, rect.top, rect.right, rect.bottom),
                count,
                theme::mono(theme::c().text_muted, theme::SMALL),
            ));
        }
    }
    root
}
