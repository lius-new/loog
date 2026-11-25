//! Layout map painting and framework hover/capture facts.
use super::layout_binding::{MapBinding, MapCommand, MapSignal};
use crate::domain::application_layout::{Content, Placement};
use crate::domain::pane_layout::{Axis, Direction};
use crate::ui::theme;
use lgui::core::{CursorIcon, LogicalKey, NamedKey, UiEventKind, UiEventPayload};
use lgui::prelude::{Element, Stroke, UiRect, VisualStyle, component, panel};
const CELL_RADIUS: f32 = 6.0;
pub fn attach(
    mut root: Element,
    map: MapBinding,
    viewport: UiRect,
    focus: lgui::core::UiFocusHandle,
) -> Element {
    let cancel = map.clone();
    root = root.on_event_capture(UiEventKind::KeyDown, move |cx, payload| {
        if let UiEventPayload::Keyboard { event } = payload
            && event.key == LogicalKey::Named(NamedKey::Escape)
            && cancel.cancel()
        {
            cx.prevent_default();
            cx.stop_propagation();
        }
    });
    let stale = map.clone();
    root = root.on_event_capture(UiEventKind::PointerDown, move |_, _| {
        stale.cancel();
    });
    for kind in [UiEventKind::PointerMove, UiEventKind::PointerDrag] {
        let map = map.clone();
        root =
            root.on_event_capture(kind, move |cx, payload| {
                if let UiEventPayload::PointerMove { pointer }
                | UiEventPayload::PointerDrag { pointer } = payload
                    && map.is_dragging()
                {
                    map.run(MapCommand::Pointer {
                        point: pointer.point,
                        viewport,
                    });
                    if map.is_active() {
                        cx.stop_propagation();
                    }
                }
            });
    }
    root.on_event_capture(UiEventKind::PointerUp, move |cx, payload| {
        if let UiEventPayload::PointerUp { pointer } = payload
            && map.is_dragging()
        {
            let result = map.run(MapCommand::Release {
                point: pointer.point,
                viewport,
            });
            if result.focus {
                focus.focus();
            }
            if result.consumed {
                cx.prevent_default();
                cx.stop_propagation();
            }
        }
    })
}

/// The overlay is its own component: map changes re-render only it.
pub fn render(viewport: UiRect, map: MapBinding, focus: lgui::core::UiFocusHandle) -> Element {
    component(viewport, move |cx, viewport| {
        cx.use_observable(map.observable(), |s: &MapSignal| s.revision);
        render_overlay(*viewport, map, focus)
    })
    .key("terminal-layout-map-component")
}

