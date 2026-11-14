//! Source Control drawer.

use crate::features::highlighting::HighlightId;
use std::path::PathBuf;

use lgui::core::{
    CursorIcon, EventPolicy, PointerButton, SemanticRole, Semantics, UiElement, UiEventContext,
    UiEventKind, UiEventPayload, UiFocusHandle, UiId, WheelUnit, clip,
};
use lgui::prelude::{Element, UiRect, VisualStyle, panel, text};

use crate::features::git::model::{ExpandedCommit, GitHistory};
use crate::features::git::{
    ChangeKind, CommitSummary, ConflictSide, DiffTarget, FileState, RepositorySnapshot,
    RepositoryState, StashInfo, TagInfo, WorktreeInfo,
};
use crate::ui::components::input::{AccessInputOptions, InputBinding, InputState, InputStyle};
use crate::ui::components::scrollbar;
use crate::ui::git::commands::{Binding, Command, Snapshot};
use crate::ui::git::menu::{GitMenu, GitMenuTarget};
use crate::ui::git::state::GitPanelState;
use crate::ui::theme;

pub(crate) const TOPLINE_H: f32 = 40.0;
pub(crate) const PROMPT_H: f32 = 196.0;
/// The composer while an operation is unfinished: a title and one button.
pub(crate) const OPERATION_PROMPT_H: f32 = 84.0;
pub(crate) const CHANGE_ROW_H: f32 = 24.0;
pub(crate) const SECTION_H: f32 = 26.0;
pub(crate) const ROW_H: f32 = 20.0;

