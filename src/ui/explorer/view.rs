//! File drawer: an empty-state prompt when no folder is open, or the folders'
//! file tree when one is. Also owns the right-click capture and resize handle.
//!
//! The tree is lazy: opening a folder reads only its top level, and a
//! sub-directory is read from disk only when first expanded. Renders read from
//! the cached listings, so no filesystem I/O happens per frame. The opened
//! folders themselves are shown as root nodes (expanded by default) with their
//! contents indented beneath it.
//!
//! The view reads only the Explorer projection and its frame inputs; anything
//! touching other features goes out as a [`Command`].

use std::path::{Path, PathBuf};

use lgui::core::{
    CursorIcon, EventPolicy, IconStyle, PointerButton, UiElement, UiEventKind, UiEventPayload,
    UiId, WheelUnit, clip, precompiled,
};
use lgui::prelude::{Color, Element, TextAlign, UiRect, VisualStyle, panel, text};

use super::commands::{Command, Commands, Decoration, EditInput, Frame, GitTone};
use super::state::{ExplorerPanelState, ExplorerReveal};
use crate::features::explorer::{ExplorerContextTarget, ExplorerCreateKind, ExplorerTargetKind};
use crate::ui::components::input::{self, AccessInputOptions, InputBinding, InputStyle};
use crate::ui::components::scrollbar;
use crate::ui::components::state_access::StateAccess;
use crate::ui::drawer::drawer_icon;
use crate::ui::theme;

const HEADER_H: f32 = theme::TABS_H;
const ROW_H: f32 = theme::DRAWER_ROW_H;
const CREATE_ROW_H: f32 = 28.0;
const INDENT: f32 = 12.0;
const TREE_ICON_SIZE: f32 = 16.0;
const TREE_ICON_GAP: f32 = 4.0;
const TREE_LABEL_OFFSET: f32 = TREE_ICON_SIZE + TREE_ICON_GAP;
/// Extra shaping room so the final glyph is not wrapped or clipped prematurely.
const TREE_TEXT_PAD: f32 = 6.0;
const DIRECTORY_RIGHT_PAD: f32 = 12.0;
const STATUS_RIGHT_PAD: f32 = 8.0;
const STATUS_WIDTH: f32 = 17.0;
const STATUS_GAP: f32 = 5.0;
const FILE_RIGHT_PAD: f32 = STATUS_RIGHT_PAD + STATUS_WIDTH + STATUS_GAP;

type Access = StateAccess<ExplorerPanelState>;

#[derive(Clone, Debug)]
struct StickyDirectory {
    key: String,
    name: String,
    depth: usize,
    source_y: f32,
    is_expanded: bool,
}

/// One row for sticky-header lookups. `dir` is the directory the row belongs
/// to (the directory itself for a directory row), as an index into
/// [`TreeIndex::dirs`].
#[derive(Clone, Copy, Debug)]
struct TreeRowMeta {
    y: f32,
    depth: usize,
    dir: u32,
}

/// A directory recorded while building the tree, linked to its parent so an
/// ancestry is walked rather than copied for every row.
#[derive(Clone, Debug)]
struct DirMeta {
    directory: StickyDirectory,
    parent: Option<u32>,
}

/// Rows and directories seen by one tree pass, as flat vectors.
#[derive(Clone, Debug, Default)]
struct TreeIndex {
    rows: Vec<TreeRowMeta>,
    dirs: Vec<DirMeta>,
}

impl TreeIndex {
    fn push_dir(&mut self, directory: StickyDirectory, parent: Option<u32>) -> u32 {
        self.dirs.push(DirMeta { directory, parent });
        (self.dirs.len() - 1) as u32
    }

    /// Directories from the root down to `dir`.
    fn chain(&self, dir: u32) -> Vec<u32> {
        let mut chain = vec![dir];
        let mut current = dir;
        while let Some(parent) = self.dirs[current as usize].parent {
            chain.push(parent);
            current = parent;
        }
        chain.reverse();
        chain
    }

    /// Whether `dir` is `ancestor` or inside it.
    fn within(&self, ancestor: u32, mut dir: u32) -> bool {
        loop {
            if dir == ancestor {
                return true;
            }
            match self.dirs[dir as usize].parent {
                Some(parent) => dir = parent,
                None => return false,
            }
        }
    }
}

#[derive(Clone, Debug)]
struct StickyRow {
    directory: StickyDirectory,
    top: f32,
}

/// Everything one tree pass needs besides the row position.
struct TreeContext<'a> {
    panel: &'a ExplorerPanelState,
    state: &'a Access,
    frame: &'a Frame<'a>,
    commands: &'a Commands,
    input: &'a EditInput,
    rect: UiRect,
    content_right: f32,
    /// Content-space `y` range whose rows get UI nodes; rows outside it only
    /// advance the layout cursor.
    visible: (f32, f32),
}

impl TreeContext<'_> {
    fn row_visible(&self, y: f32, height: f32) -> bool {
        y + height > self.visible.0 && y < self.visible.1
    }
}

/// Rows built beyond each viewport edge so short scrolls need no rebuild.
const OVERSCAN: f32 = ROW_H * 8.0;

