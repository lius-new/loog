//! Scrolling as the file tree does it: the content is laid out in its own
//! coordinates, a clip moves it by the scroll, the wheel changes the scroll, and a
//! draggable thumb is drawn over it. The track stays invisible.
use lgui::core::{EventPolicy, UiEventKind, UiEventPayload, WheelUnit, clip};
use lgui::prelude::{Element, UiRect, VisualStyle, panel};

use crate::ui::components::scrollbar;
use crate::ui::components::state_access::StateAccess;
use crate::ui::extensions_binding::{ExtensionsUiState, ScrollState};

/// Which scroll of the page an area keeps.
pub(super) type Pick = fn(&mut ExtensionsUiState) -> &mut ScrollState;

/// `items` (laid out down to `content_bottom`) scrolling inside `region`. `owner`
/// names what is shown: the scroll starts over when it changes. `line` is how far a
/// wheel notch of lines moves.
#[allow(clippy::too_many_arguments)]
pub(super) fn view(
    key: String,
    owner: String,
    region: UiRect,
    content_bottom: f32,
    items: Vec<Element>,
    current: &ScrollState,
    access: &StateAccess<ExtensionsUiState>,
    pick: Pick,
    line: f32,
) -> Element {
    let max_scroll = (content_bottom - region.bottom).max(0.0);
    let scroll = current.at(&owner).clamp(0.0, max_scroll);

    let mut clipped = clip(region, 0.0, -scroll);
    for item in items {
        clipped = clipped.child(item);
    }
    let wheel = access.clone();
    let wheel_owner = owner.clone();
    let mut area =
        panel(region, VisualStyle::default()).on_event(UiEventKind::Wheel, move |_cx, payload| {
            if let UiEventPayload::Wheel { delta } = payload {
                let step = match delta.unit {
                    WheelUnit::Lines => delta.y * line * 3.0,
                    WheelUnit::Pixels => delta.y,
                };
                let owner = wheel_owner.clone();
                wheel.update(move |ui| {
                    let state = pick(ui);
                    state.owner = owner;
                    state.offset = (scroll - step).clamp(0.0, max_scroll);
                });
            }
        });
    area = area.child(clipped);
    if max_scroll > 0.0 {
        area = area.child(thumb(region, content_bottom, scroll, owner, access, pick));
    }
    area.key(key)
}

fn thumb(
    rect: UiRect,
    content_bottom: f32,
    scroll: f32,
    owner: String,
    access: &StateAccess<ExtensionsUiState>,
    pick: Pick,
) -> Element {
    let content_h = (content_bottom - rect.top).max(1.0);
    let max_scroll = (content_bottom - rect.bottom).max(0.0);
    let track = UiRect::new(
        rect.right - scrollbar::SIZE,
        rect.top + 4.0,
        rect.right,
        rect.bottom - 4.0,
    );
    let thumb_h = (track.height() * rect.height() / content_h)
        .max(scrollbar::MIN_THUMB_LENGTH)
        .min(track.height());
    let travel = track.height() - thumb_h;
    let thumb_top = if max_scroll == 0.0 {
        track.top
    } else {
        track.top + travel * (scroll / max_scroll)
    };
    let track_top = track.top;
    let (down, drag, up) = (access.clone(), access.clone(), access.clone());
    scrollbar::thumb(
        UiRect::new(
            track.left + scrollbar::INSET,
            thumb_top,
            track.right - scrollbar::INSET,
            thumb_top + thumb_h,
        ),
        0xb8,
    )
    .key("ext-scrollbar-thumb")
    .event_policy(EventPolicy::INTERACTIVE)
    .on_pointer_down(move |_cx, p| {
        let owner = owner.clone();
        down.update(move |ui| {
            let state = pick(ui);
            state.owner = owner;
            state.offset = scroll;
            state.dragging = true;
            state.drag_offset = p.point.y - thumb_top;
        });
    })
    .on_pointer_drag(move |_cx, p| {
        drag.update(move |ui| {
            let state = pick(ui);
            if state.dragging && travel > 0.0 {
                let t = (p.point.y - track_top - state.drag_offset) / travel * max_scroll;
                state.offset = t.clamp(0.0, max_scroll);
            }
        });
    })
    .on_pointer_up(move |_cx, _p| {
        up.update(move |ui| pick(ui).dragging = false);
    })
}
