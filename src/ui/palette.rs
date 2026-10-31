//! Command palette: a card centered near the top of the window with a search
//! field over a short list of commands.
//!
//! The view draws a `Frame` the app prepares and reports what the user did as
//! a `Command`; it knows nothing about how commands are found or run.
use std::sync::Arc;

use lgui::core::{
    CursorIcon, EventPolicy, SemanticRole, Semantics, UiEventKind, UiEventPayload, UiFocusHandle,
    UiId, clip,
};
use lgui::prelude::{Color, Element, ShadowStyle, TextAlign, UiRect, VisualStyle, panel, text};

use crate::ui::components::input::{
    self, AccessInputOptions, InputBinding, InputState, InputStyle,
};
use crate::ui::components::state_access::StateAccess;
use crate::ui::tabs::measure;
use crate::ui::theme;

const CARD_W: f32 = 620.0;
const SIDE_MARGIN: f32 = 12.0;
const QUERY_H: f32 = 40.0;
const ROW_H: f32 = 26.0;
const HEADER_H: f32 = 22.0;
const LIST_PAD: f32 = 4.0;
const EMPTY_H: f32 = 64.0;
const FOOT_H: f32 = 28.0;
const PAD_X: f32 = 14.0;
const GLYPH: f32 = 16.0;
const CHIP_H: f32 = 18.0;

/// One command in the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// Position among the matches, which is what `Command` refers to.
    pub index: usize,
    pub title: String,
    pub category: String,
    pub description: String,
    /// The key binding's keys, e.g. `Ctrl`, `Shift`, `P`.
    pub keys: Vec<String>,
    /// Characters of `title` the query matched.
    pub positions: Vec<usize>,
    pub selected: bool,
    pub extension: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    Header(String),
    Item(Item),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    /// The visible part of the list.
    pub rows: Vec<Row>,
    /// `3 / 22`, or `0` when nothing matches.
    pub position: String,
    /// The query that matched nothing.
    pub empty: Option<String>,
    /// What the empty field asks for: commands, themes.
    pub placeholder: &'static str,
}

pub enum Command {
    Close,
    /// Pointer over the match at this position.
    Hover(usize),
    /// Choose the match at this position.
    Run(usize),
    /// Wheel step: `true` moves down.
    Scroll(bool),
}

#[derive(Clone)]
pub struct Binding {
    input: StateAccess<InputState>,
    read: Arc<dyn Fn() -> Frame + Send + Sync>,
    execute: Arc<dyn Fn(Command) + Send + Sync>,
}

impl Binding {
    pub fn new(
        input: StateAccess<InputState>,
        read: impl Fn() -> Frame + Send + Sync + 'static,
        execute: impl Fn(Command) + Send + Sync + 'static,
    ) -> Self {
        Self {
            input,
            read: Arc::new(read),
            execute: Arc::new(execute),
        }
    }

    pub fn frame(&self) -> Frame {
        (self.read)()
    }

    pub fn run(&self, command: Command) {
        (self.execute)(command);
    }
}

/// `a` with `amount` (0..=1) of `b` mixed in.
fn mix(a: Color, b: Color, amount: f32) -> Color {
    let channel = |shift: u32| {
        let from = ((a.0 >> shift) & 0xff) as f32;
        let to = ((b.0 >> shift) & 0xff) as f32;
        (from + (to - from) * amount).round() as u32
    };
    Color((channel(16) << 16) | (channel(8) << 8) | channel(0))
}

/// A color for a category, the same every time.
fn category_color(category: &str) -> Color {
    let palette = theme::c();
    let choices = [
        palette.accent,
        Color(palette.ansi[4]),
        Color(palette.ansi[5]),
        Color(palette.ansi[3]),
        Color(palette.ansi[2]),
        Color(palette.ansi[6]),
    ];
    let sum: usize = category.bytes().map(usize::from).sum();
    choices[sum % choices.len()]
}

/// Width of `prefix` in the interface font. Measured against a trailing
/// letter, because shaping drops trailing spaces and measures "" unreliably.
fn advance(prefix: &str) -> f32 {
    if prefix.is_empty() {
        return 0.0;
    }
    measure(&format!("{prefix}x"), theme::UI_SIZE, 400) - measure("x", theme::UI_SIZE, 400)
}

fn row_height(row: &Row) -> f32 {
    match row {
        Row::Header(_) => HEADER_H,
        Row::Item(_) => ROW_H,
    }
}

fn centered(color: Color, size: f32, weight: i32) -> lgui::prelude::TextStyle {
    let mut style = theme::mono(color, size);
    style.weight = weight;
    style.align = TextAlign::Center;
    style
}