pub fn render(
    rect: UiRect,
    state: Access,
    frame: &Frame<'_>,
    commands: Commands,
    header: Element,
    input: EditInput,
) -> Element {
    let s = state.get();
    let header_rect = UiRect::new(rect.left, rect.top, rect.right, rect.top + HEADER_H);
    let content_rect = UiRect::new(rect.left, header_rect.bottom, rect.right, rect.bottom);

    let mut bar = panel(rect, VisualStyle::filled(theme::c().sidebar));

    // Background right-click capture: covers the drawer at the lowest z-order.
    // Rows and the resize handle are drawn later and sit above it, so they win
    // hit-testing and this only receives events on empty space.
    let menu_commands = commands.clone();
    let clear_hover = state.clone();
    let capture = panel(content_rect, VisualStyle::default())
        .event_policy(EventPolicy::INTERACTIVE)
        .on_event_capture(UiEventKind::PointerMove, move |_cx, _payload| {
            clear_hover.try_update(|panel| {
                let changed = panel.ui.hovered_path.is_some();
                panel.ui.hovered_path = None;
                changed
            });
        })
        .on_pointer_down_with_button(move |_cx, p, button| {
            if button == PointerButton::Right {
                menu_commands.dispatch(Command::OpenMenu {
                    point: (p.point.x, p.point.y),
                    target: None,
                });
            }
        });
    bar = bar.child(capture);

    // Content: empty-state prompt, or all root folders in the current project.
    if s.model.roots().is_empty() {
        for el in empty_state(content_rect, &commands) {
            bar = bar.child(el);
        }
    } else {
        let roots = s
            .model
            .roots()
            .iter()
            .map(|dir| {
                let key = dir.to_string_lossy().into_owned();
                let name = dir
                    .file_name()
                    .map(|value| value.to_string_lossy().into_owned())
                    .unwrap_or_else(|| key.clone());
                (dir.clone(), key, name)
            })
            .collect::<Vec<_>>();
        let content_right = roots
            .iter()
            .map(|(_, key, name)| tree_content_right(key, name, &s, content_rect.left))
            .fold(content_rect.right, f32::max);
        let max_scroll_x = (content_right - content_rect.right).max(0.0);
        let scroll_x = s.ui.scroll_x.clamp(0.0, max_scroll_x);

        // Only rows near the viewport get UI nodes. The scroll offset is clamped
        // by the content height, which is known only after a pass, so a pass
        // built for a stale offset is redone once for the clamped one.
        let build = |scroll_hint: f32| {
            let tree = TreeContext {
                panel: &s,
                state: &state,
                frame,
                commands: &commands,
                input: &input,
                rect: content_rect,
                content_right,
                visible: (
                    content_rect.top + scroll_hint - OVERSCAN,
                    content_rect.bottom + scroll_hint + OVERSCAN,
                ),
            };
            build_roots(&tree, &roots)
        };
        let (mut tree_els, mut rows, mut content_bottom) = build(s.ui.scroll);
        let max_scroll = (content_bottom - content_rect.bottom).max(0.0);
        let mut scroll = s.ui.scroll.clamp(0.0, max_scroll);
        if let Some(target) = s.ui.reveal {
            scroll = revealed_scroll(scroll, target, content_rect.height(), max_scroll);
        }
        if scroll != s.ui.scroll {
            (tree_els, rows, content_bottom) = build(scroll);
        }
        let tree = TreeContext {
            panel: &s,
            state: &state,
            frame,
            commands: &commands,
            input: &input,
            rect: content_rect,
            content_right,
            visible: (0.0, 0.0),
        };

        let st = state.clone();
        bar = bar.on_event(UiEventKind::Wheel, move |_cx, payload| {
            if let UiEventPayload::Wheel { delta } = payload {
                let (step_x, step_y) = match delta.unit {
                    WheelUnit::Lines => (delta.x * ROW_H * 3.0, delta.y * ROW_H * 3.0),
                    WheelUnit::Pixels => (delta.x, delta.y),
                };
                st.update(move |panel| {
                    // Start from the scroll on screen, which a reveal may have moved.
                    panel.ui.reveal = None;
                    panel.ui.scroll = (scroll - step_y).clamp(0.0, max_scroll);
                    panel.ui.scroll_x = (panel.ui.scroll_x - step_x).clamp(0.0, max_scroll_x);
                });
            }
        });

        let mut clip_el = clip(content_rect, -scroll_x, -scroll);
        for el in tree_els {
            clip_el = clip_el.child(el);
        }
        bar = bar.child(clip_el);

        for sticky in sticky_rows(&rows, content_rect.top, scroll)
            .into_iter()
            .rev()
        {
            let directory = sticky.directory;
            let indent = content_rect.left + INDENT + directory.depth as f32 * INDENT;
            // The workspace root keeps its plain color, as in the tree itself.
            let label_color = if directory.depth == 0 {
                theme::c().text_soft
            } else {
                tree_label_color(
                    (frame.decorate)(Path::new(&directory.key)),
                    theme::c().text_soft,
                )
            };
            bar = bar.child(dir_row(
                &tree,
                &directory.key,
                &directory.name,
                indent,
                sticky.top,
                -scroll_x,
                directory.is_expanded,
                true,
                false,
                label_color,
            ));
        }

        if max_scroll > 0.0 && (s.ui.hovered || s.ui.scrollbar_dragging) {
            bar = bar.child(vertical_scrollbar(
                content_rect,
                content_bottom,
                scroll,
                state.clone(),
            ));
        }
        if max_scroll_x > 0.0 && (s.ui.hovered || s.ui.horizontal_scrollbar_dragging) {
            bar = bar.child(horizontal_scrollbar(
                content_rect,
                content_right,
                scroll_x,
                state.clone(),
            ));
        }
    }

    // Keep the tree's fractional top edge beneath the header separator.
    bar = bar.child(header);

    // Resize handle on the drawer's left edge. An 8px invisible hit strip
    // (drawn last, above the file rows) carries the drag; the 1px filled panel
    // inside it is the visible divider.
    if frame.resizable {
        let handle_rect = UiRect::new(rect.left, rect.top, rect.left + 8.0, rect.bottom);
        let win_right = rect.right;
        let down = commands.clone();
        let drag = commands.clone();
        let up = commands.clone();
        let resizing = frame.resizing;
        let handle = crate::ui::components::resize_handle::render(
            "file-tree-resize-handle",
            handle_rect,
            UiRect::new(rect.left, rect.top, rect.left + 1.0, rect.bottom),
            CursorIcon::ResizeHorizontal,
            resizing,
        )
        .on_pointer_down_with_button(move |_cx, _p, button| {
            if button == PointerButton::Left {
                down.dispatch(Command::ResizeStart);
            }
        })
        .on_pointer_drag(move |_cx, p| {
            drag.dispatch(Command::Resize(resized_sidebar_width(win_right, p.point.x)));
        })
        .on_pointer_up(move |_cx, _p| up.dispatch(Command::ResizeEnd));
        bar = bar.child(handle);
    }

    bar
}

fn resized_sidebar_width(window_right: f32, pointer_x: f32) -> f32 {
    (window_right - pointer_x).clamp(theme::SIDEBAR_MIN_W, theme::SIDEBAR_MAX_W)
}

