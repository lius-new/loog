//! Spatial visibility controls; geometry and commands arrive through a narrow port.
use crate::ui::theme;
mod motion;
use lgui::core::{
    AnimProperty, AnimationBinding, Color, CursorIcon, EventPolicy, IconStyle, LogicalKey,
    NamedKey, Observable, Point, PointerButton, SemanticRole, Semantics, ShadowStyle, Signal,
    Stroke, UiElement, UiEventKind, UiEventPayload, UiId,
};
use lgui::prelude::{Element, UiRect, VisualStyle, component, group, text};
use std::sync::Arc;

// Native windows schedule hover redraws through animation bindings. The visuals
// read flags directly, so this schedules a frame without a visible transition.
const HOVER_FRAME: AnimationBinding =
    AnimationBinding::new(AnimProperty::Hover, 0.0, 1.0).duration(1.0, 1.0);

#[derive(Clone)]
pub(crate) struct Region {
    pub id: u64,
    pub rect: UiRect,
    pub shown: bool,
    pub label: String,
}
#[derive(Clone)]
pub(crate) struct Drag {
    pub region: u64,
    pub origin: UiRect,
    pub pressed: Point,
}

#[derive(Clone)]
pub(crate) struct Frame {
    pub open: bool,
    pub source: UiRect,
    pub regions: Vec<Region>,
    pub drag: Option<Drag>,
}
#[derive(Clone, Copy)]
pub(crate) enum Command {
    Open,
    Close,
    Toggle(u64),
    Begin {
        region: u64,
        point: Point,
        area: UiRect,
    },
    Move(Point),
    Release(Point),
    Cancel,
}
#[derive(Clone)]
pub(crate) struct Binding {
    read: Arc<dyn Fn() -> Frame + Send + Sync>,
    run: Arc<dyn Fn(Command) + Send + Sync>,
    changes: Option<Observable<u64>>,
    pointer: Option<Signal<Point>>,
    active: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}
impl Binding {
    pub fn new(
        read: impl Fn() -> Frame + Send + Sync + 'static,
        run: impl Fn(Command) + Send + Sync + 'static,
    ) -> Self {
        Self {
            read: Arc::new(read),
            run: Arc::new(run),
            changes: None,
            pointer: None,
            active: None,
        }
    }
    pub fn with_drag(
        mut self,
        changes: Observable<u64>,
        pointer: Option<Signal<Point>>,
        active: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        self.changes = Some(changes);
        self.pointer = pointer;
        self.active = Some(Arc::new(active));
        self
    }
    fn dragging(&self) -> bool {
        self.active.as_ref().is_some_and(|active| active())
    }
    pub fn frame(&self) -> Frame {
        (self.read)()
    }
    pub fn run(&self, command: Command) {
        (self.run)(command);
    }
}

pub(crate) fn popup_rect(viewport: UiRect, anchor: UiRect) -> UiRect {
    let width = 440.0_f32.min((viewport.width() - 16.0).max(1.0));
    let height = 250.0_f32.min((viewport.bottom - anchor.bottom - 16.0).max(1.0));
    let left = (anchor.right - width)
        .max(viewport.left + 8.0)
        .min((viewport.right - width - 8.0).max(viewport.left));
    let top = anchor.bottom + 6.0;
    UiRect::new(left, top, left + width, top + height)
}

