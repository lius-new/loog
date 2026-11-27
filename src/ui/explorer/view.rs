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

use std::path::Path;

use lgui::core::{
    CursorIcon, EventPolicy, IconStyle, Point, PointerButton, UiElement, UiEventKind,
    UiEventPayload, UiId, WheelUnit, clip, precompiled,
};
use lgui::prelude::{Color, Element, TextAlign, UiRect, VisualStyle, panel, text};

use super::commands::{Command, Commands, Decoration, EditInput, Frame, GitTone};
use super::source::{self, Entry};
use super::state::{ExplorerPanelState, ExplorerReveal, RowHover};
use crate::features::explorer::ExplorerCreateKind;
use crate::ui::components::input::{self, AccessInputOptions, InputBinding, InputStyle};
use crate::ui::components::state_access::StateAccess;
use crate::ui::components::{divider, scrollbar};
use crate::ui::drawer::drawer_icon;
use crate::ui::theme;

/// What a root's width was remembered for: its key and tree revision, and the width.
type Remembered = std::collections::HashMap<(String, u32, u32), (u64, f32)>;

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
/// A root's badge chip: the space before it, its side padding and its widest size.
const BADGE_GAP: f32 = 4.0;
const BADGE_PAD_X: f32 = 6.0;
const BADGE_MAX_W: f32 = 120.0;

type Access = StateAccess<ExplorerPanelState>;