/// Empty state from the Explorer mockup, without its recent-workspace and
/// bottom workspace-switching sections.
fn empty_state(rect: UiRect, commands: &Commands) -> Vec<Element> {
    let cx = (rect.left + rect.right) / 2.0;
    let icon_rect = UiRect::new(cx - 24.0, rect.top + 28.0, cx + 24.0, rect.top + 76.0);
    let icon = theme::bordered(icon_rect, theme::c().surface, theme::c().border, 10.0, 1.0).child(
        drawer_icon(
            "sidebar.empty.folder",
            crate::ui::file_icons::FOLDER_ICON,
            UiRect::new(
                icon_rect.left + 12.0,
                icon_rect.top + 12.0,
                icon_rect.right - 12.0,
                icon_rect.bottom - 12.0,
            ),
            theme::c().text_dim,
        ),
    );

    let mut centered_title = theme::sans_semibold(theme::c().text, theme::UI_SIZE);
    centered_title.align = TextAlign::Center;
    let title = text(
        UiRect::new(
            rect.left + 14.0,
            rect.top + 88.0,
            rect.right - 14.0,
            rect.top + 108.0,
        ),
        "No Folder Opened",
        centered_title,
    );

    let mut centered_desc = theme::sans(theme::c().text_dim, theme::SMALL);
    centered_desc.align = TextAlign::Center;
    let desc_line_one = text(
        UiRect::new(
            rect.left + 14.0,
            rect.top + 111.0,
            rect.right - 14.0,
            rect.top + 128.0,
        ),
        "Open a directory to explore files,",
        centered_desc,
    );
    let desc_line_two = text(
        UiRect::new(
            rect.left + 14.0,
            rect.top + 127.0,
            rect.right - 14.0,
            rect.top + 144.0,
        ),
        "track changes, and start editing.",
        centered_desc,
    );

    let btn_rect = UiRect::new(
        rect.left + 14.0,
        rect.top + 164.0,
        rect.right - 14.0,
        rect.top + 198.0,
    );
    let open = commands.clone();
    let btn = theme::bordered(btn_rect, theme::c().surface, theme::c().border, 4.0, 1.0)
        .event_policy(EventPolicy::INTERACTIVE)
        .cursor(CursorIcon::Pointer)
        .on_click(move || open.dispatch(Command::ChooseFolder))
        .child(drawer_icon(
            "sidebar.empty.open-folder",
            crate::ui::file_icons::FOLDER_OPEN_ICON,
            UiRect::new(
                btn_rect.left + 12.0,
                btn_rect.top + 10.0,
                btn_rect.left + 26.0,
                btn_rect.top + 24.0,
            ),
            theme::c().accent,
        ))
        .child(text(
            UiRect::new(
                btn_rect.left + 34.0,
                btn_rect.top,
                btn_rect.right - 12.0,
                btn_rect.bottom,
            ),
            "Open Folder",
            theme::sans_semibold(theme::c().text, theme::UI_SIZE),
        ));

    let clone_rect = UiRect::new(
        rect.left + 14.0,
        rect.top + 202.0,
        rect.right - 14.0,
        rect.top + 232.0,
    );
    let clone = commands.clone();
    let clone_btn = panel(clone_rect, VisualStyle::default().radius(4.0))
        .event_policy(EventPolicy::INTERACTIVE)
        .cursor(CursorIcon::Pointer)
        .on_click(move || clone.dispatch(Command::CloneRepository))
        .child(drawer_icon(
            "sidebar.empty.clone",
            "git-branch",
            UiRect::new(
                clone_rect.left + 12.0,
                clone_rect.top + 8.0,
                clone_rect.left + 26.0,
                clone_rect.top + 22.0,
            ),
            theme::c().text_faint,
        ))
        .child(text(
            UiRect::new(
                clone_rect.left + 34.0,
                clone_rect.top,
                clone_rect.right - 92.0,
                clone_rect.bottom,
            ),
            "Clone Repository...",
            theme::sans(theme::c().text_muted, theme::UI_SIZE),
        ))
        .child(text(
            UiRect::new(
                clone_rect.right - 86.0,
                clone_rect.top,
                clone_rect.right - 12.0,
                clone_rect.bottom,
            ),
            "Ctrl+Shift+G",
            theme::mono_right(theme::c().text_faint, 9.0),
        ));

    vec![
        icon,
        title.into(),
        desc_line_one.into(),
        desc_line_two.into(),
        btn,
        clone_btn,
    ]
}

fn edit_border(panel: &ExplorerPanelState) -> Color {
    if panel.ui.create_error.is_some() {
        theme::c().error
    } else if panel.ui.create_input.focused {
        theme::c().accent
    } else {
        theme::c().border
    }
}

/// The shared create/rename text field; submit and cancel go to the app.
fn edit_field(tree: &TreeContext<'_>, rect: UiRect, label: &'static str) -> Element {
    let submit = tree.commands.clone();
    let cancel = tree.commands.clone();
    let blur = tree.commands.clone();
    input::render_access(
        rect,
        tree.input.id.clone(),
        tree.input.focus.clone(),
        InputBinding::new_access(
            tree.state.clone(),
            |panel| &panel.ui.create_input,
            |panel| &mut panel.ui.create_input,
        ),
        AccessInputOptions {
            label,
            placeholder: "",
            style: InputStyle {
                text: theme::mono(theme::c().text, theme::UI_SIZE),
                placeholder: theme::mono(theme::c().text_faint, theme::UI_SIZE),
                caret: theme::c().text,
                selection: theme::c().accent,
            },
            on_submit: Some(std::sync::Arc::new(move || {
                submit.dispatch(Command::SubmitEdit)
            })),
            on_cancel: Some(std::sync::Arc::new(move || {
                cancel.dispatch(Command::CancelEdit)
            })),
            on_blur: Some(std::sync::Arc::new(move || {
                blur.dispatch(Command::CancelEdit)
            })),
        },
    )
}

fn create_row(tree: &TreeContext<'_>, y: f32, indent: f32) -> Element {
    let rect = tree.rect;
    let kind = tree
        .panel
        .model
        .create_request()
        .map_or(ExplorerCreateKind::File, |request| request.kind);
    let border = edit_border(tree.panel);
    let icon = if kind == ExplorerCreateKind::Folder {
        crate::ui::file_icons::FOLDER_ICON
    } else {
        crate::ui::file_icons::DEFAULT_FILE_ICON
    };
    let input_rect = UiRect::new(
        indent + TREE_LABEL_OFFSET,
        y + 2.0,
        rect.right - 8.0,
        y + CREATE_ROW_H - 2.0,
    );
    let field_rect = UiRect::new(
        input_rect.left + 1.0,
        input_rect.top + 1.0,
        input_rect.right - 1.0,
        input_rect.bottom - 1.0,
    );
    let input = edit_field(
        tree,
        field_rect,
        if kind == ExplorerCreateKind::Folder {
            "New folder name"
        } else {
            "New file name"
        },
    );

    panel(
        UiRect::new(rect.left, y, rect.right, y + CREATE_ROW_H),
        VisualStyle::filled(theme::c().surface),
    )
    .child(precompiled(
        UiElement::icon(
            UiId::owned("tree-create-icon"),
            UiRect::new(
                indent,
                y + 6.0,
                indent + TREE_ICON_SIZE,
                y + 6.0 + TREE_ICON_SIZE,
            ),
            icon,
        )
        .icon_style(IconStyle::new(theme::c().text_muted)),
    ))
    .child(theme::bordered(input_rect, theme::c().bg, border, 3.0, 1.0))
    .child(input)
}