/// A key as a small bordered cap, ending at `right`. Returns its left edge.
fn key_cap(card: Element, label: &str, right: f32, middle: f32) -> (Element, f32) {
    let width = (measure(label, theme::SMALL, 400) + 10.0).max(CHIP_H);
    let rect = UiRect::new(
        right - width,
        middle - CHIP_H / 2.0,
        right,
        middle + CHIP_H / 2.0,
    );
    let cap = theme::bordered(rect, theme::c().surface, theme::c().border, 4.0, 1.0).child(text(
        rect,
        label.to_owned(),
        centered(theme::c().text_muted, theme::SMALL, 400),
    ));
    (card.child(cap), rect.left)
}

fn item_row(rect: UiRect, item: &Item, state: &Binding) -> Element {
    let palette = theme::c();
    let hover = state.clone();
    let run = state.clone();
    let index = item.index;
    let mut row = panel(
        rect,
        if item.selected {
            VisualStyle::filled(palette.selection)
        } else {
            VisualStyle::default()
        },
    )
    .event_policy(EventPolicy::INTERACTIVE)
    .cursor(CursorIcon::Pointer)
    .semantics(Semantics::new(SemanticRole::MenuItem).name(item.title.clone()))
    .on_event(UiEventKind::PointerMove, move |_cx, _payload| {
        hover.run(Command::Hover(index));
    })
    .on_click(move || run.run(Command::Run(index)));
    if item.selected {
        row = row.child(panel(
            UiRect::new(rect.left, rect.top + 3.0, rect.left + 2.0, rect.bottom - 3.0),
            VisualStyle::filled(palette.accent).radius(1.0),
        ));
    }

    // Category mark.
    let tint = category_color(&item.category);
    let glyph = UiRect::new(
        rect.left + PAD_X,
        rect.top + (ROW_H - GLYPH) / 2.0,
        rect.left + PAD_X + GLYPH,
        rect.top + (ROW_H + GLYPH) / 2.0,
    );
    let initial = item
        .category
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    row = row.child(
        panel(
            glyph,
            VisualStyle::filled(mix(palette.surface, tint, 0.16)).radius(4.0),
        )
        .child(text(glyph, initial, centered(tint, theme::SMALL, 700))),
    );

    // Keys, or the category when the command has none.
    let middle = (rect.top + rect.bottom) / 2.0;
    let mut right = rect.right - PAD_X;
    if item.keys.is_empty() {
        let label = item.category.as_str();
        let width = measure(label, theme::SMALL, 400) + 4.0;
        let color = if item.extension {
            palette.accent
        } else {
            palette.text_faint
        };
        row = row.child(text(
            UiRect::new(right - width, rect.top, right, rect.bottom),
            label.to_owned(),
            theme::mono_right(color, theme::SMALL),
        ));
        right -= width + 10.0;
    } else {
        for key in item.keys.iter().rev() {
            let (next, left) = key_cap(row, key, right, middle);
            row = next;
            right = left - 3.0;
        }
        right -= 7.0;
    }

    // The title, with the characters the query matched drawn apart.
    let left = glyph.right + 10.0;
    let base = if item.selected {
        palette.text_bright
    } else {
        palette.text_soft
    };
    let chars: Vec<char> = item.title.chars().collect();
    let mut start = 0;
    while start < chars.len() {
        let matched = item.positions.contains(&start);
        let mut end = start;
        while end < chars.len() && item.positions.contains(&end) == matched {
            end += 1;
        }
        let before: String = chars[..start].iter().collect();
        let run: String = chars[start..end].iter().collect();
        let x = left + advance(&before);
        let style = if matched {
            theme::mono_bold(palette.accent, theme::UI_SIZE)
        } else {
            theme::mono(base, theme::UI_SIZE)
        };
        // Each run is left-aligned from `x`; the box runs on to the keys so
        // that a width estimate slightly short of the real glyphs never hides it.
        row = row.child(text(
            UiRect::new(x, rect.top, right.max(x + 1.0), rect.bottom),
            run,
            style,
        ));
        start = end;
    }

    // What it does, in what room is left.
    let note_left = left + measure(&item.title, theme::UI_SIZE, 400) + 12.0;
    if note_left + 24.0 < right {
        row = row.child(text(
            UiRect::new(note_left, rect.top, right, rect.bottom),
            item.description.clone(),
            theme::mono(palette.text_faint, theme::SMALL + 1.0),
        ));
    }
    row
}

