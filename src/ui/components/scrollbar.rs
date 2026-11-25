//! Shared overlay scrollbar colors and dimensions for editors and drawers.

use lgui::core::{CursorIcon, UiElement};
use lgui::prelude::{Element, UiRect, VisualStyle};

use crate::ui::theme;

pub const SIZE: f32 = 12.0;
pub const INSET: f32 = 2.0;
pub const MIN_THUMB_LENGTH: f32 = 28.0;

/// The track stays transparent, even on hover, so it never covers content.
pub fn track(rect: UiRect) -> Element {
    Element::new(move |cx| {
        UiElement::panel(cx.id, rect, VisualStyle::default()).children(cx.children)
    })
    .cursor(CursorIcon::Default)
}

/// Idle thumb opacity: a faint block over the content, as in VS Code.
const THUMB_ALPHA: u8 = 0x50;
/// Thumb opacity while it is pressed or dragged. Hovering changes nothing.
const THUMB_PRESSED_ALPHA: u8 = 0xa0;

/// A scrollbar thumb with two states: idle and pressed.
pub fn thumb(rect: UiRect) -> Element {
    thumb_with_radius(rect, 2.0)
}

/// The same two-state thumb with square corners, for the editor.
pub fn square_thumb(rect: UiRect) -> Element {
    thumb_with_radius(rect, 0.0)
}

fn thumb_with_radius(rect: UiRect, radius: f32) -> Element {
    Element::new(move |cx| {
        let alpha = if cx.context.interaction_flags(&cx.id).pressed {
            THUMB_PRESSED_ALPHA
        } else {
            THUMB_ALPHA
        };
        let style = VisualStyle::filled(theme::c().text_dim)
            .alpha(alpha)
            .radius(radius);
        UiElement::panel(cx.id, rect, style).children(cx.children)
    })
    .cursor(CursorIcon::Default)
}