/// Hovering the icon opens the popover; its gap and body retain it.
pub(crate) fn attach(root: Element, viewport: UiRect, anchor: UiRect, binding: Binding) -> Element {
    let rect = popup_rect(viewport, anchor);
    // The transparent gap is part of the hover path into the popover.
    let bridge = UiRect::new(
        rect.left.min(anchor.left),
        anchor.bottom,
        rect.right.max(anchor.right),
        rect.top,
    );
    let hover = binding.clone();
    let keys = binding.clone();
    let moving = binding.clone();
    let released = binding.clone();
    let mut root = root;
    for kind in [UiEventKind::PointerMove, UiEventKind::PointerDrag] {
        let moving = moving.clone();
        root =
            root.on_event_capture(kind, move |cx, payload| {
                if let UiEventPayload::PointerMove { pointer }
                | UiEventPayload::PointerDrag { pointer } = payload
                    && moving.dragging()
                {
                    moving.run(Command::Move(pointer.point));
                    cx.stop_propagation();
                }
            });
    }
    root = root.on_event_capture(UiEventKind::PointerUp, move |cx, payload| {
        if let UiEventPayload::PointerUp { pointer } = payload
            && released.dragging()
        {
            released.run(Command::Release(pointer.point));
            if !rect.contains(pointer.point) {
                released.run(Command::Close);
            }
            cx.prevent_default();
            cx.stop_propagation();
        }
    });
    root.on_event_capture(UiEventKind::PointerMove, move |_, payload| {
        if let UiEventPayload::PointerMove { pointer } = payload {
            let frame = hover.frame();
            let open = frame.open;
            if hover.dragging() {
                return;
            }
            if anchor.contains(pointer.point) {
                if !open {
                    hover.run(Command::Open);
                }
            } else if open && !rect.contains(pointer.point) && !bridge.contains(pointer.point) {
                hover.run(Command::Close);
            }
        }
    })
    .on_event_capture(UiEventKind::KeyDown, move |cx, payload| {
        if let UiEventPayload::Keyboard { event } = payload
            && keys.frame().open
            && event.key == LogicalKey::Named(NamedKey::Escape)
        {
            keys.run(if keys.dragging() {
                Command::Cancel
            } else {
                Command::Close
            });
            cx.prevent_default();
            cx.stop_propagation();
        }
    })
    .on_event_capture(UiEventKind::PointerDown, move |_, payload| {
        if let UiEventPayload::PointerDown { pointer, .. } = payload
            && binding.frame().open
            && !rect.contains(pointer.point)
            && !anchor.contains(pointer.point)
        {
            binding.run(Command::Close);
        }
    })
}

pub(crate) fn render(viewport: UiRect, anchor: UiRect, binding: Binding) -> Element {
    component(viewport, move |cx, viewport| {
        if let Some(changes) = &binding.changes {
            cx.use_observable(changes.clone(), |revision| *revision);
        }
        render_popup(*viewport, anchor, binding)
    })
    .key("layout-visibility-component")
}

fn render_popup(viewport: UiRect, anchor: UiRect, binding: Binding) -> Element {
    let frame = binding.frame();
    if !frame.open {
        return group(UiRect::default()).key("layout-visibility-closed");
    }
    let popup = popup_rect(viewport, anchor);
    let area = UiRect::new(
        popup.left + 9.0,
        popup.top + 9.0,
        popup.right - 9.0,
        popup.bottom - 9.0,
    );
    let project = |r: UiRect| {
        UiRect::new(
            area.left + (r.left - frame.source.left) / frame.source.width().max(1.0) * area.width(),
            area.top + (r.top - frame.source.top) / frame.source.height().max(1.0) * area.height(),
            area.left
                + (r.right - frame.source.left) / frame.source.width().max(1.0) * area.width(),
            area.top
                + (r.bottom - frame.source.top) / frame.source.height().max(1.0) * area.height(),
        )
    };
    let mut view = theme::bordered(popup, theme::c().surface, theme::c().border, 4.0, 1.0)
        .key("layout-visibility-popup")
        .semantics(Semantics::new(SemanticRole::Dialog).name("Layout view"))
        .event_policy(EventPolicy {
            hover: true,
            press: true,
            focus: false,
        })
        .on_pointer_down(|cx, _| cx.stop_propagation());
    let mut lifted = None;
    for region in frame.regions {
        let bounds = project(region.rect);
        let bounds = UiRect::new(
            bounds.left + 3.0,
            bounds.top + 3.0,
            bounds.right - 3.0,
            bounds.bottom - 3.0,
        );
        let cell = if frame
            .drag
            .as_ref()
            .is_some_and(|drag| drag.region == region.id)
        {
            lifted = Some(region.clone());
            Element::new(move |_| {
                UiElement::panel(
                    UiId::owned(format!("layout-visibility-cell-{}", region.id)),
                    bounds,
                    VisualStyle::filled(theme::c().accent).alpha(24).radius(2.0),
                )
            })
            .event_policy(EventPolicy {
                hover: true,
                press: true,
                focus: true,
            })
            .cursor(CursorIcon::Move)
        } else {
            cell(
                bounds,
                region.clone(),
                area,
                frame.drag.is_some(),
                binding.clone(),
            )
        };
        view = view.child(motion::slide(region.id, bounds, cell));
    }
    if let (Some(region), Some(drag), Some(pointer)) = (lifted, frame.drag, binding.pointer.clone())
    {
        let bounds = project(drag.origin);
        let bounds = UiRect::new(
            bounds.left + 3.0,
            bounds.top + 3.0,
            bounds.right - 3.0,
            bounds.bottom - 3.0,
        );
        let mut label_style = theme::sans(theme::c().text_soft, 10.0);
        label_style.align = lgui::core::TextAlign::Center;
        let face = theme::bordered(bounds, theme::c().surface, theme::c().accent, 3.0, 1.0)
            .shadow(
                ShadowStyle::new(Color::BLACK)
                    .alpha(theme::c().shadow_alpha)
                    .offset(0.0, 4.0)
                    .blur(10.0),
            )
            .event_policy(EventPolicy::NONE)
            .child(text(bounds, region.label, label_style));
        view = view.child(motion::floating(bounds, area, drag.pressed, pointer).child(face));
    }
    view
}