pub fn render(
    viewport: UiRect,
    state: Binding,
    input_focus: UiFocusHandle,
    input_id: UiId,
) -> Element {
    let palette = theme::c();
    let frame = state.frame();
    let list_h = if frame.rows.is_empty() {
        EMPTY_H
    } else {
        LIST_PAD * 2.0 + frame.rows.iter().map(row_height).sum::<f32>()
    };
    let card_h = QUERY_H + 1.0 + list_h + FOOT_H;
    let card_w = CARD_W.min((viewport.width() - SIDE_MARGIN * 2.0).max(0.0));
    let left = viewport.left + (viewport.width() - card_w) / 2.0;
    let top = (viewport.top + viewport.height() * 0.13).max(viewport.top + 36.0);
    let top = top.min((viewport.bottom - card_h - SIDE_MARGIN).max(viewport.top));
    let card = UiRect::new(left, top, left + card_w, top + card_h);

    // Invisible backdrop: a press outside the card closes the palette.
    let close = state.clone();
    let overlay = panel(viewport, VisualStyle::default())
        .event_policy(EventPolicy::INTERACTIVE)
        .on_pointer_down_with_button(move |_cx, pointer, _button| {
            if !card.contains(pointer.point) {
                close.run(Command::Close);
            }
        });

    let wheel = state.clone();
    let mut surface = theme::bordered(card, palette.surface, palette.border, 8.0, 1.0)
        .shadow(
            ShadowStyle::new(Color::BLACK)
                .alpha(palette.shadow_alpha)
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

    // Search field: a prompt, the text, and where the selection stands.
    let query = UiRect::new(card.left + 1.0, card.top + 1.0, card.right - 1.0, card.top + QUERY_H);
    surface = surface.child(crate::ui::tabs::icon(
        UiId::new("palette.prompt"),
        "command",
        theme::icon_rect(UiRect::new(
            query.left + PAD_X,
            query.top,
            query.left + PAD_X + theme::ICON_SIZE,
            query.bottom,
        )),
        palette.accent,
    ));
    let count_w = measure(&frame.position, theme::SMALL, 400) + 4.0;
    surface = surface.child(text(
        UiRect::new(query.right - PAD_X - count_w, query.top, query.right - PAD_X, query.bottom),
        frame.position.clone(),
        theme::mono_right(palette.text_dim, theme::SMALL),
    ));
    surface = surface.child(input::render_access(
        UiRect::new(
            query.left + PAD_X + theme::ICON_SIZE + 4.0,
            query.top + 2.0,
            query.right - PAD_X - count_w - 8.0,
            query.bottom - 2.0,
        ),
        input_id,
        input_focus,
        InputBinding::new_access(state.input.clone(), |input| input, |input| input),
        AccessInputOptions {
            label: "Command",
            placeholder: frame.placeholder,
            style: InputStyle {
                text: theme::mono(palette.text_bright, theme::UI_SIZE + 1.0),
                placeholder: theme::mono(palette.text_faint, theme::UI_SIZE + 1.0),
                caret: palette.accent,
                // The row highlight is too close to the card's own color to read as a selection.
                selection: mix(palette.surface, palette.accent, 0.45),
            },
            on_submit: None,
            on_cancel: None,
            on_blur: None,
        },
    ));
    surface = surface.child(panel(
        UiRect::new(card.left + 1.0, query.bottom, card.right - 1.0, query.bottom + 1.0),
        VisualStyle::filled(palette.border),
    ));

    // The list.
    let list = UiRect::new(card.left + 1.0, query.bottom + 1.0, card.right - 1.0, query.bottom + 1.0 + list_h);
    if let Some(query) = &frame.empty {
        let message = if query.is_empty() {
            "No commands".to_owned()
        } else {
            format!("No commands match \"{query}\"")
        };
        surface = surface.child(text(
            UiRect::new(list.left + PAD_X, list.top, list.right - PAD_X, list.bottom),
            message,
            theme::mono(palette.text_dim, theme::UI_SIZE),
        ));
    } else {
        let mut rows = clip(list, 0.0, 0.0);
        let mut y = list.top + LIST_PAD;
        for row in &frame.rows {
            let height = row_height(row);
            let rect = UiRect::new(list.left, y, list.right, y + height);
            match row {
                Row::Header(label) => {
                    rows = rows.child(text(
                        UiRect::new(
                            rect.left + PAD_X,
                            rect.top + 4.0,
                            rect.right - PAD_X,
                            rect.bottom,
                        ),
                        label.to_uppercase(),
                        theme::mono_bold(palette.text_faint, theme::SMALL).tracking(0.8),
                    ));
                }
                Row::Item(item) => rows = rows.child(item_row(rect, item, &state)),
            }
            y += height;
        }
        surface = surface.child(rows);
    }

    // Footer hints.
    let foot = UiRect::new(card.left + 1.0, card.bottom - FOOT_H, card.right - 1.0, card.bottom - 1.0);
    surface = surface.child(panel(
        UiRect::new(foot.left, foot.top, foot.right, foot.top + 1.0),
        VisualStyle::filled(palette.border),
    ));
    surface = surface.child(text(
        UiRect::new(foot.left + PAD_X, foot.top, foot.right - PAD_X, foot.bottom),
        "↑ ↓ Select    Tab Complete    Enter Run    Esc Close",
        theme::mono(palette.text_dim, theme::SMALL),
    ));
    overlay.child(surface)
}
