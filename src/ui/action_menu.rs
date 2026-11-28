//! A list menu opened from a title bar or status bar control: a project tile or a label per
//! row, an optional hint on the right, and a rule before a row that starts a new group.
//! Events carry choices only; the app decides what each row does.
use std::sync::Arc;

use lgui::core::{
    AnimProperty, AnimationBinding, CursorIcon, EventPolicy, IconStyle, LogicalKey, NamedKey,
    SemanticRole, Semantics, UiElement, UiId,
};
use lgui::prelude::{Color, Element, ShadowStyle, UiRect, VisualStyle, panel};

use crate::ui::theme;

pub const FOCUS_ID: &str = "chrome.action-menu";

const WIDTH: f32 = 252.0;
const ROW_H: f32 = 26.0;
const RULE_H: f32 = 9.0;
const PAD: f32 = 5.0;
/// The menu card and its rows are rounded; a row sits inside the card's padding.
const CARD_RADIUS: f32 = 8.0;
const ROW_RADIUS: f32 = 5.0;
/// How strongly the text color veils a hovered or selected row.
const VEIL: f32 = 0.09;
const TEXT_X: f32 = 10.0;
const TILE: f32 = 18.0;

/// Where the menu opens relative to the control that opened it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Below,
    Above,
}

/// The square with a project's initial.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tile {
    pub initial: char,
    pub color: Color,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub label: String,
    /// An icon key, drawn where a project's tile would be.
    pub icon: Option<&'static str>,
    pub tile: Option<Tile>,
    /// Quiet text at the right: a shortcut, or "current".
    pub hint: Option<String>,
    /// The hint asks for attention.
    pub warn: bool,
    pub enabled: bool,
    /// Draw a rule above this row.
    pub rule_before: bool,
}

impl Item {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            icon: None,
            tile: None,
            hint: None,
            warn: false,
            enabled: true,
            rule_before: false,
        }
    }
}

#[derive(Clone, Copy)]
pub enum Event {
    Close,
    Select(usize),
    Move(bool),
    Accept,
}

fn height(items: &[Item]) -> f32 {
    PAD * 2.0
        + items.len() as f32 * ROW_H
        + items.iter().filter(|item| item.rule_before).count() as f32 * RULE_H
}

/// The card's rect for a control at `anchor`.
pub fn card_rect(viewport: UiRect, anchor: UiRect, placement: Placement, items: &[Item]) -> UiRect {
    let width = WIDTH.min(viewport.width() - 8.0).max(0.0);
    let height = height(items).min(viewport.height());
    let x = anchor.left.clamp(
        viewport.left + 4.0,
        (viewport.right - 4.0 - width).max(viewport.left + 4.0),
    );
    let y = match placement {
        Placement::Below => anchor.bottom + 4.0,
        Placement::Above => (anchor.top - height - 4.0).max(viewport.top),
    };
    UiRect::new(x, y, x + width, y + height)
}

pub fn render(
    viewport: UiRect,
    anchor: UiRect,
    placement: Placement,
    items: Vec<Item>,
    selected: Option<usize>,
    send: Arc<dyn Fn(Event) + Send + Sync>,
) -> Element {
    let card = card_rect(viewport, anchor, placement, &items);
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
        if let LogicalKey::Named(named) = event.key {
            match named {
                NamedKey::ArrowUp => key(Event::Move(false)),
                NamedKey::ArrowDown => key(Event::Move(true)),
                NamedKey::Enter => key(Event::Accept),
                _ => {}
            }
        }
        cx.prevent_default();
        cx.stop_propagation();
    });
    let palette = theme::c();
    let mut menu = theme::bordered(card, palette.surface, palette.border, CARD_RADIUS, 1.0)
        .shadow(
            ShadowStyle::new(Color::BLACK)
                .alpha(palette.shadow_alpha)
                .offset(0.0, 6.0)
                .blur(14.0),
        )
        .on_outside_pointer(move |_, _, _| outside(Event::Close))
        .event_policy(EventPolicy {
            focus: false,
            ..EventPolicy::INTERACTIVE
        });
    let mut y = card.top + PAD;
    for (index, item) in items.into_iter().enumerate() {
        if item.rule_before {
            menu = menu.child(panel(
                UiRect::new(
                    card.left + PAD + 2.0,
                    y + RULE_H / 2.0,
                    card.right - PAD - 2.0,
                    y + RULE_H / 2.0 + 1.0,
                ),
                VisualStyle::filled(palette.border),
            ));
            y += RULE_H;
        }
        let row = UiRect::new(card.left + PAD, y, card.right - PAD, y + ROW_H);
        y += ROW_H;
        menu = menu.child(row_element(
            row,
            index,
            item,
            selected == Some(index),
            send.clone(),
        ));
    }
    overlay.child(menu)
}