fn cell(rect: UiRect, region: Region, area: UiRect, dragging: bool, binding: Binding) -> Element {
    let cell_id = UiId::owned(format!("layout-visibility-cell-{}", region.id));
    let eye_id = UiId::owned(format!("layout-visibility-eye-{}", region.id));
    let hover_id = cell_id.clone();
    let child_hover_id = eye_id.clone();
    let shown = region.shown;
    let begin = binding.clone();
    let region_id = region.id;
    let mut cell = Element::new(move |cx| {
        let hovered = cx.context.interaction_flags(&hover_id).hovered
            || cx.context.interaction_flags(&child_hover_id).hovered;
        let c = theme::c();
        let border = if hovered && !dragging {
            c.accent
        } else {
            c.border
        };
        let style = if shown {
            VisualStyle::filled(c.bg)
                .stroked(Stroke::new(border, 1.0, 255))
                .radius(2.0)
        } else {
            VisualStyle::filled(c.bg)
        };
        let node = UiElement::panel(cell_id, rect, style).children(cx.children);
        if shown {
            node
        } else {
            dashed_border(node, rect, region.id, border)
        }
    })
    .key(format!("layout-visibility-cell-{}", region.id))
    .event_policy(EventPolicy {
        hover: true,
        press: true,
        focus: true,
    })
    .semantics(Semantics::new(SemanticRole::Button).name(format!("Move {}", region.label)))
    .cursor(CursorIcon::Move)
    .on_pointer_down_with_button(move |cx, pointer, button| {
        if button == PointerButton::Left {
            begin.run(Command::Begin {
                region: region_id,
                point: pointer.point,
                area,
            });
            cx.stop_propagation();
        }
    })
    .animation(HOVER_FRAME)
    .on_pointer_move(|_, _| {});

    let center = Point::new(
        (rect.left + rect.right) * 0.5,
        (rect.top + rect.bottom) * 0.5,
    );
    let eye_rect = UiRect::new(
        center.x - 12.0,
        center.y - 12.0,
        center.x + 12.0,
        center.y + 12.0,
    );
    let parent_id = UiId::owned(format!("layout-visibility-cell-{}", region.id));
    let label = format!("{} {}", if shown { "Hide" } else { "Show" }, region.label);
    let id = region.id;
    cell = cell.child(
        Element::new(move |cx| {
            let flags = cx.context.interaction_flags(&eye_id);
            let reveal = !dragging
                && (!shown
                    || flags.hovered
                    || flags.focused
                    || cx.context.interaction_flags(&parent_id).hovered);
            let mut node = UiElement::panel(eye_id, eye_rect, VisualStyle::default());
            if reveal {
                let glyph = UiRect::new(
                    center.x - 7.0,
                    center.y - 7.0,
                    center.x + 7.0,
                    center.y + 7.0,
                );
                node = node.child(
                    UiElement::icon(
                        UiId::owned(format!("layout-visibility-glyph-{id}")),
                        glyph,
                        if shown {
                            "layout-eye"
                        } else {
                            "layout-eye-off"
                        },
                    )
                    .icon_style(IconStyle::new(if flags.hovered {
                        theme::c().accent
                    } else {
                        theme::c().text_soft
                    })),
                );
            }
            node
        })
        .key(format!("layout-visibility-eye-{id}"))
        .animation(HOVER_FRAME)
        // Hovering the eye also changes the enclosing region's border.
        .paint_bounds(rect)
        .semantics(Semantics::new(SemanticRole::Button).name(label))
        .event_policy(EventPolicy::INTERACTIVE)
        .cursor(lgui::core::CursorIcon::Pointer)
        .on_pointer_down(|cx, _| cx.stop_propagation())
        .on_click(move || binding.run(Command::Toggle(id))),
    );
    cell
}