/// Keep the multiline composer anchored below the scrolling changes.
pub(crate) fn drawer_regions(
    body: UiRect,
    _changes_height: f32,
    operation: bool,
) -> (UiRect, UiRect) {
    let height = if operation {
        OPERATION_PROMPT_H
    } else {
        PROMPT_H
    };
    let top = (body.bottom - height).max(body.top);
    (
        UiRect::new(body.left, body.top, body.right, top),
        UiRect::new(body.left, top, body.right, body.bottom),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScrollRegion {
    Changes,
    Details,
    /// The history navigator's reference list.
    Navigator,
    /// The commit detail pane's changed-file tree.
    CommitFiles,
    /// The compact commit summary below the changed-file tree.
    CommitSummary,
}

impl ScrollRegion {
    fn load_more_at_bottom(self, state: &Binding, max_scroll: f32) {
        if !matches!(self, Self::Details) {
            return;
        }
        let app = state.get();
        if self.scroll(&app) >= max_scroll
            && !app.ui.collapsed_sections.contains(&HISTORY)
            && app.model.history_can_load_more()
            && let Some(history) = app.model.history()
        {
            state.dispatch(Command::LoadMoreHistory {
                root: history.repository.clone(),
            });
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Changes => "git-changes-viewport",
            Self::Details => "git-details-viewport",
            Self::Navigator => "git-navigator-viewport",
            Self::CommitFiles => "git-commit-files-viewport",
            Self::CommitSummary => "git-commit-summary-viewport",
        }
    }

    pub(crate) fn scroll(self, app: &GitPanelState) -> f32 {
        match self {
            Self::Changes => app.ui.scroll,
            Self::Details => app.ui.details_scroll,
            Self::Navigator => app.ui.history_branch_scroll,
            Self::CommitFiles => app.ui.history_files_scroll,
            Self::CommitSummary => app.ui.history_summary_scroll,
        }
    }

    pub(crate) fn set_scroll(self, app: &mut GitPanelState, value: f32) {
        match self {
            Self::Changes => app.ui.scroll = value,
            Self::Details => app.ui.details_scroll = value,
            Self::Navigator => app.ui.history_branch_scroll = value,
            Self::CommitFiles => app.ui.history_files_scroll = value,
            Self::CommitSummary => app.ui.history_summary_scroll = value,
        }
        app.ui.clear_hover_cards();
    }

    pub(crate) fn drag(self, app: &GitPanelState) -> (bool, f32) {
        match self {
            Self::Changes => (app.ui.scrollbar_dragging, app.ui.scrollbar_drag_offset),
            Self::Details => (
                app.ui.details_scrollbar_dragging,
                app.ui.details_scrollbar_drag_offset,
            ),
            Self::Navigator => (
                app.ui.navigator_scrollbar_dragging,
                app.ui.navigator_scrollbar_drag_offset,
            ),
            Self::CommitFiles => (
                app.ui.files_scrollbar_dragging,
                app.ui.files_scrollbar_drag_offset,
            ),
            Self::CommitSummary => (
                app.ui.summary_scrollbar_dragging,
                app.ui.summary_scrollbar_drag_offset,
            ),
        }
    }

    pub(crate) fn set_drag(self, app: &mut GitPanelState, dragging: bool, offset: f32) {
        match self {
            Self::Changes => {
                app.ui.scrollbar_dragging = dragging;
                app.ui.scrollbar_drag_offset = offset;
            }
            Self::Details => {
                app.ui.details_scrollbar_dragging = dragging;
                app.ui.details_scrollbar_drag_offset = offset;
            }
            Self::Navigator => {
                app.ui.navigator_scrollbar_dragging = dragging;
                app.ui.navigator_scrollbar_drag_offset = offset;
            }
            Self::CommitFiles => {
                app.ui.files_scrollbar_dragging = dragging;
                app.ui.files_scrollbar_drag_offset = offset;
            }
            Self::CommitSummary => {
                app.ui.summary_scrollbar_dragging = dragging;
                app.ui.summary_scrollbar_drag_offset = offset;
            }
        }
    }
}

pub(crate) fn scroll_region(
    rect: UiRect,
    content: Vec<Element>,
    content_bottom: f32,
    scroll: f32,
    max_scroll: f32,
    state: Binding,
    region: ScrollRegion,
) -> Element {
    if rect.height() <= 0.0 {
        return lgui::prelude::group(rect);
    }
    let mut clipped = clip(rect, 0.0, -scroll);
    for element in content {
        clipped = clipped.child(element);
    }
    let wheel_state = state.clone();
    let mut viewport = panel(rect, VisualStyle::default())
        .key(region.key())
        .child(clipped)
        .on_event(UiEventKind::Wheel, move |cx, payload| {
            if let UiEventPayload::Wheel { delta } = payload {
                let step = match delta.unit {
                    WheelUnit::Lines => delta.y * ROW_H * 3.0,
                    WheelUnit::Pixels => delta.y,
                };
                wheel_state.update(move |app| {
                    let next = (region.scroll(app) - step).clamp(0.0, max_scroll);
                    region.set_scroll(app, next);
                });
                if step < 0.0 {
                    region.load_more_at_bottom(&wheel_state, max_scroll);
                }
                cx.stop_propagation();
            }
        });
    let app = state.get();
    // Only the region under the pointer shows its bar, not every one in the drawer and panel.
    if max_scroll > 0.0 && (app.ui.scroll_hover == Some(region) || region.drag(&app).0) {
        viewport = viewport.child(git_vertical_scrollbar(
            rect,
            content_bottom,
            scroll,
            state,
            region,
        ));
    }
    viewport
}

struct SectionHeader {
    rect: UiRect,
    filter: u8,
    title: &'static str,
    collapsed: bool,
    buttons: Vec<RowButton>,
    count: usize,
}

/// Paint headers above their rows, keeping each scrolled section in its own
/// stacked slot. Hover targets use the same content-space rectangles.
fn append_section_headers(
    viewport: UiRect,
    scroll: f32,
    headers: Vec<SectionHeader>,
    content: &mut Vec<Element>,
    hover_targets: &mut Vec<(UiRect, GitHover)>,
    state: &Binding,
) {
    let mut targets = Vec::with_capacity(headers.len() + hover_targets.len());
    for (index, header) in headers.into_iter().enumerate() {
        let top = header
            .rect
            .top
            .max(viewport.top + scroll + index as f32 * SECTION_H);
        let rect = UiRect::new(header.rect.left, top, header.rect.right, top + SECTION_H);
        targets.push((rect, GitHover::Section(header.filter)));
        let element = section_header(
            rect,
            header.filter,
            header.title,
            header.collapsed,
            header.buttons,
            header.count,
            state.clone(),
        );
        content.push(element);
    }
    targets.append(hover_targets);
    *hover_targets = targets;
}

pub fn render(
    rect: UiRect,
    state: Binding,
    store: Snapshot,
    editor_focus: UiFocusHandle,
    commit_focus: UiFocusHandle,
    commit_id: UiId,
    filter_focus: UiFocusHandle,
) -> Element {
    render_local(
        rect,
        state,
        store,
        editor_focus,
        commit_focus,
        commit_id,
        filter_focus,
    )
}

fn render_local(
    rect: UiRect,
    state: Binding,
    store: Snapshot,
    editor_focus: UiFocusHandle,
    commit_focus: UiFocusHandle,
    commit_id: UiId,
    filter_focus: UiFocusHandle,
) -> Element {
    let app = state.get();
    let frame = state.frame();
    let resizing = frame.resizing;
    let git = store.get();
    let mut root = panel(rect, VisualStyle::filled(theme::c().sidebar));

    root = root.child(crate::ui::git::drawer::header(
        UiRect::new(
            rect.left,
            rect.top,
            rect.right,
            (rect.top + crate::ui::git::drawer::HEADER_H).min(rect.bottom),
        ),
        state.clone(),
        store.clone(),
        filter_focus,
    ));
    let Some(repository) = git.active() else {
        let message = if git.initializing {
            "Detecting Git and scanning repositories…".to_owned()
        } else {
            git.last_error.as_ref().map_or(
                "Open a Git repository to use Source Control.".to_owned(),
                |error| error.user_message(),
            )
        };
        root = root.child(text(
            UiRect::new(
                rect.left + 14.0,
                rect.top + crate::ui::git::drawer::HEADER_H + 18.0,
                rect.right - 14.0,
                rect.top + crate::ui::git::drawer::HEADER_H + 58.0,
            ),
            message,
            theme::sans(theme::c().text_dim, theme::UI_SIZE),
        ));
        return root.child(git_resize_handle(rect, state, resizing));
    };

    let prepared = git.presentation.repositories.get(&repository.id);
    let sections = prepared.map(|prepared| &prepared.sections);
    let body_top = (rect.top + crate::ui::git::drawer::HEADER_H).min(rect.bottom);
    let body_rect = UiRect::new(rect.left, body_top, rect.right, rect.bottom);
    let mut content = Vec::new();
    // Every section's rows, laid out before drawing so the content height
    // and scroll range are known.
    let query = app.ui.file_filter.text().to_lowercase();
    let show_section = |_filter: u8, empty: bool| !empty;
    let change_sections = [
        ("CONFLICTS", CONFLICTS),
        ("STAGED", STAGED),
        ("UNSTAGED", UNSTAGED),
        ("UNTRACKED", UNTRACKED),
    ]
    .map(|(title, filter)| {
        let files = sections.map_or(&[][..], |sections| sections[filter as usize].as_slice());
        let files = files
            .iter()
            .filter(|path| {
                query.is_empty() || path.to_string_lossy().to_lowercase().contains(&query)
            })
            .cloned()
            .collect::<Vec<_>>();
        let rows = change_rows(&files, app.ui.tree_view)
            .into_iter()
            .map(|row| match row {
                ChangeRow::Folder { path, files } => OwnedChangeRow::Folder { path, files },
                ChangeRow::File { path, nested } => OwnedChangeRow::File {
                    path: path.clone(),
                    nested,
                },
            })
            .collect::<Vec<_>>();
        (title, filter, files, rows)
    });
    let section_height = |filter: u8, rows: usize| {
        SECTION_H
            + if app.ui.collapsed_sections.contains(&filter) {
                0.0
            } else {
                rows as f32 * CHANGE_ROW_H
            }
    };
    let changes_height = 8.0
        + change_sections
            .iter()
            .filter(|(_, filter, files, _)| show_section(*filter, files.is_empty()))
            .map(|(_, filter, _, rows)| section_height(*filter, rows.len()) + 10.0)
            .sum::<f32>();
    let (content_rect, prompt_rect) = drawer_regions(
        body_rect,
        changes_height,
        crate::features::git::operation::operation_text(repository.repository_state).is_some(),
    );
    let max_scroll = (changes_height - content_rect.height()).max(0.0);
    let scroll = app.ui.scroll.clamp(0.0, max_scroll);
    let mut y = content_rect.top + 8.0;
    // Rows the pointer can hover, in content coordinates (before scrolling).
    let mut hover_targets = Vec::new();
    let mut headers = Vec::new();
    for (title, filter, files, rows) in &change_sections {
        let (title, filter) = (*title, *filter);
        if !show_section(filter, files.is_empty()) {
            continue;
        }
        let collapsed = app.ui.collapsed_sections.contains(&filter);
        let section_rect = UiRect::new(rect.left, y, content_rect.right, y + SECTION_H);
        let buttons = if app.ui.hover == Some(GitHover::Section(filter)) {
            group_buttons(filter, files.to_vec(), "section", &state, &store)
        } else {
            Vec::new()
        };
        headers.push(SectionHeader {
            rect: section_rect,
            filter,
            title,
            collapsed,
            buttons,
            count: files.len(),
        });
        y += SECTION_H;
        if collapsed {
            y += 10.0;
            continue;
        }
        let rows_top = y;
        let order = std::sync::Arc::new(
            rows.iter()
                .filter_map(|row| match row {
                    OwnedChangeRow::File { path, .. } => Some(path.clone()),
                    OwnedChangeRow::Folder { .. } => None,
                })
                .collect::<Vec<_>>(),
        );
        let visible = visible_rows_with_height(
            rows_top,
            content_rect.top + scroll,
            content_rect.bottom + scroll,
            rows.len(),
            CHANGE_ROW_H,
        );
        y += rows.len() as f32 * CHANGE_ROW_H;
        for index in visible {
            let row_y = rows_top + index as f32 * CHANGE_ROW_H;
            let row_rect = UiRect::new(rect.left, row_y, content_rect.right, row_y + CHANGE_ROW_H);
            match &rows[index] {
                OwnedChangeRow::Folder { path, files } => {
                    let hover = GitHover::Folder(filter, path.clone());
                    let hovered = app.ui.hover.as_ref() == Some(&hover);
                    hover_targets.push((row_rect, hover));
                    content.push(folder_row(
                        row_rect,
                        filter,
                        path,
                        files.clone(),
                        hovered,
                        &state,
                        &store,
                    ));
                }
                OwnedChangeRow::File { path, nested } => {
                    let file_state = repository.files.get(path).cloned().unwrap_or_default();
                    let hover = GitHover::File(filter, path.clone());
                    let hovered = app.ui.hover.as_ref() == Some(&hover);
                    hover_targets.push((row_rect, hover));
                    let target = match filter {
                        STAGED => DiffTarget::HeadToIndex,
                        CONFLICTS => DiffTarget::HeadToWorktree,
                        _ => DiffTarget::IndexToWorktree,
                    };
                    let active = [frame.active_diff.as_ref(), frame.active_diff_focus.as_ref()]
                        .into_iter()
                        .flatten()
                        .any(|focus| {
                            focus.repository_root == repository.worktree_root
                                && focus.path == *path
                                && focus.target == target
                        });
                    content.push(file_row(
                        row_rect,
                        path.clone(),
                        *nested,
                        file_state.clone(),
                        filter,
                        target,
                        active,
                        hovered,
                        app.ui.selection.contains(&(filter, path.clone())),
                        order.clone(),
                        repository.worktree_root.clone(),
                        state.clone(),
                        store.clone(),
                        editor_focus.clone(),
                    ));
                }
            }
        }
        y += 10.0;
    }
    if change_sections
        .iter()
        .all(|(_, _, files, _)| files.is_empty())
    {
        content.push(
            text(
                UiRect::new(
                    rect.left + 16.0,
                    content_rect.top + 8.0,
                    rect.right - 12.0,
                    content_rect.top + 34.0,
                ),
                if !query.is_empty() {
                    "No matching files."
                } else {
                    "Working tree clean."
                },
                theme::sans(theme::c().text_ghost, 11.0),
            )
            .into(),
        );
    }
    let changes_bottom = y;
    append_section_headers(
        content_rect,
        scroll,
        std::mem::take(&mut headers),
        &mut content,
        &mut hover_targets,
        &state,
    );
    let changes_content = std::mem::take(&mut content);
    let changes_hover = std::mem::take(&mut hover_targets);
    let changes_rect = content_rect;
    let changes_scroll = scroll;
    let changes_max_scroll = max_scroll;
    let scroll_area = changes_rect;
    let hover_regions = [(changes_rect, changes_scroll, changes_hover)];
    // One hit test per move finds the hovered row, so rows need no hover
    // state of their own and the pointer never leaves a stale highlight.
    let hover_state = state.clone();
    let hover_root = repository.worktree_root.clone();
    root = root.on_event_capture(UiEventKind::PointerMove, move |_cx, payload| {
        if let UiEventPayload::PointerMove { pointer } = payload {
            let point = pointer.point;
            let target = hover_regions
                .iter()
                .find_map(|(viewport, scroll, targets)| {
                    if !viewport.contains(point) {
                        return None;
                    }
                    targets.iter().find_map(|(rect, hover)| {
                        let visible = UiRect::new(
                            rect.left,
                            rect.top - scroll,
                            rect.right,
                            rect.bottom - scroll,
                        );
                        visible.contains(point).then_some((visible, hover))
                    })
                });
            let next = target.as_ref().map(|(_, hover)| (*hover).clone());
            let region = scroll_area.contains(point).then_some(ScrollRegion::Changes);
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
    root = root.child(scroll_region(
        changes_rect,
        changes_content,
        changes_bottom,
        changes_scroll,
        changes_max_scroll,
        state.clone(),
        ScrollRegion::Changes,
    ));
    root = root.child(crate::ui::git::drawer::composer(
        prompt_rect,
        state.clone(),
        store.clone(),
        commit_focus,
        commit_id,
    ));
    // Keep the header edge crisp and above scrolling rows and operation banners.
    root = root.child(git_resize_handle(rect, state, resizing));
    root
}

pub(crate) fn visible_rows(
    rows_top: f32,
    viewport_top: f32,
    viewport_bottom: f32,
    count: usize,
) -> std::ops::Range<usize> {
    visible_rows_with_height(rows_top, viewport_top, viewport_bottom, count, ROW_H)
}

pub(crate) fn visible_worktree_rows(
    rows_top: f32,
    viewport_top: f32,
    viewport_bottom: f32,
    count: usize,
) -> std::ops::Range<usize> {
    visible_rows_with_height(rows_top, viewport_top, viewport_bottom, count, ROW_H * 2.0)
}

pub(crate) fn visible_rows_with_height(
    rows_top: f32,
    viewport_top: f32,
    viewport_bottom: f32,
    count: usize,
    height: f32,
) -> std::ops::Range<usize> {
    let start = ((viewport_top - rows_top) / height).floor().max(0.0) as usize;
    let end = ((viewport_bottom - rows_top) / height).ceil().max(0.0) as usize;
    start.saturating_sub(1).min(count)..end.saturating_add(1).min(count)
}

pub(crate) fn git_vertical_scrollbar(
    rect: UiRect,
    content_bottom: f32,
    scroll: f32,
    state: Binding,
    region: ScrollRegion,
) -> Element {
    let content_h = (content_bottom - rect.top).max(1.0);
    let viewport_h = rect.height();
    let max_scroll = (content_bottom - rect.bottom).max(0.0);
    let track = UiRect::new(
        rect.right - scrollbar::SIZE,
        rect.top + 8.0,
        rect.right,
        rect.bottom - 8.0,
    );
    if track.height() <= 0.0 {
        return lgui::prelude::group(rect);
    }
    let thumb_h = (track.height() * viewport_h / content_h)
        .max(scrollbar::MIN_THUMB_LENGTH)
        .min(track.height());
    let travel = track.height() - thumb_h;
    let thumb_top = if max_scroll == 0.0 {
        track.top
    } else {
        track.top + travel * (scroll / max_scroll)
    };
    let track_top = track.top;
    let drag_start = state.clone();
    let drag_move = state.clone();
    let drag_end = state;
    scrollbar::thumb(
        UiRect::new(
            track.left + scrollbar::INSET,
            thumb_top,
            track.right - scrollbar::INSET,
            thumb_top + thumb_h,
        ),
        0xb8,
    )
    .key(format!("{}-scrollbar-thumb", region.key()))
    .event_policy(EventPolicy::INTERACTIVE)
    .on_pointer_down(move |_cx, pointer| {
        drag_start.update(move |app| {
            region.set_drag(app, true, pointer.point.y - thumb_top);
        });
    })
    .on_pointer_drag(move |_cx, pointer| {
        drag_move.update(move |app| {
            let (dragging, offset) = region.drag(app);
            if dragging && travel > 0.0 {
                let next = (pointer.point.y - track_top - offset) / travel * max_scroll;
                region.set_scroll(app, next.clamp(0.0, max_scroll));
            }
        });
    })
    .on_pointer_up(move |_cx, _pointer| {
        drag_end.update(|app| region.set_drag(app, false, 0.0));
        region.load_more_at_bottom(&drag_end, max_scroll);
    })
}

pub(crate) fn git_resize_handle(rect: UiRect, state: Binding, resizing: bool) -> Element {
    if !state.frame().resizable {
        // Cursor resolution includes passive nodes. A full-size placeholder
        // would mask every hover cursor beneath it in the application layout.
        return lgui::prelude::group(UiRect::new(rect.left, rect.top, rect.left, rect.top));
    }
    let handle_rect = UiRect::new(rect.right - 8.0, rect.top, rect.right, rect.bottom);
    let drawer_left = rect.left;
    let drag_start = state.clone();
    let drag_move = state.clone();
    let drag_end = state;
    crate::ui::components::resize_handle::render(
        "git-resize-handle",
        handle_rect,
        UiRect::new(rect.right - 1.0, rect.top, rect.right, rect.bottom),
        CursorIcon::ResizeHorizontal,
        resizing,
    )
    .on_pointer_down_with_button(move |_cx, _pointer, button| {
        if button == PointerButton::Left {
            drag_start.dispatch(Command::ResizeStart);
        }
    })
    .on_pointer_drag(move |_cx, pointer| {
        drag_move.dispatch(Command::Resize {
            width: (pointer.point.x - drawer_left)
                .clamp(theme::SIDEBAR_MIN_W, theme::SIDEBAR_MAX_W),
        });
    })
    .on_pointer_up(move |_cx, _pointer| {
        drag_end.dispatch(Command::ResizeEnd);
    })
}

pub(crate) fn commit_input(
    rect: UiRect,
    state: Binding,
    focus: UiFocusHandle,
    id: UiId,
) -> Element {
    let placeholder = if state.get().ui.commit_amend {
        "Amend message (empty keeps it)"
    } else {
        "Commit message"
    };
    crate::ui::components::input::render_portable(
        rect,
        id,
        focus,
        InputBinding::new_access(
            state.panel.clone(),
            commit_input_state,
            commit_input_state_mut,
        ),
        AccessInputOptions {
            label: "Commit message",
            placeholder,
            style: InputStyle {
                text: theme::sans(theme::c().text, 12.0),
                placeholder: theme::sans(theme::c().text_ghost, 12.0),
                caret: theme::c().text_faint,
                selection: theme::c().accent,
            },
            on_submit: None,
            on_cancel: None,
            on_blur: None,
        },
        true,
    )
}

pub(crate) fn commit_input_state(app: &GitPanelState) -> &InputState {
    &app.ui.commit_input
}

pub(crate) fn commit_input_state_mut(app: &mut GitPanelState) -> &mut InputState {
    &mut app.ui.commit_input
}

/// Opens the commit options menu: amend, sign-off and undo.
pub(crate) fn commit_options_button(rect: UiRect, state: Binding, enabled: bool) -> Element {
    let app = state.get();
    let open = app
        .ui
        .menu
        .as_ref()
        .is_some_and(|menu| menu.target == GitMenuTarget::Submit);
    // Amend and sign-off stay visible while the menu is closed.
    let click_state = state.clone();
    panel(
        rect,
        if open {
            VisualStyle::filled(theme::c().active_line)
        } else {
            VisualStyle::filled(if enabled {
                theme::c().accent
            } else {
                theme::c().surface
            })
        },
    )
    .key("git-commit-options")
    .event_policy(EventPolicy::INTERACTIVE)
    .semantics(Semantics::new(SemanticRole::Button).name("Commit options"))
    .cursor(CursorIcon::Pointer)
    .on_click(move || {
        click_state.update(|app| {
            app.ui.menu = Some(GitMenu {
                pos: (rect.left, rect.top),
                target: GitMenuTarget::Submit,
                hover: None,
            });
        });
    })
    .child(crate::ui::drawer::drawer_icon(
        "git-commit-options-icon",
        "chevron-down",
        theme::icon_rect(rect),
        if enabled {
            theme::c().on_accent
        } else {
            theme::c().text_ghost
        },
    ))
}

pub(crate) fn commit_button(rect: UiRect, state: Binding, store: Snapshot) -> Element {
    let app = state.get();
    let reason = crate::ui::git::drawer::commit_reason(&app, &store.get());
    let amend = app.ui.commit_amend;
    let click_state = state.clone();
    primary_button(
        rect,
        PrimaryButton {
            key: "git-commit-button",
            label: if amend { "Amend" } else { "Commit" },
            name: if amend {
                "Amend last commit"
            } else {
                "Commit changes"
            },
            reason,
        },
        move || {
            click_state.dispatch(Command::Commit);
        },
    )
}

/// The button that finishes an operation in progress: Continue, or End for a
/// bisect.
pub(crate) fn operation_button(
    rect: UiRect,
    state: Binding,
    label: &'static str,
    continues: bool,
    reason: Option<&'static str>,
) -> Element {
    primary_button(
        rect,
        PrimaryButton {
            key: "git-operation-button",
            label,
            name: label,
            reason,
        },
        move || {
            state.dispatch(if continues {
                Command::ContinueOperation
            } else {
                Command::RequestAbort
            });
        },
    )
}

pub(crate) struct PrimaryButton {
    key: &'static str,
    label: &'static str,
    name: &'static str,
    /// Why it cannot be used now; `None` when it can.
    reason: Option<&'static str>,
}

fn primary_button(
    rect: UiRect,
    button: PrimaryButton,
    on_click: impl Fn() + Send + Sync + 'static,
) -> Element {
    let PrimaryButton {
        key,
        label,
        name,
        reason,
    } = button;
    let enabled = reason.is_none();
    let mut semantics = Semantics::new(SemanticRole::Button).name(name);
    semantics.state.disabled = !enabled;
    if let Some(reason) = reason {
        semantics = semantics.description(reason);
    }
    Element::new(move |cx| {
        let hovered = cx.context.interaction_flags(&cx.id).hovered;
        let fill = if enabled {
            if hovered {
                theme::c().accent_hover
            } else {
                theme::c().accent
            }
        } else {
            theme::c().surface
        };
        UiElement::panel(cx.id, rect, VisualStyle::filled(fill)).children(cx.children)
    })
    .key(key)
    .event_policy(EventPolicy::INTERACTIVE)
    .semantics(semantics)
    .cursor(if enabled {
        CursorIcon::Pointer
    } else {
        CursorIcon::NotAllowed
    })
    .on_click(move || {
        if enabled {
            on_click();
        }
    })
    .child(text(
        rect,
        label,
        theme::sans_semibold(
            if enabled {
                theme::c().on_accent
            } else {
                theme::c().text_ghost
            },
            12.0,
        )
        .centered(),
    ))
}

/// Change-section indices in `RepositoryPresentation::sections`, plus stable
/// identifiers used by `git_collapsed_sections` for the bottom detail sections.
pub const CONFLICTS: u8 = 0;
pub const STAGED: u8 = 1;
pub const UNSTAGED: u8 = 2;
pub const UNTRACKED: u8 = 3;
/// Listed after the change sections; its rows come from the snapshot.
pub const STASHES: u8 = 4;
/// The recent commits, last in the drawer.
pub const HISTORY: u8 = 5;
pub const WORKTREES: u8 = 6;
pub const TAGS: u8 = 7;

/// The drawer row under the pointer, which shows its action buttons.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHover {
    Worktree(PathBuf),
    Section(u8),
    File(u8, PathBuf),
    /// A folder row of a section grouped by folder.
    Folder(u8, PathBuf),
    /// A stash, by commit id.
    Stash(String),
    /// A tag, by its full name.
    Tag(String),
    /// A history commit, by commit id.
    Commit(String),
    /// A file of an expanded history commit.
    CommitFile(String, PathBuf),
}

/// A history commit's hover card, beside its row.
#[derive(Clone, Debug, PartialEq)]
pub struct CommitCard {
    pub oid: String,
    /// The commit's row on screen when the card was requested.
    pub anchor: UiRect,
    pub details: Result<crate::features::git::CommitDetails, String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TagCard {
    pub repository: PathBuf,
    pub tag: TagInfo,
    pub anchor: UiRect,
    pub details: Result<crate::features::git::TagDetails, String>,
}

pub(crate) const CARD_W: f32 = 420.0;
pub(crate) const CARD_PAD: f32 = 12.0;
pub(crate) const CARD_LINE_H: f32 = 18.0;
pub(crate) const CARD_MAX_MESSAGE_LINES: usize = 16;

pub(crate) enum CardLine {
    Text(Vec<(String, lgui::prelude::Color)>, bool),
    Separator,
}

/// The hovered history commit's card, to the right of Source Control.
pub fn render_commit_card(viewport: UiRect, app: &GitPanelState) -> Option<Element> {
    let card = app.ui.commit_card.as_ref()?;
    if app.ui.hover.as_ref() != Some(&GitHover::Commit(card.oid.clone())) {
        return None;
    }
    let summary = app
        .model
        .history()
        .and_then(|history| history.commits.as_ref().ok())
        .and_then(|commits| commits.iter().find(|commit| commit.oid == card.oid));
    let width = CARD_W.min(viewport.width() - 16.0).max(160.0);
    let inner = width - CARD_PAD * 2.0;

    let mut lines = Vec::new();
    append_commit_card_lines(&mut lines, &card.details, inner);
    lines.push(CardLine::Separator);
    let mut footer = vec![(
        crate::features::git::diff_document::short_oid(&card.oid).to_owned(),
        theme::c().text_soft,
    )];
    if let Some(summary) = summary.filter(|summary| !summary.decorations.is_empty()) {
        footer.push((
            format!(
                "  {}",
                summary
                    .decorations
                    .iter()
                    .map(|value| super::history_badges::display_name(value))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            theme::c().text_dim,
        ));
    }
    lines.push(CardLine::Text(footer, false));

    Some(render_hover_card(viewport, card.anchor, lines))
}

pub(crate) fn append_commit_card_lines(
    lines: &mut Vec<CardLine>,
    details: &Result<crate::features::git::CommitDetails, String>,
    inner: f32,
) {
    match details {
        Ok(details) => {
            lines.push(CardLine::Text(
                vec![(details.author.clone(), theme::c().text_bright)],
                true,
            ));
            lines.push(CardLine::Text(
                vec![(
                    format!("{}  ·  {}", details.author_email, details.date),
                    theme::c().text_dim,
                )],
                false,
            ));
            lines.push(CardLine::Separator);
            let mut message = wrap_text(&details.message.replace('\t', "    "), inner);
            if message.len() > CARD_MAX_MESSAGE_LINES {
                message.truncate(CARD_MAX_MESSAGE_LINES);
                message.push("…".into());
            }
            lines.extend(
                message
                    .into_iter()
                    .map(|line| CardLine::Text(vec![(line, theme::c().text)], false)),
            );
            if details.files_changed > 0 {
                lines.push(CardLine::Separator);
                let plural = |count: usize, word: &str| {
                    format!("{count} {word}{}", if count == 1 { "" } else { "s" })
                };
                let mut stats = vec![(
                    format!("{} changed", plural(details.files_changed, "file")),
                    theme::c().text_soft,
                )];
                if details.insertions > 0 {
                    stats.push((", ".into(), theme::c().text_soft));
                    stats.push((
                        format!("{}(+)", plural(details.insertions, "insertion")),
                        theme::c().git.added,
                    ));
                }
                if details.deletions > 0 {
                    stats.push((", ".into(), theme::c().text_soft));
                    stats.push((
                        format!("{}(-)", plural(details.deletions, "deletion")),
                        theme::c().git.deleted,
                    ));
                }
                lines.push(CardLine::Text(stats, false));
            }
        }
        Err(error) => lines.extend(
            wrap_text(error, inner)
                .into_iter()
                .map(|line| CardLine::Text(vec![(line, theme::c().error_text)], false)),
        ),
    }
}

pub(crate) fn render_hover_card(
    viewport: UiRect,
    anchor: UiRect,
    mut lines: Vec<CardLine>,
) -> Element {
    let width = CARD_W.min(viewport.width() - 16.0).max(160.0);
    let separator = 11.0;
    let available = (viewport.height() - 16.0 - CARD_PAD * 2.0).max(0.0);
    let mut used = 0.0;
    let keep = lines
        .iter()
        .take_while(|line| {
            used += if matches!(line, CardLine::Separator) {
                separator
            } else {
                CARD_LINE_H
            };
            used <= available
        })
        .count();
    if keep < lines.len() {
        lines.truncate(keep);
        while !lines.is_empty() {
            let height: f32 = lines
                .iter()
                .map(|line| {
                    if matches!(line, CardLine::Separator) {
                        separator
                    } else {
                        CARD_LINE_H
                    }
                })
                .sum();
            if height + CARD_LINE_H <= available {
                break;
            }
            lines.pop();
        }
        if available >= CARD_LINE_H {
            lines.push(CardLine::Text(
                vec![("…".into(), theme::c().text_dim)],
                false,
            ));
        }
    }
    let height = CARD_PAD * 2.0
        + lines
            .iter()
            .map(|line| match line {
                CardLine::Text(..) => CARD_LINE_H,
                CardLine::Separator => separator,
            })
            .sum::<f32>();
    let left = (anchor.right + 6.0)
        .min(viewport.right - width - 8.0)
        .max(viewport.left + 8.0);
    let top = anchor
        .top
        .min(viewport.bottom - height - 8.0)
        .max(viewport.top + 8.0);
    let rect = UiRect::new(left, top, left + width, top + height);
    let mut surface = theme::bordered(rect, theme::c().surface, theme::c().border, 6.0, 1.0)
        .shadow(
            lgui::prelude::ShadowStyle::new(lgui::prelude::Color::BLACK)
                .alpha(theme::c().shadow_alpha)
                .offset(0.0, 2.0)
                .blur(3.0),
        );
    let mut y = rect.top + CARD_PAD;
    for line in lines {
        match line {
            CardLine::Separator => {
                let line_y = y + (separator - 1.0) / 2.0;
                surface = surface.child(panel(
                    UiRect::new(
                        rect.left + CARD_PAD,
                        line_y,
                        rect.right - CARD_PAD,
                        line_y + 1.0,
                    ),
                    VisualStyle::filled(theme::c().border),
                ));
                y += separator;
            }
            CardLine::Text(pieces, bold) => {
                let mut x = rect.left + CARD_PAD;
                for (piece, color) in pieces {
                    let piece_w = crate::ui::tabs::measure(
                        &piece,
                        theme::UI_SIZE,
                        if bold { 700 } else { 400 },
                    );
                    let piece = crate::ui::tabs::ellipsize(
                        &piece,
                        (rect.right - CARD_PAD - x).max(0.0),
                        theme::UI_SIZE,
                        if bold { 700 } else { 400 },
                    );
                    surface = surface.child(text(
                        UiRect::new(x, y, rect.right - CARD_PAD, y + CARD_LINE_H),
                        piece,
                        if bold {
                            theme::mono_bold(color, theme::UI_SIZE)
                        } else {
                            theme::mono(color, theme::UI_SIZE)
                        },
                    ));
                    x += piece_w;
                }
                y += CARD_LINE_H;
            }
        }
    }
    surface
}

/// Break `text` into lines no wider than `width`: at spaces where possible,
/// otherwise between characters, as CJK text needs.
pub(crate) fn wrap_text(text: &str, width: f32) -> Vec<String> {
    wrap_text_sized(text, width, theme::UI_SIZE)
}

/// `wrap_text` for text drawn in the monospace font at `size`.
pub(crate) fn wrap_text_sized(text: &str, width: f32, size: f32) -> Vec<String> {
    let fits = |line: &str| crate::ui::tabs::measure(line, size, 400) <= width;
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        for word in paragraph.split_inclusive(' ') {
            if fits(format!("{line}{word}").trim_end()) {
                line.push_str(word);
                continue;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line).trim_end().to_owned());
            }
            for ch in word.chars() {
                line.push(ch);
                if !fits(&line) && line.chars().count() > 1 {
                    line.pop();
                    lines.push(std::mem::take(&mut line));
                    line.push(ch);
                }
            }
        }
        lines.push(line.trim_end().to_owned());
    }
    lines
}

/// The folder name shown for a repository.
pub(crate) fn repository_name(root: &std::path::Path) -> String {
    root.file_name().map_or_else(
        || crate::ui::display_path(root),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// A row of a change section.
#[derive(Clone, Debug, PartialEq, Eq)]
enum OwnedChangeRow {
    Folder { path: PathBuf, files: Vec<PathBuf> },
    File { path: PathBuf, nested: bool },
}

pub(crate) enum ChangeRow<'a> {
    /// A folder and the section's files directly in it.
    Folder { path: PathBuf, files: Vec<PathBuf> },
    /// A file; `nested` when listed under its folder row.
    File { path: &'a PathBuf, nested: bool },
}

/// `files` as rows: as given, or grouped under one row per folder (files
/// at the repository root first, without a folder row).
pub(crate) fn change_rows(files: &[PathBuf], grouped: bool) -> Vec<ChangeRow<'_>> {
    if !grouped {
        return files
            .iter()
            .map(|path| ChangeRow::File {
                path,
                nested: false,
            })
            .collect();
    }
    let mut folders = std::collections::BTreeMap::<PathBuf, Vec<&PathBuf>>::new();
    let mut rows = Vec::new();
    for path in files {
        match path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            Some(parent) => folders.entry(parent.to_path_buf()).or_default().push(path),
            None => rows.push(ChangeRow::File {
                path,
                nested: false,
            }),
        }
    }
    for (folder, paths) in folders {
        rows.push(ChangeRow::Folder {
            path: folder,
            files: paths.iter().map(|path| (*path).clone()).collect(),
        });
        rows.extend(
            paths
                .into_iter()
                .map(|path| ChangeRow::File { path, nested: true }),
        );
    }
    rows
}

/// A section's bulk actions for `paths`, as buttons.
pub(crate) fn group_buttons(
    filter: u8,
    paths: Vec<PathBuf>,
    key: &str,
    state: &Binding,
    store: &Snapshot,
) -> Vec<RowButton> {
    section_actions(filter)
        .iter()
        .map(|&action| {
            let action_state = state.clone();
            let action_store = store.clone();
            let paths = paths.clone();
            RowButton {
                key: format!("git-{key}-{filter}-{action:?}"),
                icon: action.icon(),
                name: action.section_name(filter),
                on_click: Box::new(move || {
                    action.run(&action_state, &action_store, filter, paths.clone());
                }),
            }
        })
        .collect()
}

/// A folder of a grouped section, with the section's actions for its files.
pub(crate) fn folder_row(
    rect: UiRect,
    filter: u8,
    path: &std::path::Path,
    files: Vec<PathBuf>,
    hovered: bool,
    state: &Binding,
    store: &Snapshot,
) -> Element {
    let label = path.display().to_string();
    let key = format!("folder-{}", label);
    let mut row = panel(
        rect,
        if hovered {
            VisualStyle::filled(theme::c().surface)
        } else {
            VisualStyle::default()
        },
    )
    .key(format!("git-{key}-{filter}"))
    .child(crate::ui::drawer::drawer_icon(
        "git-folder-row-icon",
        "folder",
        UiRect::new(
            rect.left + 11.0,
            rect.top + (rect.height() - 12.0) / 2.0,
            rect.left + 23.0,
            rect.top + (rect.height() + 12.0) / 2.0,
        ),
        theme::c().text_ghost,
    ))
    .child(
        clip(
            UiRect::new(rect.left + 30.0, rect.top, rect.right - 52.0, rect.bottom),
            0.0,
            0.0,
        )
        .child(text(
            UiRect::new(rect.left + 30.0, rect.top, rect.left + 2000.0, rect.bottom),
            label,
            theme::mono(theme::c().text_muted, theme::SMALL),
        )),
    );
    if hovered {
        let buttons = group_buttons(filter, files, &key, state, store);
        row = row_buttons(row, rect, rect.right - 8.0, buttons);
    }
    row
}

/// `path`'s file name, then its folder dimmed when `show_folder`, clipped
/// to `viewport`. The name leads so a deep path still shows which file it
/// is; a long folder is cut at the right.
pub(crate) fn path_label(
    viewport: UiRect,
    path: &std::path::Path,
    show_folder: bool,
    emphasized: bool,
) -> Element {
    let name_style = theme::sans(
        if emphasized {
            theme::c().text
        } else {
            theme::c().text_soft
        },
        12.0,
    );
    styled_path_label(viewport, path, show_folder, name_style, false)
}

/// Shared change styling for the drawer and the selected history entry's files.
pub(crate) fn change_color(kind: ChangeKind) -> lgui::prelude::Color {
    match kind {
        ChangeKind::Added | ChangeKind::Untracked | ChangeKind::Copied => theme::c().git.added_text,
        ChangeKind::Deleted => theme::c().git.deleted,
        ChangeKind::Unmerged => theme::c().error_text,
        ChangeKind::Unmodified => theme::c().text_soft,
        ChangeKind::Modified | ChangeKind::Renamed | ChangeKind::TypeChanged => {
            theme::c().git.modified
        }
    }
}

pub(crate) fn change_path_label(
    viewport: UiRect,
    path: &std::path::Path,
    show_folder: bool,
    kind: ChangeKind,
) -> Element {
    styled_path_label(
        viewport,
        path,
        show_folder,
        theme::sans(change_color(kind), 12.0),
        kind == ChangeKind::Deleted,
    )
}

fn styled_path_label(
    viewport: UiRect,
    path: &std::path::Path,
    show_folder: bool,
    name_style: lgui::core::TextStyle,
    deleted: bool,
) -> Element {
    let (name, folder) = split_path(path);
    let folder_style = theme::sans(theme::c().text_ghost, 10.0);
    // Lay each text out at its natural width and let the viewport clip it.
    // Narrower rects make the text engine wrap at path separators instead.
    let name_w = crate::ui::components::text::SingleLineText::new(
        &name,
        UiRect::new(0.0, 0.0, 10000.0, viewport.height()),
        name_style,
    )
    .width()
        + TEXT_PAD;
    let mut label = clip(viewport, 0.0, 0.0).child(text(
        UiRect::new(
            viewport.left,
            viewport.top,
            viewport.left + name_w,
            viewport.bottom,
        ),
        name,
        name_style,
    ));
    if deleted {
        // Match the measured filename, excluding layout padding and folder text.
        // The surrounding clip also trims the strike in narrow drawers.
        let y = (viewport.top + viewport.height() / 2.0).floor();
        label = label.child(panel(
            UiRect::new(
                viewport.left,
                y,
                viewport.left + (name_w - TEXT_PAD).max(0.0),
                y + 1.0,
            ),
            VisualStyle::filled(name_style.color),
        ));
    }
    if let Some(folder) = folder.filter(|_| show_folder) {
        let left = viewport.left + name_w + 4.0;
        let folder_w = crate::ui::components::text::SingleLineText::new(
            &folder,
            UiRect::new(0.0, 0.0, 10000.0, viewport.height()),
            folder_style,
        )
        .width()
            + TEXT_PAD;
        label = label.child(text(
            UiRect::new(left, viewport.top, left + folder_w, viewport.bottom),
            folder,
            folder_style,
        ));
    }
    label
}

/// Extra width past a measured text so its last glyph is never clipped.
pub(crate) const TEXT_PAD: f32 = 6.0;

/// `path` as its file name and its folder, `None` at the repository root.
pub(crate) fn split_path(path: &std::path::Path) -> (String, Option<String>) {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let folder = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.display().to_string());
    (name, folder)
}

/// A row of the history section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HistoryRow {
    /// A commit, and whether it is expanded.
    Commit(CommitSummary, bool),
    /// A file changed by the expanded commit `oid`.
    File {
        oid: String,
        kind: ChangeKind,
        path: PathBuf,
    },
    /// Loading, empty or failure text.
    Message(String),
}

pub(crate) fn history_rows(
    history: Option<&GitHistory>,
    expanded: Option<&ExpandedCommit>,
) -> Vec<HistoryRow> {
    history_rows_matching(history, expanded, "", false)
}

/// Whether `commit` matches every word of `query` in its subject, author or hash.
pub(crate) fn commit_matches(commit: &CommitSummary, query: &str) -> bool {
    let haystack = format!(
        "{} {} {} {}",
        commit.subject, commit.author, commit.author_email, commit.oid
    )
    .to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|word| haystack.contains(word))
}

