//! Opaque separators aligned to the display's physical pixel grid.

use lgui::core::{PhysicalRect, Point, UiElement, UiScale};
use lgui::prelude::{Color, Element, UiRect, VisualStyle};

use crate::ui::theme;

pub fn bottom(rect: UiRect) -> Element {
    horizontal(rect, true, scaled_border)
}

pub fn top(rect: UiRect) -> Element {
    horizontal(rect, false, scaled_border)
}

pub fn bottom_pixel(rect: UiRect, color: Color) -> Element {
    horizontal(rect, true, move |_| (1, color))
}

pub fn top_pixel(rect: UiRect, color: Color) -> Element {
    horizontal(rect, false, move |_| (1, color))
}

#[derive(Clone, Copy)]
pub enum VerticalPlacement {
    Scrolled(f32, f32),
    /// Restore a guide over a row whose fill touches partial pixels at its edges.
    Overlay,
}

pub fn vertical_pixel(
    start: Point,
    bottom: f32,
    viewport: UiRect,
    placement: VerticalPlacement,
    color: Color,
) -> Element {
    Element::new(move |cx| {
        match vertical_pixel_rect(start, bottom, viewport, placement, cx.context.scale()) {
            Some(rect) => UiElement::panel(cx.id, rect, VisualStyle::filled(color)),
            None => UiElement::group(cx.id, UiRect::default()),
        }
    })
}

/// Snap a guide after scroll, using only whole pixels inside the viewport.
pub fn vertical_pixel_rect(
    start: Point,
    bottom: f32,
    viewport: UiRect,
    placement: VerticalPlacement,
    scale: UiScale,
) -> Option<UiRect> {
    let (scroll_x, scroll_y) = match placement {
        VerticalPlacement::Scrolled(x, y) => (x, y),
        VerticalPlacement::Overlay => (0.0, 0.0),
    };
    if start.x < viewport.left + scroll_x || start.x >= viewport.right + scroll_x {
        return None;
    }
    let clip = scale.physical_ui_rect(viewport);
    let left = clip.left.ceil() as i32;
    let right = clip.right.floor() as i32;
    let (top, bottom) = match placement {
        VerticalPlacement::Scrolled(..) => (
            scale.physical_value(start.y - scroll_y),
            scale.physical_value(bottom - scroll_y),
        ),
        VerticalPlacement::Overlay => (
            (start.y * scale.factor()).floor() as i32,
            (bottom * scale.factor()).ceil() as i32,
        ),
    };
    let top = top.max(clip.top.ceil() as i32);
    let bottom = bottom.min(clip.bottom.floor() as i32);
    if left >= right || top >= bottom {
        return None;
    }
    let x = scale
        .physical_value(start.x - scroll_x)
        .clamp(left, right - 1);
    // The scrolling clip will translate the element back to the snapped position.
    Some(
        scale
            .logical_rect(PhysicalRect::new(x, top, x + 1, bottom))
            .translate(scroll_x, scroll_y),
    )
}

fn scaled_border(scale: UiScale) -> (i32, Color) {
    (scale.physical_length(1.0), theme::c().border)
}

fn horizontal(
    rect: UiRect,
    bottom_edge: bool,
    style: impl Fn(UiScale) -> (i32, Color) + 'static,
) -> Element {
    Element::new(move |cx| {
        let scale = cx.context.scale();
        crate::ui::device_scale::observe(scale.factor());
        let (width, color) = style(scale);
        let top = if bottom_edge {
            scale.physical_value(rect.bottom) - width
        } else {
            scale.physical_value(rect.top)
        };
        let line = scale.logical_rect(PhysicalRect::new(
            scale.physical_value(rect.left),
            top,
            scale.physical_value(rect.right),
            top + width,
        ));
        UiElement::panel(cx.id, line, VisualStyle::filled(color))
    })
}
