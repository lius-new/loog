//! Tab commands and cross-pane transactions; UI retains geometry and painting.
use crate::app::context::AppContext;
use crate::app::state::{AppState, MainSurface, TabContextMenuState, TabDragState, TabDrop};
use crate::domain::identity::{PaneId, ViewId};
#[cfg(test)]
use crate::domain::pane_layout::Direction;
use crate::ui::chrome::tabs_binding::{Binding, Command, EditorFrame, Frame, Pending, Snapshot};
use crate::ui::tab_layout;
use crate::ui::tab_layout::TabLayoutResult;
use crate::ui::tabs::*;
#[cfg(test)]
use crate::ui::theme;
use lgui::core::{Point, Signal, UiEventKind, UiEventPayload, UiFocusHandle};
use lgui::prelude::{Element, UiRect};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
pub fn frame(app: &AppState) -> Frame<'_> {
    Frame {
        workspace: &app.workspace.core,
        editor: EditorFrame {
            tabs: &app.editor.tabs,
            pending_file: app.editor.pending_file.as_ref().map(|p| Pending {
                pane: p.pane,
                path: p.path.clone(),
                indicator: p.indicator,
                generation: p.generation,
            }),
        },
        welcome_panes: app
            .workspace
            .panes
            .keys()
            .copied()
            .filter(|pane| app.pane_surface(*pane) == MainSurface::Welcome)
            .collect(),
    }
}
pub fn binding(state: &AppContext) -> Binding {
    let reader = state.clone();
    let writer = state.clone();
    Binding::new(
        move || {
            let app = reader.get();
            let welcome_panes = frame(&app).welcome_panes;
            Snapshot {
                core: app.workspace.core,
                tabs: app.editor.tabs,
                pending: app.editor.pending_file.map(|p| Pending {
                    pane: p.pane,
                    path: p.path,
                    indicator: p.indicator,
                    generation: p.generation,
                }),
                welcome_panes,
            }
        },
        move |command| run(&writer, command),
    )
}
pub fn run(state: &AppContext, command: Command) {
    if let Command::Close(pane, id) = command {
        crate::app::workspace_commands::request_close_tab(state, pane, id);
        return;
    }
    state.update(|app| match command {
        Command::Wheel(pane, step, max) => {
            let next = (tab_scroll_x(app, pane) - step).clamp(0.0, max);
            set_tab_scroll_x(app, pane, next);
        }
        Command::Press(pane, id, index, point, grab) => {
            if !app
                .workspace
                .pane(pane)
                .is_some_and(|pane| pane.items().contains(&id))
            {
                return;
            }
            crate::app::file_loading::cancel(app);
            let clicks = app.editor.tab_click_count(id, point.x, point.y);
            app.workspace.activate(pane, id);
            // The shared surface can retain native focus across this switch,
            // so its new view will not receive another focus event.
            app.editor.focused = true;
            app.editor.tabs.tab_context_menu = None;
            if clicks > 1 {
                app.workspace.promote_preview(pane, id);
                app.editor.tabs.tab_drag = None;
            } else {
                app.editor.tabs.tab_drag = Some(TabDragState::new(pane, id, index, point, grab));
            }
        }
        Command::Menu(pane, id, position) => {
            if !app
                .workspace
                .pane(pane)
                .is_some_and(|pane| pane.items().contains(&id))
            {
                return;
            }
            app.editor.tabs.tab_drag = None;
            app.editor.menu = None;
            app.explorer.ui.menu = None;
            app.explorer.ui.menu_target = None;
            app.explorer.ui.menu_hover = None;
            app.editor.tabs.tab_context_menu = Some(TabContextMenuState {
                position,
                pane,
                target: id,
                hovered: None,
            });
        }
        Command::CancelPending(generation) => {
            if app
                .editor
                .pending_file
                .as_ref()
                .is_some_and(|p| p.generation == generation)
            {
                crate::app::file_loading::cancel(app);
            }
        }
        Command::Close(..) => {}
    });
}

fn pane_items(app: &AppState, pane: PaneId) -> &[ViewId] {
    app.workspace
        .pane(pane)
        .map(|pane| pane.items())
        .unwrap_or(&[])
}

fn pane_active(app: &AppState, pane: PaneId) -> Option<ViewId> {
    crate::ui::tabs::pane_active(&frame(app), pane)
}

fn natural_tab_widths(app: &AppState, pane: PaneId) -> Vec<f32> {
    crate::ui::tabs::natural_tab_widths(&frame(app), pane)
}

fn tab_layout(rect: UiRect, app: &AppState, pane: PaneId, reveal_active: bool) -> TabLayoutResult {
    crate::ui::tabs::tab_layout(rect, &frame(app), pane, reveal_active)
}

pub fn tab_scroll_x(app: &AppState, pane: PaneId) -> f32 {
    crate::ui::tabs::tab_scroll_x(&frame(app), pane)
}

pub fn active_visible_scroll(rect: UiRect, app: &AppState, pane: PaneId) -> f32 {
    crate::ui::tabs::active_visible_scroll(rect, &frame(app), pane)
}

/// Where a file dragged from the Explorer would open: a tab strip inserts it,
/// a pane surface opens it there or splits that pane near an edge.
pub fn file_drop_target(
    app: &AppState,
    regions: &[PaneRegion],
    pointer_x: f32,
    pointer_y: f32,
) -> Option<TabDrop> {
    crate::ui::tabs::file_drop_target(&frame(app), regions, pointer_x, pointer_y)
}

pub fn render_tooltip(
    viewport: UiRect,
    strips: &[(PaneId, UiRect)],
    app: &AppState,
    allowed: bool,
) -> Element {
    crate::ui::tabs::render_tooltip(viewport, strips, &frame(app), allowed)
}

pub(crate) fn render_dragged_tab(app: &AppState, pointer_x: Signal<f32>) -> Option<Element> {
    crate::ui::tabs::render_dragged_tab(&frame(app), pointer_x)
}
fn set_tab_scroll_x(app: &mut AppState, pane: PaneId, value: f32) -> bool {
    if (tab_scroll_x(app, pane) - value).abs() <= f32::EPSILON {
        return false;
    }
    app.editor.tabs.tab_scroll.insert(pane, value);
    true
}

