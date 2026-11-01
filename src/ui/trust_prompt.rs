//! Modal question shown when an opened folder has not been decided on yet.

use std::sync::Arc;

use lgui::core::{
    CursorIcon, EventPolicy, SemanticRole, Semantics, UiElement, UiEventContext, UiFocusHandle,
    UiId,
};
use lgui::prelude::{Element, TextAlign, UiRect, VisualStyle, panel, text};

use crate::ui::theme;

const CARD_W: f32 = 540.0;
const CARD_H: f32 = 190.0;
const TITLE: &str = "Do you trust this folder?";

/// Called with `true` to trust the folder and `false` for a remembered "no".
pub type Answer = Arc<dyn Fn(bool) + Send + Sync>;

pub fn render(
    viewport: UiRect,
    folder: String,
    answer: Answer,
    dialog_id: UiId,
    editor_focus: UiFocusHandle,
) -> Element {
    let center_x = viewport.left + viewport.width() / 2.0;
    let top = viewport.top + (viewport.height() - CARD_H) * 0.36;
    let card = UiRect::new(
        center_x - CARD_W / 2.0,
        top,
        center_x + CARD_W / 2.0,
        top + CARD_H,
    );

    // The overlay owns keyboard focus while it is open, so Enter and Esc arrive as
    // `dialog::` actions (see `perform`) and every other key stops here.
    let overlay_style = VisualStyle::filled(theme::c().bg).alpha(214);
    let mut root = Element::new(move |cx| {
        UiElement::panel(dialog_id.clone(), viewport, overlay_style).children(cx.children)
    })
    .event_policy(EventPolicy::INTERACTIVE)
    .semantics(Semantics::new(SemanticRole::Dialog).name(TITLE))
    .on_key_down(move |cx, _event| {
        cx.prevent_default();
        cx.stop_propagation();
    })
    .child(theme::bordered(
        card,
        theme::c().sidebar,
        theme::c().border,
        8.0,
        1.0,
    ));

    let line = |top: f32, bottom: f32| {
        UiRect::new(
            card.left + 22.0,
            card.top + top,
            card.right - 22.0,
            card.top + bottom,
        )
    };
    root = root.child(text(
        line(20.0, 42.0),
        TITLE,
        theme::sans_semibold(theme::c().text_bright, 15.0),
    ));
    root = root.child(text(
        line(52.0, 75.0),
        folder,
        theme::sans(theme::c().text_muted, theme::UI_SIZE),
    ));
    root = root.child(text(
        line(80.0, 98.0),
        "Trusting it lets extensions run programs for this folder, such as language servers.",
        theme::sans(theme::c().text_dim, theme::SMALL),
    ));
    root = root.child(text(
        line(98.0, 116.0),
        "Without trust you can still open and edit files; those programs stay off.",
        theme::sans(theme::c().text_dim, theme::SMALL),
    ));

    let button_top = card.bottom - 46.0;
    let decline_rect = UiRect::new(
        card.right - 226.0,
        button_top,
        card.right - 112.0,
        button_top + 30.0,
    );
    let trust_rect = UiRect::new(
        card.right - 104.0,
        button_top,
        card.right - 22.0,
        button_top + 30.0,
    );

    let decline = answer.clone();
    let decline_focus = editor_focus.clone();
    root = root.child(
        theme::bordered(
            decline_rect,
            theme::c().sidebar,
            theme::c().border,
            4.0,
            1.0,
        )
        .event_policy(EventPolicy::INTERACTIVE)
        .cursor(CursorIcon::Pointer)
        .on_click(move |_cx: &mut UiEventContext| {
            decline(false);
            decline_focus.focus();
        })
        .child(text(
            decline_rect,
            "Don't Trust",
            centered(theme::sans(theme::c().text_soft, theme::UI_SIZE)),
        )),
    );

    root.child(
        panel(
            trust_rect,
            VisualStyle::filled(theme::c().accent).radius(4.0),
        )
        .event_policy(EventPolicy::INTERACTIVE)
        .cursor(CursorIcon::Pointer)
        .on_click(move |_cx: &mut UiEventContext| {
            answer(true);
            editor_focus.focus();
        })
        .child(text(
            trust_rect,
            "Trust",
            centered(theme::sans_semibold(theme::c().on_accent, theme::UI_SIZE)),
        )),
    )
}

fn centered(mut style: lgui::prelude::TextStyle) -> lgui::prelude::TextStyle {
    style.align = TextAlign::Center;
    style
}