fn render_overlay(viewport: UiRect, map: MapBinding, focus: lgui::core::UiFocusHandle) -> Element {
    use lgui::core::{EventPolicy, UiElement, UiId};
    let release_map = map.clone();
    let mut overlay = Element::new(move |cx| {
        let release = release_map.with(|d| {
            let drag = d?;
            let point = drag.release?;
            let hovered = drag.scene.as_ref().is_some_and(|scene| {
                (0..scene.zones.len()).any(|i| {
                    cx.context
                        .interaction_flags(&UiId::owned(format!("terminal-map-zone-{i}")))
                        .hovered
                })
            });
            Some((point, hovered))
        });
        if let Some((point, hovered)) = release {
            let result = release_map.run(MapCommand::Finish {
                point,
                viewport,
                hovered,
            });
            if result.focus {
                focus.focus();
            }
        }
        // This container only owns the preview; its children own all drop targets.
        // An idle full-size hit rectangle masks every underlying hover cursor.
        UiElement::panel(cx.id, viewport, VisualStyle::default())
            .hit_rect(UiRect::new(0.0, 0.0, 0.0, 0.0))
            .children(cx.children)
    })
    .key("terminal-layout-map");
    let Some(drag) = map.snapshot() else {
        return overlay;
    };
    if let (Some(strip), Some(index)) = (&drag.strip, drag.reorder) {
        // Keep the old line in the strip's drop tolerance. Inside the strip
        // the tab and its neighbors show the insertion directly.
        if !drag.floating
            && let Some(x) = strip.marker_x(drag.session, index)
        {
            let bar = UiRect::new(
                x - 1.0,
                strip.rect.top + crate::ui::tabs::REORDER_MARKER_INSET,
                x + 1.0,
                strip.rect.bottom - crate::ui::tabs::REORDER_MARKER_INSET,
            );
            overlay = overlay.child(panel(
                bar,
                VisualStyle::filled(theme::c().accent).radius(1.0),
            ));
        }
        return overlay;
    }
    let Some(scene) = &drag.scene else {
        return overlay;
    };
    // Invisible during the pending click. This lets the very first movement
    // into the map establish hover before the release/cancel distinction.
    for (i, zone) in scene.zones.iter().enumerate() {
        let rect = zone.rect;
        overlay = overlay.child(
            Element::new(move |cx| {
                UiElement::panel(
                    UiId::owned(format!("terminal-map-zone-{i}")),
                    rect,
                    VisualStyle::default(),
                )
                .children(cx.children)
            })
            .key(format!("map-hit-{i}"))
            .event_policy(EventPolicy {
                hover: true,
                press: false,
                focus: false,
            })
            .on_pointer_move(|_, _| {}),
        );
    }
    if !drag.active {
        return overlay;
    }
    let c = theme::c();
    // Only the miniature responds while dragging; the real window is left
    // untouched until release.
    let zone = drag.hover.map(|i| &scene.zones[i]);
    let moving = |content: &Content| matches!(content, Content::Terminal { tabs, .. } if tabs.contains(&drag.session));
    for (_, content, rect) in &scene.cells {
        let fill = match content {
            Content::Editor => c.surface,
            Content::Git | Content::Files => c.sidebar,
            Content::Terminal { .. } => c.bg,
        };
        let border = if moving(content) { c.accent } else { c.border };
        overlay = overlay.child(theme::bordered(
            inset(*rect, 1.0),
            fill,
            border,
            CELL_RADIUS,
            1.0,
        ));
    }
    // Window-edge bands have no cell to suggest them; mark each with a rail.
    for zone in &scene.zones {
        if let Placement::Outer(dir) = zone.placement {
            let r = zone.rect;
            let (mx, my) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
            let rail = match dir {
                Direction::Left | Direction::Right => {
                    UiRect::new(mx - 1.5, r.top + 8.0, mx + 1.5, r.bottom - 8.0)
                }
                Direction::Up | Direction::Down => {
                    UiRect::new(r.left + 8.0, my - 1.5, r.right - 8.0, my + 1.5)
                }
            };
            overlay = overlay.child(panel(
                rail,
                VisualStyle::filled(c.text_faint).alpha(110).radius(1.5),
            ));
        }
    }
    if let Some(zone) = zone {
        let color = if zone.plan.is_some() {
            c.accent
        } else {
            c.warning
        };
        // A body drop outlines the cell; bands are narrow enough to fill solidly.
        let style = if zone.center {
            VisualStyle::filled(color)
                .alpha(40)
                .stroked(Stroke::new(color, 2.0, 230))
                .radius(CELL_RADIUS - 2.0)
        } else {
            VisualStyle::filled(color).alpha(200).radius(3.0)
        };
        overlay = overlay.child(panel(zone.rect, style));
    }
    overlay
}

fn inset(r: UiRect, by: f32) -> UiRect {
    UiRect::new(r.left + by, r.top + by, r.right - by, r.bottom - by)
}

pub fn render_divider(
    divider: crate::domain::application_layout::Divider,
    map: MapBinding,
) -> Element {
    use lgui::core::PointerButton;
    let p = divider.position;
    let (line, hit, cursor) = match divider.axis {
        Axis::Horizontal => (
            UiRect::new(p - 0.5, divider.parent.top, p + 0.5, divider.parent.bottom),
            UiRect::new(p - 4.0, divider.parent.top, p + 4.0, divider.parent.bottom),
            CursorIcon::ResizeHorizontal,
        ),
        Axis::Vertical => (
            UiRect::new(divider.parent.left, p - 0.5, divider.parent.right, p + 0.5),
            UiRect::new(divider.parent.left, p - 4.0, divider.parent.right, p + 4.0),
            CursorIcon::ResizeVertical,
        ),
    };
    let active = map.sash_active(&divider);
    let key = format!(
        "application-sash-{}-{}-{}",
        divider.split, divider.before, divider.after
    );
    let down = map.clone();
    let moving = map.clone();
    let up = map;
    crate::ui::components::resize_handle::render(key, hit, line, cursor, active)
        .on_pointer_down_with_button(move |cx, _, button| {
            if button == PointerButton::Left {
                down.run(MapCommand::DividerStart(divider.clone()));
                cx.stop_propagation();
            }
        })
        .on_pointer_drag(move |cx, pointer| {
            moving.run(MapCommand::DividerDrag(pointer.point));
            cx.stop_propagation();
        })
        .on_pointer_up(move |cx, _| {
            up.run(MapCommand::DividerEnd);
            cx.stop_propagation();
        })
}