#[derive(Clone, Debug)]
struct StickyDirectory {
    key: String,
    name: String,
    depth: usize,
    source_y: f32,
    is_expanded: bool,
    entry: Entry,
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
    scroll: (f32, f32),
    content_right: f32,
    tips: &'a std::cell::RefCell<Vec<PathTip>>,
    /// The directory whose guide is the active row's, found once per frame.
    active_guide: Option<&'a Path>,
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
    input: EditInput,
    tree_focus: lgui::core::UiFocusHandle,
) -> Element {
    let s = state.get();
    let tips = std::cell::RefCell::new(Vec::new());
    // The tree starts at the top of the drawer; Search and Outline sit below it.
    let content_rect = rect;

    let mut bar = Element::new(move |cx| {
        UiElement::panel(
            UiId::new("explorer-tree"),
            rect,
            VisualStyle::filled(theme::c().sidebar),
        )
        .children(cx.children)
    })
    .event_policy(EventPolicy::INTERACTIVE);

    // Background right-click capture: covers the drawer at the lowest z-order.
    // Rows and the resize handle are drawn later and sit above it, so they win
    // hit-testing and this only receives events on empty space.
    let menu_commands = commands.clone();
    let clear_hover = frame.hover.clone();
    let capture = panel(content_rect, VisualStyle::default())
        .event_policy(EventPolicy::INTERACTIVE)
        .on_event_capture(UiEventKind::PointerMove, move |_cx, _payload| {
            clear_hover.set(None);
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
    if s.model.roots().is_empty()
        && s.resources.roots.is_empty()
        && s.resources.bookmarks.is_empty()
    {
        for el in empty_state(content_rect, &commands) {
            bar = bar.child(el);
        }
    } else {
        let roots = source::roots(&s);
        let content_right = roots
            .iter()
            .map(|entry| tree_content_right(entry, &s, content_rect.left))
            .fold(content_rect.right, f32::max);
        let max_scroll_x = (content_right - content_rect.right).max(0.0);
        let scroll_x = s.ui.scroll_x.clamp(0.0, max_scroll_x);

        // Only rows near the viewport get UI nodes. The scroll offset is clamped
        // by the content height, which is known only after a pass, so a pass
        // built for a stale offset is redone once for the clamped one.
        let active_guide = source::active_guide(&s, frame);
        let build = |scroll_hint: f32| {
            tips.borrow_mut().clear();
            let tree = TreeContext {
                panel: &s,
                state: &state,
                frame,
                commands: &commands,
                input: &input,
                rect: content_rect,
                scroll: (scroll_x, scroll_hint),
                content_right,
                tips: &tips,
                active_guide,
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
            scroll: (scroll_x, scroll),
            content_right,
            tips: &tips,
            active_guide,
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

        let stickies = sticky_rows(&rows, content_rect.top, scroll);
        // The pinned rows above each pinned row that are its ancestors: their
        // guides run beside it.
        let mut ancestors: Vec<Vec<usize>> = Vec::with_capacity(stickies.len());
        let mut stack: Vec<usize> = Vec::new();
        for (index, sticky) in stickies.iter().enumerate() {
            while stack
                .last()
                .is_some_and(|&above| stickies[above].directory.depth >= sticky.directory.depth)
            {
                stack.pop();
            }
            ancestors.push(stack.clone());
            stack.push(index);
        }
        for (index, sticky) in stickies.iter().enumerate().rev() {
            let directory = &sticky.directory;
            let indent = content_rect.left + INDENT + directory.depth as f32 * INDENT;
            // The workspace root keeps its plain color, as in the tree itself.
            let label_color = if directory.depth == 0 {
                theme::c().text_soft
            } else {
                tree_label_color(directory.entry.decoration(frame), theme::c().text_soft)
            };
            bar = bar.child(dir_row(
                &tree,
                &directory.entry,
                indent,
                sticky.top,
                -scroll_x,
                directory.is_expanded,
                true,
                false,
                label_color,
            ));
            // The pinned row hides the guides of the directories above it, so
            // they are drawn again over it. The rows of those directories come
            // after this one and stay on top of any guide they overlap.
            for &above in &ancestors[index] {
                let above = &stickies[above].directory;
                let active = above.entry.guide_active(active_guide);
                if !(active || s.ui.hovered) {
                    continue;
                }
                let indent = content_rect.left + INDENT + above.depth as f32 * INDENT;
                let x = indent + TREE_ICON_SIZE / 2.0 - scroll_x;
                bar = bar.child(divider::vertical_pixel(
                    Point::new(x, sticky.top),
                    sticky.top + ROW_H,
                    content_rect,
                    divider::VerticalPlacement::Overlay(0.0, 0.0),
                    guide_color(active),
                ));
            }
        }

        if max_scroll > 0.0 && (s.ui.hovered || s.ui.scrollbar_dragging) {
            bar = bar.child(vertical_scrollbar(
                content_rect,
                content_bottom,
                scroll,
                state.clone(),
            ));
        }
    }

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

    let bar = bar.child(path_tooltips(
        tips.into_inner(),
        s.ui.menu.is_none() && s.ui.resource_menu.is_none() && !frame.resizing,
    ));
    super::navigation::attach(
        bar,
        content_rect,
        state,
        commands,
        tree_focus,
        frame.active_resource.cloned(),
    )
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

fn create_row(tree: &TreeContext<'_>, y: f32, indent: f32, kind: ExplorerCreateKind) -> Element {
    let rect = tree.rect;
    let border = edit_border(tree.panel);
    let icon = if kind == ExplorerCreateKind::Folder {
        crate::ui::file_icons::CHEVRON_RIGHT_ICON
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

fn rename_row(
    tree: &TreeContext<'_>,
    y: f32,
    indent: f32,
    entry: &Entry,
    is_expanded: bool,
) -> Element {
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
    let (label, icon) = if !entry.directory {
        (
            "Rename file",
            crate::ui::file_icons::icon_for_file(&entry.name),
        )
    } else {
        (
            "Rename folder",
            if is_expanded {
                crate::ui::file_icons::CHEVRON_DOWN_ICON
            } else {
                crate::ui::file_icons::CHEVRON_RIGHT_ICON
            },
        )
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

/// A visible row's path, kept outside the scrolling clip so the hint can
/// extend into the editor without being cropped by the drawer.
struct PathTip {
    row: UiId,
    target: UiId,
    rect: UiRect,
    resource: loog_workspace_api::ResourceRef,
}

fn record_path_tip(
    tree: &TreeContext<'_>,
    entry: &Entry,
    row: UiId,
    mut rect: UiRect,
    label_left: f32,
    sticky: bool,
) {
    let Some(resource) = &entry.resource else {
        return;
    };
    rect.left = label_left;
    if !sticky {
        rect.left -= tree.scroll.0;
        rect.top -= tree.scroll.1;
        rect.bottom -= tree.scroll.1;
    }
    if rect.bottom <= tree.rect.top || rect.top >= tree.rect.bottom {
        return;
    }
    // Moving a row by scrolling restarts the delay even if it stays under the pointer.
    let target = UiId::owned(format!(
        "path-tip-{}-{}-{}",
        row.as_str(),
        rect.top.to_bits(),
        rect.left.to_bits()
    ));
    tree.tips.borrow_mut().push(PathTip {
        row,
        target,
        rect,
        resource: resource.clone(),
    });
}

fn path_tooltips(tips: Vec<PathTip>, allowed: bool) -> Element {
    Element::new(move |cx| {
        let hovered = allowed
            .then(|| {
                tips.iter().rev().find(|tip| {
                    let flags = cx.context.interaction_flags(&tip.row);
                    flags.hovered && !flags.pressed
                })
            })
            .flatten();
        let ready =
            crate::ui::components::tab_tooltip::ready(&cx, hovered.map(|tip| tip.target.clone()));
        let mut layer = UiElement::group(cx.id.clone(), UiRect::default());
        let Some(tip) = hovered.filter(|tip| Some(&tip.target) == ready.as_ref()) else {
            return layer;
        };
        let path = tip
            .resource
            .local_path()
            .map(crate::ui::display_path)
            .unwrap_or_else(|| tip.resource.display().to_string());
        let viewport = cx.context.viewport();
        let style = theme::mono(theme::c().text_soft, theme::SMALL)
            .wrap(lgui::core::TextWrap::Words)
            .vertical_align(lgui::text::TextVerticalAlign::Top);
        // Shape and paint the same complete string with the same wrapping style.
        // A single-line text node silently drops everything after its first break.
        // Keep the hint's right edge at this item's label, including its depth
        // and scroll offset. Wrap within the space to its left instead of
        // clamping an oversized hint to the same window edge for every row.
        let natural =
            crate::ui::tabs::measure_once(&path, theme::SMALL, 400) + TREE_TEXT_PAD + 18.0;
        // Flip to the right of the label when the left side cannot hold the hint.
        let opens_right = path_tip_opens_right(tip.rect, viewport, natural);
        let room = if opens_right {
            viewport.right - tip.rect.left - 8.0
        } else {
            tip.rect.left - viewport.left - 8.0
        };
        let max_width = room
            .clamp(80.0, 600.0)
            .min((viewport.width() - 16.0).max(20.0));
        let width = natural.min(max_width);
        let text_width = width - 18.0;
        let bounds = UiRect::new(0.0, 0.0, text_width, 10_000.0);
        let request = lgui::text::TextLayoutRequest::for_style(&path, bounds, &style);
        let text_height = lgui::text::layout(&request).map_or_else(
            || {
                (crate::ui::tabs::measure_once(&path, theme::SMALL, 400) / text_width)
                    .ceil()
                    .max(1.0)
                    * 16.0
            },
            |layout| layout.height.ceil(),
        );
        let rect = path_tip_rect(tip.rect, viewport, width, text_height + 8.0, opens_right);
        let (left, top) = (rect.left, rect.top);
        layer = layer.child(
            UiElement::panel(
                UiId::new("explorer-path-tooltip"),
                rect,
                VisualStyle::filled(theme::c().border).radius(4.0),
            )
            // Escape content clips and paint the complete hint above tree rows.
            .render_phase(lgui::core::RenderPhase::Popup)
            .overflow(lgui::core::Overflow::Clip)
            .child(UiElement::panel(
                cx.auto_id(),
                UiRect::new(left + 1.0, top + 1.0, rect.right - 1.0, rect.bottom - 1.0),
                VisualStyle::filled(theme::c().surface).radius(3.0),
            ))
            .child(UiElement::text(
                cx.auto_id(),
                UiRect::new(left + 9.0, top + 4.0, rect.right - 9.0, rect.bottom - 4.0),
                path,
                style,
            )),
        );
        layer
    })
    .key("explorer-path-tips")
}

/// Open to the left of the label unless that side cannot hold the whole hint
/// and the right side has more room.
fn path_tip_opens_right(anchor: UiRect, viewport: UiRect, width: f32) -> bool {
    let left_room = (anchor.left - viewport.left - 8.0).max(0.0);
    let right_room = (viewport.right - anchor.left - 8.0).max(0.0);
    left_room < width && right_room > left_room
}

/// Extend toward the editor, with the right edge near the row's label. Prefer
/// the lower left; flip above the row when the lower space cannot hold the hint.
/// With `right`, the left edge sits at the label and the hint extends rightward.
fn path_tip_rect(anchor: UiRect, viewport: UiRect, width: f32, height: f32, right: bool) -> UiRect {
    let wanted = if right {
        anchor.left
    } else {
        anchor.left - width
    };
    let left = wanted.clamp(
        viewport.left + 8.0,
        (viewport.right - width - 8.0).max(viewport.left + 8.0),
    );
    let top = if anchor.bottom + 4.0 + height <= viewport.bottom - 8.0 {
        anchor.bottom + 4.0
    } else {
        (anchor.top - height - 4.0).max(viewport.top + 8.0)
    };
    UiRect::new(left, top, left + width, top + height)
}

/// Hover tracking shared by directory and file rows.
fn track_hover(
    hover: &RowHover,
    key: &str,
) -> impl Fn(&mut lgui::core::UiEventContext, lgui::core::PointerData) + Send + Sync + 'static {
    let hover = hover.clone();
    let key = key.to_string();
    move |_cx, _pointer| hover.set(Some(key.clone()))
}

/// Keeps a row's fill following the hover without re-rendering. ixed is a fill that wins over
/// the hover, such as the selection.
fn follow_hover(row: Element, hover: &RowHover, key: &str, fixed: Option<Color>) -> Element {
    let key = key.to_string();
    row.bind(hover, move |hovered, node| {
        node.style = row_fill(fixed, hovered.as_deref() == Some(key.as_str()));
    })
}

/// Color of an indent guide: a brighter tone for the active row's guide, a
/// dimmer one for the others, which show only while the pointer is over the
/// drawer.
fn guide_color(active: bool) -> Color {
    if active {
        theme::c().text_ghost
    } else {
        theme::c().border
    }
}

fn row_fill(fixed: Option<Color>, hovered: bool) -> VisualStyle {
    match fixed {
        Some(color) => VisualStyle::filled(color),
        None if hovered => VisualStyle::filled(theme::c().surface),
        None => VisualStyle::default(),
    }
}

/// A directory node row: left-click toggles expansion; right-click opens the
/// context menu for this directory.
#[allow(clippy::too_many_arguments)]
fn dir_row(
    tree: &TreeContext<'_>,
    entry: &Entry,
    indent: f32,
    y: f32,
    content_offset_x: f32,
    is_expanded: bool,
    sticky: bool,
    hovered: bool,
    label_color: Color,
) -> Element {
    let key = &entry.key;
    let name = &entry.name;
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
        crate::ui::file_icons::CHEVRON_DOWN_ICON
    } else {
        crate::ui::file_icons::CHEVRON_RIGHT_ICON
    };
    let layer = if sticky { "sticky" } else { "content" };
    let fid = UiId::owned(format!("tree-{layer}-{key}"));
    // A selected directory is filled like a selected file. Sticky rows keep the
    // sidebar fill that hides the rows scrolling beneath them.
    let fixed = (!sticky)
        .then(|| entry.selected(tree.panel, tree.frame))
        .filter(|selected| *selected)
        .map(|_| theme::c().active_line);
    let style = if sticky {
        VisualStyle::filled(theme::c().sidebar)
    } else {
        row_fill(fixed, hovered)
    };

    let row_id = UiId::owned(format!("tree-row-{layer}-{key}"));
    record_path_tip(
        tree,
        entry,
        row_id.clone(),
        row_rect,
        indent + TREE_LABEL_OFFSET,
        sticky,
    );
    let mut row =
        Element::new(move |cx| UiElement::panel(row_id, row_rect, style).children(cx.children))
            .event_policy(EventPolicy::INTERACTIVE);
    if !sticky {
        row = follow_hover(row, tree.frame.hover, key, fixed)
            .on_pointer_move(track_hover(tree.frame.hover, key));
    }

    let commands = tree.commands.clone();
    let click_entry = entry.clone();
    let row = row
        .on_pointer_down_with_button(move |cx, pointer, button| {
            if let Some(command) = click_entry.press((pointer.point.x, pointer.point.y), button) {
                if button == PointerButton::Left
                    && let Some(path) = click_entry.local_path()
                {
                    commands.dispatch(Command::SelectDirectory(path.clone()));
                }
                commands.dispatch(command);
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
    let row = match entry.badge.as_deref() {
        Some(badge) => {
            let left = indent + TREE_LABEL_OFFSET + tree_label_width(name) + BADGE_GAP;
            row.child(badge_chip(left, y, text_right, badge))
        }
        None => row,
    };
    if sticky {
        // Sticky rows are outside the scrolling tree's clip. Keep their names
        // and icons inside the drawer when horizontal scrolling moves them left.
        clip(row_rect, 0.0, 0.0).child(row)
    } else {
        row
    }
}

fn file_row(tree: &TreeContext<'_>, entry: &Entry, indent: f32, y: f32) -> Element {
    let name = &entry.name;
    let key = &entry.key;
    let content_right = tree.content_right;
    let row_rect = UiRect::new(tree.rect.left, y, content_right, y + ROW_H);
    let icon_id = UiId::owned(format!("tree-file-icon-{key}"));
    let icon = crate::ui::file_icons::icon_for_file(name);
    let decoration = entry.decoration(tree.frame);
    let label_color = tree_label_color(decoration, theme::c().text_muted);
    let fixed = entry
        .selected(tree.panel, tree.frame)
        .then(|| theme::c().active_line);
    let hovered = tree.frame.hover.get().as_deref() == Some(key.as_str());
    let row_style = row_fill(fixed, hovered);
    let commands = tree.commands.clone();
    let pressed = entry.clone();
    let released = entry.clone();
    let release_commands = tree.commands.clone();
    let row_id = UiId::owned(format!("tree-row-file-{key}"));
    record_path_tip(
        tree,
        entry,
        row_id.clone(),
        row_rect,
        indent + TREE_LABEL_OFFSET,
        false,
    );
    follow_hover(
        Element::new(move |cx| UiElement::panel(row_id, row_rect, row_style).children(cx.children)),
        tree.frame.hover,
        key,
        fixed,
    )
    .event_policy(EventPolicy::INTERACTIVE)
    .on_pointer_move(track_hover(tree.frame.hover, key))
    .on_pointer_down_with_button(move |cx, pointer, button| {
        if let Some(command) = pressed.press((pointer.point.x, pointer.point.y), button) {
            commands.dispatch(command);
        }
        cx.stop_propagation();
    })
    .on_pointer_up(move |_, pointer| {
        if let Some(command) = released.release((pointer.point.x, pointer.point.y)) {
            release_commands.dispatch(command);
        }
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
        name,
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

/// The badge's text and the width of its chip: the whole text when it fits, else cut short
/// with an ellipsis so one long machine name cannot take the row.
fn badge_fit(badge: &str) -> (String, f32) {
    let width = |text: &str| crate::ui::tabs::measure(text, theme::SMALL, 400);
    let room = BADGE_MAX_W - 2.0 * BADGE_PAD_X;
    if width(badge) <= room {
        return (badge.to_owned(), width(badge) + 2.0 * BADGE_PAD_X);
    }
    let mut cut: String = badge.to_owned();
    while !cut.is_empty() && width(&format!("{cut}…")) > room {
        cut.pop();
    }
    (format!("{cut}…"), BADGE_MAX_W)
}

/// A small chip with the root's badge, after its name and never past `right`.
fn badge_chip(left: f32, y: f32, right: f32, badge: &str) -> Element {
    let (label, width) = badge_fit(badge);
    let chip = UiRect::new(
        left,
        y + 3.0,
        (left + width).min(right.max(left)),
        y + ROW_H - 3.0,
    );
    let colors = theme::c();
    clip(chip, 0.0, 0.0).child(
        panel(
            UiRect::new(left, chip.top, left + width, chip.bottom),
            VisualStyle::filled(crate::ui::highlight_menu::mix(
                colors.surface,
                colors.accent,
                0.22,
            ))
            .radius(3.0),
        )
        .child(text(
            UiRect::new(left, chip.top, left + width, chip.bottom),
            label,
            crate::ui::highlight_menu::centered(colors.accent, theme::SMALL, 400),
        )),
    )
}

/// Collects every root and its descendants into one scrollable list, returning
/// the visible-window elements, the metadata of all rows and the content
/// bottom. Each root starts a fresh ancestry; sticky roots stack while nested
/// directories follow the currently visible project's path.
fn build_roots(tree: &TreeContext<'_>, roots: &[Entry]) -> (Vec<Element>, TreeIndex, f32) {
    let mut y = tree.rect.top;
    let mut index = TreeIndex::default();
    let mut tree_els = Vec::new();
    for entry in roots {
        build_tree(
            tree,
            entry,
            &mut y,
            0,
            None,
            &mut index,
            &mut tree_els,
            &mut std::collections::HashSet::new(),
        );
    }
    (tree_els, index, y)
}

/// One layout and interaction path for every data source. Only visible rows
/// allocate UI elements; sticky metadata is retained up to the viewport end.
#[allow(clippy::too_many_arguments)]
fn build_tree(
    tree: &TreeContext<'_>,
    entry: &Entry,
    y: &mut f32,
    depth: usize,
    parent: Option<u32>,
    index: &mut TreeIndex,
    elements: &mut Vec<Element>,
    visited: &mut std::collections::HashSet<String>,
) {
    if !visited.insert(entry.key.clone()) {
        return;
    }
    let indent = tree.rect.left + INDENT + depth as f32 * INDENT;
    let expanded = entry.expanded(tree.panel);
    let mut parent = parent;
    if *y < tree.visible.1 {
        if entry.directory {
            parent = Some(index.push_dir(
                StickyDirectory {
                    key: entry.key.clone(),
                    name: entry.name.clone(),
                    depth,
                    source_y: *y,
                    is_expanded: expanded,
                    entry: entry.clone(),
                },
                parent,
            ));
        }
        if let Some(dir) = parent {
            index.rows.push(TreeRowMeta { y: *y, depth, dir });
        }
    }
    if entry.renaming(tree.panel) {
        elements.push(rename_row(tree, *y, indent, entry, expanded));
    } else if tree.row_visible(*y, ROW_H) {
        if entry.directory {
            elements.push(dir_row(
                tree,
                entry,
                indent,
                *y,
                0.0,
                expanded,
                false,
                tree.frame.hover.get().as_deref() == Some(entry.key.as_str()),
                tree_label_color(entry.decoration(tree.frame), theme::c().text_soft),
            ));
        } else {
            elements.push(file_row(tree, entry, indent, *y));
        }
    }
    *y += ROW_H;
    if let Some(kind) = entry.creating(tree.panel) {
        elements.push(create_row(tree, *y, indent, kind));
        *y += CREATE_ROW_H;
    }
    if entry.directory && expanded {
        let children_top = *y;
        match entry
            .local_path()
            .filter(|_| source::can_skip_rows(tree.panel))
        {
            Some(path) => {
                // Only the rows near the viewport become entries. A sibling whose whole subtree
                // lies above or below it just advances the layout cursor by its row count.
                if let Some(listing) = tree.panel.model.listing(&entry.key) {
                    for child in listing.iter() {
                        let rows = 1 + if child.is_dir {
                            tree.panel.model.child_rows_below(&entry.key, child.name)
                        } else {
                            0
                        };
                        let height = rows as f32 * ROW_H;
                        if *y + height <= tree.visible.0 || *y >= tree.visible.1 {
                            *y += height;
                            continue;
                        }
                        let child = Entry::local(
                            path.join(child.name),
                            child.name.to_owned(),
                            child.is_dir,
                        );
                        build_tree(tree, &child, y, depth + 1, parent, index, elements, visited);
                    }
                }
            }
            None => {
                for child in entry.children(tree.panel, depth) {
                    build_tree(tree, &child, y, depth + 1, parent, index, elements, visited);
                }
            }
        }
        // Like VS Code: every guide shows while the pointer is over the drawer,
        // and only the active row's guide when it is not. Two neighbouring tones
        // tell them apart: the brighter one for the active row's guide. The cheap
        // visibility checks come first, since this runs for every row of the pass.
        if *y > children_top && tree.row_visible(children_top, *y - children_top) {
            let active = entry.guide_active(tree.active_guide);
            if active || tree.panel.ui.hovered {
                let guide_x = indent + TREE_ICON_SIZE / 2.0;
                elements.push(divider::vertical_pixel(
                    Point::new(guide_x, children_top),
                    *y,
                    tree.rect,
                    divider::VerticalPlacement::Scrolled(tree.scroll.0, tree.scroll.1),
                    guide_color(active),
                ));
            }
        }
    }
}

/// Right edge required by expanded tree rows. Reuse the tab strip's cached
/// shaped widths so CJK and fallback glyphs contribute their actual advances.
fn tree_content_right(entry: &Entry, s: &ExplorerPanelState, left: f32) -> f32 {
    // Measuring every expanded row is slow in a large tree, and the answer only changes when a
    // listing or an expansion does, so a local root's width is remembered per tree revision.
    thread_local! {
        static REMEMBERED: std::cell::RefCell<Remembered> = Default::default();
    }
    let remembered = entry
        .local_path()
        .map(|_| {
            // The width of one letter stands in for the current font.
            let font = crate::ui::tabs::measure("W", theme::UI_SIZE, 400).to_bits();
            (entry.key.clone(), left.to_bits(), font)
        })
        .filter(|_| source::can_skip_rows(s));
    let revision = s.model.revision();
    if let Some(key) = &remembered
        && let Some(width) = REMEMBERED.with(|cache| {
            cache
                .borrow()
                .get(key)
                .filter(|(cached, _)| *cached == revision)
                .map(|(_, width)| *width)
        })
    {
        return width;
    }
    let mut right = left;
    if entry.local_path().is_some() && entry.badge.is_none() {
        right = local_content_right(entry, s, left);
    } else {
        extend_content_right(
            entry,
            0,
            s,
            left,
            &mut right,
            &mut std::collections::HashSet::new(),
        );
    }
    if let Some(key) = remembered {
        REMEMBERED.with(|cache| cache.borrow_mut().insert(key, (revision, right)));
    }
    right
}

/// The same answer as [extend_content_right] for a local root, without visiting every row: the
/// cache remembers the widest directory name and widest file name of each listing, so only the
/// expanded directories are walked and each name is measured once, when its listing is loaded.
fn local_content_right(entry: &Entry, s: &ExplorerPanelState, left: f32) -> f32 {
    let mut right = row_content_right(left, 0, &entry.name, entry.directory);
    // One letter's width stands in for the current font.
    let epoch = crate::ui::tabs::measure("W", theme::UI_SIZE, 400).to_bits();
    if entry.directory && s.model.is_expanded(&entry.key) {
        widen_to_listing(s, &entry.key, 0, left, epoch, &mut right);
    }
    right
}

fn widen_to_listing(
    s: &ExplorerPanelState,
    key: &str,
    depth: usize,
    left: f32,
    epoch: u32,
    right: &mut f32,
) {
    let widths = s.model.name_widths(key, epoch, |listing| {
        let (mut widest_dir, mut widest_file) = (0.0f32, 0.0f32);
        for entry in listing.iter() {
            let width =
                crate::ui::tabs::measure_once(entry.name, theme::UI_SIZE, 400) + TREE_TEXT_PAD;
            if entry.is_dir {
                widest_dir = widest_dir.max(width);
            } else {
                widest_file = widest_file.max(width);
            }
        }
        (widest_dir, widest_file)
    });
    let Some((widest_dir, widest_file)) = widths else {
        return;
    };
    // The listing's rows sit one level below the directory.
    let base = left + INDENT + (depth + 1) as f32 * INDENT + TREE_LABEL_OFFSET;
    if widest_dir > 0.0 {
        *right = right.max(base + widest_dir + DIRECTORY_RIGHT_PAD);
    }
    if widest_file > 0.0 {
        *right = right.max(base + widest_file + FILE_RIGHT_PAD);
    }
    for child in s.model.expanded_children(key) {
        widen_to_listing(s, &child, depth + 1, left, epoch, right);
    }
}

/// Width measurement uses the same source adapter and expanded nodes as layout.
fn extend_content_right(
    entry: &Entry,
    depth: usize,
    s: &ExplorerPanelState,
    left: f32,
    right: &mut f32,
    visited: &mut std::collections::HashSet<String>,
) {
    if !visited.insert(entry.key.clone()) {
        return;
    }
    let mut row_right = row_content_right(left, depth, &entry.name, entry.directory);
    if let Some(badge) = entry.badge.as_deref() {
        row_right += BADGE_GAP + badge_fit(badge).1;
    }
    *right = right.max(row_right);
    if entry.directory && entry.expanded(s) {
        for child in entry.children(s, depth) {
            extend_content_right(&child, depth + 1, s, left, right, visited);
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
    scrollbar::thumb(UiRect::new(
        track.left + scrollbar::INSET,
        thumb_top,
        track.right - scrollbar::INSET,
        thumb_top + thumb_h,
    ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::explorer::DirEntry;

    fn target(row: usize, covered: usize) -> ExplorerReveal {
        ExplorerReveal { row, covered }
    }

    #[test]
    fn path_hints_open_to_the_left_below_or_above_the_row() {
        let viewport = UiRect::new(0.0, 0.0, 800.0, 600.0);
        let below = path_tip_rect(
            UiRect::new(500.0, 200.0, 800.0, 220.0),
            viewport,
            320.0,
            28.0,
            false,
        );
        assert_eq!(below, UiRect::new(180.0, 224.0, 500.0, 252.0));
        let above = path_tip_rect(
            UiRect::new(500.0, 560.0, 800.0, 580.0),
            viewport,
            320.0,
            28.0,
            false,
        );
        assert_eq!(above, UiRect::new(180.0, 528.0, 500.0, 556.0));
        let narrow = path_tip_rect(
            UiRect::new(30.0, 200.0, 300.0, 220.0),
            viewport,
            600.0,
            48.0,
            false,
        );
        assert_eq!(narrow.left, 8.0);
        assert!(narrow.right < viewport.right);
    }

    #[test]
    fn path_hints_flip_to_the_right_when_the_left_side_cannot_hold_them() {
        let viewport = UiRect::new(0.0, 0.0, 800.0, 600.0);
        // An explorer at the left window edge leaves no room on its left.
        let anchor = UiRect::new(60.0, 200.0, 200.0, 220.0);
        assert!(path_tip_opens_right(anchor, viewport, 300.0));
        let rect = path_tip_rect(anchor, viewport, 300.0, 28.0, true);
        assert_eq!(rect, UiRect::new(60.0, 224.0, 360.0, 252.0));
        // Enough room on the left keeps the original side.
        assert!(!path_tip_opens_right(
            UiRect::new(500.0, 200.0, 800.0, 220.0),
            viewport,
            320.0
        ));
        // When neither side fits, the roomier side wins.
        assert!(!path_tip_opens_right(
            UiRect::new(500.0, 200.0, 700.0, 220.0),
            viewport,
            600.0
        ));
    }

    #[test]
    fn a_short_badge_is_kept_whole_and_a_long_one_is_cut_to_the_widest_chip() {
        let (short, width) = badge_fit("web-01");
        assert_eq!(short, "web-01");
        assert!(width < BADGE_MAX_W);
        let (long, width) = badge_fit("hkg20-20260326100823aada56-and-more-and-more");
        assert!(long.ends_with('…') && long.chars().count() < 40);
        assert_eq!(width, BADGE_MAX_W);
    }

    #[test]
    fn only_a_root_with_a_badge_gets_one_and_it_widens_the_content() {
        use loog_workspace_api::{Capability, EnvironmentKey, PresentedRoot, ResourceRef};
        let mut panel = ExplorerPanelState::default();
        let resource = ResourceRef::provided(
            EnvironmentKey::new("fixture", "tree").unwrap(),
            "opaque:root",
        )
        .unwrap();
        // A root keeps the descriptor it was presented with, so each case has its own scope.
        let root = |scope: u64, badge: Option<&str>| PresentedRoot {
            scope,
            resource: resource.clone(),
            name: "app".into(),
            capabilities: vec![Capability::FilesRead],
            badge: badge.map(str::to_owned),
        };
        panel.resources.sync(vec![root(7, Some("web-01"))]);
        let with = source::roots(&panel);
        assert_eq!(with[0].badge.as_deref(), Some("web-01"));
        panel.resources.sync(vec![root(8, None)]);
        let without = source::roots(&panel);
        assert_eq!(without[0].badge, None);
        let width = |entries: &[Entry]| tree_content_right(&entries[0], &panel, 0.0);
        assert!(width(&with) > width(&without));
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
            entry: Entry::local(key.into(), key.into(), true),
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
            [DirEntry::new(
                &std::sync::Arc::from(Path::new(root_key)),
                "src".to_string(),
                true,
            )]
            .into_iter()
            .collect::<crate::features::explorer::Listing>()
            .into(),
        );
        state.model.insert_listing(
            nested_key.to_string(),
            [DirEntry::new(
                &std::sync::Arc::from(Path::new(nested_key)),
                long_name.to_string(),
                false,
            )]
            .into_iter()
            .collect::<crate::features::explorer::Listing>()
            .into(),
        );

        assert_eq!(
            tree_content_right(
                &Entry::local(root_key.into(), "root".into(), true),
                &state,
                100.0
            ),
            row_content_right(100.0, 2, long_name, false)
        );

        state.model.set_expanded(nested_key, false);
        assert_eq!(
            tree_content_right(
                &Entry::local(root_key.into(), "root".into(), true),
                &state,
                100.0
            ),
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

#[cfg(test)]
mod width_tests {
    use super::*;
    use crate::features::explorer::Listing;
    use std::sync::Arc;

    fn listing(parent: &str, entries: &[(&str, bool)]) -> Arc<Listing> {
        Arc::new(Listing::ordered(
            Arc::from(parent),
            entries.iter().map(|(name, dir)| (*name, *dir)),
        ))
    }

    #[test]
    fn the_fast_width_matches_walking_every_row() {
        let root = std::path::PathBuf::from("proj");
        let key = |parts: &[&str]| {
            parts
                .iter()
                .fold(root.clone(), |path, part| path.join(part))
                .to_string_lossy()
                .into_owned()
        };
        let mut panel = ExplorerPanelState::default();
        let model = &mut panel.model;
        model.insert_listing(
            key(&[]),
            listing(
                &key(&[]),
                &[
                    ("src", true),
                    ("a-rather-long-file-name.rs", false),
                    ("z", false),
                ],
            ),
        );
        model.insert_listing(
            key(&["src"]),
            listing(
                &key(&["src"]),
                &[("nested-directory-with-a-long-name", true), ("b.rs", false)],
            ),
        );
        model.insert_listing(
            key(&["src", "nested-directory-with-a-long-name"]),
            listing(
                &key(&["src", "nested-directory-with-a-long-name"]),
                &[(
                    "deep-file-with-an-even-longer-name-than-the-rest.txt",
                    false,
                )],
            ),
        );
        model.insert_listing(
            key(&["unopened"]),
            listing(&key(&["unopened"]), &[("never-counted.txt", false)]),
        );
        for path in [
            key(&[]),
            key(&["src"]),
            key(&["src", "nested-directory-with-a-long-name"]),
        ] {
            model.set_expanded(&path, true);
        }
        let entry = Entry::local(root.clone(), "proj".to_owned(), true);

        let mut walked = 0.0f32;
        extend_content_right(
            &entry,
            0,
            &panel,
            12.0,
            &mut walked,
            &mut std::collections::HashSet::new(),
        );
        let fast = local_content_right(&entry, &panel, 12.0);
        assert!((fast - walked).abs() < 0.01, "fast {fast} walked {walked}");
        assert!(fast > 12.0);
        // The widest name is the deepest one, so collapsing its directory narrows the result.
        panel
            .model
            .set_expanded(&key(&["src", "nested-directory-with-a-long-name"]), false);
        let narrower = local_content_right(&entry, &panel, 12.0);
        assert!(narrower < fast);
    }
}
