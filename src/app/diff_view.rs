//! Diff input coordination; geometry and painting stay in UI.
use crate::app::context::AppContext;
use crate::app::state::{AppState, MainSurface};
use crate::domain::identity::PaneId;
use crate::features::git::diff_document::ChangesLayout;
use crate::features::git::diff_selection::{DiffSelection, SelectionUnit};
use crate::features::keymap::action::Action;
use crate::ui::chrome::diff_binding::{Binding, Command, EditorFrame, Snapshot};
use crate::ui::diff_editor::{TOOLBAR_H, code_left, row_h, scroll_metrics};
use crate::ui::diff_selection::{Geometry, point_at};
use lgui::core::UiFocusHandle;
use lgui::prelude::{Element, UiRect};
pub fn binding(state: &AppContext) -> Binding {
    let reader = state.clone();
    let writer = state.clone();
    Binding::new(
        move || {
            let app = reader.get();
            Snapshot {
                workspace: app.workspace.core,
                split: app.git.ui.split_diff,
                change_highlight: app.git.ui.diff_change_highlight,
                editor: EditorFrame {
                    diff_split: app.editor.diff_split,
                    editor_hovered: app.editor.editor_hovered,
                    editor_vertical_scrollbar_dragging: app
                        .editor
                        .editor_vertical_scrollbar_dragging,
                    editor_horizontal_scrollbar_dragging: app
                        .editor
                        .editor_horizontal_scrollbar_dragging,
                    selection: app.editor.diff_selection,
                    change_focus: app.editor.diff_change_focus.clone(),
                },
            }
        },
        move |c| run(&writer, c),
    )
}
pub fn run(state: &AppContext, c: Command) -> bool {
    if let Command::Open(path) = c {
        let loader = state.services().and_then(|runtime| runtime.file_loader());
        let mut opened = false;
        state.update(|app| {
            opened = crate::app::explorer::open_tree_file(app, path, 2, loader.as_ref());
        });
        return opened;
    }
    state.update(|app| match c {
        Command::Wheel(pane, dx, dy, mx, my) => {
            let (dx, dy) = if app.editor.modifiers.shift() {
                (dx + dy, 0.0)
            } else {
                (dx, dy)
            };
            let (x, y) = app.workspace.scroll(pane);
            app.workspace
                .set_scroll(pane, (x - dx).clamp(0.0, mx), (y - dy).clamp(0.0, my));
        }
        Command::Activate(pane) => {
            app.editor.diff_change_focus = None;
            app.workspace.activate_pane(pane);
        }
        Command::SplitStart(pane) => {
            app.editor.diff_change_focus = None;
            app.editor.diff_split.drag = Some(pane);
        }
        Command::SplitDrag(pane, rect, x) => {
            if app.editor.diff_split.drag == Some(pane) && rect.width() > 0.0 {
                let position = crate::ui::pane_sash::split_position(rect.left, rect.right, x);
                app.editor.diff_split.ratio = (position - rect.left) / rect.width();
            }
        }
        Command::SplitEnd => app.editor.diff_split.drag = None,
        Command::Layout(pane, split) => {
            app.editor.diff_selection = None;
            app.editor.diff_change_focus = None;
            app.git.ui.split_diff = split;
            app.workspace.activate_pane(pane);
            app.workspace.set_scroll(pane, 0.0, 0.0);
        }
        Command::ToggleChangeHighlight => {
            app.git.ui.diff_change_highlight ^= true;
            if !app.git.ui.diff_change_highlight {
                app.editor.diff_change_focus = None;
            }
        }
        Command::Navigate(pane, starts, next) => {
            app.workspace.activate_pane(pane);
            navigate_change(app, &starts, next);
        }
        Command::FocusChange(pane, focus, reveal_y) => {
            let Some(view) = app.workspace.pane(pane).and_then(|pane| pane.active()) else {
                return;
            };
            if !app.git.ui.split_diff
                || !app.git.ui.diff_change_highlight
                || !app
                    .workspace
                    .diff_record(view)
                    .is_some_and(|record| focus.is_current(view, &record.content))
            {
                return;
            }
            app.workspace.activate_pane(pane);
            let same_block = app
                .editor
                .diff_change_focus
                .as_ref()
                .is_some_and(|current| {
                    current.view == focus.view
                        && current.document.ptr_eq(&focus.document)
                        && current.block == focus.block
                });
            app.editor.diff_change_focus = (!same_block).then_some(focus);
            if !same_block && let Some(y) = reveal_y {
                let (x, _) = app.workspace.scroll(pane);
                app.workspace.set_scroll(pane, x, y);
            }
        }
        Command::DismissChangeFocus(view) => {
            if app
                .editor
                .diff_change_focus
                .as_ref()
                .is_some_and(|focus| focus.view == view)
            {
                app.editor.diff_change_focus = None;
            }
        }
        Command::Toggle(pane, request) => {
            app.editor.diff_selection = None;
            if app.editor.diff_change_focus.as_ref().is_some_and(|focus| {
                app.workspace.pane(pane).and_then(|pane| pane.active()) == Some(focus.view)
                    && focus
                        .document
                        .upgrade()
                        .is_some_and(|doc| doc.request() == request)
            }) {
                app.editor.diff_change_focus = None;
            }
            app.workspace.activate_pane(pane);
            app.workspace.toggle_diff_section(pane, request);
        }
        Command::ToggleAll(pane) => {
            app.editor.diff_selection = None;
            if app.editor.diff_change_focus.as_ref().is_some_and(|focus| {
                app.workspace.pane(pane).and_then(|pane| pane.active()) == Some(focus.view)
            }) {
                app.editor.diff_change_focus = None;
            }
            let Some(sections) = app
                .workspace
                .diff(pane)
                .and_then(|doc| doc.sections.as_ref())
            else {
                return;
            };
            let collapsed = app.workspace.collapsed_diffs(pane);
            let all_collapsed = sections
                .iter()
                .all(|section| collapsed.contains(&section.request()));
            let requests: Vec<_> = sections
                .iter()
                .map(|section| section.request())
                .filter(|request| collapsed.contains(request) == all_collapsed)
                .collect();
            for request in requests {
                app.workspace.toggle_diff_section(pane, request);
            }
            app.workspace.activate_pane(pane);
            if !all_collapsed {
                app.workspace.set_scroll(pane, 0.0, 0.0);
            }
        }
        Command::SelectStart(pane, body, point) => {
            app.workspace.activate_pane(pane);
            let Some(view) = app.workspace.active() else {
                return;
            };
            let clicks = app.editor.click_count(view, point.0, point.1);
            let extend = app
                .editor
                .modifiers
                .shift()
                .then_some(app.editor.diff_selection)
                .flatten();
            let split = app.git.ui.split_diff;
            let Some((geometry, layout)) = selection_geometry(app, pane, body) else {
                return;
            };
            let document = app.workspace.diff(pane).expect("geometry needs a diff");
            let hit = point_at(document, layout.as_ref(), geometry, None, point);
            app.editor.diff_selection =
                hit.filter(|(_, _, inside)| *inside)
                    .map(|(side, point, _)| {
                        let unit = match clicks {
                            3 => SelectionUnit::Line,
                            2 => SelectionUnit::Word,
                            _ => SelectionUnit::Character,
                        };
                        DiffSelection::start(
                            document,
                            split,
                            pane,
                            side,
                            point,
                            unit,
                            extend.as_ref(),
                        )
                    });
        }
        Command::SelectDrag(pane, body, point) => {
            let Some(mut selection) = app
                .editor
                .diff_selection
                .filter(|selection| selection.dragging && selection.pane == pane)
            else {
                return;
            };
            // Holding the pointer past an edge scrolls toward it.
            let (x, y) = app.workspace.scroll(pane);
            let step = row_h();
            let toward = |value: f32, low: f32, high: f32| {
                if value < low {
                    -step
                } else if value > high {
                    step
                } else {
                    0.0
                }
            };
            let (max_x, max_y) = scroll_limits(app, pane, body);
            app.workspace.set_scroll(
                pane,
                (x + toward(point.0, body.left, body.right)).clamp(0.0, max_x),
                (y + toward(point.1, body.top, body.bottom)).clamp(0.0, max_y),
            );
            let split = app.git.ui.split_diff;
            let Some((geometry, layout)) = selection_geometry(app, pane, body) else {
                return;
            };
            let document = app.workspace.diff(pane).expect("geometry needs a diff");
            if let Some((_, point, _)) = point_at(
                document,
                layout.as_ref(),
                geometry,
                Some(selection.side),
                point,
            ) {
                selection.drag_to(document, split, point);
                app.editor.diff_selection = Some(selection);
            }
        }
        Command::SelectEnd => {
            if let Some(selection) = &mut app.editor.diff_selection {
                selection.dragging = false;
            }
        }
        Command::Focus(focused) => {
            app.editor.focused = focused;
            if !focused && let Some(selection) = &mut app.editor.diff_selection {
                selection.dragging = false;
            }
        }
        Command::Open(_) => unreachable!(),
    });
    true
}
fn selection_geometry(
    app: &AppState,
    pane: PaneId,
    body: UiRect,
) -> Option<(Geometry, Option<ChangesLayout>)> {
    let document = app.workspace.diff(pane)?;
    let split = app.git.ui.split_diff;
    let layout = document
        .sections
        .as_ref()
        .map(|_| document.changes_layout(split, &app.workspace.collapsed_diffs(pane)));
    let (scroll_x, scroll_y) = app.workspace.scroll(pane);
    let geometry = Geometry {
        body,
        split,
        ratio: app.editor.diff_split.ratio,
        scroll_x,
        scroll_y,
    };
    Some((geometry, layout))
}
fn scroll_limits(app: &AppState, pane: PaneId, body: UiRect) -> (f32, f32) {
    let Some((_, layout)) = selection_geometry(app, pane, body) else {
        return (0.0, 0.0);
    };
    let Some(document) = app.workspace.diff(pane) else {
        return (0.0, 0.0);
    };
    let split = app.git.ui.split_diff;
    let rows = layout.map_or_else(
        || {
            if split {
                document.split_rows.len()
            } else {
                document.rows.len()
            }
        },
        |layout| layout.row_count,
    );
    let metrics = scroll_metrics(
        body,
        split,
        rows,
        document.longest_line(split),
        app.editor.diff_split.ratio,
    );
    (metrics.max_x, metrics.max_y)
}
/// Copy, select-all and clear for text selected in the diff view.
pub(crate) fn key_action(
    state: &AppContext,
    action: Action,
    clipboard: lgui::services::ClipboardHandle,
) -> bool {
    let app = state.get();
    if app.main_surface() != MainSurface::Diff || !app.editor.focused {
        return false;
    }
    let pane = app.workspace.active_pane();
    let split = app.git.ui.split_diff;
    let selection = app
        .editor
        .diff_selection
        .filter(|selection| selection.pane == pane);
    let change_focused = action == Action::Cancel
        && split
        && app.workspace.active().is_some_and(|view| {
            app.workspace.diff_record(view).is_some_and(|record| {
                app.editor
                    .diff_change_focus
                    .as_ref()
                    .is_some_and(|focus| focus.is_current(view, &record.content))
            })
        });
    match action {
        Action::Copy => {
            let Some(text) =
                selection
                    .zip(app.workspace.diff(pane))
                    .and_then(|(selection, document)| {
                        selection.text(document, split, &app.workspace.collapsed_diffs(pane))
                    })
            else {
                return false;
            };
            drop(app);
            if let Err(error) = clipboard.write_text(&text) {
                state.update(|app| app.show_error(format!("Clipboard: {error}")));
            }
            true
        }
        Action::SelectAll => {
            let all = app.workspace.diff(pane).and_then(|document| {
                DiffSelection::all(document, split, pane, &app.workspace.collapsed_diffs(pane))
            });
            drop(app);
            state.update(|app| app.editor.diff_selection = all);
            true
        }
        Action::Cancel if selection.is_some() || change_focused => {
            drop(app);
            state.update(|app| {
                app.editor.diff_selection = None;
                if change_focused {
                    app.editor.diff_change_focus = None;
                }
            });
            true
        }
        _ => false,
    }
}
pub fn render(
    pane: PaneId,
    rect: UiRect,
    state: AppContext,
    git: crate::ui::git::commands::Binding,
    surface_id: lgui::core::UiId,
    focus: UiFocusHandle,
    scrollbars: crate::ui::editor_binding::Binding,
) -> Element {
    crate::ui::diff_editor::render(
        pane,
        rect,
        binding(&state),
        git,
        surface_id,
        focus,
        scrollbars,
    )
}
/// Use the same window-level drag handling as the text editor, including
/// movements outside the scrollbar strip.
pub(crate) fn drag_scrollbars(app: &mut AppState, rect: UiRect, x: f32, y: f32) -> bool {
    if !app.editor.editor_vertical_scrollbar_dragging
        && !app.editor.editor_horizontal_scrollbar_dragging
    {
        return false;
    }
    let split = app.git.ui.split_diff;
    let Some(document) = app.workspace.diff(app.workspace.active_pane()) else {
        return crate::app::editor::finish_scrollbar_drag(app);
    };
    let body = UiRect::new(rect.left, rect.top + TOOLBAR_H, rect.right, rect.bottom);
    let rows = if document.sections.is_some() {
        document
            .changes_layout(
                split,
                &app.workspace.collapsed_diffs(app.workspace.active_pane()),
            )
            .row_count
    } else if split {
        document.split_rows.len()
    } else {
        document.rows.len()
    };
    let metrics = scroll_metrics(
        body,
        split,
        rows,
        document.longest_line(split),
        app.editor.diff_split.ratio,
    );
    crate::app::editor::drag_scrollbars_with_metrics(
        app,
        body,
        code_left(body, split),
        metrics,
        x,
        y,
    )
}
fn navigate_change(app: &mut AppState, starts: &[usize], next: bool) {
    if starts.is_empty() {
        return;
    }
    let (x, y) = app.workspace.active_scroll();
    let current = (y / row_h()).floor() as usize;
    let target = if next {
        starts
            .iter()
            .copied()
            .find(|start| *start > current)
            .unwrap_or(starts[0])
    } else {
        starts
            .iter()
            .rev()
            .copied()
            .find(|start| *start < current)
            .unwrap_or(*starts.last().expect("change list is not empty"))
    };
    app.workspace.set_active_scroll(x, target as f32 * row_h());
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::git::GitStoreSnapshot;
    use crate::features::git::{DiffTarget, diff_document::DiffDocument};
    use crate::ui::editor::view::SCROLLBAR_SIZE;
    use crate::ui::{
        diff_editor::{current_change_number, render_inline, render_split, split_middle},
        theme,
    };
    use lgui::prelude::State;
    use std::path::PathBuf;
    fn render(
        pane: PaneId,
        rect: UiRect,
        state: AppContext,
        store: State<GitStoreSnapshot>,
        focus: UiFocusHandle,
    ) -> Element {
        let binding = crate::app::editor_view::binding(state.clone(), store.clone(), pane, rect);
        super::render(
            pane,
            rect,
            state.clone(),
            crate::app::git_view::binding(&state, &store),
            lgui::core::UiId::new("diff-test-surface"),
            focus,
            binding,
        )
    }

    fn sample_overview(extra_files: usize) -> DiffDocument {
        use crate::features::git::{
            ChangeDiff, ChangeGroup, DiffContent, DiffRequest, diff::parse_unified,
        };
        let file = |path: &str, group, target, patch: &str| ChangeDiff {
            path: path.into(),
            group,
            target,
            content: Ok(DiffContent::Unified(parse_unified(patch).unwrap())),
        };
        let mut files = vec![
            file(
                "src/app.rs",
                ChangeGroup::Staged,
                DiffTarget::HeadToIndex,
                "diff --git a/src/app.rs b/src/app.rs\n--- a/src/app.rs\n+++ b/src/app.rs\n@@ -1,3 +1,3 @@\n pub fn render() {\n-    open_diff(file);\n+    open_changes(repository, file);\n }\n",
            ),
            file(
                "src/ui/git_panel.rs",
                ChangeGroup::Unstaged,
                DiffTarget::IndexToWorktree,
                "diff --git a/src/ui/git_panel.rs b/src/ui/git_panel.rs\n--- a/src/ui/git_panel.rs\n+++ b/src/ui/git_panel.rs\n@@ -1,3 +1,3 @@\n fn on_click() {\n-    show_file_diff();\n+    show_all_changes();\n }\n",
            ),
            ChangeDiff {
                path: "notes/review.md".into(),
                group: ChangeGroup::Untracked,
                target: DiffTarget::IndexToWorktree,
                content: Ok(DiffContent::Untracked(
                    "# Review notes\n\nOne Changes tab for the whole repository.\n".into(),
                )),
            },
        ];
        files.extend((0..extra_files).map(|index| ChangeDiff {
            path: format!("generated/file-{index:04}.txt").into(),
            group: ChangeGroup::Untracked,
            target: DiffTarget::IndexToWorktree,
            content: Ok(DiffContent::Untracked("first\nsecond\nthird\n".into())),
        }));
        DiffDocument::from_content(
            DiffRequest {
                repository_root: "loog".into(),
                path: PathBuf::new(),
                target: DiffTarget::Changes,
            },
            DiffContent::Changes(files),
        )
    }

    #[test]
    fn changes_view_collapses_files_and_virtualizes_inline_and_split_sections() {
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{
            InputEvent, Point, PointerButton, PointerData, UiScale, dispatch_runtime_output,
        };
        use lgui::session::UiSession;
        use std::sync::{Arc, Mutex};
        let mut original_chrome: Option<Vec<String>> = None;
        for split in [false, true] {
            let exposed = Arc::new(Mutex::new(None::<AppContext>));
            let output = exposed.clone();
            let viewport = UiRect::new(0.0, 0.0, 900.0, 600.0);
            let view: AppView = Arc::new(move |cx| {
                let state = AppContext::detached(cx.state_with(|| {
                    let mut app = AppState::new();
                    app.git.ui.split_diff = split;
                    app.workspace.open_diff(sample_overview(0));
                    app
                }));
                *output.lock().unwrap() = Some(state.clone());
                let store = cx.state_with(GitStoreSnapshot::default);
                let id = cx.use_stable_id();
                let focus = cx.focus_handle(id);
                render(
                    state.get().workspace.active_pane(),
                    viewport,
                    state,
                    store,
                    focus,
                )
            });
            let context = ApplicationContext::empty(Default::default());
            let _text = lgui::backend::install_text_environment(
                &context,
                lgui::render_skia::skia_text_system_handle(),
            );
            let mut session = UiSession::new();
            session.render_view(&view, viewport, UiScale::ONE);
            let state = exposed.lock().unwrap().clone().unwrap();
            assert!(!session.tree().nodes().iter().any(|node| {
                node.layout_rect.left == viewport.right - SCROLLBAR_SIZE + 1.0
                    && node.layout_rect.width() == 4.0
                    && matches!(node.style.fill, Some(color) if color == theme::c().diff.add_mark
                        || color == theme::c().diff.del_mark)
            }), "All Changes must not show overview marks");
            let texts = || {
                session
                    .tree()
                    .nodes()
                    .iter()
                    .filter_map(|node| node.text.as_deref())
                    .collect::<Vec<_>>()
            };
            // Both modes must share the same toolbar sizes and file headings.
            // Only the Inline/Split button's selected appearance may differ, and
            // split adds the linked-change toggle at the right end, which moves
            // the right-aligned buttons sideways, so toolbar x positions are left out.
            let mut chrome = session
                .tree()
                .nodes()
                .iter()
                .filter(|node| node.layout_rect.bottom <= TOOLBAR_H && node.text.is_some())
                .map(|node| {
                    let rect = node.layout_rect;
                    format!(
                        "{} {} {} {:?}",
                        rect.width(),
                        rect.top,
                        rect.bottom,
                        node.text
                    )
                })
                .collect::<Vec<_>>();
            for expected in ["src/app.rs", "src/ui/git_panel.rs", "notes/review.md"] {
                assert!(texts().contains(&expected), "Missing {expected}");
                let heading = format!("Collapse {expected}");
                let headers = session
                    .tree()
                    .nodes()
                    .iter()
                    .filter(|node| {
                        node.semantics.as_ref().and_then(|s| s.name.as_deref())
                            == Some(heading.as_str())
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    headers.len(),
                    1,
                    "each file must retain one shared heading, split={split}"
                );
                assert_eq!(
                    (headers[0].layout_rect.left, headers[0].layout_rect.right),
                    (viewport.left, viewport.right)
                );
                assert_eq!(headers[0].style.fill, Some(theme::c().sidebar));
                let header = headers[0].layout_rect;
                chrome.extend(session.tree().nodes().iter().filter_map(|node| {
                    let rect = node.layout_rect;
                    (rect.top >= header.top && rect.bottom <= header.bottom).then(|| {
                        // Inline has separate added/deleted rows, so later files
                        // naturally start lower. Compare within each heading.
                        let local = UiRect::new(
                            rect.left,
                            rect.top - header.top,
                            rect.right,
                            rect.bottom - header.top,
                        );
                        format!(
                            "{:?} {:?} {:?} {:?}",
                            local, node.style, node.text, node.semantics
                        )
                    })
                }));
                for label in [
                    format!("Open Changes for {expected}"),
                    format!("Open File {expected}"),
                ] {
                    assert!(
                        session.tree().nodes().iter().any(|node| node
                            .semantics
                            .as_ref()
                            .and_then(|s| s.name.as_deref())
                            == Some(label.as_str())),
                        "existing file action {label} must remain available, split={split}"
                    );
                }
            }
            if let Some(original) = &original_chrome {
                assert_eq!(chrome.len(), original.len(), "existing chrome node count");
                for (index, (current, original)) in chrome.iter().zip(original).enumerate() {
                    assert_eq!(
                        current, original,
                        "Split must preserve toolbar/file heading node {index}"
                    );
                }
            } else {
                original_chrome = Some(chrome);
            }
            let preview = std::env::var_os(if split {
                "LOOG_SPLIT_CHANGES_PREVIEW"
            } else {
                "LOOG_CHANGES_PREVIEW"
            });
            if let Some(path) = preview {
                use lgui::core::PhysicalRect;
                use lgui::icons::IconRegistration;
                use lgui::renderer::{FrameInfo, FrameReason};
                context
                    .resources()
                    .provide(IconRegistration(Arc::new(crate::app::svg_icons())));
                let scale = UiScale::new(2.0);
                let mut surface = lgui::render_skia::SkiaSoftwareSurface::new(12 * 1024 * 1024);
                let frame = FrameInfo::new(
                    PhysicalRect::new(0, 0, 1800, 1200),
                    &[],
                    scale,
                    FrameReason::SceneChange,
                    true,
                );
                lgui::assets::backend::with_render_resources(
                    &context,
                    lgui::RenderResources::new(),
                    || {
                        surface
                            .draw(&session.tree().scene().project_to_physical(scale), &frame)
                            .unwrap();
                    },
                );
                let mut pixels = surface.pixels().to_vec();
                for pixel in pixels.as_chunks_mut::<4>().0 {
                    pixel.swap(0, 2);
                }
                image::save_buffer(path, &pixels, 1800, 1200, image::ColorType::Rgba8).unwrap();
            }
            let header = session
                .tree()
                .nodes()
                .iter()
                .find(|node| {
                    node.semantics.as_ref().and_then(|s| s.name.as_deref())
                        == Some("Collapse src/app.rs")
                })
                .unwrap()
                .layout_rect;
            let point = Point::new(header.left + 48.0, (header.top + header.bottom) / 2.0);
            for input in [
                InputEvent::PointerDown {
                    pointer: PointerData::mouse(point),
                    button: PointerButton::Left,
                },
                InputEvent::PointerUp {
                    pointer: PointerData::mouse(point),
                    button: PointerButton::Left,
                },
            ] {
                let events = session.handle_input(input);
                dispatch_runtime_output(
                    events,
                    &context,
                    &lgui::window::WindowId::new("changes-view-test"),
                    |action| session.handle_default_action(action),
                    |_| {},
                );
                session.render_view(&view, viewport, UiScale::ONE);
            }
            let app = state.get();
            let collapsed = app.workspace.collapsed_diffs(app.workspace.active_pane());
            assert_eq!(collapsed.len(), 1);
            assert_eq!(
                collapsed.iter().next().unwrap().target,
                DiffTarget::HeadToIndex
            );
            state.update(|app| {
                app.workspace.open_diff(sample_overview(1000));
                app.workspace.set_active_scroll(0.0, 100_000.0);
            });
            session.render_view(&view, viewport, UiScale::ONE);
            let texts = session
                .tree()
                .nodes()
                .iter()
                .filter_map(|node| node.text.as_deref())
                .collect::<Vec<_>>();
            assert!(texts.contains(&"generated/file-0999.txt"));
            assert!(!texts.contains(&"src/app.rs"));
            assert!(
                session.tree().nodes().len() < 600,
                "Offscreen files must not create UI nodes"
            );
        }
    }

    #[test]
    fn inline_overview_draws_added_and_deleted_display_rows_without_scrolling() {
        use crate::features::git::diff::parse_unified;
        use lgui::application::AppView;
        use lgui::core::UiScale;
        use lgui::session::UiSession;
        use std::sync::Arc;

        // Source line 10 appears at display row 1 after the hunk header.
        let document = DiffDocument::from_unified(
            "repo".into(),
            "a.txt".into(),
            DiffTarget::IndexToWorktree,
            parse_unified(
                "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -10,3 +12,3 @@\n-old\n-older\n+new\n+newer\n same\n",
            )
            .unwrap(),
        );
        let viewport = UiRect::new(0.0, 0.0, 600.0, 300.0);
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.git.ui.split_diff = false;
                app.workspace.open_diff(document.clone());
                app
            }));
            let store = cx.state_with(GitStoreSnapshot::default);
            let id = cx.use_stable_id();
            render(
                state.get().workspace.active_pane(),
                viewport,
                state,
                store,
                cx.focus_handle(id),
            )
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let marks = session
            .tree()
            .nodes()
            .iter()
            .filter(|node| {
                node.layout_rect.left == viewport.right - SCROLLBAR_SIZE + 1.0
                    && node.layout_rect.width() == 4.0
                    && node.style.fill.is_some()
            })
            .map(|node| (node.layout_rect, node.style.fill.unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(
            marks,
            [
                (
                    UiRect::new(589.0, TOOLBAR_H + row_h(), 593.0, TOOLBAR_H + 3.0 * row_h()),
                    theme::c().diff.del_mark,
                ),
                (
                    UiRect::new(
                        589.0,
                        TOOLBAR_H + 3.0 * row_h(),
                        593.0,
                        TOOLBAR_H + 5.0 * row_h()
                    ),
                    theme::c().diff.add_mark,
                ),
            ]
        );
    }

    /// The text a view shows for `document`.
    fn texts_of(document: DiffDocument) -> Vec<String> {
        use lgui::application::AppView;
        use lgui::core::UiScale;
        use lgui::session::UiSession;
        use std::sync::Arc;
        let viewport = UiRect::new(0.0, 0.0, 600.0, 300.0);
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.workspace.open_diff(document.clone());
                app
            }));
            let store = cx.state_with(GitStoreSnapshot::default);
            let id = cx.use_stable_id();
            render(
                state.get().workspace.active_pane(),
                viewport,
                state,
                store,
                cx.focus_handle(id),
            )
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        session
            .tree()
            .nodes()
            .iter()
            .filter_map(|node| node.text.as_ref().map(|text| text.to_string()))
            .collect()
    }

    #[test]
    fn an_unresolved_conflict_that_matches_head_says_so_instead_of_no_changes() {
        use crate::features::git::diff::parse_unified;
        use crate::features::git::{ChangeDiff, ChangeGroup, DiffContent, DiffRequest};
        let empty = |target| {
            DiffDocument::from_unified(
                "repo".into(),
                "a.txt".into(),
                target,
                parse_unified("").unwrap(),
            )
        };
        let conflict = texts_of(empty(DiffTarget::HeadToWorktree));
        assert!(
            conflict
                .iter()
                .any(|text| text.starts_with("Matches the current version, still unresolved")),
            "{conflict:?}"
        );
        // An ordinary empty diff keeps its message.
        let ordinary = texts_of(empty(DiffTarget::IndexToWorktree));
        assert!(
            ordinary
                .iter()
                .any(|text| text.starts_with("No textual changes"))
        );

        // The same in the repository-wide Changes tab.
        let section = |group, target| {
            DiffDocument::from_content(
                DiffRequest {
                    repository_root: "repo".into(),
                    path: PathBuf::new(),
                    target: DiffTarget::Changes,
                },
                DiffContent::Changes(vec![ChangeDiff {
                    path: "a.txt".into(),
                    target,
                    group,
                    content: Ok(DiffContent::Unified(parse_unified("").unwrap())),
                }]),
            )
        };
        let changes = texts_of(section(ChangeGroup::Conflicts, DiffTarget::HeadToWorktree));
        assert!(
            changes.contains(
                &"Matches the current version. Mark it as resolved to finish.".to_owned()
            ),
            "{changes:?}"
        );
        let unstaged = texts_of(section(ChangeGroup::Unstaged, DiffTarget::IndexToWorktree));
        assert!(unstaged.contains(&"No textual changes.".to_owned()));
    }

    #[test]
    fn split_diff_keeps_numbers_central_and_updates_curves_while_scrolling() {
        use crate::features::git::diff::parse_unified;
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{InputEvent, PointerData, UiPathCommand, UiScale};
        use lgui::session::UiSession;
        use std::sync::{Arc, Mutex};
        let mut patch = "diff --git a/.env.example b/.env.example\n--- a/.env.example\n+++ b/.env.example\n@@ -1,52 +1,55 @@\n".to_owned();
        for line in [
            "# msites — CMS configuration",
            "# Copy this file to .env.local before starting.",
            "",
            "# R2 object storage",
            "MSITES_CMS_R2_ACCOUNT_ID=",
            "MSITES_CMS_R2_ACCESS_KEY_ID=",
            "MSITES_CMS_R2_SECRET_ACCESS_KEY=",
            "MSITES_CMS_R2_BUCKET=msites",
            "",
            "# Public images",
        ] {
            patch.push_str(&format!(" {line}\n"));
        }
        patch.push_str("-# Optional HTTPS public image domain.\n-# Leave empty to serve private R2 objects through the API.\n+# Required HTTPS public image domain.\n+# Pages use this origin plus the actual object path.\n+# The API does not proxy images.\n+# Image requests go directly to the public origin.\n MSITES_CMS_R2_PUBLIC_URL=\n \n # Uploads\n MSITES_CMS_UPLOAD_MAX_SIZE=10485760\n+# Accepted image formats\n+MSITES_CMS_IMAGE_FORMATS=jpeg,png,webp\n+\n # Image delivery\n-# Proxy image requests through the CMS API.\n-MSITES_CMS_IMAGE_PROXY=true\n \n # Cache policy\n-MSITES_CMS_ASSET_CACHE_TTL=3600\n+MSITES_CMS_ASSET_CACHE_TTL=86400\n");
        for index in 0..30 {
            patch.push_str(&format!(" MSITES_CMS_OPTION_{index:02}=default\n"));
        }
        let document = DiffDocument::from_unified(
            "repo".into(),
            ".env.example".into(),
            DiffTarget::IndexToWorktree,
            parse_unified(&patch).unwrap(),
        );
        let exposed = Arc::new(Mutex::new(None::<AppContext>));
        let output = exposed.clone();
        let viewport = UiRect::new(0.0, 0.0, 1100.0, 440.0);
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.git.ui.split_diff = true;
                app.workspace.open_diff(document.clone());
                app
            }));
            *output.lock().unwrap() = Some(state.clone());
            let id = cx.use_stable_id();
            render(
                state.get().workspace.active_pane(),
                viewport,
                state,
                cx.state_with(GitStoreSnapshot::default),
                cx.focus_handle(id),
            )
        });
        let context = ApplicationContext::empty(Default::default());
        let _text = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let columns = crate::ui::diff_split::Columns::new(viewport, 0.5);
        let numbers = session
            .tree()
            .nodes()
            .iter()
            .filter(|node| node.text.as_deref() == Some("1"))
            .collect::<Vec<_>>();
        assert_eq!(numbers.len(), 2);
        assert!(
            numbers
                .iter()
                .all(|node| node.layout_rect.left >= columns.gutter.left
                    && node.layout_rect.right <= columns.gutter.right)
        );
        for label in ["Split", "Inline", "Open File"] {
            assert!(
                session
                    .tree()
                    .nodes()
                    .iter()
                    .any(|node| node.text.as_deref() == Some(label)),
                "existing control {label} must remain available"
            );
        }
        let stage = session
            .tree()
            .nodes()
            .iter()
            .find(|node| {
                node.semantics
                    .as_ref()
                    .and_then(|semantics| semantics.name.as_deref())
                    == Some("Stage change")
            })
            .expect("the working diff has a stage button");
        let button = stage.layout_rect;
        let point = lgui::core::Point::new(
            (button.left + button.right) / 2.0,
            (button.top + button.bottom) / 2.0,
        );
        assert_eq!(session.tree().hit_test(point).unwrap().id, stage.id);
        let pointer = PointerData::mouse(point);
        let hover = session.handle_input(InputEvent::PointerMove(pointer));
        assert!(
            hover.animation_changed,
            "a Git button schedules the change preview"
        );
        assert!(
            hover.dirty_bounds.is_some_and(|bounds| {
                bounds.left == viewport.left && bounds.right == viewport.right
            }),
            "hover invalidates both code columns"
        );
        let _ = session.handle_input(InputEvent::PointerLeave(pointer));
        let slope = |session: &UiSession| {
            session
                .tree()
                .nodes()
                .iter()
                .find_map(|node| {
                    let commands = node.path.as_ref()?.commands();
                    if let [
                        UiPathCommand::MoveTo(start),
                        UiPathCommand::CubicTo { to, .. },
                        ..,
                    ] = commands
                    {
                        Some(to.y - start.y)
                    } else {
                        None
                    }
                })
                .expect("a curved change connector")
        };
        let before = slope(&session);
        let state = exposed.lock().unwrap().clone().unwrap();
        state.update(|app| app.workspace.set_active_scroll(0.0, 8.0 * row_h()));
        session.render_view(&view, viewport, UiScale::ONE);
        assert!(
            (slope(&session) - before).abs() > 1.0,
            "the connector must change shape, not just translate"
        );
        if let Some(output) = std::env::var_os("LOOG_SPLIT_PREVIEW") {
            use lgui::core::PhysicalRect;
            use lgui::renderer::{FrameInfo, FrameReason};
            let scale = UiScale::new(1.5);
            let frame = FrameInfo::new(
                PhysicalRect::new(0, 0, 1650, 660),
                &[],
                scale,
                FrameReason::SceneChange,
                true,
            );
            let mut surface = lgui::render_skia::SkiaSoftwareSurface::new(12 * 1024 * 1024);
            lgui::assets::backend::with_render_resources(
                &context,
                lgui::RenderResources::new(),
                || {
                    surface
                        .draw(&session.tree().scene().project_to_physical(scale), &frame)
                        .unwrap()
                },
            );
            let mut pixels = surface.pixels().to_vec();
            for pixel in pixels.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
            image::save_buffer(output, &pixels, 1650, 660, image::ColorType::Rgba8).unwrap();
        }
    }

    #[test]
    fn split_change_focus_follows_the_curve_and_reveals_the_other_side() {
        use crate::features::git::diff::parse_unified;
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{
            CursorIcon, InputEvent, Point, PointerButton, PointerData, UiScale,
            dispatch_event_handlers, dispatch_runtime_output,
        };
        use lgui::session::UiSession;
        use std::sync::{Arc, Mutex};

        let patch = format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -2,16 +2,32 @@\n start\n{} between\n still here\n-old value\n+new value\n{}",
            "+added\n".repeat(16),
            " context\n".repeat(12)
        );
        let document = DiffDocument::commit(
            "repo".into(),
            "a.txt".into(),
            "1234567".into(),
            parse_unified(&patch).unwrap(),
        );
        let exposed = Arc::new(Mutex::new(None::<AppContext>));
        let output = exposed.clone();
        let height = row_h();
        let viewport = UiRect::new(0.0, 0.0, 840.0, TOOLBAR_H + 8.0 * height);
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.git.ui.split_diff = true;
                app.workspace.open_diff(document.clone());
                app
            }));
            *output.lock().unwrap() = Some(state.clone());
            let id = cx.use_stable_id();
            let store = cx.state_with(GitStoreSnapshot::default);
            let pane = state.get().workspace.active_pane();
            super::render(
                pane,
                viewport,
                state.clone(),
                crate::app::git_view::binding(&state, &store),
                id.clone(),
                cx.focus_handle(id),
                crate::app::editor_view::binding(state, store, pane, viewport),
            )
        });
        let context = ApplicationContext::empty(Default::default());
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let state = exposed.lock().unwrap().clone().unwrap();
        let window = lgui::window::WindowId::new("diff-change-focus-test");
        let send = |session: &mut UiSession, input| {
            let events = session.handle_input(input);
            let hover_frame = events.animation_changed;
            dispatch_runtime_output(
                events,
                &context,
                &window,
                |action| session.handle_default_action(action),
                |_| {},
            );
            for event in session.apply_pending_updates().focus_events {
                for handler in session.tree().handler_events(&event) {
                    dispatch_event_handlers(&handler, &context, &window);
                }
            }
            if hover_frame {
                let _ = session.advance(16.0);
            }
            session.render_view(&view, viewport, UiScale::ONE);
            hover_frame
        };
        let click = |session: &mut UiSession, point| {
            let pointer = PointerData::mouse(point);
            send(
                session,
                InputEvent::PointerDown {
                    pointer,
                    button: PointerButton::Left,
                },
            );
            send(
                session,
                InputEvent::PointerUp {
                    pointer,
                    button: PointerButton::Left,
                },
            );
        };
        let shows = |session: &UiSession, label| {
            session
                .tree()
                .nodes()
                .iter()
                .any(|node| node.text.as_deref() == Some(label))
        };
        let ribbon_fills = |session: &UiSession| {
            session
                .tree()
                .nodes()
                .iter()
                .filter_map(|node| {
                    let stroke = node.path_style.stroke?;
                    assert_eq!(stroke.width, 1.0, "the ribbon outline stays thin");
                    node.path_style.fill
                })
                .collect::<Vec<_>>()
        };
        let initial_fills = ribbon_fills(&session);
        let columns = crate::ui::diff_split::Columns::new(viewport, 0.5);
        let bridge_x = (columns.bridge.left + columns.bridge.right) / 2.0;
        let addition = Point::new(bridge_x, TOOLBAR_H + 6.0 * height);
        assert!(send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(addition)),
        ));
        assert_eq!(
            session.tree().cursor_at(addition),
            Some(CursorIcon::Pointer)
        );
        assert_ne!(ribbon_fills(&session), initial_fills);
        assert!(
            session.tree().nodes().iter().any(|node| {
                node.text.as_deref() == Some("3")
                    && node.layout_rect.left == columns.numbers[1].left
                    && node
                        .text_style
                        .as_ref()
                        .is_some_and(|style| style.color == theme::c().text_bright)
            }),
            "a highlighted line number uses the brighter text color"
        );
        assert!(
            session.tree().nodes().iter().any(|node| {
                node.style.fill == Some(theme::c().diff.add_mark)
                    && (node.layout_rect.top - (TOOLBAR_H + 2.0 * height)).abs() < 1.0
                    && node.layout_rect.left >= columns.numbers[0].left
                    && node.layout_rect.right <= columns.numbers[0].right
            }),
            "a pure addition has a marker on the old side"
        );
        assert!(send(
            &mut session,
            InputEvent::PointerLeave(PointerData::mouse(addition)),
        ));
        assert_eq!(
            ribbon_fills(&session),
            initial_fills,
            "window leave clears the preview"
        );
        assert!(send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(addition)),
        ));
        click(&mut session, addition);
        assert_eq!(
            state.get().editor.diff_change_focus.as_ref().unwrap().block,
            0
        );
        click(&mut session, addition);
        assert!(state.get().editor.diff_change_focus.is_none());

        let blank = Point::new(bridge_x, TOOLBAR_H + height);
        assert!(send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(blank)),
        ));
        assert_ne!(session.tree().cursor_at(blank), Some(CursorIcon::Pointer));
        assert_eq!(ribbon_fills(&session), initial_fills);
        assert!(!shows(&session, "new value"));

        let number = columns.numbers[0];
        let old_line = Point::new((number.left + number.right) / 2.0, TOOLBAR_H + 4.5 * height);
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(old_line)),
        );
        click(&mut session, old_line);
        assert_eq!(
            state.get().editor.diff_change_focus.as_ref().unwrap().block,
            1
        );
        assert!(state.get().editor.diff_selection.is_none());
        assert!(
            shows(&session, "new value"),
            "the other side was scrolled into view"
        );
        let focused_fills = ribbon_fills(&session);
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(blank)),
        );
        assert_eq!(
            ribbon_fills(&session),
            focused_fills,
            "focus survives mouse movement"
        );
        assert!(
            state.get().editor.focused,
            "a change click focuses the diff editor"
        );
        click(&mut session, Point::new(100.0, TOOLBAR_H + height));
        assert!(state.get().editor.diff_change_focus.is_none());
        assert_ne!(ribbon_fills(&session), focused_fills);
    }

    #[test]
    fn inline_diff_expands_tabs_before_rendering() {
        diff_expands_tabs_before_rendering(false);
    }

    #[test]
    fn split_diff_expands_tabs_before_rendering() {
        diff_expands_tabs_before_rendering(true);
    }

    fn diff_expands_tabs_before_rendering(split: bool) {
        use crate::features::git::diff_document::DiffDocument;
        use crate::features::git::{DiffTarget, diff::parse_unified};
        use lgui::application::AppView;
        use lgui::core::UiScale;
        use lgui::session::UiSession;
        use std::sync::Arc;

        let diff = parse_unified(
            "diff --git a/a.vue b/a.vue\n--- a/a.vue\n+++ b/a.vue\n@@ -1,2 +1,2 @@\n \tend\n-\t\t<div>\n+a\t中\tb\n",
        )
        .unwrap();
        let document = DiffDocument::from_unified(
            "repo".into(),
            "a.vue".into(),
            DiffTarget::IndexToWorktree,
            diff,
        );
        let viewport = UiRect::new(0.0, 0.0, 600.0, 100.0);
        let view: AppView = Arc::new(move |_| {
            if split {
                render_split(viewport, &document, 0.0, 0.0, 0.5)
            } else {
                render_inline(viewport, &document.rows, 0.0, 0.0)
            }
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let rendered = session
            .tree()
            .nodes()
            .iter()
            .filter_map(|node| node.text.as_deref())
            .collect::<Vec<_>>();
        for expected in ["    end", "        <div>", "a   中  b"] {
            assert!(rendered.contains(&expected), "rendered text: {rendered:?}");
        }
        assert!(rendered.iter().all(|text| !text.contains('\t')));
    }

    #[test]
    fn inline_scrollbars_drag_and_release_outside_the_track() {
        scrollbars_drag_and_release(false);
    }

    #[test]
    fn split_scrollbars_drag_and_release_outside_the_track() {
        scrollbars_drag_and_release(true);
    }

    fn scrollbars_drag_and_release(split: bool) {
        use crate::features::git::DiffTarget;
        use crate::features::git::diff_document::DiffDocument;
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{
            InputEvent, Point, PointerButton, PointerData, UiEventKind, UiEventPayload, UiScale,
            dispatch_runtime_output,
        };
        use lgui::prelude::group;
        use lgui::session::UiSession;
        use std::sync::{Arc, Mutex};

        let exposed = Arc::new(Mutex::new(None::<AppContext>));
        let output = exposed.clone();
        let viewport = UiRect::new(0.0, 0.0, 500.0, 300.0);
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.git.ui.split_diff = split;
                app.workspace.open_diff(DiffDocument::added(
                    "test-repo".into(),
                    "long.txt".into(),
                    DiffTarget::HeadToWorktree,
                    &format!("{}\n", "x".repeat(200)).repeat(100),
                ));
                app
            }));
            *output.lock().unwrap() = Some(state.clone());
            let id = cx.use_stable_id();
            let focus = cx.focus_handle(id);
            let drag_state = state.clone();
            let up_state = state.clone();
            group(viewport)
                .on_event_capture(UiEventKind::PointerMove, move |_, payload| {
                    if let UiEventPayload::PointerMove { pointer } = payload {
                        drag_state.try_update(|app| {
                            drag_scrollbars(app, viewport, pointer.point.x, pointer.point.y)
                        });
                    }
                })
                .on_event_capture(UiEventKind::PointerUp, move |_, _| {
                    up_state.try_update(crate::app::editor::finish_scrollbar_drag);
                })
                .child(render(
                    PaneId::new(1),
                    viewport,
                    state,
                    cx.state(crate::features::git::GitStoreSnapshot::default()),
                    focus,
                ))
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let state = exposed.lock().unwrap().clone().unwrap();
        let context = ApplicationContext::empty(Default::default());
        let mut send = |input| {
            let events = session.handle_input(input);
            dispatch_runtime_output(
                events,
                &context,
                &lgui::window::WindowId::new("diff-scrollbar-test"),
                |action| session.handle_default_action(action),
                |_| {},
            );
            session.render_view(&view, viewport, UiScale::ONE);
        };
        let pointer = |x, y| PointerData::mouse(Point::new(x, y));
        let body_top = TOOLBAR_H;

        if split {
            let body = UiRect::new(0.0, TOOLBAR_H, viewport.right, viewport.bottom);
            let sash = crate::ui::diff_split::Columns::new(body, 0.5).gutter.left;
            send(InputEvent::PointerDown {
                pointer: pointer(sash, 80.0),
                button: PointerButton::Left,
            });
            assert_eq!(state.get().editor.diff_split.drag, Some(PaneId::new(1)));
            send(InputEvent::PointerMove(pointer(900.0, 80.0)));
            assert!(state.get().editor.diff_split.ratio > 0.5);
            assert!(
                (split_middle(body, state.get().editor.diff_split.ratio) - (body.right - 200.0))
                    .abs()
                    < 0.01
            );
            send(InputEvent::PointerUp {
                pointer: pointer(900.0, 80.0),
                button: PointerButton::Left,
            });
            assert_eq!(state.get().editor.diff_split.drag, None);
            let ratio = state.get().editor.diff_split.ratio;
            send(InputEvent::PointerMove(pointer(20.0, 80.0)));
            assert_eq!(state.get().editor.diff_split.ratio, ratio);
        }

        // Grab the thumb, then move away from its strip and beyond the viewport.
        send(InputEvent::PointerDown {
            pointer: pointer(496.0, body_top + 4.0),
            button: PointerButton::Left,
        });
        assert!(state.get().editor.editor_vertical_scrollbar_dragging);
        send(InputEvent::PointerMove(pointer(30.0, 500.0)));
        let bottom = state.get().workspace.active_scroll().1;
        assert!(bottom > 0.0);
        send(InputEvent::PointerUp {
            pointer: pointer(30.0, 500.0),
            button: PointerButton::Left,
        });
        assert!(!state.get().editor.editor_vertical_scrollbar_dragging);
        send(InputEvent::PointerMove(pointer(30.0, body_top)));
        assert_eq!(state.get().workspace.active_scroll().1, bottom);

        // Clicking the track jumps toward the pointer and starts a new drag.
        send(InputEvent::PointerDown {
            pointer: pointer(499.0, body_top + 40.0),
            button: PointerButton::Left,
        });
        assert!(state.get().workspace.active_scroll().1 < bottom);
        send(InputEvent::PointerUp {
            pointer: pointer(499.0, body_top + 40.0),
            button: PointerButton::Left,
        });

        let left = code_left(viewport, split);
        send(InputEvent::PointerDown {
            pointer: pointer(left + 4.0, 296.0),
            button: PointerButton::Left,
        });
        assert!(state.get().editor.editor_horizontal_scrollbar_dragging);
        send(InputEvent::PointerMove(pointer(900.0, 100.0)));
        let right = state.get().workspace.active_scroll().0;
        assert!(right > 0.0);
        send(InputEvent::PointerUp {
            pointer: pointer(900.0, 100.0),
            button: PointerButton::Left,
        });
        assert!(!state.get().editor.editor_horizontal_scrollbar_dragging);
        send(InputEvent::PointerMove(pointer(left, 100.0)));
        assert_eq!(state.get().workspace.active_scroll().0, right);
        send(InputEvent::PointerDown {
            pointer: pointer(left + 30.0, 299.0),
            button: PointerButton::Left,
        });
        assert!(state.get().workspace.active_scroll().0 < right);
        send(InputEvent::PointerUp {
            pointer: pointer(left + 30.0, 299.0),
            button: PointerButton::Left,
        });
    }

    #[test]
    fn change_counter_tracks_the_nearest_change_above_the_viewport() {
        let starts = vec![3, 10, 20];
        assert_eq!(current_change_number(&starts, 0), Some(1));
        assert_eq!(current_change_number(&starts, 10), Some(2));
        assert_eq!(current_change_number(&starts, 19), Some(2));
        assert_eq!(current_change_number(&[], 19), None);
    }

    #[test]
    fn change_navigation_wraps_in_both_directions() {
        let mut app = AppState::new();
        app.workspace.open_path("a.txt".into(), "text".into());
        navigate_change(&mut app, &[2, 8], true);
        assert_eq!(app.workspace.active_scroll().1, 2.0 * row_h());
        app.workspace.set_active_scroll(0.0, 8.0 * row_h());
        navigate_change(&mut app, &[2, 8], true);
        assert_eq!(app.workspace.active_scroll().1, 2.0 * row_h());
        navigate_change(&mut app, &[2, 8], false);
        assert_eq!(app.workspace.active_scroll().1, 8.0 * row_h());
    }

    #[derive(Default)]
    struct TestClipboard(std::sync::Mutex<Option<String>>);

    impl lgui::services::Clipboard for TestClipboard {
        fn read_text(&self) -> Result<Option<String>, lgui::services::ClipboardError> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn write_text(&self, text: &str) -> Result<(), lgui::services::ClipboardError> {
            *self.0.lock().unwrap() = Some(text.to_owned());
            Ok(())
        }
    }

    #[test]
    fn dragging_across_changes_selects_code_and_copy_uses_the_clipboard() {
        use crate::ui::fonts;
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{InputEvent, Point, PointerButton, PointerData, UiScale};
        use lgui::session::UiSession;
        use std::sync::{Arc, Mutex};
        for split in [false, true] {
            let exposed = Arc::new(Mutex::new(None::<AppContext>));
            let output = exposed.clone();
            let viewport = UiRect::new(0.0, 0.0, 900.0, 600.0);
            let view: AppView = Arc::new(move |cx| {
                let state = AppContext::detached(cx.state_with(|| {
                    let mut app = AppState::new();
                    app.git.ui.split_diff = split;
                    app.workspace.open_diff(sample_overview(0));
                    app
                }));
                *output.lock().unwrap() = Some(state.clone());
                let store = cx.state_with(GitStoreSnapshot::default);
                let id = cx.use_stable_id();
                let focus = cx.focus_handle(id.clone());
                let pane = state.get().workspace.active_pane();
                super::render(
                    pane,
                    viewport,
                    state.clone(),
                    crate::app::git_view::binding(&state, &store),
                    id,
                    focus,
                    crate::app::editor_view::binding(state.clone(), store.clone(), pane, viewport),
                )
            });
            let context = ApplicationContext::empty(Default::default());
            let _text = lgui::backend::install_text_environment(
                &context,
                lgui::render_skia::skia_text_system_handle(),
            );
            let mut session = UiSession::new();
            session.render_view(&view, viewport, UiScale::ONE);
            let state = exposed.lock().unwrap().clone().unwrap();
            assert!(!state.get().editor.focused, "nothing has focus yet");
            let find = |session: &UiSession, label: &str| {
                session
                    .tree()
                    .nodes()
                    .iter()
                    .find(|node| node.text.as_deref() == Some(label))
                    .unwrap_or_else(|| panic!("missing {label}"))
                    .layout_rect
            };
            let char_w = fonts::editor().char_w;
            let start = find(&session, "    open_changes(repository, file);");
            let end = find(&session, "    show_all_changes();");
            let down = Point::new(
                start.left + 3.0 * char_w + 1.0,
                (start.top + start.bottom) / 2.0,
            );
            let drag = Point::new(end.left + 10.0 * char_w + 1.0, (end.top + end.bottom) / 2.0);
            for input in [
                InputEvent::PointerDown {
                    pointer: PointerData::mouse(down),
                    button: PointerButton::Left,
                },
                InputEvent::PointerMove(PointerData::mouse(drag)),
                InputEvent::PointerUp {
                    pointer: PointerData::mouse(drag),
                    button: PointerButton::Left,
                },
            ] {
                let events = session.handle_input(input);
                lgui::core::dispatch_runtime_output(
                    events,
                    &context,
                    &lgui::window::WindowId::new("changes-select-test"),
                    |action| session.handle_default_action(action),
                    |_| {},
                );
                session.render_view(&view, viewport, UiScale::ONE);
            }
            let selection = state.get().editor.diff_selection.expect("a selection");
            assert!(!selection.dragging && !selection.is_empty());

            let clipboard = Arc::new(TestClipboard::default());
            assert!(key_action(&state, Action::Copy, clipboard.clone()));
            let copied = clipboard.0.lock().unwrap().clone().unwrap();
            let expected = if split {
                " open_changes(repository, file);\n}\nfn on_click() {\n    show_a"
            } else {
                " open_changes(repository, file);\n}\nfn on_click() {\n    show_file_diff();\n    show_a"
            };
            assert_eq!(copied, expected, "split={split}");

            assert!(key_action(&state, Action::Cancel, clipboard.clone()));
            assert!(state.get().editor.diff_selection.is_none());
            assert!(!key_action(&state, Action::Copy, clipboard.clone()));
        }
    }
}