fn rename_row(tree: &TreeContext<'_>, y: f32, indent: f32, is_expanded: bool) -> Element {
    let rect = tree.rect;
    let border = edit_border(tree.panel);
    let input_rect = UiRect::new(
        indent + TREE_LABEL_OFFSET,
        y + 2.0,
        rect.right - 8.0,
        y + ROW_H - 2.0,
    );
    let field_rect = UiRect::new(
        input_rect.left + 1.0,
        input_rect.top + 1.0,
        input_rect.right - 1.0,
        input_rect.bottom - 1.0,
    );
    let request = tree.panel.model.rename_request();
    let (label, icon) = match request.map(|request| request.kind) {
        Some(ExplorerTargetKind::File) => {
            let icon = request
                .and_then(|request| request.path.file_name())
                .map(|name| crate::ui::file_icons::icon_for_file(&name.to_string_lossy()))
                .unwrap_or(crate::ui::file_icons::DEFAULT_FILE_ICON);
            ("Rename file", icon)
        }
        _ => (
            "Rename folder",
            if is_expanded {
                crate::ui::file_icons::FOLDER_OPEN_ICON
            } else {
                crate::ui::file_icons::FOLDER_ICON
            },
        ),
    };
    let input = edit_field(tree, field_rect, label);
    panel(
        UiRect::new(rect.left, y, rect.right, y + ROW_H),
        VisualStyle::filled(theme::c().surface),
    )
    .child(precompiled(
        UiElement::icon(
            UiId::owned("tree-rename-icon"),
            UiRect::new(
                indent,
                y + 2.0,
                indent + TREE_ICON_SIZE,
                y + 2.0 + TREE_ICON_SIZE,
            ),
            icon,
        )
        .icon_style(IconStyle::new(theme::c().text_muted)),
    ))
    .child(theme::bordered(input_rect, theme::c().bg, border, 3.0, 1.0))
    .child(input)
}

/// Explorer label color: git changes and ignored paths override `default`,
/// using the same precedence as Zed's project panel.
fn tree_label_color(decoration: Decoration, default: Color) -> Color {
    let colors = theme::c().git;
    match decoration.tone {
        Some(GitTone::Conflict) => theme::c().error_text,
        Some(GitTone::Deleted) => colors.deleted,
        Some(GitTone::Modified) => colors.modified,
        Some(GitTone::Added) => colors.added_text,
        Some(GitTone::Ignored) => colors.ignored,
        None => default,
    }
}

/// Hover tracking shared by directory and file rows.
fn track_hover(
    state: &Access,
    key: &str,
) -> impl Fn(&mut lgui::core::UiEventContext, lgui::core::PointerData) + Send + Sync + 'static {
    let state = state.clone();
    let key = key.to_string();
    move |_cx, _pointer| {
        let key = key.clone();
        state.try_update(move |panel| {
            let changed = panel.ui.hovered_path.as_deref() != Some(key.as_str());
            panel.ui.hovered_path = Some(key);
            changed
        });
    }
}

/// A directory node row: left-click toggles expansion; right-click opens the
/// context menu for this directory.
#[allow(clippy::too_many_arguments)]
fn dir_row(
    tree: &TreeContext<'_>,
    key: &str,
    name: &str,
    indent: f32,
    y: f32,
    content_offset_x: f32,
    is_expanded: bool,
    sticky: bool,
    hovered: bool,
    label_color: Color,
) -> Element {
    let rect = tree.rect;
    let content_right = tree.content_right;
    let row_right = if sticky { rect.right } else { content_right };
    let row_rect = UiRect::new(rect.left, y, row_right, y + ROW_H);
    let indent = indent + content_offset_x;
    let text_right = if sticky {
        rect.right - DIRECTORY_RIGHT_PAD
    } else {
        content_right - DIRECTORY_RIGHT_PAD
    };
    let icon = if is_expanded {
        crate::ui::file_icons::FOLDER_OPEN_ICON
    } else {
        crate::ui::file_icons::FOLDER_ICON
    };
    let layer = if sticky { "sticky" } else { "content" };
    let fid = UiId::owned(format!("tree-{layer}-{key}"));
    let style = if sticky {
        VisualStyle::filled(theme::c().sidebar)
    } else if hovered {
        VisualStyle::filled(theme::c().surface)
    } else {
        VisualStyle::default()
    };

    let mut row = panel(row_rect, style).event_policy(EventPolicy::INTERACTIVE);
    if !sticky {
        row = row.on_pointer_move(track_hover(tree.state, key));
    }

    let commands = tree.commands.clone();
    let click_key = key.to_string();
    let row = row
        .on_pointer_down_with_button(move |cx, pointer, button| {
            let key = click_key.clone();
            match button {
                PointerButton::Left => commands.dispatch(Command::ToggleDirectory(key)),
                PointerButton::Right => commands.dispatch(Command::OpenMenu {
                    point: (pointer.point.x, pointer.point.y),
                    target: Some(ExplorerContextTarget {
                        path: key.into(),
                        kind: ExplorerTargetKind::Directory,
                    }),
                }),
                _ => return,
            }
            cx.stop_propagation();
        })
        .child(precompiled(
            UiElement::icon(
                fid,
                UiRect::new(
                    indent,
                    y + 2.0,
                    indent + TREE_ICON_SIZE,
                    y + 2.0 + TREE_ICON_SIZE,
                ),
                icon,
            )
            .icon_style(IconStyle::new(theme::c().text_muted)),
        ))
        .child(tree_label(
            UiRect::new(indent + TREE_LABEL_OFFSET, y + 2.0, text_right, y + 18.0),
            name,
            label_color,
        ));
    if sticky {
        // Sticky rows are outside the scrolling tree's clip. Keep their names
        // and icons inside the drawer when horizontal scrolling moves them left.
        clip(row_rect, 0.0, 0.0).child(row)
    } else {
        row
    }
}

