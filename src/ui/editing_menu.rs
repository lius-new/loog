//! Small status menu. Events contain choices only; document ownership stays in app.
use crate::ui::theme;
use lgui::{
    core::{EventPolicy, LogicalKey, NamedKey, SemanticRole, Semantics, UiElement, UiId},
    prelude::{Element, UiRect, VisualStyle, panel, text},
};
use std::sync::Arc;
pub const FOCUS_ID: &str = "editing.status-menu";
#[derive(Clone, Copy)]
pub enum Event {
    Close,
    Select(usize),
    Move(bool),
    Accept,
}
pub fn render(
    viewport: UiRect,
    anchor: UiRect,
    anchor_id: &'static str,
    labels: Vec<String>,
    selected: usize,
    send: Arc<dyn Fn(Event) + Send + Sync>,
) -> Element {
    let width = 240.0f32.min(viewport.width());
    let height = (labels.len() as f32 * 26.0 + 8.0).min(viewport.height());
    let x =
        (anchor.right - width).clamp(viewport.left, (viewport.right - width).max(viewport.left));
    let y = (anchor.top - height - 4.0).max(viewport.top);
    let card = UiRect::new(x, y, x + width, y + height);
    let close = send.clone();
    let outside = send.clone();
    let key = send.clone();
    let overlay = Element::new(move |cx| {
        UiElement::panel(UiId::new(FOCUS_ID), viewport, VisualStyle::default())
            .children(cx.children)
    })
    .event_policy(EventPolicy::INTERACTIVE)
    .focus_scope()
    .on_close_requested(move |_| close(Event::Close))
    .on_key_down(move |cx, event| {
        if let LogicalKey::Named(k) = event.key {
            match k {
                NamedKey::ArrowUp => key(Event::Move(false)),
                NamedKey::ArrowDown => key(Event::Move(true)),
                NamedKey::Enter => key(Event::Accept),
                _ => {}
            }
        }
        cx.prevent_default();
        cx.stop_propagation();
    });
    let mut menu = theme::bordered(card, theme::c().surface, theme::c().border, 5.0, 1.0)
        .position(lgui::core::Position::Anchor {
            target: anchor_id.into(),
            side: lgui::core::AnchorSide::Top,
            gap: lgui::core::Dimension::Points(4.0),
            offset: lgui::core::Point::new(anchor.width() - width, 0.0),
            flip: true,
        })
        .on_outside_pointer(move |_, _, _| outside(Event::Close))
        .event_policy(EventPolicy {
            focus: false,
            ..EventPolicy::INTERACTIVE
        });
    for (i, label) in labels.into_iter().enumerate() {
        let r = UiRect::new(
            x + 4.0,
            y + 4.0 + i as f32 * 26.0,
            x + width - 4.0,
            y + 4.0 + (i + 1) as f32 * 26.0,
        );
        let click = send.clone();
        menu = menu.child(
            panel(
                r,
                if i == selected {
                    VisualStyle::filled(theme::c().active_line)
                } else {
                    VisualStyle::default()
                },
            )
            .event_policy(EventPolicy {
                focus: false,
                ..EventPolicy::INTERACTIVE
            })
            .cursor(lgui::core::CursorIcon::Pointer)
            .semantics(Semantics::new(SemanticRole::Button).name(label.clone()))
            .on_click(move || click(Event::Select(i)))
            .child(text(
                r.translate(8.0, 0.0),
                label,
                theme::mono(theme::c().text_soft, theme::SMALL),
            )),
        );
    }
    overlay.child(menu)
}