pub(crate) fn track_pointer(
    root: Element,
    state: &AppContext,
    regions: &[PaneRegion],
    motion: Signal<f32>,
) -> Element {
    let regions: Arc<[PaneRegion]> = regions.into();
    let hover_regions = regions.clone();
    let hover = state.model();
    let drag = state.model();
    let release = state.clone();
    // The pointer route owns its measured widths. Scrolling only changes the
    // offset, and a new route is installed when the application rebuilds.
    let layouts = Mutex::new(DragLayouts::new());
    // Transient tab input does not need document service or extension sync.
    // The release still uses the full application transaction.
    root.on_event_capture(UiEventKind::PointerMove, move |_, payload| {
        if let UiEventPayload::PointerMove { pointer } = payload {
            hover.try_update(|app| {
                update_hover(app, &hover_regions, pointer.point.x, pointer.point.y)
            });
        }
    })
    .on_event_capture(UiEventKind::PointerDrag, move |_, payload| {
        if let UiEventPayload::PointerDrag { pointer } = payload {
            drag.try_update(|app| {
                update_drag(
                    app,
                    &regions,
                    &mut layouts.lock().expect("tab drag layout cache"),
                    pointer.point,
                    Some(&motion),
                )
            });
        }
    })
    .on_event_capture(UiEventKind::PointerUp, move |_, _| {
        release.try_update(finish_pointer_interaction);
    })
}

fn update_hover(
    app: &mut AppState,
    regions: &[PaneRegion],
    pointer_x: f32,
    pointer_y: f32,
) -> bool {
    if app
        .editor
        .tabs
        .tab_drag
        .as_ref()
        .is_some_and(|drag| drag.active)
    {
        return false;
    }

    let hovered = regions
        .iter()
        .find(|region| point_inside(region.strip, pointer_x, pointer_y))
        .and_then(|region| {
            let (pane, rect) = (region.pane, region.strip);
            let layout = tab_layout(rect, app, pane, false);
            if pointer_x > rect.left + layout.viewport_width {
                return None;
            }
            let content_x = pointer_x - rect.left + layout.scroll_x;
            layout
                .items
                .iter()
                .find(|item| content_x >= item.left && content_x <= item.right)
                .and_then(|item| pane_items(app, pane).get(item.index).copied())
                .map(|id| (pane, id))
        });
    if app.editor.tabs.tab_hovered != hovered {
        app.editor.tabs.tab_hovered = hovered;
        return true;
    }
    false
}

type DragLayouts = HashMap<PaneId, TabLayoutResult>;

/// Pointer-only updates move the composited tab through its signal. The app
/// model changes at activation, on a new drop target, or when the strip scrolls.
fn update_drag(
    app: &mut AppState,
    regions: &[PaneRegion],
    layouts: &mut DragLayouts,
    pointer: Point,
    motion: Option<&Signal<f32>>,
) -> bool {
    let Some(mut drag) = app.editor.tabs.tab_drag.take() else {
        return false;
    };
    let mut changed = false;
    if !drag.active
        && (pointer.x - drag.origin.x).hypot(pointer.y - drag.origin.y) >= DRAG_THRESHOLD
        && let Some(region) = regions.iter().find(|region| region.pane == drag.pane)
    {
        layouts.clear();
        let layout = layouts
            .entry(drag.pane)
            .or_insert_with(|| tab_layout(region.strip, app, drag.pane, false));
        if let Some(item) = layout.items.get(drag.source_index) {
            drag.preview_width = item.width.min(layout.viewport_width);
            drag.active = true;
            app.editor.tabs.tab_hovered = None;
            changed = true;
        }
    }
    if drag.active {
        let drop = drag_drop_target(app, regions, layouts, &mut drag, pointer, &mut changed);
        if drag.drop != drop {
            drag.drop = drop;
            changed = true;
        }
        if drag.floating
            && let Some(motion) = motion
        {
            motion.set(pointer.x);
        }
    }
    app.editor.tabs.tab_drag = Some(drag);
    changed
}

/// Resolve where an active tab drag would land, auto-scrolling the strip
/// under the pointer near its edges.
fn drag_drop_target(
    app: &mut AppState,
    regions: &[PaneRegion],
    layouts: &mut DragLayouts,
    drag: &mut TabDragState,
    pointer: Point,
    changed: &mut bool,
) -> Option<TabDrop> {
    let (pointer_x, pointer_y) = (pointer.x, pointer.y);
    let hit_strip = regions.iter().find(|region| region.strip.contains(pointer));
    let floating = hit_strip.is_some_and(|region| region.pane == drag.pane);
    if drag.floating != floating {
        drag.floating = floating;
        *changed = true;
    }

    // The existing drop tolerance still applies just outside the source
    // strip. Only the feedback switches at the strip's actual boundary.
    let target_strip = hit_strip.or_else(|| {
        regions.iter().find(|region| {
            region.pane == drag.pane
                && UiRect::new(
                    region.strip.left,
                    region.strip.top - DRAG_REORDER_SLACK,
                    region.strip.right,
                    region.strip.bottom + DRAG_REORDER_SLACK,
                )
                .contains(pointer)
        })
    });
    if let Some(region) = target_strip {
        let (pane, rect) = (region.pane, region.strip);
        let layout = layouts
            .entry(pane)
            .or_insert_with(|| tab_layout(rect, app, pane, false));
        let viewport_right = rect.left + layout.viewport_width;
        let scroll = tab_scroll_x(app, pane).clamp(0.0, layout.max_scroll_x);
        let next = if pointer_x <= rect.left + DRAG_EDGE_W {
            (scroll - DRAG_SCROLL_STEP).max(0.0)
        } else if pointer_x >= viewport_right - DRAG_EDGE_W {
            (scroll + DRAG_SCROLL_STEP).min(layout.max_scroll_x)
        } else {
            scroll
        };
        if set_tab_scroll_x(app, pane, next) {
            *changed = true;
        }
        let content_x = pointer_x - rect.left + next;
        if pane == drag.pane {
            let tabs = pane_items(app, pane);
            let count = tabs.len().min(layout.items.len());
            if tabs.get(drag.source_index) != Some(&drag.source) {
                drag.source_index = tabs.iter().position(|id| *id == drag.source)?;
            }
            let slot_at =
                |x| tab_layout::reorder_slot(&layout.items[..count], drag.source_index, x);
            let target = if drag.floating {
                let center_x = content_x + (0.5 - drag.grab.x) * drag.preview_width;
                crate::ui::components::tab_motion::reorder_slot(
                    drag.target_index,
                    center_x,
                    slot_at,
                )
            } else {
                slot_at(content_x)
            };
            if let Some(target_index) = target.filter(|index| *index != drag.target_index) {
                drag.target_index = target_index;
                *changed = true;
            }
            return None;
        }
        return Some(TabDrop::Insert {
            pane,
            index: insertion_index(layout, content_x),
        });
    }
    regions
        .iter()
        .find(|region| point_inside(region.body, pointer_x, pointer_y))
        .map(|region| TabDrop::Pane {
            pane: region.pane,
            split: drop_zone(region.body, pointer_x, pointer_y),
        })
}