/// The list narrowed to `query`. A narrowed or file-scoped list is drawn without
/// lanes, since its commits are no longer neighbours in the graph (`plain`).
pub(crate) fn history_rows_matching(
    history: Option<&GitHistory>,
    expanded: Option<&ExpandedCommit>,
    query: &str,
    plain: bool,
) -> Vec<HistoryRow> {
    let commits = match history.map(|history| &history.commits) {
        None => return vec![HistoryRow::Message("Loading commits…".into())],
        Some(Err(error)) => return vec![HistoryRow::Message(error.clone())],
        Some(Ok(commits)) if commits.is_empty() => {
            return vec![HistoryRow::Message("No commits yet.".into())];
        }
        Some(Ok(commits)) => commits,
    };
    let query = query.trim();
    let mut rows = Vec::new();
    let matching = commits
        .iter()
        .filter(|commit| commit_matches(commit, query))
        .collect::<Vec<_>>();
    for (index, commit) in matching.iter().enumerate() {
        let open = expanded.filter(|expanded| expanded.oid == commit.oid);
        let mut shown = (*commit).clone();
        if plain {
            // Neighbours in the list are chained into one lane, as the real
            // parents are not necessarily listed.
            shown.parents = matching
                .get(index + 1)
                .map(|next| vec![next.oid.clone()])
                .unwrap_or_default();
        }
        rows.push(HistoryRow::Commit(shown, open.is_some()));
        let Some(open) = open else {
            continue;
        };
        match &open.files {
            None => rows.push(HistoryRow::Message("Loading files…".into())),
            Some(Err(error)) => rows.push(HistoryRow::Message(error.clone())),
            Some(Ok(files)) => rows.extend(files.iter().map(|(kind, path)| HistoryRow::File {
                oid: commit.oid.clone(),
                kind: *kind,
                path: path.clone(),
            })),
        }
    }
    if rows.is_empty() {
        rows.push(HistoryRow::Message("No commits match.".into()));
    }
    rows
}

