//! Toast notification card, centered above the status bar.
//!
//! The card sizes to its message (long messages end in "…"), takes its icon
//! color from the toast kind, and dismisses itself on click; the timer that
//! clears it lives in `app/shell.rs`.

use lgui::core::{CursorIcon, EventPolicy, UiEventContext};
use lgui::prelude::{Color, Element, ShadowStyle, UiRect, VisualStyle, panel, text};

use super::tabs::{ellipsize, measure};
use crate::domain::notification::{Toast, ToastKind};
use crate::ui::theme;

const HEIGHT: f32 = 34.0;
const MAX_W: f32 = 560.0;
const MIN_W: f32 = 200.0;
/// Clearance from the window edges and the status bar.
const MARGIN: f32 = 16.0;
const PAD_L: f32 = 12.0;
const PAD_R: f32 = 10.0;
const ICON_W: f32 = 16.0;
const GAP: f32 = 8.0;
const CLOSE_W: f32 = 16.0;

fn icon(kind: ToastKind) -> (&'static str, Color) {
    let c = theme::c();
    match kind {
        ToastKind::Info => ("✦", c.accent),
        ToastKind::Success => ("✓", c.git.added),
        ToastKind::Error => ("⚠", c.error),
    }
}

pub fn render(
    rect: UiRect,
    toast: Option<Toast>,
    dismiss: impl Fn(u64) + Send + Sync + 'static,
) -> Element {
    let Some(toast) = toast else {
        return panel(UiRect::new(0.0, 0.0, 0.0, 0.0), VisualStyle::default());
    };
    let c = theme::c();

    let chrome = PAD_L + ICON_W + GAP + GAP + CLOSE_W + PAD_R;
    let max_w = MAX_W.min(rect.right - rect.left - 2.0 * MARGIN).max(MIN_W);
    let message = ellipsize(&toast.message, max_w - chrome, theme::UI_SIZE, 400);
    let w = (measure(&message, theme::UI_SIZE, 400) + chrome).clamp(MIN_W, max_w);

    let cx = rect.left + (rect.right - rect.left) / 2.0;
    let bottom = rect.bottom - theme::STATUS_H - MARGIN;
    let card = UiRect::new(cx - w / 2.0, bottom - HEIGHT, cx + w / 2.0, bottom);

    let (glyph, glyph_color) = icon(toast.kind);
    let border = if toast.kind == ToastKind::Error {
        c.error
    } else {
        c.border
    };
    let icon_rect = UiRect::new(
        card.left + PAD_L,
        card.top,
        card.left + PAD_L + ICON_W,
        card.bottom,
    );
    let close_rect = UiRect::new(
        card.right - PAD_R - CLOSE_W,
        card.top,
        card.right - PAD_R,
        card.bottom,
    );
    let message_rect = UiRect::new(
        icon_rect.right + GAP,
        card.top,
        close_rect.left - GAP,
        card.bottom,
    );

    let id = toast.id;
    theme::bordered(card, c.surface, border, 8.0, 1.0)
        .shadow(
            ShadowStyle::new(Color::BLACK)
                .alpha(c.shadow_alpha)
                .offset(0.0, 4.0)
                .blur(12.0),
        )
        .event_policy(EventPolicy::INTERACTIVE)
        .position(lgui::core::Position::Layer {
            horizontal: lgui::core::Align::Center,
            vertical: lgui::core::Align::End,
            offset: lgui::core::Point::new(0.0, -theme::STATUS_H - MARGIN),
        })
        .z_index(100)
        .cursor(CursorIcon::Pointer)
        .on_click(move |_cx: &mut UiEventContext| {
            dismiss(id);
        })
        .child(text(
            icon_rect,
            glyph,
            theme::sans_semibold(glyph_color, theme::UI_SIZE).centered(),
        ))
        .child(text(
            message_rect,
            message,
            theme::sans(c.text_bright, theme::UI_SIZE),
        ))
        .child(text(
            close_rect,
            "✕",
            theme::sans(c.text_muted, theme::SMALL).centered(),
        ))
}