fn file_row(
    tree: &TreeContext<'_>,
    entry_path: &Path,
    name: String,
    key: &str,
    indent: f32,
    y: f32,
) -> Element {
    let content_right = tree.content_right;
    let row_rect = UiRect::new(tree.rect.left, y, content_right, y + ROW_H);
    let icon_id = UiId::owned(format!("tree-file-icon-{key}"));
    let icon = crate::ui::file_icons::icon_for_file(&name);
    let decoration = (tree.frame.decorate)(entry_path);
    let label_color = tree_label_color(decoration, theme::c().text_muted);
    let row_style = if tree.panel.ui.selection.contains(entry_path) {
        VisualStyle::filled(theme::c().active_line)
    } else if tree.frame.active_path == Some(entry_path) {
        VisualStyle::filled(theme::c().active_line)
    } else if tree.panel.ui.hovered_path.as_deref() == Some(key) {
        VisualStyle::filled(theme::c().surface)
    } else {
        VisualStyle::default()
    };
    let commands = tree.commands.clone();
    let action_path = entry_path.to_path_buf();
    panel(row_rect, row_style)
        .event_policy(EventPolicy::INTERACTIVE)
        .on_pointer_move(track_hover(tree.state, key))
        .on_pointer_down_with_button(move |cx, pointer, button| {
            let path = action_path.clone();
            match button {
                // Opening waits for the release: a press that moves becomes
                // a drag into the editor.
                PointerButton::Left => commands.dispatch(Command::PressFile {
                    path,
                    point: (pointer.point.x, pointer.point.y),
                }),
                PointerButton::Middle => commands.dispatch(Command::OpenPinned(path)),
                PointerButton::Right => commands.dispatch(Command::OpenMenu {
                    point: (pointer.point.x, pointer.point.y),
                    target: Some(ExplorerContextTarget {
                        path,
                        kind: ExplorerTargetKind::File,
                    }),
                }),
                _ => return,
            }
            cx.stop_propagation();
        })
        .child(precompiled(
            UiElement::icon(
                icon_id,
                UiRect::new(
                    indent,
                    y + 2.0,
                    indent + TREE_ICON_SIZE,
                    y + 2.0 + TREE_ICON_SIZE,
                ),
                icon,
            )
            .icon_style(IconStyle::new(theme::c().text_muted)),
        ))
        .child(tree_label(
            UiRect::new(
                indent + TREE_LABEL_OFFSET,
                y + 2.0,
                content_right - FILE_RIGHT_PAD,
                y + 18.0,
            ),
            &name,
            label_color,
        ))
        .child(text(
            UiRect::new(
                content_right - STATUS_RIGHT_PAD - STATUS_WIDTH,
                y + 2.0,
                content_right - STATUS_RIGHT_PAD,
                y + 18.0,
            ),
            decoration.indicator,
            theme::mono(label_color, theme::SMALL),
        ))
}

/// Shape the entire name at its natural width, then crop only at the label's
/// viewport. A narrow layout rect would break at spaces or separators first.
fn tree_label(viewport: UiRect, name: &str, color: Color) -> Element {
    let width = tree_label_width(name);
    let viewport = UiRect::new(
        viewport.left,
        viewport.top,
        viewport.right.max(viewport.left),
        viewport.bottom,
    );
    clip(viewport, 0.0, 0.0).child(text(
        UiRect::new(
            viewport.left,
            viewport.top,
            viewport.left + width,
            viewport.bottom,
        ),
        name.to_owned(),
        theme::mono(color, theme::UI_SIZE),
    ))
}

fn tree_label_width(name: &str) -> f32 {
    crate::ui::tabs::measure(name, theme::UI_SIZE, 400) + TREE_TEXT_PAD
}

/// Collects every root and its descendants into one scrollable list, returning
/// the visible-window elements, the metadata of all rows and the content
/// bottom. Each root starts a fresh ancestry; sticky roots stack while nested
/// directories follow the currently visible project's path.
fn build_roots(
    tree: &TreeContext<'_>,
    roots: &[(PathBuf, String, String)],
) -> (Vec<Element>, TreeIndex, f32) {
    let s = tree.panel;
    let content_rect = tree.rect;
    let mut y = content_rect.top;
    let indent = content_rect.left + INDENT;
    let mut index = TreeIndex::default();
    let mut tree_els = Vec::new();

    for (root_path, key, root_name) in roots {
        let is_expanded = s.model.is_expanded(key);
        let root = index.push_dir(
            StickyDirectory {
                key: key.clone(),
                name: root_name.clone(),
                depth: 0,
                source_y: y,
                is_expanded,
            },
            None,
        );
        index.rows.push(TreeRowMeta {
            y,
            depth: 0,
            dir: root,
        });
        let renaming = s
            .model
            .rename_request()
            .is_some_and(|request| request.path == *root_path);
        if renaming {
            tree_els.push(rename_row(tree, y, indent, is_expanded));
        } else if tree.row_visible(y, ROW_H) {
            tree_els.push(dir_row(
                tree,
                key,
                root_name,
                indent,
                y,
                0.0,
                is_expanded,
                false,
                s.ui.hovered_path.as_deref() == Some(key.as_str()),
                theme::c().text_soft,
            ));
        }
        y += ROW_H;

        if s.model
            .create_request()
            .is_some_and(|request| request.parent == *root_path)
        {
            tree_els.push(create_row(tree, y, indent));
            y += CREATE_ROW_H;
        }

        if is_expanded {
            let children_top = y;
            let guide_x = indent + TREE_ICON_SIZE / 2.0;
            let mut dir_path = root_path.clone();
            tree_els.extend(build_tree(tree, &mut dir_path, &mut y, 1, root, &mut index));
            if y > children_top && tree.row_visible(children_top, y - children_top) {
                tree_els.push(panel(
                    UiRect::new(guide_x, children_top, guide_x + 1.0, y),
                    VisualStyle::filled(theme::c().border),
                ));
            }
        }
    }
    (tree_els, index, y)
}