/// Width of the history list's author and time columns.
pub(crate) const AUTHOR_W: f32 = 96.0;
pub(crate) const TIME_W: f32 = 72.0;
const COLUMN_GAP: f32 = 12.0;

/// Where the author column starts, counted back from the row's right edge.
pub(crate) fn author_left(right: f32) -> f32 {
    right - 12.0 - TIME_W - COLUMN_GAP - AUTHOR_W
}

/// How long ago `timestamp` (Unix seconds) was, for the history's time column.
pub(crate) fn relative_time(timestamp: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    let seconds = (now - timestamp).max(0);
    let (value, unit) = match seconds {
        0..=59 => return "just now".to_owned(),
        60..=3599 => (seconds / 60, "min"),
        3600..=86_399 => (seconds / 3600, "h"),
        86_400..=2_591_999 => (seconds / 86_400, "d"),
        2_592_000..=31_535_999 => (seconds / 2_592_000, "mo"),
        _ => (seconds / 31_536_000, "y"),
    };
    format!("{value} {unit} ago")
}

pub(crate) fn history_row(
    rect: UiRect,
    row: &HistoryRow,
    hovered: bool,
    columns: bool,
    graph: super::history_graph::RenderedRow,
    repository: &std::path::Path,
    state: &Binding,
    editor_focus: &UiFocusHandle,
) -> Element {
    let content = UiRect::new(
        rect.left + graph.width + 6.0,
        rect.top,
        rect.right,
        rect.bottom,
    );
    let background = if hovered || matches!(row, HistoryRow::Commit(_, true)) {
        VisualStyle::filled(theme::c().surface)
    } else {
        VisualStyle::default()
    };
    match row {
        HistoryRow::Message(message) => panel(rect, VisualStyle::default())
            .child(graph.element)
            .child(text(
                UiRect::new(content.left, rect.top, rect.right - 12.0, rect.bottom),
                message.clone(),
                theme::mono(theme::c().text_ghost, theme::SMALL),
            )),
        HistoryRow::Commit(commit, open) => {
            let toggle_state = state.clone();
            let root = repository.to_path_buf();
            let oid = commit.oid.clone();
            let menu_state = state.clone();
            let menu_root = repository.to_path_buf();
            let menu_oid = commit.oid.clone();
            let author_left = author_left(rect.right);
            // Badges keep their right edge; the columns take the row's end.
            let badge_area = if columns {
                UiRect::new(
                    content.left,
                    rect.top,
                    author_left - COLUMN_GAP + 12.0,
                    rect.bottom,
                )
            } else {
                content
            };
            let (badges, subject_right) =
                super::history_badges::render(badge_area, &commit.oid, &commit.decorations);
            let subject = crate::ui::tabs::ellipsize(
                &commit.subject,
                (subject_right - content.left).max(0.0),
                theme::SMALL,
                400,
            );
            let mut row = panel(rect, background)
                .key(format!("git-commit-{}", commit.oid))
                .event_policy(EventPolicy::INTERACTIVE)
                .semantics({
                    let mut semantics =
                        Semantics::new(SemanticRole::Button).name(commit.subject.clone());
                    semantics.state.expanded = Some(*open);
                    semantics
                })
                .cursor(CursorIcon::Pointer)
                .on_click(move || {
                    // A right-click that opened this row's menu is not a click.
                    let mut menu_open = false;
                    toggle_state.try_update(|app| {
                        menu_open = app.ui.menu.is_some();
                        false
                    });
                    if !menu_open {
                        {
                            toggle_state.dispatch(Command::ToggleCommit {
                                root: root.clone(),
                                oid: oid.clone(),
                            });
                        };
                    }
                })
                .on_pointer_down_with_button(move |_cx, pointer, button| {
                    if button == PointerButton::Right {
                        let target = GitMenuTarget::HistoryCommit {
                            repository: menu_root.clone(),
                            oid: menu_oid.clone(),
                        };
                        menu_state.update(move |app| {
                            app.ui.menu = Some(GitMenu {
                                pos: (pointer.point.x, pointer.point.y),
                                target,
                                hover: None,
                            });
                        });
                    }
                })
                .child(graph.element)
                .child(
                    clip(
                        UiRect::new(content.left, rect.top, subject_right, rect.bottom),
                        0.0,
                        0.0,
                    )
                    .child(text(
                        UiRect::new(content.left, rect.top, rect.left + 2000.0, rect.bottom),
                        subject,
                        theme::mono(
                            if hovered {
                                theme::c().text
                            } else {
                                theme::c().text_dim
                            },
                            theme::SMALL,
                        ),
                    )),
                );
            for badge in badges {
                row = row.child(badge);
            }
            if columns {
                row = row
                    .child(
                        clip(
                            UiRect::new(author_left, rect.top, author_left + AUTHOR_W, rect.bottom),
                            0.0,
                            0.0,
                        )
                        .child(text(
                            UiRect::new(author_left, rect.top, author_left + AUTHOR_W, rect.bottom),
                            crate::ui::tabs::ellipsize(&commit.author, AUTHOR_W, theme::SMALL, 400),
                            theme::mono(theme::c().text_muted, theme::SMALL),
                        )),
                    )
                    .child(text(
                        UiRect::new(
                            rect.right - 12.0 - TIME_W,
                            rect.top,
                            rect.right - 12.0,
                            rect.bottom,
                        ),
                        relative_time(commit.timestamp),
                        theme::mono_right(theme::c().text_ghost, theme::SMALL),
                    ));
            }
            row
        }
        HistoryRow::File { oid, kind, path } => {
            let open_state = state.clone();
            let open_focus = editor_focus.clone();
            let root = repository.to_path_buf();
            let open_oid = oid.clone();
            let open_path = path.clone();
            let color = match kind {
                ChangeKind::Added | ChangeKind::Copied => theme::c().git.added,
                ChangeKind::Deleted => theme::c().git.deleted,
                _ => theme::c().git.modified,
            };
            panel(rect, background)
                .key(format!("git-commit-{oid}-{}", path.display()))
                .child(graph.element)
                .event_policy(EventPolicy::INTERACTIVE)
                .cursor(CursorIcon::Pointer)
                .on_click(move || {
                    {
                        open_state.dispatch(Command::OpenCommitChanges {
                            root: root.clone(),
                            oid: open_oid.clone(),
                            focus: Some(open_path.clone()),
                        });
                    };
                    open_focus.focus();
                })
                .child(text(
                    UiRect::new(
                        content.left + 12.0,
                        rect.top,
                        content.left + 24.0,
                        rect.bottom,
                    ),
                    kind.indicator(),
                    theme::mono_bold(color, theme::SMALL),
                ))
                .child(path_label(
                    UiRect::new(
                        content.left + 26.0,
                        rect.top,
                        rect.right - 12.0,
                        rect.bottom,
                    ),
                    path,
                    true,
                    hovered,
                ))
        }
    }
}