fn row_element(
    row: UiRect,
    index: usize,
    item: Item,
    selected: bool,
    send: Arc<dyn Fn(Event) + Send + Sync>,
) -> Element {
    let name = item.label.clone();
    let enabled = item.enabled;
    let element = Element::new(move |cx| {
        let palette = theme::c();
        // The hover fades in with the element's own animation, as the title bar buttons do;
        // a keyboard selection holds the fill.
        let hover = if enabled {
            cx.animation_value(AnimProperty::Hover).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let strength = if selected && enabled { 1.0 } else { hover };
        // A veil of the text color, not a fixed fill: it stands out from the menu's surface in
        // every theme, light or dark. The row's text and icon lift with it.
        let lifted = strength > 0.5;
        let fill = VisualStyle {
            fill: Some(palette.text_bright),
            fill_alpha: (strength * VEIL * 255.0_f32).round() as u8,
            ..VisualStyle::default()
        }
        .radius(ROW_RADIUS);
        let id = |part: &str| UiId::owned(format!("chrome.action-menu.{index}.{part}"));
        let middle = (row.top + row.bottom) / 2.0;
        let mut text_left = row.left + TEXT_X;
        let mut node = UiElement::panel(cx.id, row, fill);
        if let Some(tile) = item.tile {
            let rect = UiRect::new(
                row.left + 6.0,
                middle - TILE / 2.0,
                row.left + 6.0 + TILE,
                middle + TILE / 2.0,
            );
            node = node.child(
                UiElement::panel(
                    id("tile"),
                    rect,
                    VisualStyle::filled(tile.color).radius(4.0),
                )
                .child(UiElement::text(
                    id("initial"),
                    rect,
                    tile.initial.to_string(),
                    crate::ui::highlight_menu::centered(palette.sidebar, 10.0, 700),
                )),
            );
            text_left = rect.right + 8.0;
        } else if let Some(key) = item.icon {
            // The same slot as a tile, so labels line up in a menu with both.
            let slot = UiRect::new(
                row.left + 6.0,
                middle - TILE / 2.0,
                row.left + 6.0 + TILE,
                middle + TILE / 2.0,
            );
            let color = match (enabled, lifted) {
                (false, _) => palette.text_ghost,
                (true, true) => palette.text_soft,
                (true, false) => palette.text_muted,
            };
            node = node.child(
                UiElement::icon(id("icon"), theme::icon_rect(slot), key)
                    .icon_style(IconStyle::new(color)),
            );
            text_left = slot.right + 8.0;
        }
        let label_color = match (enabled, lifted) {
            (false, _) => palette.text_ghost,
            (true, true) => palette.text_bright,
            (true, false) => palette.text_soft,
        };
        let hint_width = item.hint.as_ref().map_or(0.0, |hint| {
            crate::ui::tabs::measure(hint, theme::SMALL, 400) + 8.0
        });
        node = node.child(UiElement::text(
            id("label"),
            UiRect::new(text_left, row.top, row.right - hint_width - 8.0, row.bottom),
            item.label.clone(),
            theme::mono(label_color, theme::SMALL + 1.0),
        ));
        if let Some(hint) = &item.hint {
            let color = if item.warn {
                palette.warning
            } else if lifted {
                palette.text_dim
            } else {
                palette.text_faint
            };
            node = node.child(UiElement::text(
                id("hint"),
                UiRect::new(
                    row.right - hint_width - 8.0,
                    row.top,
                    row.right - 8.0,
                    row.bottom,
                ),
                hint.clone(),
                theme::mono_right(color, theme::SMALL),
            ));
        }
        node
    })
    .key(format!("chrome.action-menu.row.{index}"))
    .animation(AnimationBinding::new(AnimProperty::Hover, 0.0, 1.0))
    .semantics(Semantics::new(SemanticRole::Button).name(name));
    if !enabled {
        return element;
    }
    element
        .event_policy(EventPolicy {
            focus: false,
            ..EventPolicy::INTERACTIVE
        })
        .cursor(CursorIcon::Pointer)
        .on_click(move || send(Event::Select(index)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_grows_with_rows_and_rules() {
        let mut items = vec![Item::new("One"), Item::new("Two")];
        let plain = height(&items);
        items[1].rule_before = true;
        assert_eq!(height(&items), plain + RULE_H);
    }

    #[test]
    fn a_menu_above_its_control_ends_above_it_and_stays_in_the_window() {
        let viewport = UiRect::new(0.0, 0.0, 800.0, 600.0);
        let anchor = UiRect::new(12.0, 576.0, 36.0, 600.0);
        let items = vec![Item::new("One"), Item::new("Two")];
        let card = card_rect(viewport, anchor, Placement::Above, &items);
        assert!(card.bottom < anchor.top);
        assert!(card.left >= viewport.left && card.right <= viewport.right);
        let below = card_rect(
            viewport,
            UiRect::new(6.0, 0.0, 90.0, 32.0),
            Placement::Below,
            &items,
        );
        assert!(below.top > 32.0);
    }
}