/// Renders the cached entries of the directory at `dir_path`, advancing `y` by
/// one row per entry. Child directories render only when expanded. Rows
/// outside the tree's visible window advance `y` without creating UI nodes,
/// and rows below it are not even recorded, so a pass allocates in proportion
/// to the window and to the rows above it rather than to the whole tree.
/// `dir_path` is extended and restored in place instead of building a path
/// per entry. `parent` is the index of the directory being listed.
fn build_tree(
    tree: &TreeContext<'_>,
    dir_path: &mut PathBuf,
    y: &mut f32,
    depth: usize,
    parent: u32,
    index: &mut TreeIndex,
) -> Vec<Element> {
    let mut els = Vec::new();
    let model = &tree.panel.model;

    let Some(entries) = model.listing(&dir_path.to_string_lossy()) else {
        return els;
    };

    for entry in entries.iter() {
        let is_dir = entry.is_dir;
        // From here until the `pop` below, `dir_path` is this entry's path.
        dir_path.push(&entry.name);
        let indent = tree.rect.left + INDENT + depth as f32 * INDENT;
        let renaming = model
            .rename_request()
            .is_some_and(|request| request.path == *dir_path);
        let recorded = *y < tree.visible.1;
        let visible = tree.row_visible(*y, ROW_H);

        let mut children_parent = parent;
        let mut is_expanded = false;
        if is_dir {
            is_expanded = model.is_expanded(&dir_path.to_string_lossy());
            if recorded {
                children_parent = index.push_dir(
                    StickyDirectory {
                        key: dir_path.to_string_lossy().into_owned(),
                        name: entry.name.clone(),
                        depth,
                        source_y: *y,
                        is_expanded,
                    },
                    Some(parent),
                );
                index.rows.push(TreeRowMeta {
                    y: *y,
                    depth,
                    dir: children_parent,
                });
            }
            if renaming {
                els.push(rename_row(tree, *y, indent, is_expanded));
            } else if visible {
                let directory = &index.dirs[children_parent as usize].directory;
                els.push(dir_row(
                    tree,
                    &directory.key,
                    &directory.name,
                    indent,
                    *y,
                    0.0,
                    is_expanded,
                    false,
                    tree.panel.ui.hovered_path.as_deref() == Some(directory.key.as_str()),
                    tree_label_color(
                        (tree.frame.decorate)(dir_path.as_path()),
                        theme::c().text_soft,
                    ),
                ));
            }
        } else {
            if recorded {
                index.rows.push(TreeRowMeta {
                    y: *y,
                    depth,
                    dir: parent,
                });
            }
            if renaming {
                els.push(rename_row(tree, *y, indent, false));
            } else if visible {
                let key = dir_path.to_string_lossy().into_owned();
                els.push(file_row(
                    tree,
                    dir_path.as_path(),
                    entry.name.clone(),
                    &key,
                    indent,
                    *y,
                ));
            }
        }

        *y += ROW_H;

        if is_dir
            && model
                .create_request()
                .is_some_and(|request| request.parent == *dir_path)
        {
            els.push(create_row(tree, *y, indent));
            *y += CREATE_ROW_H;
        }

        if is_dir && is_expanded {
            // Indent guide under an expanded folder's icon: spans from the
            // folder row bottom to the bottom of its last child.
            let children_top = *y;
            let guide_x = indent + TREE_ICON_SIZE / 2.0;
            els.extend(build_tree(
                tree,
                dir_path,
                y,
                depth + 1,
                children_parent,
                index,
            ));
            if *y > children_top && tree.row_visible(children_top, *y - children_top) {
                els.push(panel(
                    UiRect::new(guide_x, children_top, guide_x + 1.0, *y),
                    VisualStyle::filled(theme::c().border),
                ));
            }
        }
        dir_path.pop();
    }

    els
}

/// Right edge required by expanded tree rows. Reuse the tab strip's cached
/// shaped widths so CJK and fallback glyphs contribute their actual advances.
fn tree_content_right(root_key: &str, root_name: &str, s: &ExplorerPanelState, left: f32) -> f32 {
    let mut right = row_content_right(left, 0, root_name, true);
    if s.model.is_expanded(root_key) {
        extend_content_right(&mut PathBuf::from(root_key), 1, s, left, &mut right);
    }
    right
}

/// Walks the expanded directories below `dir_path`, extending and restoring
/// the one path buffer rather than allocating a path per directory.
fn extend_content_right(
    dir_path: &mut PathBuf,
    depth: usize,
    s: &ExplorerPanelState,
    left: f32,
    right: &mut f32,
) {
    let Some(entries) = s.model.listing(&dir_path.to_string_lossy()) else {
        return;
    };

    for entry in entries.iter() {
        *right = right.max(row_content_right(left, depth, &entry.name, entry.is_dir));
        if entry.is_dir {
            dir_path.push(&entry.name);
            if s.model.is_expanded(&dir_path.to_string_lossy()) {
                extend_content_right(dir_path, depth + 1, s, left, right);
            }
            dir_path.pop();
        }
    }
}

fn row_content_right(left: f32, depth: usize, name: &str, is_dir: bool) -> f32 {
    let right_pad = if is_dir {
        DIRECTORY_RIGHT_PAD
    } else {
        FILE_RIGHT_PAD
    };
    left + INDENT + depth as f32 * INDENT + TREE_LABEL_OFFSET + tree_label_width(name) + right_pad
}

/// Keep scrolled project roots in stacked slots, followed by the current
/// project's directory ancestry. Sibling directories still replace one another.
fn sticky_rows(index: &TreeIndex, viewport_top: f32, scroll: f32) -> Vec<StickyRow> {
    if scroll <= 0.0 {
        return Vec::new();
    }

    let mut pinned = Vec::new();
    let mut active: Option<u32> = None;
    for row in &index.rows {
        if row.depth == 0 {
            let slot_top = viewport_top + pinned.len() as f32 * ROW_H;
            if row.y - scroll > slot_top {
                break;
            }
            pinned.push(StickyRow {
                directory: index.dirs[row.dir as usize].directory.clone(),
                top: slot_top,
            });
            active = Some(row.dir);
            continue;
        }
        let slot_top = viewport_top + (pinned.len().saturating_sub(1) + row.depth) as f32 * ROW_H;
        if row.y - scroll <= slot_top {
            active = Some(row.dir);
        }
    }

    let previous_roots = pinned.len().saturating_sub(1);
    let chain = active.map(|dir| index.chain(dir)).unwrap_or_default();
    pinned.extend(chain.into_iter().skip(1).map(|dir| {
        let directory = index.dirs[dir as usize].directory.clone();
        let slot_top = viewport_top + (previous_roots + directory.depth) as f32 * ROW_H;
        let boundary_top = index
            .rows
            .iter()
            .find(|row| row.y > directory.source_y && !index.within(dir, row.dir))
            .map(|row| row.y - scroll - ROW_H)
            .unwrap_or(slot_top);
        StickyRow {
            directory,
            top: slot_top.min(boundary_top),
        }
    }));
    pinned
}