/// Tag rows expose hover details and context actions.
pub(crate) fn tag_row(
    rect: UiRect,
    tag: &TagInfo,
    hovered: bool,
    repository: &std::path::Path,
    state: &Binding,
) -> Element {
    let label_rect = UiRect::new(rect.left + 30.0, rect.top, rect.right - 12.0, rect.bottom);
    let label =
        crate::ui::tabs::ellipsize(&tag.name, label_rect.width().max(0.0), theme::SMALL, 400);
    let row = panel(
        rect,
        if hovered {
            VisualStyle::filled(theme::c().surface)
        } else {
            VisualStyle::default()
        },
    )
    .key(format!("git-tag-{}", tag.name))
    .semantics(Semantics::new(SemanticRole::Label).name(format!("Tag: {}", tag.name)))
    .child(crate::ui::drawer::drawer_icon(
        UiId::owned(format!("git-tag-icon-{}", tag.name)),
        "git-ref-tag",
        theme::icon_rect(UiRect::new(
            rect.left + 10.0,
            rect.top + 4.0,
            rect.left + 22.0,
            rect.bottom - 4.0,
        )),
        theme::syntax_color(HighlightId::TYPE),
    ))
    .child(clip(label_rect, 0.0, 0.0).child(text(
        label_rect,
        label,
        theme::mono(
            if hovered {
                theme::c().text
            } else {
                theme::c().text_dim
            },
            theme::SMALL,
        ),
    )));
    tag_context_menu(row, repository, tag.name.clone(), state)
}