fn dashed_border(
    mut node: UiElement,
    rect: UiRect,
    id: u64,
    color: lgui::core::Color,
) -> UiElement {
    let mut index = 0;
    for y in [rect.top, rect.bottom - 1.0] {
        let mut x = rect.left;
        while x < rect.right {
            node = node.child(UiElement::panel(
                UiId::owned(format!("layout-visibility-dash-{id}-{index}")),
                UiRect::new(x, y, (x + 3.0).min(rect.right), y + 1.0),
                VisualStyle::filled(color),
            ));
            x += 6.0;
            index += 1;
        }
    }
    for x in [rect.left, rect.right - 1.0] {
        let mut y = rect.top;
        while y < rect.bottom {
            node = node.child(UiElement::panel(
                UiId::owned(format!("layout-visibility-dash-{id}-{index}")),
                UiRect::new(x, y, x + 1.0, (y + 3.0).min(rect.bottom)),
                VisualStyle::filled(color),
            ));
            y += 6.0;
            index += 1;
        }
    }
    node
}

#[cfg(test)]
mod tests {
    use super::*;
    use lgui::application::AppView;
    use lgui::core::{InputEvent, PointerData, UiScale};
    use lgui::session::UiSession;

    fn bounds(session: &UiSession, id: u64) -> UiRect {
        session
            .tree()
            .node(&UiId::owned(format!("layout-visibility-cell-{id}")))
            .unwrap()
            .layout_rect
    }

    fn assert_active(session: &UiSession, active: Option<u64>) {
        for id in 1..=3 {
            let node = session
                .tree()
                .node(&UiId::owned(format!("layout-visibility-cell-{id}")))
                .unwrap();
            assert_eq!(
                node.style.stroke.unwrap().color,
                if active == Some(id) {
                    theme::c().accent
                } else {
                    theme::c().border
                },
                "region {id} retained the wrong hover border"
            );
            assert_eq!(
                session
                    .tree()
                    .node(&UiId::owned(format!("layout-visibility-glyph-{id}")))
                    .is_some(),
                active == Some(id),
                "region {id} retained the wrong eye"
            );
        }
    }

    #[test]
    fn native_hover_frames_move_highlight_and_eye_between_regions_and_clear_on_leave() {
        let viewport = UiRect::new(0.0, 0.0, 1000.0, 700.0);
        let anchor = UiRect::new(800.0, 0.0, 832.0, 30.0);
        let frame = Frame {
            open: true,
            drag: None,
            source: UiRect::new(0.0, 0.0, 900.0, 600.0),
            regions: (1..=3)
                .map(|id| Region {
                    id,
                    rect: UiRect::new((id - 1) as f32 * 300.0, 0.0, id as f32 * 300.0, 600.0),
                    shown: true,
                    label: format!("Region {id}"),
                })
                .collect(),
        };
        let view: AppView = Arc::new(move |_| {
            let frame = frame.clone();
            render(
                viewport,
                anchor,
                Binding::new(move || frame.clone(), |_| {}),
            )
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        assert_active(&session, None);
        let body = |r: UiRect| Point::new(r.left + 5.0, r.top + 5.0);
        let eye = |r: UiRect| Point::new((r.left + r.right) * 0.5, (r.top + r.bottom) * 0.5);
        let middle = bounds(&session, 2);
        let right = bounds(&session, 3);
        let moves = [
            (body(middle), Some(2)),
            (body(right), Some(3)),
            (eye(right), Some(3)),
            (eye(middle), Some(2)),
            (body(middle), Some(2)),
            (body(right), Some(3)),
        ];
        for (point, active) in moves {
            native_frame(
                &mut session,
                &view,
                viewport,
                InputEvent::PointerMove(PointerData::mouse(point)),
            );
            assert_active(&session, active);
        }
        native_frame(
            &mut session,
            &view,
            viewport,
            InputEvent::PointerLeave(PointerData::mouse(eye(right))),
        );
        assert_active(&session, None);
    }

    /// Follow native window scheduling: a hover must schedule an animation tick,
    /// and that tick must request repaint before rendering the new view.
    fn native_frame(session: &mut UiSession, view: &AppView, viewport: UiRect, input: InputEvent) {
        let output = session.handle_input(input);
        assert!(
            output.animation_changed,
            "hover did not schedule a native frame: {:?}",
            output.events
        );
        if let Some(rect) = output.dirty_bounds {
            session.invalidations_mut().invalidate_rect(rect);
        }
        let tick = session.advance(16.0);
        assert!(
            tick.animation_changed,
            "hover tick did not request a redraw"
        );
        if let Some(rect) = tick.dirty_bounds {
            session.invalidations_mut().invalidate_rect(rect);
        }
        session.render_view(view, viewport, UiScale::ONE);
    }
}