/// Scroll offset that shows the revealed row. A row already in sight, below
/// the directory rows pinned above it, leaves the tree where it is; otherwise
/// the row is centred.
fn revealed_scroll(scroll: f32, target: ExplorerReveal, viewport_h: f32, max_scroll: f32) -> f32 {
    let top = target.row as f32 * ROW_H;
    let covered = target.covered as f32 * ROW_H;
    if top - covered >= scroll && top + ROW_H <= scroll + viewport_h {
        return scroll;
    }
    (top - (viewport_h - ROW_H) / 2.0).clamp(0.0, max_scroll)
}

/// Fixed vertical scrollbar for the file tree. Only the draggable thumb is
/// painted; the track remains invisible like the scrollbars in modern editors.
fn vertical_scrollbar(rect: UiRect, content_bottom: f32, scroll: f32, state: Access) -> Element {
    let content_top = rect.top + 8.0;
    let content_h = (content_bottom - content_top).max(1.0);
    let viewport_h = rect.height();
    let max_scroll = (content_bottom - rect.bottom).max(0.0);

    let track = UiRect::new(
        rect.right - scrollbar::SIZE,
        rect.top + 8.0,
        rect.right,
        rect.bottom - 8.0,
    );
    let thumb_h = (track.height() * viewport_h / content_h)
        .max(scrollbar::MIN_THUMB_LENGTH)
        .min(track.height());
    let travel = track.height() - thumb_h;
    let thumb_top = if max_scroll == 0.0 {
        track.top
    } else {
        track.top + travel * (scroll / max_scroll)
    };

    // Draggable thumb: on pointer-down we remember how far above the thumb's
    // top the cursor is, then map the pointer's vertical movement through the
    // track's travel range onto the scroll range. Track geometry and the
    // scroll range are fixed during a drag (content height can't change), so
    // capturing them here is safe.
    let track_top = track.top;
    let st_down = state.clone();
    let st_move = state.clone();
    let st_up = state;
    scrollbar::thumb(
        UiRect::new(
            track.left + scrollbar::INSET,
            thumb_top,
            track.right - scrollbar::INSET,
            thumb_top + thumb_h,
        ),
        0xb8,
    )
    .key("file-tree-vertical-scrollbar-thumb")
    .event_policy(EventPolicy::INTERACTIVE)
    .on_pointer_down(move |_cx, p| {
        st_down.update(move |panel| {
            panel.ui.reveal = None;
            panel.ui.scrollbar_dragging = true;
            panel.ui.scrollbar_drag_offset = p.point.y - thumb_top;
        });
    })
    .on_pointer_drag(move |_cx, p| {
        st_move.update(move |panel| {
            if panel.ui.scrollbar_dragging && travel > 0.0 {
                let t =
                    (p.point.y - track_top - panel.ui.scrollbar_drag_offset) / travel * max_scroll;
                panel.ui.scroll = t.clamp(0.0, max_scroll);
            }
        });
    })
    .on_pointer_up(move |_cx, _p| {
        st_up.update(move |panel| panel.ui.scrollbar_dragging = false);
    })
}

