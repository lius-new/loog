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
use lgui::core::UiFocusHandle;
use lgui::prelude::{Element, UiRect};
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
        Command::Press(pane, id, index, x, y) => {
            if !app
                .workspace
                .pane(pane)
                .is_some_and(|pane| pane.items().contains(&id))
            {
                return;
            }
            crate::app::file_loading::cancel(app);
            let clicks = app.editor.tab_click_count(id, x, y);
            app.workspace.activate(pane, id);
            // The shared surface can retain native focus across this switch,
            // so its new view will not receive another focus event.
            app.editor.focused = true;
            app.editor.tabs.tab_context_menu = None;
            if clicks > 1 {
                app.workspace.promote_preview(pane, id);
                app.editor.tabs.tab_drag = None;
            } else {
                app.editor.tabs.tab_drag = Some(TabDragState {
                    pane,
                    source: id,
                    pointer_origin_x: x,
                    pointer_origin_y: y,
                    target_index: index,
                    active: false,
                    drop: None,
                });
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
fn set_tab_scroll_x(app: &mut AppState, pane: PaneId, value: f32) -> bool {
    if (tab_scroll_x(app, pane) - value).abs() <= f32::EPSILON {
        return false;
    }
    app.editor.tabs.tab_scroll.insert(pane, value);
    true
}

/// Track tab hover across every pane's strip. Only a `PointerDrag` advances
/// tab drag gestures; ordinary hover must not promote a pending click.
pub fn update_pointer(
    app: &mut AppState,
    regions: &[PaneRegion],
    pointer_x: f32,
    pointer_y: f32,
    is_pointer_drag: bool,
) -> bool {
    let mut changed = false;

    if is_pointer_drag && let Some(mut drag) = app.editor.tabs.tab_drag.take() {
        let moved = (pointer_x - drag.pointer_origin_x).hypot(pointer_y - drag.pointer_origin_y);
        if !drag.active && moved >= DRAG_THRESHOLD {
            drag.active = true;
            changed = true;
        }
        if drag.active {
            let drop =
                drag_drop_target(app, regions, &mut drag, pointer_x, pointer_y, &mut changed);
            if drag.drop != drop {
                drag.drop = drop;
                changed = true;
            }
        }
        app.editor.tabs.tab_drag = Some(drag);
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
        changed = true;
    }

    changed
}

/// Resolve where an active tab drag would land, auto-scrolling the strip
/// under the pointer near its edges.
fn drag_drop_target(
    app: &mut AppState,
    regions: &[PaneRegion],
    drag: &mut TabDragState,
    pointer_x: f32,
    pointer_y: f32,
    changed: &mut bool,
) -> Option<TabDrop> {
    // A reorder inside the dragged tab's own strip tolerates vertical drift, so
    // a slightly sloppy drag does not fall through to a pane drop.
    let own_strip = regions.iter().find(|region| {
        region.pane == drag.pane
            && point_inside(
                UiRect::new(
                    region.strip.left,
                    region.strip.top - DRAG_REORDER_SLACK,
                    region.strip.right,
                    region.strip.bottom + DRAG_REORDER_SLACK,
                ),
                pointer_x,
                pointer_y,
            )
    });
    if let Some(region) = regions
        .iter()
        .find(|region| point_inside(region.strip, pointer_x, pointer_y))
        .or(own_strip)
    {
        let (pane, rect) = (region.pane, region.strip);
        let mut layout = tab_layout(rect, app, pane, false);
        let viewport_right = rect.left + layout.viewport_width;
        let scroll = tab_scroll_x(app, pane);
        let next = if pointer_x <= rect.left + DRAG_EDGE_W {
            (scroll - DRAG_SCROLL_STEP).max(0.0)
        } else if pointer_x >= viewport_right - DRAG_EDGE_W {
            (scroll + DRAG_SCROLL_STEP).min(layout.max_scroll_x)
        } else {
            scroll
        };
        if set_tab_scroll_x(app, pane, next) {
            *changed = true;
            layout = tab_layout(rect, app, pane, false);
        }
        let content_x = pointer_x - rect.left + layout.scroll_x;
        if pane == drag.pane {
            let source_index = pane_items(app, pane)
                .iter()
                .position(|id| *id == drag.source)
                .unwrap_or(usize::MAX);
            if let Some(target_index) =
                tab_layout::reorder_slot(&layout.items, source_index, content_x)
                && target_index != drag.target_index
            {
                drag.target_index = target_index;
                *changed = true;
            }
            return None;
        }
        return Some(TabDrop::Insert {
            pane,
            index: insertion_index(&layout, content_x),
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
        app.editor.tabs.tab_drag = Some(TabDragState {
            pane: app.workspace.active_pane(),
            source: first,
            pointer_origin_x: 20.0,
            pointer_origin_y: 10.0,
            target_index: 0,
            active: false,
            drop: None,
        });
        let rect = UiRect::new(0.0, 0.0, 700.0, theme::TABS_H);
        let pane = app.workspace.active_pane();

        assert!(update_pointer(
            &mut app,
            &[region(pane, rect)],
            22.0,
            10.0,
            true
        ));
        assert!(!app.editor.tabs.tab_drag.as_ref().unwrap().active);
        assert!(finish_pointer_interaction(&mut app));
        assert_eq!(app.workspace.active_items(), &[first, second, third]);

        app.editor.tabs.tab_drag = Some(TabDragState {
            pane: app.workspace.active_pane(),
            source: first,
            pointer_origin_x: 20.0,
            pointer_origin_y: 10.0,
            target_index: 0,
            active: false,
            drop: None,
        });
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
        app.editor.tabs.tab_drag = Some(TabDragState {
            pane: app.workspace.active_pane(),
            source: first,
            pointer_origin_x: 20.0,
            pointer_origin_y: 10.0,
            target_index: 0,
            active: false,
            drop: None,
        });
        let rect = UiRect::new(0.0, 0.0, 700.0, theme::TABS_H);
        let pane = app.workspace.active_pane();
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
        app.editor.tabs.tab_drag = Some(TabDragState {
            pane,
            source,
            pointer_origin_x: x,
            pointer_origin_y: y,
            target_index: 0,
            active: false,
            drop: None,
        });
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
        assert_eq!(drag.drop, None);
        assert_eq!(drag.target_index, 0);

        update_pointer(&mut app, &regions, centre(1) + 5.0, drifting, true);
        assert_eq!(app.editor.tabs.tab_drag.as_ref().unwrap().target_index, 1);

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