/// Advance a pressed Explorer file into a drag once the pointer has moved
/// far enough, tracking its drop target.
pub fn update_file_drag(
    app: &mut AppState,
    regions: &[PaneRegion],
    pointer_x: f32,
    pointer_y: f32,
) -> bool {
    if let Some(press) = app.explorer.ui.pressed_resource.as_mut() {
        let dragged = press.dragged
            || (pointer_x - press.point.0).hypot(pointer_y - press.point.1) >= DRAG_THRESHOLD;
        let changed = dragged != press.dragged;
        press.dragged = dragged;
        return changed;
    }
    let Some(drag) = app.editor.tabs.file_drag.as_ref() else {
        return false;
    };
    let moved = (pointer_x - drag.origin.0).hypot(pointer_y - drag.origin.1);
    let active = drag.active || moved >= DRAG_THRESHOLD;
    let drop = if active {
        file_drop_target(app, regions, pointer_x, pointer_y)
    } else {
        None
    };
    let drag = app.editor.tabs.file_drag.as_mut().expect("checked above");
    let changed = drag.active != active
        || drag.drop != drop
        || (active && drag.point != (pointer_x, pointer_y));
    drag.active = active;
    drag.drop = drop;
    drag.point = (pointer_x, pointer_y);
    changed
}

/// Commit a completed tab drag and release all tab pointer state. Holding
/// Ctrl (Alt on macOS) copies a file into the target pane instead of moving.
pub fn finish_pointer_interaction(app: &mut AppState) -> bool {
    let Some(drag) = app.editor.tabs.tab_drag.take() else {
        return false;
    };
    if !drag.active {
        return true;
    }
    let copy = if cfg!(target_os = "macos") {
        app.editor.modifiers.alt()
    } else {
        app.editor.modifiers.ctrl()
    };
    let (from, id) = (drag.pane, drag.source);
    let workspace = &mut app.workspace;
    match drag.drop {
        None => {
            workspace.move_tab(from, id, drag.target_index);
        }
        Some(TabDrop::Insert { pane, index }) => {
            if copy {
                workspace.copy_item(from, id, pane, index);
            } else {
                workspace.move_item(from, id, pane, index);
            }
        }
        Some(TabDrop::Pane { pane, split: None }) => {
            if pane != from {
                if copy {
                    workspace.copy_item(from, id, pane, usize::MAX);
                } else {
                    workspace.move_item(from, id, pane, usize::MAX);
                }
            }
        }
        Some(TabDrop::Pane {
            pane,
            split: Some(direction),
        }) => {
            workspace.split_with_item(from, id, pane, direction, copy);
        }
    }
    true
}

/// Cancel transient pointer state without applying a pending reorder.
pub fn cancel_pointer_interaction(app: &mut AppState) -> bool {
    let changed = app.editor.tabs.tab_drag.is_some()
        || app.editor.tabs.file_drag.is_some()
        || app.editor.tabs.tab_hovered.is_some();
    app.editor.tabs.tab_drag = None;
    app.editor.tabs.file_drag = None;
    app.editor.tabs.tab_hovered = None;
    changed
}
pub fn render(
    pane: PaneId,
    rect: UiRect,
    state: AppContext,
    editor_focus: UiFocusHandle,
) -> Element {
    crate::ui::tabs::render(pane, rect, binding(&state), editor_focus)
}
#[cfg(test)]
mod tests {
    use super::*;

    fn region(pane: PaneId, strip: UiRect) -> PaneRegion {
        PaneRegion {
            pane,
            strip,
            body: UiRect::new(strip.left, strip.bottom, strip.right, strip.bottom + 400.0),
        }
    }
    use lgui::text::TextFontSlant;
    use std::path::PathBuf;

    fn update_pointer(
        app: &mut AppState,
        regions: &[PaneRegion],
        x: f32,
        y: f32,
        dragging: bool,
    ) -> bool {
        if dragging {
            update_drag(
                app,
                regions,
                &mut DragLayouts::new(),
                Point::new(x, y),
                None,
            )
        } else {
            update_hover(app, regions, x, y)
        }
    }