pub(crate) fn tag_context_menu(
    element: Element,
    repository: &std::path::Path,
    name: String,
    state: &Binding,
) -> Element {
    let state = state.clone();
    let repository = repository.to_path_buf();
    element
        .event_policy(EventPolicy::INTERACTIVE)
        .on_pointer_down_with_button(move |cx, pointer, button| {
            if button == PointerButton::Right {
                state.update(|app| {
                    app.ui.clear_hover_cards();
                    app.ui.menu = Some(GitMenu {
                        pos: (pointer.point.x, pointer.point.y),
                        target: GitMenuTarget::Tags {
                            repository: repository.clone(),
                            name: name.clone(),
                        },
                        hover: None,
                    });
                });
                cx.stop_propagation();
            }
        })
}

/// A collapsible section title with right-hand buttons.
pub(crate) fn section_header(
    rect: UiRect,
    filter: u8,
    title: &'static str,
    collapsed: bool,
    buttons: Vec<RowButton>,
    count: usize,
    state: Binding,
) -> Element {
    let visual_title = match filter {
        CONFLICTS => "Conflicts",
        STAGED => "Staged",
        UNSTAGED => "Unstaged",
        UNTRACKED => "Untracked",
        _ => title,
    };
    let title_width = crate::ui::components::text::SingleLineText::new(
        visual_title,
        UiRect::new(0.0, 0.0, 1000.0, rect.height()),
        theme::sans(theme::c().text_muted, 11.0),
    )
    .width();
    let count_left = rect.left + 31.0 + title_width;
    let chevron_id = match filter {
        CONFLICTS => "git-section-conflicts-chevron",
        STAGED => "git-section-staged-chevron",
        UNSTAGED => "git-section-unstaged-chevron",
        UNTRACKED => "git-section-untracked-chevron",
        STASHES => "git-section-stashes-chevron",
        WORKTREES => "git-section-worktrees-chevron",
        TAGS => "git-section-tags-chevron",
        _ => "git-section-history-chevron",
    };
    let section = panel(rect, VisualStyle::filled(theme::c().sidebar))
        .key(format!("git-section-{filter}"))
        .semantics({
            let mut semantics = Semantics::new(SemanticRole::Button).name(title);
            semantics.state.expanded = Some(!collapsed);
            semantics
        })
        .event_policy(EventPolicy::INTERACTIVE)
        .cursor(CursorIcon::Pointer)
        .on_click(move || {
            state.update(move |app| {
                if filter == TAGS && app.ui.menu.is_some() {
                    return;
                }
                if !app.ui.collapsed_sections.insert(filter) {
                    app.ui.collapsed_sections.remove(&filter);
                }
                if filter >= STASHES {
                    app.ui.details_scroll = 0.0;
                    app.ui.clear_hover_cards();
                }
            });
        })
        .child(crate::ui::drawer::drawer_icon(
            chevron_id,
            if collapsed {
                "chevron-right"
            } else {
                "chevron-down"
            },
            theme::icon_rect(UiRect::new(
                rect.left + 9.0,
                rect.top + 5.0,
                rect.left + 21.0,
                rect.top + 17.0,
            )),
            theme::c().text_ghost,
        ))
        .child(text(
            UiRect::new(rect.left + 25.0, rect.top, rect.right - 12.0, rect.bottom),
            visual_title,
            theme::sans(theme::c().text_muted, 11.0),
        ))
        .child(text(
            UiRect::new(count_left, rect.top, count_left + 42.0, rect.bottom),
            count.to_string(),
            theme::sans(theme::c().text_ghost, 10.0),
        ));
    if buttons.is_empty() {
        section
    } else {
        row_buttons(section, rect, rect.right - 12.0, buttons)
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn worktree_row(
    rect: UiRect,
    worktree: &WorktreeInfo,
    repository: &RepositorySnapshot,
    main: bool,
    hovered: bool,
    enabled: bool,
    state: Binding,
    _store: Snapshot,
) -> Element {
    let current = worktree.path == repository.worktree_root;
    let mut buttons = Vec::new();
    if hovered && enabled && !worktree.bare {
        let open_state = state.clone();
        let path = worktree.path.clone();
        buttons.push(RowButton {
            key: format!("worktree-open-{}", path.display()),
            icon: "worktree-open",
            name: "Open Worktree (Add and Select)",
            on_click: Box::new(move || {
                open_state.dispatch(Command::OpenWorktree { path: path.clone() });
            }),
        });
        let add_state = state.clone();
        let path = worktree.path.clone();
        buttons.push(RowButton {
            key: format!("worktree-add-{}", path.display()),
            icon: "plus",
            name: "Add Worktree to Workspace",
            on_click: Box::new(move || {
                add_state.dispatch(Command::AddWorktreeFolder { path: path.clone() });
            }),
        });
        if !main && !current && worktree.locked.is_none() {
            let source = repository.worktree_root.clone();
            let path = worktree.path.clone();
            let delete_state = state.clone();
            buttons.push(RowButton {
                key: format!("worktree-delete-{}", path.display()),
                icon: "trash",
                name: "Delete Worktree",
                on_click: Box::new(move || {
                    delete_state.dispatch(Command::RequestRemoveWorktree {
                        repository: source.clone(),
                        path: path.clone(),
                        force: false,
                    });
                }),
            });
        }
    }
    let label_right = rect.right - BUTTON_W * buttons.len() as f32 - 12.0;
    let mut detail = worktree.branch.clone().unwrap_or_else(|| {
        if worktree.bare {
            "Bare repository".into()
        } else {
            format!(
                "Detached HEAD · {}",
                worktree.oid.chars().take(8).collect::<String>()
            )
        }
    });
    if current {
        detail.insert_str(0, "Current · ");
    }
    if let Some(reason) = &worktree.locked {
        detail.insert_str(0, "Locked · ");
        if !reason.is_empty() {
            detail.push_str(&format!(": {reason}"));
        }
    }
    let name = crate::ui::tabs::ellipsize(
        &repository_name(&worktree.path),
        (label_right - rect.left - 26.0).max(0.0),
        theme::UI_SIZE,
        400,
    );
    let detail_label = crate::ui::tabs::ellipsize(
        &detail,
        (label_right - rect.left - 26.0).max(0.0),
        theme::SMALL,
        400,
    );
    let open_state = state;
    let path = worktree.path.clone();
    let bare = worktree.bare;
    let mut row = panel(
        rect,
        if hovered {
            VisualStyle::filled(theme::c().active_line)
        } else {
            VisualStyle::default()
        },
    )
    .key(format!("git-worktree-{}", worktree.path.display()))
    .event_policy(EventPolicy::INTERACTIVE)
    .semantics(Semantics::new(SemanticRole::Button).name(format!(
        "{} · {detail}",
        crate::ui::display_path(&worktree.path)
    )))
    .cursor(if bare {
        CursorIcon::Default
    } else {
        CursorIcon::Pointer
    })
    .on_click(move || {
        if !bare {
            open_state.dispatch(Command::OpenWorktree { path: path.clone() });
        }
    })
    .child(text(
        UiRect::new(rect.left + 20.0, rect.top, label_right, rect.top + ROW_H),
        name,
        theme::mono(
            if current {
                theme::c().accent
            } else {
                theme::c().text
            },
            theme::UI_SIZE,
        ),
    ))
    .child(text(
        UiRect::new(rect.left + 20.0, rect.top + ROW_H, label_right, rect.bottom),
        detail_label,
        theme::mono(theme::c().text_dim, theme::SMALL),
    ));
    row = row_buttons(row, rect, rect.right - 8.0, buttons);
    row
}

/// A stash: its message, and apply, pop and drop while hovered.
pub(crate) fn stash_row(
    rect: UiRect,
    stash: &StashInfo,
    hovered: bool,
    state: Binding,
    _store: Snapshot,
) -> Element {
    let trailing_w = if hovered { BUTTON_W * 3.0 + 12.0 } else { 64.0 };
    let mut row = panel(
        rect,
        if hovered {
            VisualStyle::filled(theme::c().surface)
        } else {
            VisualStyle::default()
        },
    )
    .key(format!("git-stash-{}", stash.oid))
    .child(crate::ui::drawer::drawer_icon(
        "git-stash-row-icon",
        "git-stash",
        UiRect::new(
            rect.left + 11.0,
            rect.top + (rect.height() - 12.0) / 2.0,
            rect.left + 23.0,
            rect.top + (rect.height() + 12.0) / 2.0,
        ),
        theme::c().text_ghost,
    ))
    .child(
        clip(
            UiRect::new(
                rect.left + 30.0,
                rect.top,
                rect.right - trailing_w,
                rect.bottom,
            ),
            0.0,
            0.0,
        )
        .child(text(
            UiRect::new(rect.left + 30.0, rect.top, rect.left + 2000.0, rect.bottom),
            stash.message.clone(),
            theme::mono(
                if hovered {
                    theme::c().text
                } else {
                    theme::c().text_dim
                },
                theme::SMALL,
            ),
        )),
    );
    if !hovered {
        return row.child(text(
            UiRect::new(rect.right - 62.0, rect.top, rect.right - 12.0, rect.bottom),
            format!("stash@{{{}}}", stash.index),
            theme::mono_right(theme::c().text_ghost, 9.0),
        ));
    }
    let buttons = [
        ("git-stash-apply", "Apply Stash", StashAction::Apply),
        ("git-stash-pop", "Pop Stash", StashAction::Pop),
        ("trash", "Drop Stash", StashAction::Drop),
    ]
    .into_iter()
    .map(|(icon, name, action)| {
        let action_state = state.clone();

        let oid = stash.oid.clone();
        RowButton {
            key: format!("git-stash-{}-{action:?}", stash.oid),
            icon,
            name,
            on_click: Box::new(move || match action {
                StashAction::Apply => {
                    action_state.dispatch(Command::ApplyStash {
                        oid: oid.clone(),
                        pop: false,
                    });
                }
                StashAction::Pop => {
                    action_state.dispatch(Command::ApplyStash {
                        oid: oid.clone(),
                        pop: true,
                    });
                }
                StashAction::Drop => {
                    action_state.dispatch(Command::RequestDropStash { oid: oid.clone() });
                }
            }),
        }
    })
    .collect();
    row = row_buttons(row, rect, rect.right - 8.0, buttons);
    row
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum StashAction {
    Apply,
    Pop,
    Drop,
}

/// The title bar label for an operation in progress, with its progress.
pub(crate) fn operation_label(repository: &RepositorySnapshot) -> Option<String> {
    let label = match repository.repository_state {
        RepositoryState::MergeInProgress => "MERGING",
        RepositoryState::RebaseInProgress => "REBASING",
        RepositoryState::CherryPickInProgress => "CHERRY-PICKING",
        RepositoryState::RevertInProgress => "REVERTING",
        RepositoryState::Bisecting => "BISECTING",
        RepositoryState::Normal | RepositoryState::DetachedHead | RepositoryState::UnbornBranch => {
            return None;
        }
    };
    Some(match repository.operation_step {
        Some((step, total)) => format!("{label} {step}/{total}"),
        None => label.to_owned(),
    })
}

pub(crate) const BUTTON_W: f32 = 20.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowAction {
    OpenFile,
    Stage,
    Unstage,
    Discard,
    /// Resolve a conflict with the checked-out side (`--ours`).
    KeepCurrent,
    /// Resolve a conflict with the side being merged in (`--theirs`).
    TakeIncoming,
}

/// Buttons on a section header, left to right.
pub(crate) fn section_actions(section: u8) -> &'static [RowAction] {
    match section {
        STAGED => &[RowAction::Unstage],
        UNSTAGED | UNTRACKED => &[RowAction::Discard, RowAction::Stage],
        // Marking every conflict resolved at once is too easy to regret.
        _ => &[],
    }
}

/// Buttons on a file row, left to right.
pub(crate) fn file_actions(section: u8) -> &'static [RowAction] {
    match section {
        STAGED => &[RowAction::OpenFile, RowAction::Unstage],
        CONFLICTS => &[
            RowAction::OpenFile,
            RowAction::KeepCurrent,
            RowAction::TakeIncoming,
            RowAction::Stage,
        ],
        _ => &[RowAction::OpenFile, RowAction::Discard, RowAction::Stage],
    }
}

impl RowAction {
    pub(crate) fn icon(self) -> &'static str {
        match self {
            Self::OpenFile => "go-to-file",
            Self::Stage => "plus",
            Self::Unstage => "minus",
            Self::Discard => "discard",
            Self::KeepCurrent => "arrow-left",
            Self::TakeIncoming => "arrow-right",
        }
    }

    pub(crate) fn section_name(self, section: u8) -> &'static str {
        match self {
            Self::OpenFile | Self::KeepCurrent | Self::TakeIncoming => self.file_name(section),
            Self::Stage => "Stage All Changes",
            Self::Unstage => "Unstage All Changes",
            Self::Discard if section == UNTRACKED => "Delete All Untracked Files",
            Self::Discard => "Discard All Changes",
        }
    }

    pub(crate) fn file_name(self, section: u8) -> &'static str {
        match self {
            Self::OpenFile => "Open File",
            Self::Stage if section == CONFLICTS => "Mark as Resolved",
            Self::Stage => "Stage Changes",
            Self::Unstage => "Unstage Changes",
            Self::Discard if section == UNTRACKED => "Delete File",
            Self::Discard => "Discard Changes",
            Self::KeepCurrent => "Keep Current (Ours)",
            Self::TakeIncoming => "Take Incoming (Theirs)",
        }
    }

    /// Run a Git action on `paths` from `section`. Opening a file needs the
    /// editor, so rows handle it themselves.
    pub(crate) fn run(self, state: &Binding, _store: &Snapshot, section: u8, paths: Vec<PathBuf>) {
        match self {
            Self::OpenFile => {}
            Self::Stage => {
                state.dispatch(Command::StagePaths { paths });
            }
            Self::Unstage => {
                state.dispatch(Command::UnstagePaths { paths });
            }
            Self::Discard if section == UNTRACKED => {
                state.dispatch(Command::RequestDiscard {
                    tracked: Vec::new(),
                    untracked: paths,
                });
            }
            Self::Discard => {
                state.dispatch(Command::RequestDiscard {
                    tracked: paths,
                    untracked: Vec::new(),
                });
            }
            Self::KeepCurrent | Self::TakeIncoming => {
                let side = if self == Self::KeepCurrent {
                    ConflictSide::Ours
                } else {
                    ConflictSide::Theirs
                };
                for path in paths {
                    state.dispatch(Command::ResolveConflict { path, side });
                }
            }
        }
    }
}