/// Fixed horizontal scrollbar for wide or deeply nested tree rows. As with
/// the vertical scrollbar, only the draggable thumb is painted.
fn horizontal_scrollbar(rect: UiRect, content_right: f32, scroll: f32, state: Access) -> Element {
    let content_w = (content_right - rect.left).max(1.0);
    let viewport_w = rect.width();
    let max_scroll = (content_right - rect.right).max(0.0);
    let track = UiRect::new(
        rect.left + 8.0,
        rect.bottom - scrollbar::SIZE,
        rect.right - 8.0,
        rect.bottom,
    );
    let thumb_w = (track.width() * viewport_w / content_w)
        .max(scrollbar::MIN_THUMB_LENGTH)
        .min(track.width());
    let travel = track.width() - thumb_w;
    let thumb_left = if max_scroll == 0.0 {
        track.left
    } else {
        track.left + travel * (scroll / max_scroll)
    };

    let track_left = track.left;
    let st_down = state.clone();
    let st_move = state.clone();
    let st_up = state;
    scrollbar::thumb(
        UiRect::new(
            thumb_left,
            track.top + scrollbar::INSET,
            thumb_left + thumb_w,
            track.bottom - scrollbar::INSET,
        ),
        0xb8,
    )
    .key("file-tree-horizontal-scrollbar-thumb")
    .event_policy(EventPolicy::INTERACTIVE)
    .on_pointer_down(move |_cx, p| {
        st_down.update(move |panel| {
            panel.ui.horizontal_scrollbar_dragging = true;
            panel.ui.horizontal_scrollbar_drag_offset = p.point.x - thumb_left;
        });
    })
    .on_pointer_drag(move |_cx, p| {
        st_move.update(move |panel| {
            if panel.ui.horizontal_scrollbar_dragging && travel > 0.0 {
                let t = (p.point.x - track_left - panel.ui.horizontal_scrollbar_drag_offset)
                    / travel
                    * max_scroll;
                panel.ui.scroll_x = t.clamp(0.0, max_scroll);
            }
        });
    })
    .on_pointer_up(move |_cx, _p| {
        st_up.update(move |panel| panel.ui.horizontal_scrollbar_dragging = false);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::explorer::DirEntry;

    fn target(row: usize, covered: usize) -> ExplorerReveal {
        ExplorerReveal { row, covered }
    }

    #[test]
    fn revealed_row_in_sight_leaves_the_scroll_alone() {
        // 200px show ten rows; at scroll 100 those are rows 5 to 14.
        assert_eq!(revealed_scroll(100.0, target(8, 2), 200.0, 1000.0), 100.0);
    }

    #[test]
    fn revealed_row_under_pinned_directories_counts_as_hidden() {
        // Row 5 sits at 100px, below two pinned rows covering 100..140, so the
        // row is centred rather than left under them.
        assert_eq!(revealed_scroll(100.0, target(5, 2), 200.0, 1000.0), 10.0);
    }

    #[test]
    fn revealed_row_out_of_sight_is_centred_and_clamped() {
        assert_eq!(revealed_scroll(0.0, target(40, 1), 200.0, 2000.0), 710.0);
        assert_eq!(revealed_scroll(500.0, target(1, 1), 200.0, 2000.0), 0.0);
        assert_eq!(revealed_scroll(0.0, target(40, 1), 200.0, 650.0), 650.0);
    }

    fn directory(key: &str, depth: usize, source_y: f32) -> StickyDirectory {
        StickyDirectory {
            key: key.to_string(),
            name: key.to_string(),
            depth,
            source_y,
            is_expanded: true,
        }
    }

    type TestRow = (f32, usize, Vec<StickyDirectory>);

    fn row(y: f32, depth: usize, path: &[StickyDirectory]) -> TestRow {
        (y, depth, path.to_vec())
    }

    /// Builds an index from rows given with their full directory ancestry,
    /// sharing a directory between rows by its key.
    fn index_of(rows: Vec<TestRow>) -> TreeIndex {
        let mut index = TreeIndex::default();
        for (y, depth, path) in rows {
            let mut parent = None;
            for directory in &path {
                let existing = index
                    .dirs
                    .iter()
                    .position(|dir| dir.directory.key == directory.key);
                parent = Some(match existing {
                    Some(position) => position as u32,
                    None => index.push_dir(directory.clone(), parent),
                });
            }
            index.rows.push(TreeRowMeta {
                y,
                depth,
                dir: parent.unwrap(),
            });
        }
        index
    }

    #[test]
    fn pins_the_current_directory_and_its_ancestors() {
        let root = directory("root", 0, 8.0);
        let src = directory("src", 1, 28.0);
        let rows = index_of(vec![
            row(8.0, 0, std::slice::from_ref(&root)),
            row(28.0, 1, &[root.clone(), src.clone()]),
            row(48.0, 2, &[root.clone(), src.clone()]),
            row(68.0, 2, &[root.clone(), src.clone()]),
        ]);

        let sticky = sticky_rows(&rows, 0.0, 48.0);

        assert_eq!(sticky.len(), 2);
        assert_eq!(sticky[0].directory.key, "root");
        assert_eq!(sticky[0].top, 0.0);
        assert_eq!(sticky[1].directory.key, "src");
        assert_eq!(sticky[1].top, ROW_H);
    }

    #[test]
    fn next_sibling_pushes_the_current_directory_out() {
        let root = directory("root", 0, 8.0);
        let src = directory("src", 1, 28.0);
        let tests = directory("tests", 1, 88.0);
        let rows = index_of(vec![
            row(8.0, 0, std::slice::from_ref(&root)),
            row(28.0, 1, &[root.clone(), src.clone()]),
            row(48.0, 2, &[root.clone(), src.clone()]),
            row(68.0, 2, &[root.clone(), src.clone()]),
            row(88.0, 1, &[root.clone(), tests.clone()]),
        ]);

        let being_pushed = sticky_rows(&rows, 0.0, 55.0);
        assert_eq!(being_pushed[1].directory.key, "src");
        assert_eq!(being_pushed[1].top, 13.0);

        let replaced = sticky_rows(&rows, 0.0, 68.0);
        assert_eq!(replaced[1].directory.key, "tests");
        assert_eq!(replaced[1].top, ROW_H);
    }

    #[test]
    fn waits_until_a_directory_reaches_its_sticky_slot() {
        let root = directory("root", 0, 8.0);
        let rows = index_of(vec![row(8.0, 0, std::slice::from_ref(&root))]);

        assert!(sticky_rows(&rows, 0.0, 7.0).is_empty());
        assert_eq!(sticky_rows(&rows, 0.0, 8.0).len(), 1);
    }

    #[test]
    fn project_roots_stack_above_the_current_projects_directory_path() {
        let first = directory("first", 0, 8.0);
        let src = directory("first/src", 1, 28.0);
        let second = directory("second", 0, 88.0);
        let lib = directory("second/lib", 1, 108.0);
        let third = directory("third", 0, 168.0);
        let rows = index_of(vec![
            row(8.0, 0, std::slice::from_ref(&first)),
            row(28.0, 1, &[first.clone(), src.clone()]),
            row(48.0, 2, &[first.clone(), src.clone()]),
            row(68.0, 2, &[first.clone(), src.clone()]),
            row(88.0, 0, std::slice::from_ref(&second)),
            row(108.0, 1, &[second.clone(), lib.clone()]),
            row(128.0, 2, &[second.clone(), lib.clone()]),
            row(148.0, 2, &[second.clone(), lib.clone()]),
            row(168.0, 0, std::slice::from_ref(&third)),
        ]);
        let signature = |scroll| {
            sticky_rows(&rows, 0.0, scroll)
                .into_iter()
                .map(|sticky| (sticky.directory.key, sticky.top))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            signature(88.0),
            vec![
                ("first".into(), 0.0),
                ("second".into(), 20.0),
                ("second/lib".into(), 40.0)
            ]
        );
        assert_eq!(
            signature(128.0),
            vec![
                ("first".into(), 0.0),
                ("second".into(), 20.0),
                ("third".into(), 40.0)
            ]
        );
        assert_eq!(
            signature(20.0),
            vec![("first".into(), 0.0), ("first/src".into(), 20.0)]
        );
    }

    #[test]
    fn horizontal_range_uses_only_visible_tree_rows() {
        let root_key = "root";
        let nested = Path::new("root").join("src").to_string_lossy().into_owned();
        let nested_key = nested.as_str();
        let long_name = "a_very_long_nested_file_name.rs";
        let mut state = ExplorerPanelState::default();
        state.model.set_expanded(root_key, true);
        state.model.set_expanded(nested_key, true);
        state.model.insert_listing(
            root_key.to_string(),
            vec![DirEntry::new(
                &std::sync::Arc::from(Path::new(root_key)),
                "src".to_string(),
                true,
            )]
            .into(),
        );
        state.model.insert_listing(
            nested_key.to_string(),
            vec![DirEntry::new(
                &std::sync::Arc::from(Path::new(nested_key)),
                long_name.to_string(),
                false,
            )]
            .into(),
        );

        assert_eq!(
            tree_content_right(root_key, "root", &state, 100.0),
            row_content_right(100.0, 2, long_name, false)
        );

        state.model.set_expanded(nested_key, false);
        assert_eq!(
            tree_content_right(root_key, "root", &state, 100.0),
            row_content_right(100.0, 1, "src", true)
        );
    }

    #[test]
    fn sidebar_resize_tracks_both_drag_directions_and_clamps() {
        assert_eq!(resized_sidebar_width(1_000.0, 750.0), 250.0);
        assert_eq!(resized_sidebar_width(1_000.0, 900.0), theme::SIDEBAR_MIN_W);
        assert_eq!(resized_sidebar_width(1_000.0, 500.0), theme::SIDEBAR_MAX_W);
    }
}