    #[test]
    fn tab_and_drawer_dividers_stay_crisp_at_fractional_scales() {
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{UiId, UiScale};
        use lgui::prelude::{VisualStyle, group, panel};
        use lgui::renderer::{FrameInfo, FrameReason};
        use lgui::session::UiSession;
        use std::sync::Arc;

        let viewport = UiRect::new(0.0, 0.0, 760.0, 100.0);
        let title = UiRect::new(0.0, 0.0, viewport.right, theme::TITLEBAR_H);
        let strip = UiRect::new(0.0, title.bottom, 400.0, title.bottom + theme::TABS_H);
        let drawer = UiRect::new(440.0, title.bottom, 760.0, strip.bottom);
        let details = UiRect::new(
            drawer.left,
            drawer.bottom + 20.0,
            drawer.right,
            viewport.bottom,
        );
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.drawer.visible = true;
                for index in 0..20 {
                    app.workspace
                        .test_open_view(PathBuf::from(format!("file{index}.rs")), String::new());
                }
                let first = app.workspace.active_items()[0];
                app.workspace.set_active(first);
                let pane = app.workspace.active_pane();
                app.editor.tabs.tab_scroll.insert(pane, 10.0);
                app
            }));
            let pane = state.get().workspace.active_pane();
            panel(viewport, VisualStyle::filled(theme::c().bg))
                .child(panel(
                    UiRect::new(strip.left, strip.bottom, strip.right, viewport.bottom),
                    VisualStyle::filled(theme::c().bg),
                ))
                .child(crate::ui::titlebar::render(
                    title,
                    crate::app::chrome::title_binding(&state),
                    group(title),
                ))
                .child(render(
                    pane,
                    strip,
                    state,
                    cx.focus_handle(UiId::new("tab-border-test")),
                ))
                .child(
                    panel(
                        UiRect::new(drawer.left, drawer.top, drawer.right, viewport.bottom),
                        VisualStyle::filled(theme::c().sidebar),
                    )
                    .child(crate::ui::drawer::header(
                        drawer,
                        crate::ui::drawer::DrawerMode::Files,
                        Arc::new(|_| {}),
                    )),
                )
                .child(crate::ui::components::divider::top(details))
                .child(crate::ui::titlebar::bottom_border(title))
        });
        let context = ApplicationContext::empty(Default::default());
        context
            .resources()
            .provide(lgui::icons::IconRegistration(Arc::new(
                crate::app::svg_icons(),
            )));
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        for factor in [0.75, 0.8, 0.85, 0.875, 0.9, 0.96, 1.0, 1.25, 1.5, 2.0] {
            let scale = UiScale::new(factor);
            let mut session = UiSession::new();
            session.render_view(&view, viewport, scale);
            let physical = scale.physical_rect(viewport);
            let frame = FrameInfo::new(physical, &[], scale, FrameReason::SceneChange, true);
            let mut surface = lgui::render_skia::SkiaSoftwareSurface::new(4 * 1024 * 1024);
            lgui::assets::backend::with_render_resources(
                &context,
                lgui::assets::backend::render_resources(),
                || surface.draw(&session.tree().scene().project_to_physical(scale), &frame),
            )
            .unwrap();
            let pixel = |x: i32, y: i32| {
                let offset = (y as usize * physical.width() as usize + x as usize) * 4;
                &surface.pixels()[offset..offset + 4]
            };
            for x in [200.0, 396.0, 600.0] {
                let color = theme::c().border.0;
                let expected = &[
                    (color & 255) as u8,
                    ((color >> 8) & 255) as u8,
                    ((color >> 16) & 255) as u8,
                    255,
                ];
                for row in 1..=scale.physical_length(1.0) {
                    assert_eq!(
                        pixel(
                            scale.physical_value(x),
                            scale.physical_value(strip.bottom) - row
                        ),
                        expected,
                        "scale={factor}, x={x}, row={row}",
                    );
                    assert_eq!(
                        pixel(
                            scale.physical_value(x),
                            scale.physical_value(title.bottom) - row
                        ),
                        expected,
                        "title bar divider at scale={factor}, x={x}, row={row}",
                    );
                }
            }
            let accent = theme::c().accent.0;
            let border = theme::c().border.0;
            for row in 0..scale.physical_length(1.0) {
                assert_eq!(
                    pixel(
                        scale.physical_value(600.0),
                        scale.physical_value(details.top) + row
                    ),
                    &[
                        (border & 255) as u8,
                        ((border >> 8) & 255) as u8,
                        ((border >> 16) & 255) as u8,
                        255
                    ],
                    "top divider must stay crisp at scale={factor}",
                );
            }
            assert_eq!(
                pixel(
                    scale.physical_value(20.0),
                    scale.physical_ui_value(strip.top).ceil() as i32
                ),
                &[
                    (accent & 255) as u8,
                    ((accent >> 8) & 255) as u8,
                    ((accent >> 16) & 255) as u8,
                    255
                ],
                "active tab accent line must remain visible at scale={factor}",
            );
        }
    }

    #[test]
    fn preview_tab_names_are_italic() {
        assert_eq!(tab_name_style(true, true).font_slant, TextFontSlant::Italic);
        assert_eq!(
            tab_name_style(true, false).font_slant,
            TextFontSlant::Upright
        );
    }

    #[test]
    fn long_file_names_preserve_the_extension_and_fit_the_budget() {
        let budget = measure("very-long….rs", theme::UI_SIZE, 400);
        let result = ellipsize(
            "very-long-file-name-that-keeps-going.rs",
            budget,
            theme::UI_SIZE,
            400,
        );

        assert!(result.contains('…'));
        assert!(result.ends_with(".rs"));
        assert!(measure(&result, theme::UI_SIZE, 400) <= budget);
    }

    #[test]
    fn short_file_names_remain_unchanged() {
        assert_eq!(ellipsize("main.rs", 200.0, theme::UI_SIZE, 400), "main.rs");
    }

    #[test]
    fn editor_tab_tooltip_waits_for_hover_and_closes_on_pointer_leave() {
        use lgui::application::AppView;
        use lgui::core::{InputEvent, Point, PointerData, UiId, UiScale};
        use lgui::prelude::group;
        use lgui::session::UiSession;
        use std::sync::Arc;
        use std::time::Duration;

        let viewport = UiRect::new(0.0, 0.0, 800.0, 100.0);
        let strip = UiRect::new(0.0, 0.0, 800.0, theme::TABS_H);
        let pointer = PointerData::mouse(Point::new(40.0, strip.bottom / 2.0));
        let path = PathBuf::from("nested").join("hovered.rs");
        let label = crate::ui::display_path(&path);
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.workspace.test_open_view(path.clone(), String::new());
                let pane = app.workspace.active_pane();
                update_pointer(
                    &mut app,
                    &[region(pane, strip)],
                    pointer.point.x,
                    pointer.point.y,
                    false,
                );
                app
            }));
            let app = state.get();
            let pane = app.workspace.active_pane();
            group(viewport)
                .child(render(
                    pane,
                    strip,
                    state,
                    cx.focus_handle(UiId::new("editor-tab-tooltip-test")),
                ))
                .child(render_tooltip(viewport, &[(pane, strip)], &app, true))
        });
        let mut session = UiSession::new();
        let render = |session: &mut UiSession| {
            session.render_view(&view, viewport, UiScale::ONE);
        };
        let visible = |session: &UiSession| {
            session
                .tree()
                .nodes()
                .iter()
                .any(|node| node.text.as_deref() == Some(label.as_str()))
        };
        render(&mut session);
        session.handle_input(InputEvent::PointerMove(pointer));
        render(&mut session);
        assert!(!visible(&session));

        std::thread::sleep(Duration::from_millis(510));
        assert!(session.advance(510.0).animation_changed);
        render(&mut session);
        assert!(visible(&session));

        session.handle_input(InputEvent::PointerLeave(pointer));
        render(&mut session);
        assert!(!visible(&session));
        session.handle_input(InputEvent::PointerMove(pointer));
        render(&mut session);
        assert!(!visible(&session));
    }

    #[test]
    fn overflowing_tabs_scroll_and_accept_clicks_at_the_bottom_edge() {
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::dispatch_runtime_output;
        use lgui::core::{
            InputEvent, Point, PointerButton, PointerData, UiId, UiScale, WheelDelta,
        };
        use lgui::session::UiSession;
        use std::sync::{Arc, Mutex};

        let strip = UiRect::new(0.0, 0.0, 360.0, theme::TABS_H);
        let y = strip.bottom - 4.0;
        let exposed = Arc::new(Mutex::new(None));
        let output = exposed.clone();
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                for index in 0..8 {
                    app.workspace
                        .test_open_view(PathBuf::from(format!("file{index}.rs")), String::new());
                }
                let first = app.workspace.active_items()[0];
                app.workspace.set_active(first);
                let pane = app.workspace.active_pane();
                update_pointer(&mut app, &[region(pane, strip)], 60.0, y, false);
                app
            }));
            *output.lock().unwrap() = Some(state.clone());
            let pane = state.get().workspace.active_pane();
            render(
                pane,
                strip,
                state,
                cx.focus_handle(UiId::new("overflowing-editor-tabs")),
            )
        });
        let mut session = UiSession::new();
        session.render_view(&view, strip, UiScale::ONE);
        let state = exposed.lock().unwrap().clone().unwrap();
        let context = ApplicationContext::empty(Default::default());
        let mut send = |input| {
            dispatch_runtime_output(
                session.handle_input(input),
                &context,
                &lgui::window::WindowId::new("overflowing-editor-tabs"),
                |action| session.handle_default_action(action),
                |_| {},
            );
            session.render_view(&view, strip, UiScale::ONE);
        };
        let pane = state.get().workspace.active_pane();
        send(InputEvent::Wheel {
            point: Point::new(60.0, y),
            delta: WheelDelta::lines(0.0, -100.0),
        });
        let app = state.get();
        let layout = tab_layout(strip, &app, pane, false);
        assert!(layout.scroll_x > 0.0);
        assert_eq!(layout.scroll_x, layout.max_scroll_x);
        let last = *app.workspace.active_items().last().unwrap();
        let point = Point::new(
            layout.items.last().unwrap().left - layout.scroll_x + 20.0,
            y,
        );
        send(InputEvent::PointerDown {
            pointer: PointerData::mouse(point),
            button: PointerButton::Right,
        });
        assert_eq!(
            state.get().editor.tabs.tab_context_menu.unwrap().target,
            last
        );
        send(InputEvent::PointerDown {
            pointer: PointerData::mouse(point),
            button: PointerButton::Left,
        });
        assert_eq!(state.get().workspace.active(), Some(last));
    }

    #[test]
    fn tab_reorder_waits_for_the_drag_threshold_and_commits_on_release() {
        let mut app = AppState::new();
        let first = app
            .workspace
            .test_open_view(PathBuf::from("first.rs"), String::new());
        let second = app
            .workspace
            .test_open_view(PathBuf::from("second.rs"), String::new());
        let third = app
            .workspace
            .test_open_view(PathBuf::from("third.rs"), String::new());
        app.workspace.set_active(first);
        let rect = UiRect::new(0.0, 0.0, 700.0, theme::TABS_H);
        let pane = app.workspace.active_pane();
        drag_from(&mut app, pane, first, 20.0, 10.0);

        assert!(!update_pointer(
            &mut app,
            &[region(pane, rect)],
            22.0,
            10.0,
            true
        ));
        assert!(!app.editor.tabs.tab_drag.as_ref().unwrap().active);
        assert!(finish_pointer_interaction(&mut app));
        assert_eq!(app.workspace.active_items(), &[first, second, third]);

        drag_from(&mut app, pane, first, 20.0, 10.0);
        assert!(update_pointer(
            &mut app,
            &[region(pane, rect)],
            650.0,
            10.0,
            true
        ));
        assert!(app.editor.tabs.tab_drag.as_ref().unwrap().active);
        assert!(finish_pointer_interaction(&mut app));
        assert_eq!(app.workspace.active_items(), &[second, third, first]);
    }

    /// Manual probe for Phase 6: `cargo test --release -- --ignored --nocapture
    /// pointer_updates_with_many_tabs`.
    #[test]
    #[ignore]
    fn pointer_updates_with_many_tabs() {
        for count in [100, 500] {
            let mut app = AppState::new();
            for index in 0..count {
                app.workspace.test_open_view(
                    PathBuf::from(format!("dir{}/file-{index}.rs", index % 7)),
                    String::new(),
                );
            }
            let rect = UiRect::new(0.0, 0.0, 1200.0, theme::TABS_H);
            let pane = app.workspace.active_pane();
            let started = std::time::Instant::now();
            for step in 0..200 {
                update_pointer(
                    &mut app,
                    &[region(pane, rect)],
                    (step * 5) as f32,
                    10.0,
                    false,
                );
            }
            println!(
                "{count} tabs: {:?} per pointer update",
                started.elapsed() / 200
            );
        }
    }

    #[test]
    fn ordinary_hover_never_promotes_a_pending_click_to_tab_drag() {
        let mut app = AppState::new();
        let first = app
            .workspace
            .test_open_view(PathBuf::from("first.rs"), String::new());
        let second = app
            .workspace
            .test_open_view(PathBuf::from("second.rs"), String::new());
        app.workspace.set_active(first);
        let rect = UiRect::new(0.0, 0.0, 700.0, theme::TABS_H);
        let pane = app.workspace.active_pane();
        drag_from(&mut app, pane, first, 20.0, 10.0);
        let layout = tab_layout(rect, &app, pane, false);
        let second_tab = layout.items[1];
        let hover_x = rect.left + (second_tab.left + second_tab.right) / 2.0 - layout.scroll_x;

        assert!(update_pointer(
            &mut app,
            &[region(pane, rect)],
            hover_x,
            10.0,
            false
        ));
        assert_eq!(app.editor.tabs.tab_hovered, Some((pane, second)));
        assert!(!app.editor.tabs.tab_drag.as_ref().unwrap().active);
        assert!(finish_pointer_interaction(&mut app));
        assert_eq!(app.workspace.active_items(), &[first, second]);
    }

    #[test]
    fn drop_zones_merge_in_the_centre_and_split_near_edges() {
        let body = UiRect::new(0.0, 0.0, 900.0, 600.0);
        assert_eq!(drop_zone(body, 450.0, 300.0), None);
        assert_eq!(drop_zone(body, 20.0, 300.0), Some(Direction::Left));
        assert_eq!(drop_zone(body, 880.0, 300.0), Some(Direction::Right));
        assert_eq!(drop_zone(body, 450.0, 20.0), Some(Direction::Up));
        assert_eq!(drop_zone(body, 450.0, 590.0), Some(Direction::Down));
        assert_eq!(
            drop_preview(body, Some(Direction::Right)),
            UiRect::new(450.0, 0.0, 900.0, 600.0)
        );
    }

    fn drag_from(app: &mut AppState, pane: PaneId, source: ViewId, x: f32, y: f32) {
        let index = pane_items(app, pane)
            .iter()
            .position(|id| *id == source)
            .unwrap();
        app.editor.tabs.tab_drag = Some(TabDragState::new(
            pane,
            source,
            index,
            Point::new(x, y),
            Point::new(0.5, y),
        ));
    }

    #[test]
    fn editor_tabs_show_the_pointer_cursor() {
        use lgui::application::AppView;
        use lgui::core::{CursorIcon, Point, UiId, UiScale};
        use lgui::session::UiSession;
        use std::sync::Arc;
        let viewport = UiRect::new(0.0, 0.0, 760.0, 100.0);
        let strip = UiRect::new(0.0, 30.0, 700.0, 30.0 + theme::TABS_H);
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                for index in 0..3 {
                    app.workspace
                        .test_open_view(PathBuf::from(format!("file{index}.rs")), String::new());
                }
                app
            }));
            let pane = state.get().workspace.active_pane();
            lgui::prelude::panel(viewport, lgui::prelude::VisualStyle::default()).child(render(
                pane,
                strip,
                state,
                cx.focus_handle(UiId::new("probe")),
            ))
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        // Active and inactive tabs show the same hand as the terminal tabs,
        // including along the active tab's accent line.
        for (x, y) in [
            (20.0, strip.top + 10.0),
            (60.0, strip.top + 10.0),
            (200.0, strip.top + 10.0),
            (60.0, strip.top + 1.0),
        ] {
            assert_eq!(
                session.tree().cursor_at(Point::new(x, y)),
                Some(CursorIcon::Pointer),
                "({x}, {y})"
            );
        }
        // Empty strip space keeps the default cursor.
        assert_eq!(
            session
                .tree()
                .cursor_at(Point::new(650.0, strip.top + 10.0)),
            None
        );
    }

    #[test]
    fn dragged_tab_moves_in_a_retained_layer_and_neighbors_slide_before_release() {
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{
            CursorIcon, InputEvent, PointerButton, PointerData, UiId, UiScale,
            dispatch_runtime_output,
        };
        use lgui::prelude::{VisualStyle, component, group, panel};
        use lgui::renderer::{FrameInfo, FrameReason};
        use lgui::session::UiSession;
        use loog_editor_core::text::EditKind;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        let viewport = UiRect::new(0.0, 0.0, 900.0, 600.0);
        let strip = UiRect::new(0.0, 50.3, 900.0, 50.3 + theme::TABS_H);
        let scale = UiScale::new(1.25);
        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let renders = Arc::new(AtomicUsize::new(0));
        let exposed_state = Arc::new(Mutex::new(None));
        let capture = exposed_state.clone();
        let counter = renders.clone();
        let view: AppView = Arc::new(move |cx| {
            let (capture, counter) = (capture.clone(), counter.clone());
            component(cx.viewport(), move |cx, _| {
                counter.fetch_add(1, Ordering::Relaxed);
                let state = AppContext::detached(cx.state_with(|| {
                    let mut app = AppState::new();
                    for name in [
                        "a-filename-long-enough-to-test-the-selected-tab-width.rs",
                        "b.rs",
                        "c.rs",
                    ] {
                        app.workspace
                            .test_open_view(PathBuf::from(name), String::new());
                    }
                    let first = app.workspace.active_items()[0];
                    app.workspace.set_active(first);
                    app.workspace
                        .active_editor_mut()
                        .unwrap()
                        .replace_selection("unsaved", EditKind::Typing)
                        .unwrap();
                    app
                }));
                *capture.lock().unwrap() = Some(state.clone());
                let app = state.get();
                let pane = app.workspace.active_pane();
                let motion = cx.signal(0.0_f32);
                let focus = cx.focus_handle(UiId::new("drag-test-editor"));
                let root = crate::app::shell::window_root(viewport).child(component(strip, {
                    let state = state.clone();
                    move |_, strip| render(pane, *strip, state, focus)
                }));
                let root = track_pointer(root, &state, &[region(pane, strip)], motion.clone());
                let mut feedback = group(UiRect::default()).key("drag-feedback");
                if let Some(floating) = render_dragged_tab(&app, motion) {
                    feedback = feedback.child(floating);
                }
                root.child(feedback)
                    .child(panel(viewport, VisualStyle::default()).key("later-overlay"))
            })
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, scale);
        let state = exposed_state.lock().unwrap().clone().unwrap();
        let app = state.get();
        let pane = app.workspace.active_pane();
        let items = app.workspace.active_items().to_vec();
        let layout = tab_layout(strip, &app, pane, false);
        assert!(layout.items[0].width > 220.0);
        let center = |i: usize| (layout.items[i].left + layout.items[i].right) / 2.0;
        let y = strip.top + strip.height() / 2.0;
        let pointer = |x| PointerData::mouse(Point::new(x, y));
        let window = lgui::window::WindowId::new("tab-drag");
        let send = |session: &mut UiSession, input| {
            let output = session.handle_input(input);
            let output = dispatch_runtime_output(
                output,
                &context,
                &window,
                |action| session.handle_default_action(action),
                |_| {},
            );
            if let Some(bounds) = output.dirty_bounds {
                session.invalidations_mut().invalidate_rect(bounds);
            }
            session.render_view(&view, viewport, scale)
        };
        let ghost = UiId::new("editor-dragged-tab");
        let original_id = UiId::owned(format!("editor-tab-{}-{:?}", pane.get(), items[0]));
        let original_key = format!("{}-{:?}", pane.get(), items[0]);
        let original_style = session.tree().node(&original_id).unwrap().style;
        let text_styles = |session: &UiSession, parent: &UiId| {
            session
                .tree()
                .nodes()
                .iter()
                .filter(|node| node.parent.as_ref() == Some(parent))
                .filter_map(|node| Some((node.text.as_ref()?.to_string(), node.text_style)))
                .collect::<Vec<_>>()
        };
        let original_text = text_styles(&session, &original_id);
        assert_eq!(
            original_text.len(),
            2,
            "the title and unsaved marker are visible"
        );
        let icon_style = |session: &UiSession, role: &str, key: &str| {
            let id = UiId::owned(format!("tabs.{role}.{key}"));
            let node = session.tree().node(&id).unwrap();
            (node.icon_key.clone(), node.icon_style)
        };
        let original_badge = icon_style(&session, "badge", &original_key);
        let original_close = icon_style(&session, "close", &original_key);
        let neighbor = session
            .tree()
            .nodes()
            .iter()
            .find(|node| node.text.as_deref() == Some("b.rs"))
            .unwrap();
        let text_rect = scale.physical_rect_outward(neighbor.layout_rect);
        let physical = scale.physical_rect(viewport);
        let label_pixels = |session: &UiSession| {
            let mut surface = lgui::render_skia::SkiaSoftwareSurface::new(4 * 1024 * 1024);
            let dirty = [physical];
            let frame = FrameInfo::new(physical, &dirty, scale, FrameReason::SceneChange, true);
            lgui::assets::backend::with_render_resources(
                &context,
                lgui::RenderResources::new(),
                || surface.draw(&session.tree().scene().project_to_physical(scale), &frame),
            )
            .unwrap();
            let stride = physical.width() as usize * 4;
            let left = text_rect.left as usize * 4;
            let right = text_rect.right as usize * 4;
            (text_rect.top as usize..text_rect.bottom as usize)
                .flat_map(|row| &surface.pixels()[row * stride + left..row * stride + right])
                .copied()
                .collect::<Vec<_>>()
        };
        let stationary_text_pixels = label_pixels(&session);
        assert!(
            stationary_text_pixels
                .chunks_exact(4)
                .any(|pixel| pixel != &stationary_text_pixels[..4]),
            "the reference crop must contain the rendered label"
        );
        send(
            &mut session,
            InputEvent::PointerDown {
                pointer: pointer(center(0)),
                button: PointerButton::Left,
            },
        );
        send(
            &mut session,
            InputEvent::PointerMove(pointer(center(0) + 2.0)),
        );
        assert!(session.tree().node(&ghost).is_none());
        let lifted = send(
            &mut session,
            InputEvent::PointerMove(pointer(center(0) + 12.0)),
        );
        assert!(!lifted.damage.dirty.is_full());
        assert!(
            label_pixels(&session) == stationary_text_pixels,
            "an untouched tab's label changed when the drag started"
        );
        let tree = session.tree();
        let floating = tree.node(&ghost).unwrap();
        let base = tree.node(&floating.children[0]).unwrap();
        let face = tree.node(&base.children[0]).unwrap();
        assert_eq!(floating.layout_rect.width(), layout.items[0].width);
        assert_eq!(base.style, VisualStyle::filled(theme::c().sidebar));
        assert_eq!(face.style, original_style);
        assert_eq!(text_styles(&session, &face.id), original_text);
        assert_eq!(
            icon_style(&session, "badge", ghost.as_str()),
            original_badge
        );
        assert_eq!(
            icon_style(&session, "close", ghost.as_str()),
            original_close
        );
        assert!(
            base.children
                .iter()
                .any(|id| tree.node(id).unwrap().style == VisualStyle::filled(theme::c().accent))
        );
        let float_x = |session: &UiSession| {
            session
                .tree()
                .node(&ghost)
                .unwrap()
                .compositing_layer
                .unwrap()
                .transform
                .translation_x()
        };
        let float_y = |session: &UiSession| {
            let tab = session.tree().node(&ghost).unwrap();
            tab.layout_rect.top + tab.compositing_layer.unwrap().transform.translation_y()
        };
        let first_x = float_x(&session);
        assert!((float_y(&session) - strip.top).abs() < 0.001);
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(Point::new(center(0) + 12.0, y - 4.0))),
        );
        assert_eq!(float_x(&session), first_x);
        assert!((float_y(&session) - strip.top).abs() < 0.001);
        let rendered = renders.load(Ordering::Relaxed);
        let commit = send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(Point::new(center(0) + 22.0, y - 4.0))),
        );
        assert_eq!(float_x(&session), first_x + 10.0);
        assert!((float_y(&session) - strip.top).abs() < 0.001);
        assert_eq!(renders.load(Ordering::Relaxed), rendered);
        assert!(!commit.damage.dirty.is_empty());
        assert!(!commit.damage.dirty.is_full());
        assert!(
            commit
                .damage
                .dirty
                .rects()
                .iter()
                .all(|r| r.top >= strip.top - 20.0
                    && r.bottom <= strip.bottom + 22.0
                    && r.width() <= layout.items[0].width + 24.0),
            "{:?}",
            commit.damage.dirty.rects()
        );

        let swap = send(
            &mut session,
            InputEvent::PointerMove(pointer(center(1) + 8.0)),
        );
        assert!(!swap.damage.dirty.is_full());
        assert_eq!(state.get().workspace.active_items(), &items);
        let second_tab = UiId::owned(format!("editor-tab-{}-{:?}", pane.get(), items[1]));
        let offset = |session: &UiSession| {
            let node = session.tree().node(&second_tab).unwrap();
            session
                .tree()
                .node(node.parent.as_ref().unwrap())
                .unwrap()
                .compositing_layer
                .unwrap()
                .transform
                .translation_x()
        };
        assert_eq!(offset(&session), 0.0);
        std::thread::sleep(Duration::from_millis(130));
        let frame = session.advance(130.0);
        assert!(frame.animation_changed);
        if let Some(bounds) = frame.dirty_bounds {
            session.invalidations_mut().invalidate_rect(bounds);
        }
        let commit = session.render_view(&view, viewport, scale);
        assert!((offset(&session) + layout.items[0].width).abs() < 0.01);
        assert!(!commit.damage.dirty.is_full());

        let outside_strip = PointerData::mouse(Point::new(center(1) + 8.0, strip.bottom + 1.0));
        let left_strip = send(&mut session, InputEvent::PointerMove(outside_strip));
        assert!(
            !left_strip.damage.dirty.is_full(),
            "leaving the strip damaged the entire viewport: {:?}, {:?}",
            left_strip.damage.dirty.fallback_reason(),
            left_strip.damage.reasons,
        );
        assert!(session.tree().node(&ghost).is_none());
        assert_eq!(text_styles(&session, &original_id), original_text);
        assert_eq!(
            session.tree().cursor_at_id(&original_id),
            Some(CursorIcon::Move)
        );
        let marker_style = VisualStyle::filled(theme::c().accent).radius(1.0);
        let marker_visible = |session: &UiSession| {
            session
                .tree()
                .nodes()
                .iter()
                .any(|node| node.style == marker_style)
        };
        assert!(marker_visible(&session));

        let reentered = send(
            &mut session,
            InputEvent::PointerMove(pointer(center(1) + 8.0)),
        );
        assert!(!reentered.damage.dirty.is_full());
        assert!(session.tree().node(&ghost).is_some());
        assert_eq!(
            session.tree().cursor_at_id(&original_id),
            Some(CursorIcon::Pointer)
        );
        assert!(!marker_visible(&session));

        let released = send(
            &mut session,
            InputEvent::PointerUp {
                pointer: pointer(center(1) + 8.0),
                button: PointerButton::Left,
            },
        );
        assert!(!released.damage.dirty.is_full());
        assert!(session.tree().node(&ghost).is_none());
        assert_eq!(
            state.get().workspace.active_items(),
            &[items[1], items[0], items[2]]
        );
    }

    #[test]
    fn reordering_follows_centres_and_tolerates_vertical_drift_in_the_own_strip() {
        let mut app = AppState::new();
        let pane = app.workspace.active_pane();
        let first = app
            .workspace
            .test_open_view(PathBuf::from("first.rs"), String::new());
        let second = app
            .workspace
            .test_open_view(PathBuf::from("second.rs"), String::new());
        let third = app
            .workspace
            .test_open_view(PathBuf::from("third.rs"), String::new());
        let ids = app.workspace.pane(pane).unwrap().items().to_vec();
        assert_eq!(ids, vec![first, second, third]);
        let strip = UiRect::new(0.0, 100.0, 900.0, 100.0 + theme::TABS_H);
        let regions = [region(pane, strip)];
        let layout = tab_layout(strip, &app, pane, false);
        let centre = |i: usize| (layout.items[i].left + layout.items[i].right) / 2.0;
        drag_from(&mut app, pane, first, centre(0), strip.top + 5.0);

        // Inside tab 1's left half: tab 1's centre is not crossed yet.
        let drifting = strip.bottom + 10.0;
        update_pointer(&mut app, &regions, centre(1) - 5.0, drifting, true);
        let drag = app.editor.tabs.tab_drag.as_ref().unwrap();
        assert!(drag.active);
        assert!(!drag.floating);
        assert_eq!(drag.drop, None);
        assert_eq!(drag.target_index, 0);

        update_pointer(&mut app, &regions, centre(1) + 5.0, drifting, true);
        assert_eq!(app.editor.tabs.tab_drag.as_ref().unwrap().target_index, 1);

        update_pointer(
            &mut app,
            &regions,
            centre(1) - 2.0,
            strip.bottom - 1.0,
            true,
        );
        assert!(app.editor.tabs.tab_drag.as_ref().unwrap().floating);
        assert_eq!(app.editor.tabs.tab_drag.as_ref().unwrap().target_index, 1);
        update_pointer(
            &mut app,
            &regions,
            centre(1) - 5.0,
            strip.bottom - 1.0,
            true,
        );
        assert_eq!(app.editor.tabs.tab_drag.as_ref().unwrap().target_index, 0);

        // Far below the slack the drag targets the pane surface again.
        update_pointer(
            &mut app,
            &regions,
            centre(1) + 5.0,
            strip.bottom + 60.0,
            true,
        );
        assert!(matches!(
            app.editor.tabs.tab_drag.as_ref().unwrap().drop,
            Some(TabDrop::Pane { .. })
        ));

        update_pointer(&mut app, &regions, centre(2) + 5.0, drifting, true);
        assert!(finish_pointer_interaction(&mut app));
        assert_eq!(
            app.workspace.pane(pane).unwrap().items(),
            &[second, third, first]
        );
    }

    #[test]
    fn dragging_a_tab_onto_another_strip_moves_it_there() {
        let mut app = AppState::new();
        let left = app.workspace.active_pane();
        let keep = app
            .workspace
            .test_open_view(PathBuf::from("keep.rs"), String::new());
        let moved = app
            .workspace
            .test_open_view(PathBuf::from("moved.rs"), String::new());
        let right = app.workspace.split(left, Direction::Right).unwrap();
        let copied = app.workspace.active().unwrap();
        assert_ne!(copied, moved);
        let regions = [
            region(left, UiRect::new(0.0, 0.0, 600.0, theme::TABS_H)),
            region(right, UiRect::new(600.0, 0.0, 1200.0, theme::TABS_H)),
        ];
        drag_from(&mut app, left, keep, 20.0, 10.0);

        assert!(update_pointer(&mut app, &regions, 1150.0, 10.0, true));
        assert!(!app.editor.tabs.tab_drag.as_ref().unwrap().floating);
        assert_eq!(
            app.editor.tabs.tab_drag.as_ref().unwrap().drop,
            Some(TabDrop::Insert {
                pane: right,
                index: 1
            })
        );
        assert!(finish_pointer_interaction(&mut app));

        assert_eq!(app.workspace.pane(left).unwrap().items(), &[moved]);
        assert_eq!(app.workspace.pane(right).unwrap().items(), &[copied, keep]);
        assert_eq!(app.workspace.active_pane(), right);
    }

    #[test]
    fn dragging_a_tab_to_a_surface_edge_splits_that_pane() {
        let mut app = AppState::new();
        let pane = app.workspace.active_pane();
        let first = app
            .workspace
            .test_open_view(PathBuf::from("first.rs"), String::new());
        let second = app
            .workspace
            .test_open_view(PathBuf::from("second.rs"), String::new());
        let regions = [region(pane, UiRect::new(0.0, 0.0, 900.0, theme::TABS_H))];
        drag_from(&mut app, pane, second, 200.0, 10.0);

        update_pointer(&mut app, &regions, 880.0, 200.0, true);
        assert!(!app.editor.tabs.tab_drag.as_ref().unwrap().floating);
        assert_eq!(
            app.editor.tabs.tab_drag.as_ref().unwrap().drop,
            Some(TabDrop::Pane {
                pane,
                split: Some(Direction::Right)
            })
        );
        finish_pointer_interaction(&mut app);

        let panes = app.workspace.pane_ids();
        assert_eq!(panes.len(), 2);
        assert_eq!(app.workspace.pane(panes[0]).unwrap().items(), &[first]);
        assert_eq!(app.workspace.pane(panes[1]).unwrap().items(), &[second]);
    }

    #[test]
    fn splitting_a_pane_with_its_only_tab_is_a_no_op() {
        let mut app = AppState::new();
        let pane = app.workspace.active_pane();
        let only = app
            .workspace
            .test_open_view(PathBuf::from("only.rs"), String::new());
        let regions = [region(pane, UiRect::new(0.0, 0.0, 900.0, theme::TABS_H))];
        drag_from(&mut app, pane, only, 40.0, 10.0);

        update_pointer(&mut app, &regions, 20.0, 200.0, true);
        finish_pointer_interaction(&mut app);

        assert_eq!(app.workspace.pane_ids(), vec![pane]);
        assert_eq!(app.workspace.active_items(), &[only]);
    }
}
