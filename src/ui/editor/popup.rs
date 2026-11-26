//! A list that opens over the code: completion items, definitions, symbols, code
//! actions. It never takes focus from the editor; the keyboard reaches it
//! through the keymap, the mouse through its rows.
use super::chrome::{self, FOOTER_H};
use super::view::card_origin;
use crate::ui::theme;
use lgui::core::{CursorIcon, EventPolicy, UiEventContext, WheelUnit, clip};
use lgui::prelude::{Element, UiRect, VisualStyle, panel, text};
use std::sync::Arc;

pub(crate) const ROW_H: f32 = 24.0;
const PAD: f32 = 4.0;
/// The slot of a kind chip, with the gap after it.
const BADGE_W: f32 = 28.0;
const CHIP_W: f32 = 20.0;
const CHIP_H: f32 = 16.0;
const MIN_W: f32 = 220.0;
const MAX_W: f32 = 640.0;
const INPUT_H: f32 = 26.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub label: String,
    /// Faint text after the label (a signature, a path, a source).
    pub detail: String,
    /// One or two characters naming the kind of thing.
    pub badge: &'static str,
    /// Character positions of the label to emphasise (the fuzzy match).
    pub matches: Vec<usize>,
    pub faint: bool,
    /// A small outlined label after the name (such as "auto-import").
    pub tag: &'static str,
}

#[derive(Clone)]
pub(crate) struct View {
    pub panel: bool,
    pub rows: Arc<[Row]>,
    pub selected: usize,
    /// The first row shown.
    pub scroll: usize,
    /// How many rows fit at most.
    pub visible: usize,
    /// The text typed into the popup's own field, when it has one.
    pub query: Option<String>,
    pub title: Option<String>,
    pub empty: String,
    /// The offset of the text it opens beside; at the top centre, like a command
    /// palette, when there is none.
    pub anchor: Option<usize>,
    /// A bar under the list with the keys that work on it and the item count.
    pub footer: bool,
}

pub(crate) enum Event {
    Focus,
    Hover(usize),
    Pick(usize),
    Dismiss,
    Scroll(isize),
}

pub(crate) type Sender = Arc<dyn Fn(Event) + Send + Sync>;

/// The hue of a kind chip, from the short kind name of the row: callables blue,
/// types amber, values purple, keywords pink, snippets green.
pub(crate) fn chip_tint(badge: &str) -> lgui::prelude::Color {
    let c = theme::c();
    match badge {
        "fn" | "m" | "op" => lgui::prelude::Color(0x60a5fa),
        "C" | "I" | "S" | "E" | "T" | "{}" | "ev" => c.warning,
        "v" | "f" | "V" | "p" | "c" | "e" => lgui::prelude::Color(0xa78bfa),
        "k" => lgui::prelude::Color(0xf472b6),
        "sn" => lgui::prelude::Color(0x4ade80),
        _ => c.accent,
    }
}

/// Width in the proportional font the popup draws with, at `weight`.
fn measure_weight(text: &str, weight: i32) -> f32 {
    crate::ui::tabs::measure_sans(text, theme::UI_SIZE, weight)
}

fn measure(text: &str) -> f32 {
    measure_weight(text, 400)
}

/// The width and height of the popup for `view`, before it is placed.
fn size(view: &View, area: UiRect) -> (f32, f32) {
    let shown = view.rows.len().min(view.visible).max(1);
    let widest = view
        .rows
        .iter()
        .map(|row| {
            let tag = if row.tag.is_empty() {
                0.0
            } else {
                measure(row.tag) + 18.0
            };
            BADGE_W + measure(&row.label) + tag + 20.0 + measure(&row.detail)
        })
        .fold(0.0f32, f32::max);
    let width = (widest + 2.0 * PAD + 16.0)
        .clamp(MIN_W, MAX_W)
        .min((area.width() - 8.0).max(120.0));
    let header = if view.query.is_some() { INPUT_H } else { 0.0 }
        + if view.title.is_some() { ROW_H } else { 0.0 };
    let footer = if view.footer { FOOTER_H } else { 0.0 };
    (width, header + shown as f32 * ROW_H + 2.0 * PAD + footer)
}