pub(crate) struct RowButton {
    key: String,
    icon: &'static str,
    name: &'static str,
    on_click: Box<dyn Fn() + Send + Sync>,
}

/// Add `buttons` to `row`, right-aligned to end at `right`.
pub(crate) fn row_buttons(
    mut row: Element,
    rect: UiRect,
    right: f32,
    buttons: Vec<RowButton>,
) -> Element {
    let mut left = right - BUTTON_W * buttons.len() as f32;
    for button in buttons {
        let slot = UiRect::new(left, rect.top + 1.0, left + BUTTON_W, rect.bottom - 1.0);
        let on_click = button.on_click;
        let icon_key = button.key.clone();
        row = row.child(
            Element::new(move |cx| {
                let style = if cx.context.interaction_flags(&cx.id).hovered {
                    VisualStyle::filled(theme::c().active_line).radius(3.0)
                } else {
                    VisualStyle::default()
                };
                UiElement::panel(cx.id, slot, style).children(cx.children)
            })
            .key(button.key)
            .event_policy(EventPolicy::INTERACTIVE)
            .semantics(Semantics::new(SemanticRole::Button).name(button.name))
            .cursor(CursorIcon::Pointer)
            .on_pointer_down_with_button(move |cx, _pointer, button| {
                if button == PointerButton::Left {
                    cx.stop_propagation();
                }
            })
            .on_click(move |cx: &mut UiEventContext| {
                on_click();
                cx.stop_propagation();
            })
            .child(crate::ui::drawer::drawer_icon(
                lgui::core::UiId::owned(format!("{icon_key}-icon")),
                button.icon,
                UiRect::new(
                    slot.left + 3.0,
                    slot.top + (slot.height() - 14.0) / 2.0,
                    slot.left + 17.0,
                    slot.top + (slot.height() + 14.0) / 2.0,
                ),
                theme::c().text_muted,
            )),
        );
        left += BUTTON_W;
    }
    row
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn file_row(
    rect: UiRect,
    path: PathBuf,
    // Listed under its folder row: indented, by file name.
    nested: bool,
    file_state: FileState,
    section: u8,
    target: DiffTarget,
    active: bool,
    hovered: bool,
    // Picked with Ctrl/Shift-click, together with `order`: the section's
    // visible files in row order, for Shift range selection.
    selected: bool,
    order: std::sync::Arc<Vec<PathBuf>>,
    repository_root: PathBuf,
    state: Binding,
    store: Snapshot,
    editor_focus: UiFocusHandle,
) -> Element {
    let staged = section == STAGED;
    let kind = file_state.display_kind();
    let side_kind = if staged {
        file_state.index
    } else {
        file_state.worktree
    };
    let side_kind = if file_state.conflict.is_some() || section == CONFLICTS {
        ChangeKind::Unmerged
    } else if side_kind == ChangeKind::Unmodified {
        kind
    } else {
        side_kind
    };
    let indicator = side_kind.indicator();
    let indicator_color = change_color(side_kind);
    let actions = file_actions(section)
        .iter()
        .copied()
        .filter(|action| {
            section == CONFLICTS || !matches!(action, RowAction::Stage | RowAction::Unstage)
        })
        .collect::<Vec<_>>();
    // Reserve room for action buttons only while the row is hovered.
    let trailing_w = if hovered {
        BUTTON_W * actions.len() as f32 + 32.0
    } else {
        32.0
    };
    let indent = if nested { 14.0 } else { 0.0 };
    let name_left = rect.left + 58.0 + indent;
    let name_viewport = UiRect::new(name_left, rect.top, rect.right - trailing_w, rect.bottom);
    let row_key = format!("git-file-{target:?}-{}", path.display());
    let menu_state = state.clone();
    let menu_repository = repository_root.clone();
    let menu_path = path.clone();
    let row = Element::new(move |cx| {
        let style = if active || selected {
            VisualStyle::filled(theme::c().active_line)
        } else if hovered {
            VisualStyle::filled(theme::c().surface)
        } else {
            VisualStyle::default()
        };
        UiElement::panel(cx.id, rect, style).children(cx.children)
    })
    .key(row_key.clone())
    .event_policy(EventPolicy::INTERACTIVE)
    .cursor(CursorIcon::Pointer)
    .on_pointer_down_with_button(move |cx, pointer, button| {
        match button {
            PointerButton::Left => {
                // Like Explorer, defer opening until release so a drag can
                // land on a pane or create a split instead of opening Changes.
                let request = crate::features::git::DiffRequest {
                    repository_root: menu_repository.clone(),
                    path: menu_path.clone(),
                    target,
                };
                let point = (pointer.point.x, pointer.point.y);
                menu_state.dispatch(Command::PressChange {
                    request,
                    untracked: section == UNTRACKED,
                    point,
                    section,
                    order: order.clone(),
                });
            }
            PointerButton::Right => {
                let target = GitMenuTarget::File {
                    repository: menu_repository.clone(),
                    path: menu_path.clone(),
                    section,
                };
                let selection_key = (section, menu_path.clone());
                menu_state.update(move |app| {
                    // Right-clicking outside the selection starts over.
                    if !app.ui.selection.contains(&selection_key) {
                        app.ui.selection.clear();
                        app.ui.selection_anchor = None;
                    }
                    app.ui.menu = Some(GitMenu {
                        pos: (pointer.point.x, pointer.point.y),
                        target,
                        hover: None,
                    });
                });
            }
            _ => return,
        }
        cx.stop_propagation();
    })
    .child(crate::ui::drawer::drawer_icon(
        UiId::owned(format!("{row_key}-file-icon")),
        crate::ui::file_icons::icon_for_file(
            &path.file_name().unwrap_or_default().to_string_lossy(),
        ),
        UiRect::new(
            rect.left + 36.0 + indent,
            rect.top + (rect.height() - 16.0) / 2.0,
            rect.left + 52.0 + indent,
            rect.top + (rect.height() + 16.0) / 2.0,
        ),
        theme::c().text_muted,
    ))
    .child(change_path_label(
        name_viewport,
        &path,
        // Grouped rows sit under their folder row already.
        !nested,
        side_kind,
    ));
    let stage_state = state.clone();
    let stage_path = path.clone();
    let enabled = store.get().operation.is_none() && section != CONFLICTS;
    let mut row = row.child(crate::ui::git::drawer::checkbox(
        UiRect::new(
            rect.left + 10.0 + indent,
            rect.top,
            rect.left + 30.0 + indent,
            rect.bottom,
        ),
        format!("{row_key}-staged"),
        format!(
            "{} {}",
            if staged { "Unstage" } else { "Stage" },
            path.display()
        ),
        staged,
        enabled,
        move || {
            stage_state.dispatch(if staged {
                Command::UnstagePaths {
                    paths: vec![stage_path.clone()],
                }
            } else {
                Command::StagePaths {
                    paths: vec![stage_path.clone()],
                }
            });
        },
    ));
    if active {
        row = row.child(panel(
            UiRect::new(rect.left, rect.top, rect.left + 2.0, rect.bottom),
            VisualStyle::filled(theme::c().accent),
        ));
    }
    row = row.child(text(
        UiRect::new(rect.right - 24.0, rect.top, rect.right - 8.0, rect.bottom),
        indicator,
        theme::mono(indicator_color, 10.0),
    ));
    if !hovered {
        return row;
    }
    let buttons = actions
        .iter()
        .map(|&action| {
            let action_state = state.clone();
            let action_store = store.clone();
            let action_path = path.clone();
            let open_path = repository_root.join(&path);
            let open_focus = editor_focus.clone();
            RowButton {
                key: format!("{row_key}-{action:?}"),
                icon: action.icon(),
                name: action.file_name(section),
                on_click: Box::new(move || {
                    if action == RowAction::OpenFile {
                        if action_state.dispatch(Command::OpenFile {
                            path: open_path.clone(),
                        }) {
                            open_focus.focus();
                        }
                    } else {
                        action.run(
                            &action_state,
                            &action_store,
                            section,
                            vec![action_path.clone()],
                        );
                    }
                }),
            }
        })
        .collect();
    row_buttons(row, rect, rect.right - 32.0, buttons)
}
