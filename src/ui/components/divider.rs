//! Opaque separators aligned to the display's physical pixel grid.

use lgui::core::{PhysicalRect, UiElement, UiScale};
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