/// Draws the popup and returns where it is.
pub(crate) fn render(
    view: &View,
    anchor: Option<UiRect>,
    area: UiRect,
    send: Sender,
) -> (UiRect, Element) {
    let (mut width, mut height) = size(view, area);
    if view.panel {
        width = (area.width() - 8.0).max(0.0);
        height = height.min(
            (area.height() * 0.45)
                .max(90.0)
                .min((area.height() - 8.0).max(0.0)),
        );
    }
    let (left, top) = if view.panel {
        (area.left + 4.0, area.bottom - height - 4.0)
    } else {
        match anchor {
            Some(anchor) => card_origin(anchor, area, width, height),
            None => (
                area.left + ((area.width() - width) / 2.0).max(4.0),
                area.top + 24.0,
            ),
        }
    };
    let card = UiRect::new(left, top, left + width, top + height);
    let policy = EventPolicy {
        focus: false,
        ..EventPolicy::INTERACTIVE
    };
    let wheel = send.clone();
    let mut body = chrome::surface(card)
        .key("language-popup")
        .event_policy(policy)
        .on_wheel(move |cx, delta| {
            let rows = match delta.unit {
                WheelUnit::Lines => delta.y.round() as isize,
                WheelUnit::Pixels => (delta.y / ROW_H).round() as isize,
            };
            if rows != 0 {
                wheel(Event::Scroll(-rows));
            }
            cx.stop_propagation();
        });
    let mut y = top + PAD;
    if view.panel {
        let r = UiRect::new(
            card.right - 26.0,
            card.top + 4.0,
            card.right - 4.0,
            card.top + 26.0,
        );
        let close = send.clone();
        body = body.child(
            panel(r, VisualStyle::default())
                .event_policy(policy)
                .cursor(CursorIcon::Pointer)
                .on_click(move |cx: &mut UiEventContext| {
                    cx.stop_propagation();
                    close(Event::Dismiss);
                })
                .child(crate::ui::tabs::close_glyph(
                    lgui::core::UiId::new("language-popup.close"),
                    r,
                    theme::c().text_soft,
                )),
        );
    }
    if let Some(title) = &view.title {
        body = body.child(text(
            UiRect::new(left + PAD + 4.0, y, left + width - PAD, y + ROW_H),
            title.clone(),
            theme::sans(theme::c().text_faint, theme::SMALL),
        ));
        y += ROW_H;
    }
    if let Some(query) = &view.query {
        let field = UiRect::new(left + PAD, y, left + width - PAD, y + INPUT_H - 4.0);
        body = body.child(
            panel(field, VisualStyle::filled(theme::c().bg).radius(3.0))
                .event_policy(policy)
                .on_pointer_down({
                    let focus = send.clone();
                    move |cx, _| {
                        cx.stop_propagation();
                        focus(Event::Focus);
                    }
                })
                .child(text(
                    UiRect::new(field.left + 6.0, field.top, field.right - 6.0, field.bottom),
                    format!("{query}\u{258F}"),
                    theme::sans(theme::c().text_bright, theme::UI_SIZE),
                )),
        );
        y += INPUT_H;
    }
    if view.rows.is_empty() {
        body = body.child(text(
            UiRect::new(left + PAD + 4.0, y, left + width - PAD, y + ROW_H),
            view.empty.clone(),
            theme::sans(theme::c().text_muted, theme::UI_SIZE),
        ));
    }
    let footer_h = if view.footer { FOOTER_H } else { 0.0 };
    let list = UiRect::new(
        left + PAD,
        y,
        left + width - PAD,
        card.bottom - PAD - footer_h,
    );
    let mut rows = clip(list, 0.0, 0.0);
    for (n, row) in view
        .rows
        .iter()
        .enumerate()
        .skip(view.scroll)
        .take(view.visible)
    {
        let r = UiRect::new(
            list.left,
            list.top + (n - view.scroll) as f32 * ROW_H,
            list.right,
            list.top + (n - view.scroll + 1) as f32 * ROW_H,
        );
        let hover = send.clone();
        let pick = send.clone();
        let selected = n == view.selected;
        let style = if selected {
            VisualStyle::filled(theme::c().selection).radius(5.0)
        } else {
            VisualStyle::default()
        };
        let colour = if row.faint {
            theme::c().text_faint
        } else if selected {
            theme::c().text_bright
        } else {
            theme::c().text_soft
        };
        let mut item = panel(r, style)
            .event_policy(policy)
            .cursor(CursorIcon::Pointer)
            .on_pointer_move(move |_, _| hover(Event::Hover(n)))
            .on_click(move |cx: &mut UiEventContext| {
                pick(Event::Pick(n));
                cx.stop_propagation();
            });
        if selected {
            item = item.child(panel(
                UiRect::new(r.left + 1.0, r.top + 6.0, r.left + 3.0, r.bottom - 6.0),
                VisualStyle::filled(theme::c().accent).radius(1.0),
            ));
        }
        if !row.badge.is_empty() {
            // A chip tinted by the kind, as in the command palette's rows.
            let tint = chip_tint(row.badge);
            let chip = UiRect::new(
                r.left + 6.0,
                r.top + (ROW_H - CHIP_H) / 2.0,
                r.left + 6.0 + CHIP_W,
                r.top + (ROW_H + CHIP_H) / 2.0,
            );
            let mut style = theme::mono(tint, theme::SMALL);
            style.align = lgui::prelude::TextAlign::Center;
            item = item.child(
                panel(
                    chip,
                    VisualStyle::filled(crate::ui::palette::mix(theme::c().surface, tint, 0.16))
                        .radius(4.0),
                )
                .child(text(chip, row.badge, style)),
            );
        }
        let label_left = r.left + BADGE_W;
        // The label in runs: the fuzzy-matched characters bold and brighter,
        // each run placed after the measured width of the one before it, so
        // nothing is drawn twice.
        let mut label_right = label_left;
        let mut run = String::new();
        let mut run_matched = false;
        let mut runs: Vec<(String, bool)> = Vec::new();
        for (at, ch) in row.label.chars().enumerate() {
            let matched = row.matches.contains(&at);
            if !run.is_empty() && matched != run_matched {
                runs.push((std::mem::take(&mut run), run_matched));
            }
            run_matched = matched;
            run.push(ch);
        }
        if !run.is_empty() {
            runs.push((run, run_matched));
        }
        for (piece, matched) in runs {
            let width = measure_weight(&piece, if matched { 600 } else { 400 });
            item = item.child(text(
                UiRect::new(label_right, r.top, label_right + width + 2.0, r.bottom),
                piece,
                if matched {
                    theme::sans_semibold(theme::c().accent, theme::UI_SIZE)
                } else {
                    theme::sans(colour, theme::UI_SIZE)
                },
            ));
            label_right += width;
        }
        if !row.tag.is_empty() {
            let w = measure(row.tag) + 10.0;
            let tag = UiRect::new(
                label_right + 8.0,
                r.top + (ROW_H - 16.0) / 2.0,
                label_right + 8.0 + w,
                r.top + (ROW_H + 16.0) / 2.0,
            );
            if tag.right < r.right - 8.0 {
                let mut style = theme::sans(theme::c().text_dim, theme::SMALL);
                style.align = lgui::prelude::TextAlign::Center;
                item = item.child(
                    theme::bordered(tag, theme::c().surface, theme::c().border, 4.0, 1.0)
                        .child(text(tag, row.tag, style)),
                );
                label_right = tag.right - 8.0 - 4.0;
            }
        }
        if !row.detail.is_empty() {
            // Right-aligned, so the short types line up down the list.
            let x = label_right + 12.0;
            if x < r.right - 8.0 {
                let mut style = theme::sans(
                    if selected {
                        theme::c().text_muted
                    } else {
                        theme::c().text_faint
                    },
                    theme::SMALL,
                );
                style.align = lgui::prelude::TextAlign::Right;
                item = item.child(text(
                    UiRect::new(x, r.top, r.right - 8.0, r.bottom),
                    row.detail.clone(),
                    style,
                ));
            }
        }
        rows = rows.child(item);
    }
    body = body.child(rows);
    if view.footer {
        let count = if view.rows.len() == 1 {
            "1 item".to_owned()
        } else {
            format!("{} items", view.rows.len())
        };
        body = chrome::Footer::new(card)
            .icon_cap("arrow-up")
            .icon_cap("arrow-down")
            .word("select")
            .cap("Tab")
            .word("accept")
            .note(&count)
            .attach(body);
    }
    if view.panel {
        body = body.on_pointer_down_with_button(|cx, _, _| cx.stop_propagation());
    }
    let mut out = if view.panel {
        body
    } else {
        panel(area, VisualStyle::default()).child(body)
    };
    let rect = card;
    // A press outside the popup dismisses it.
    let dismiss = send;
    if !view.panel {
        out = out
            .event_policy(policy)
            .on_pointer_down_with_button(move |cx, p, _| {
                if !rect.contains(p.point) {
                    dismiss(Event::Dismiss);
                } else {
                    cx.stop_propagation();
                }
            });
    }
    (rect, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(n: usize) -> Arc<[Row]> {
        (0..n)
            .map(|i| Row {
                label: format!("item {i}"),
                detail: String::new(),
                badge: "f",
                matches: Vec::new(),
                faint: false,
                tag: "",
            })
            .collect::<Vec<_>>()
            .into()
    }

    fn view(count: usize, visible: usize) -> View {
        View {
            panel: false,
            rows: rows(count),
            selected: 0,
            scroll: 0,
            visible,
            query: None,
            title: None,
            empty: "none".into(),
            anchor: None,
            footer: false,
        }
    }

    #[test]
    fn the_height_follows_the_rows_shown_not_all_rows() {
        let area = UiRect::new(0.0, 0.0, 800.0, 600.0);
        let (_, few) = size(&view(3, 8), area);
        let (_, many) = size(&view(300, 8), area);
        assert_eq!(few, 3.0 * ROW_H + 2.0 * PAD);
        assert_eq!(many, 8.0 * ROW_H + 2.0 * PAD);
        // An empty list still has room for its message.
        let (_, none) = size(&view(0, 8), area);
        assert_eq!(none, ROW_H + 2.0 * PAD);
    }

    #[test]
    fn a_field_and_a_title_add_a_header() {
        let area = UiRect::new(0.0, 0.0, 800.0, 600.0);
        let mut v = view(2, 8);
        let (_, plain) = size(&v, area);
        v.query = Some(String::new());
        v.title = Some("Symbols".into());
        let (_, headed) = size(&v, area);
        assert_eq!(headed - plain, INPUT_H + ROW_H);
    }

    #[test]
    fn the_width_is_bounded_by_the_area_and_the_maximum() {
        let mut v = view(1, 8);
        v.rows = vec![Row {
            label: "x".repeat(400),
            detail: String::new(),
            badge: "",
            matches: Vec::new(),
            faint: false,
            tag: "",
        }]
        .into();
        let (wide, _) = size(&v, UiRect::new(0.0, 0.0, 2000.0, 600.0));
        assert_eq!(wide, MAX_W);
        let (narrow, _) = size(&v, UiRect::new(0.0, 0.0, 300.0, 600.0));
        assert_eq!(narrow, 292.0);
    }
}
