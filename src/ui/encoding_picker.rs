//! Compact encoding menu anchored above the status bar encoding item.
use crate::domain::encoding::{Charset, TextFormat};
use crate::ui::chrome::encoding_binding::{Binding, Command};
use crate::ui::theme;
use lgui::core::{
    CursorIcon, EventPolicy, SemanticRole, Semantics, UiElement, UiEventKind, UiEventPayload, UiId,
    clip,
};
use lgui::prelude::{Color, Element, ShadowStyle, TextAlign, UiRect, VisualStyle, panel, text};

pub(crate) const FOCUS_ID: &str = "encoding.picker";
const MENU_W: f32 = 224.0;
const MENU_PAD: f32 = 4.0;
const ITEM_H: f32 = 22.0;
const SEP_H: f32 = 5.0;
const CHECK_W: f32 = 22.0;
const ANCHOR_GAP: f32 = 4.0;

pub(crate) fn encoding_label(format: TextFormat) -> String {
    let name = match format.charset {
        Charset::Utf16Le => "UTF-16 LE",
        Charset::Utf16Be => "UTF-16 BE",
        Charset::Gb18030 => "GB18030",
        Charset::Windows1252 => "Windows-1252",
        _ => format.charset.name(),
    };
    format!("{name}{}", if format.bom { " BOM" } else { "" })
}

pub fn render(viewport: UiRect, anchor: Option<UiRect>, state: Binding) -> Element {
    let Some(picker) = state.frame() else {
        return panel(viewport, VisualStyle::default());
    };
    let Some(anchor) = anchor else {
        return panel(viewport, VisualStyle::default());
    };
    let width = MENU_W.min(viewport.width().max(0.0));
    let bottom = (anchor.top - ANCHOR_GAP).clamp(viewport.top, viewport.bottom);
    let available = bottom - viewport.top;
    let has_suggestion =
        picker.suggested.is_some() && picker.formats.first().copied() == picker.suggested;
    let separator_h = if has_suggestion && available >= MENU_PAD * 2.0 + ITEM_H * 2.0 + SEP_H {
        SEP_H
    } else {
        0.0
    };
    let visible = ((available - MENU_PAD * 2.0 - separator_h) / ITEM_H)
        .floor()
        .max(0.0) as usize;
    let visible = visible.min(picker.formats.len());
    let pinned = usize::from(has_suggestion && visible > 0);
    let normal_count = visible.saturating_sub(pinned);
    let height = (MENU_PAD * 2.0 + visible as f32 * ITEM_H + separator_h).min(available);
    let left =
        (anchor.right - width).clamp(viewport.left, (viewport.right - width).max(viewport.left));
    let card = UiRect::new(left, bottom - height, left + width, bottom);
    let selected = picker.selected.min(picker.formats.len().saturating_sub(1));
    let start = selected
        .saturating_sub(normal_count.saturating_sub(1))
        .max(pinned);

    let clear_hover = state.clone();
    let overlay = Element::new(move |cx| {
        UiElement::panel(UiId::new(FOCUS_ID), viewport, VisualStyle::default())
            .children(cx.children)
    })
    .event_policy(EventPolicy::INTERACTIVE)
    .on_event_capture(UiEventKind::PointerMove, move |_cx, _payload| {
        clear_hover.run(Command::Hover(None));
    })
    .on_key_down(move |cx, _event| {
        cx.prevent_default();
        cx.stop_propagation();
    });
    let wheel = state.clone();
    let close = state.clone();
    let mut surface = theme::bordered(card, theme::c().surface, theme::c().border, 6.0, 1.0)
        .position(lgui::core::Position::Anchor {
            target: "statusbar.encoding".into(),
            side: lgui::core::AnchorSide::Top,
            gap: lgui::core::Dimension::Points(ANCHOR_GAP),
            offset: lgui::core::Point::new(anchor.width() - width, 0.0),
            flip: true,
        })
        .on_outside_pointer(move |_, _, _| close.run(Command::Close))
        .shadow(
            ShadowStyle::new(Color::BLACK)
                .alpha(theme::c().shadow_alpha)
                .offset(0.0, 2.0)
                .blur(3.0),
        )
        .event_policy(EventPolicy::INTERACTIVE)
        .on_event(UiEventKind::Wheel, move |cx, payload| {
            if let UiEventPayload::Wheel { delta } = payload {
                wheel.run(Command::Scroll(delta.y < 0.0));
                cx.stop_propagation();
            }
        });
    let mut contents = clip(card, 0.0, 0.0);
    let mut top = card.top + MENU_PAD;
    for (position, index) in (0..pinned).chain(start..start + normal_count).enumerate() {
        if pinned == 1 && position == 1 {
            let line_y = top + (separator_h - 1.0) / 2.0;
            contents = contents.child(panel(
                UiRect::new(card.left + 10.0, line_y, card.right - 10.0, line_y + 1.0),
                VisualStyle::filled(theme::c().border),
            ));
            top += separator_h;
        }
        let format = picker.formats[index];
        let suggested = picker.suggested == Some(format);
        let row = UiRect::new(card.left + 1.0, top, card.right - 1.0, top + ITEM_H);
        let hovered = picker.hovered == Some(index);
        let color = if hovered {
            theme::c().text_bright
        } else {
            theme::c().text_soft
        };
        let hover = state.clone();
        let choose = state.clone();
        let mut item = panel(
            row,
            if hovered {
                VisualStyle::filled(theme::c().selection)
            } else {
                VisualStyle::default()
            },
        )
        .event_policy(EventPolicy::INTERACTIVE)
        .cursor(CursorIcon::Pointer)
        .semantics(Semantics::new(SemanticRole::MenuItem).name(if suggested {
            format!("{}, Suggested", encoding_label(format))
        } else {
            encoding_label(format)
        }))
        .on_event(UiEventKind::PointerMove, move |_cx, _payload| {
            hover.run(Command::Hover(Some(index)));
        })
        .on_click(move || choose.run(Command::Select(format)));
        if picker.current == Some(format) {
            item = item.child(crate::ui::tabs::icon(
                UiId::new("encoding.menu.check"),
                "check",
                UiRect::new(
                    row.left + 10.0,
                    row.top + 5.0,
                    row.left + 22.0,
                    row.top + 17.0,
                ),
                color,
            ));
        }
        if suggested {
            let mut style = theme::sans(theme::c().text_dim, 11.0);
            style.align = TextAlign::Right;
            item = item.child(text(
                UiRect::new(row.right - 70.0, row.top, row.right - 10.0, row.bottom),
                "Suggested",
                style,
            ));
        }
        contents = contents.child(item.child(text(
            UiRect::new(
                row.left + 10.0 + CHECK_W,
                row.top,
                row.right - if suggested { 76.0 } else { 10.0 },
                row.bottom,
            ),
            encoding_label(format),
            theme::mono(color, theme::UI_SIZE),
        )));
        top += ITEM_H;
    }
    surface = surface.child(contents);
    overlay.child(surface)
}
