//! The language panel: a card centered near the top of the window with the
//! languages in use, every language with syntax, and the ones to install.
//!
//! The view draws a `Frame` the app prepares and reports what the user did as a
//! `Command`; it knows nothing about languages or the registry.
use std::sync::Arc;

use lgui::core::{
    CursorIcon, EventPolicy, SemanticRole, Semantics, UiEventKind, UiFocusHandle, UiId,
};
use lgui::prelude::{Color, Element, ShadowStyle, TextAlign, UiRect, VisualStyle, panel, text};

use crate::ui::components::input::{
    self, AccessInputOptions, InputBinding, InputState, InputStyle,
};
use crate::ui::components::state_access::StateAccess;
use crate::ui::tabs::{icon, measure};
use crate::ui::theme;

const CARD_W: f32 = 560.0;
const SIDE_MARGIN: f32 = 12.0;
const PAD_X: f32 = 14.0;
const HEADER_H: f32 = 28.0;
/// The last group's header holds the filter, so it is taller; the field has the
/// same room above and below.
const MORE_HEADER_H: f32 = 34.0;
const SECTION_PAD: f32 = 10.0;
const BADGE_H: f32 = 22.0;
const BADGE_GAP: f32 = 8.0;
const MIN_BADGE_W: f32 = 42.0;
const ROW_H: f32 = 30.0;
const NOTE_H: f32 = 24.0;
const FOOT_H: f32 = 28.0;
const FILTER_W: f32 = 168.0;
const FILTER_H: f32 = 24.0;
const MAX_ROWS: usize = 8;

/// A language that works: only its short mark is drawn, and its name is told when
/// the pointer is over it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Badge {
    pub name: String,
    pub text: String,
    /// What the color follows, so a language looks the same installed or not.
    pub key: String,
}

/// A language that can be installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub extension: String,
    pub name: String,
    pub text: String,
    pub key: String,
    pub note: String,
    pub installing: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    /// Installed, but languages are read when Loog starts.
    pub active: Vec<Badge>,
    pub all: Vec<Badge>,
    pub recommended: Vec<Row>,
    /// The last group, after the filter.
    pub more: Vec<Row>,
    pub more_total: usize,
    pub more_open: bool,
    pub filtering: bool,
    /// The name of the badge the pointer is over.
    pub hover: Option<String>,
    /// There is a registry index to offer languages from.
    pub registry_ready: bool,
}

pub enum Command {
    Close,
    ToggleMore,
    Hover(Option<String>),
    Install(String),
    OpenExtensions,
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
pub(crate) fn mix(a: Color, b: Color, amount: f32) -> Color {
    let channel = |shift: u32| {
        let from = ((a.0 >> shift) & 0xff) as f32;
        let to = ((b.0 >> shift) & 0xff) as f32;
        (from + (to - from) * amount).round() as u32
    };
    Color((channel(16) << 16) | (channel(8) << 8) | channel(0))
}

/// The color of a language's badge: the theme's where it has one, else one picked
/// by the name, the same every time.
pub fn tint(key: &str) -> Color {
    let palette = theme::c();
    match key {
        "rust" => return palette.badge.rust,
        "typescript" => return palette.badge.typescript,
        "javascript" => return palette.badge.javascript,
        "csharp" => return palette.badge.csharp,
        _ => {}
    }
    let choices = [
        palette.accent,
        Color(palette.ansi[1]),
        Color(palette.ansi[2]),
        Color(palette.ansi[3]),
        Color(palette.ansi[4]),
        Color(palette.ansi[5]),
        Color(palette.ansi[6]),
    ];
    let sum: usize = key.bytes().map(usize::from).sum();
    choices[sum % choices.len()]
}

pub(crate) fn centered(color: Color, size: f32, weight: i32) -> lgui::prelude::TextStyle {
    let mut style = theme::mono(color, size);
    style.weight = weight;
    style.align = TextAlign::Center;
    style
}

fn badge_width(label: &str) -> f32 {
    (measure(label, theme::SMALL + 1.0, 700) + 16.0).max(MIN_BADGE_W)
}

/// Where each badge goes in a row that wraps at `available`: offsets from the
/// top-left of the flow, and the flow's height.
fn flow(badges: &[Badge], available: f32) -> (Vec<(f32, f32)>, f32) {
    let (mut x, mut y) = (0.0, 0.0);
    let mut places = Vec::with_capacity(badges.len());
    for badge in badges {
        let width = badge_width(&badge.text);
        if x > 0.0 && x + width > available {
            x = 0.0;
            y += BADGE_H + BADGE_GAP;
        }
        places.push((x, y));
        x += width + BADGE_GAP;
    }
    let height = if badges.is_empty() { 0.0 } else { y + BADGE_H };
    (places, height)
}

/// The room a group of badges takes: its header, the badges or a line saying
/// there are none, and some air below.
fn badges_height(badges: &[Badge], available: f32) -> f32 {
    let (_, height) = flow(badges, available);
    HEADER_H + if badges.is_empty() { NOTE_H } else { height } + SECTION_PAD
}

fn rows_height(count: usize) -> f32 {
    HEADER_H + count as f32 * ROW_H + SECTION_PAD / 2.0
}

fn shown(frame: &Frame) -> bool {
    frame.more_open || frame.filtering
}

/// What the last group's body says instead of rows, when it has none to show.
fn more_note(frame: &Frame) -> Option<&'static str> {
    if !frame.registry_ready {
        Some("The extension registry is not available yet.")
    } else if frame.more.is_empty() {
        Some(if frame.filtering {
            "No language matches that."
        } else {
            "Every language of the registry is installed."
        })
    } else if frame.more.len() > MAX_ROWS {
        Some("More match; keep typing to narrow the list.")
    } else {
        None
    }
}

fn more_height(frame: &Frame) -> f32 {
    let mut height = MORE_HEADER_H;
    if shown(frame) {
        height += frame.more.len().min(MAX_ROWS) as f32 * ROW_H;
        if more_note(frame).is_some() {
            height += NOTE_H;
        }
        height += SECTION_PAD / 2.0;
    }
    height
}

fn card_height(frame: &Frame, available: f32) -> f32 {
    let mut height = 1.0 + FOOT_H;
    height += badges_height(&frame.active, available);
    height += 1.0 + badges_height(&frame.all, available);
    if !frame.recommended.is_empty() {
        height += 1.0 + rows_height(frame.recommended.len());
    }
    height + 1.0 + more_height(frame)
}

pub(crate) fn divider(card: UiRect, y: f32) -> Element {
    panel(
        UiRect::new(card.left + 1.0, y, card.right - 1.0, y + 1.0),
        VisualStyle::filled(theme::c().border),
    )
}

pub(crate) fn header(card: UiRect, y: f32, label: String) -> Element {
    panel(
        UiRect::new(card.left, y, card.right, y + HEADER_H),
        VisualStyle::default(),
    )
    .child(text(
        UiRect::new(card.left + PAD_X, y, card.right - PAD_X, y + HEADER_H),
        label,
        theme::mono_bold(theme::c().text_faint, theme::SMALL).tracking(0.8),
    ))
}

/// A language's short mark on a tint of its color.
pub(crate) fn mark(rect: UiRect, label: &str, key: &str, size: f32) -> Element {
    let tint = tint(key);
    panel(
        rect,
        VisualStyle::filled(mix(theme::c().surface, tint, 0.18)).radius(4.0),
    )
    .child(text(rect, label.to_owned(), centered(tint, size, 700)))
}

fn badge_flow(
    card: UiRect,
    y: f32,
    badges: &[Badge],
    empty: &str,
    state: &Binding,
    available: f32,
) -> Element {
    let mut layer = lgui::prelude::group(card);
    if badges.is_empty() {
        return layer.child(text(
            UiRect::new(card.left + PAD_X, y, card.right - PAD_X, y + NOTE_H),
            empty.to_owned(),
            theme::mono(theme::c().text_dim, theme::SMALL + 1.0),
        ));
    }
    let (places, _) = flow(badges, available);
    for (badge, (dx, dy)) in badges.iter().zip(places) {
        let left = card.left + PAD_X + dx;
        let rect = UiRect::new(
            left,
            y + dy,
            left + badge_width(&badge.text),
            y + dy + BADGE_H,
        );
        let hover = state.clone();
        let name = badge.name.clone();
        layer = layer.child(
            mark(rect, &badge.text, &badge.key, theme::SMALL + 1.0)
                .event_policy(EventPolicy::INTERACTIVE)
                .semantics(Semantics::new(SemanticRole::Button).name(badge.name.clone()))
                .on_event(UiEventKind::PointerMove, move |cx, _payload| {
                    hover.run(Command::Hover(Some(name.clone())));
                    cx.stop_propagation();
                }),
        );
    }
    layer
}

fn offer_rows(card: UiRect, mut y: f32, rows: &[Row], state: &Binding) -> Element {
    let palette = theme::c();
    let mut layer = lgui::prelude::group(card);
    for row in rows {
        let rect = UiRect::new(card.left + 1.0, y, card.right - 1.0, y + ROW_H);
        let middle = (rect.top + rect.bottom) / 2.0;
        let mark_rect = UiRect::new(
            rect.left + PAD_X,
            middle - 9.0,
            rect.left + PAD_X + 38.0,
            middle + 9.0,
        );
        let button_w = 76.0;
        let button = UiRect::new(
            rect.right - PAD_X - button_w,
            middle - 12.0,
            rect.right - PAD_X,
            middle + 12.0,
        );
        let mut element = panel(rect, VisualStyle::default())
            .child(mark(mark_rect, &row.text, &row.key, theme::SMALL))
            .child(text(
                UiRect::new(
                    mark_rect.right + 10.0,
                    rect.top,
                    button.left - 8.0,
                    rect.bottom,
                ),
                row.name.clone(),
                theme::mono(palette.text_soft, theme::UI_SIZE),
            ));
        if !row.note.is_empty() {
            let width = measure(&row.note, theme::SMALL + 1.0, 400) + 4.0;
            element = element.child(text(
                UiRect::new(
                    button.left - 12.0 - width,
                    rect.top,
                    button.left - 12.0,
                    rect.bottom,
                ),
                row.note.clone(),
                theme::mono_right(palette.text_faint, theme::SMALL + 1.0),
            ));
        }
        let mut action = theme::bordered(button, palette.sidebar, palette.border, 4.0, 1.0);
        if row.installing {
            action = action.child(text(
                button,
                "Installing…",
                centered(palette.text_dim, theme::SMALL + 1.0, 400),
            ));
        } else {
            let install = state.clone();
            let extension = row.extension.clone();
            action = action
                .event_policy(EventPolicy::INTERACTIVE)
                .cursor(CursorIcon::Pointer)
                .semantics(
                    Semantics::new(SemanticRole::Button).name(format!("Install {}", row.name)),
                )
                .on_click(move || install.run(Command::Install(extension.clone())))
                .child(text(
                    button,
                    "Install",
                    centered(palette.text_soft, theme::SMALL + 1.0, 400),
                ));
        }
        layer = layer.child(element.child(action));
        y += ROW_H;
    }
    layer
}

pub fn render(
    viewport: UiRect,
    state: Binding,
    input_focus: UiFocusHandle,
    input_id: UiId,
) -> Element {
    let palette = theme::c();
    let frame = state.frame();
    let card_w = CARD_W.min((viewport.width() - SIDE_MARGIN * 2.0).max(0.0));
    let available = (card_w - PAD_X * 2.0).max(0.0);
    let card_h = card_height(&frame, available);
    let left = viewport.left + (viewport.width() - card_w) / 2.0;
    let top = (viewport.top + viewport.height() * 0.13).max(viewport.top + 36.0);
    let top = top.min((viewport.bottom - card_h - SIDE_MARGIN).max(viewport.top));
    let card = UiRect::new(left, top, left + card_w, top + card_h);

    // Invisible backdrop: a press outside the card closes the panel.
    let close = state.clone();
    let overlay = panel(viewport, VisualStyle::default())
        .event_policy(EventPolicy::INTERACTIVE)
        .on_pointer_down_with_button(move |_cx, pointer, _button| {
            if !card.contains(pointer.point) {
                close.run(Command::Close);
            }
        });

    // A pointer over the card but not over a badge ends the name in the footer.
    let leave = state.clone();
    let mut surface = theme::bordered(card, palette.surface, palette.border, 8.0, 1.0)
        .shadow(
            ShadowStyle::new(Color::BLACK)
                .alpha(palette.shadow_alpha)
                .offset(0.0, 2.0)
                .blur(3.0),
        )
        .event_policy(EventPolicy::INTERACTIVE)
        .on_event(UiEventKind::PointerMove, move |_cx, _payload| {
            leave.run(Command::Hover(None));
        });

    let mut y = card.top + 1.0;

    // In use, then everything installed.
    surface = surface.child(header(card, y, format!("ACTIVE · {}", frame.active.len())));
    surface = surface.child(badge_flow(
        card,
        y + HEADER_H,
        &frame.active,
        "No open file has a language with syntax.",
        &state,
        available,
    ));
    y += badges_height(&frame.active, available);

    surface = surface.child(divider(card, y));
    y += 1.0;
    surface = surface.child(header(card, y, format!("ALL · {}", frame.all.len())));
    surface = surface.child(badge_flow(
        card,
        y + HEADER_H,
        &frame.all,
        "No language has syntax yet.",
        &state,
        available,
    ));
    y += badges_height(&frame.all, available);

    // What the open files could use.
    if !frame.recommended.is_empty() {
        surface = surface.child(divider(card, y));
        y += 1.0;
        surface = surface.child(header(
            card,
            y,
            format!("RECOMMENDED · {}", frame.recommended.len()),
        ));
        surface = surface.child(offer_rows(card, y + HEADER_H, &frame.recommended, &state));
        y += rows_height(frame.recommended.len());
    }

    // The rest of the registry, with a filter.
    surface = surface.child(divider(card, y));
    y += 1.0;
    let label = format!("MORE · {}", frame.more_total);
    let label_w = measure(&label, theme::SMALL, 700) + 22.0;
    let toggle = UiRect::new(
        card.left + PAD_X,
        y,
        card.left + PAD_X + label_w + 24.0,
        y + MORE_HEADER_H,
    );
    let toggle_state = state.clone();
    surface = surface.child(
        panel(toggle, VisualStyle::default())
            .event_policy(EventPolicy::INTERACTIVE)
            .cursor(CursorIcon::Pointer)
            .semantics(Semantics::new(SemanticRole::Button).name("Show more languages"))
            .on_click(move || toggle_state.run(Command::ToggleMore)),
    );
    surface = surface.child(icon(
        UiId::new("languages.chevron"),
        if shown(&frame) {
            "chevron-down"
        } else {
            "chevron-right"
        },
        UiRect::new(
            card.left + PAD_X,
            y + (MORE_HEADER_H - 12.0) / 2.0,
            card.left + PAD_X + 12.0,
            y + (MORE_HEADER_H + 12.0) / 2.0,
        ),
        palette.text_dim,
    ));
    surface = surface.child(text(
        UiRect::new(
            card.left + PAD_X + 18.0,
            y,
            card.left + PAD_X + 18.0 + label_w,
            y + MORE_HEADER_H,
        ),
        label,
        theme::mono_bold(palette.text_faint, theme::SMALL).tracking(0.8),
    ));

    // The filter: a bordered field with a search mark, like the one of the Extensions page.
    let field = UiRect::new(
        card.right - PAD_X - FILTER_W,
        y + (MORE_HEADER_H - FILTER_H) / 2.0,
        card.right - PAD_X,
        y + (MORE_HEADER_H + FILTER_H) / 2.0,
    );
    surface = surface
        .child(theme::bordered(field, palette.bg, palette.border, 5.0, 1.0))
        .child(icon(
            UiId::new("languages.search"),
            "search",
            UiRect::new(
                field.left + 7.0,
                field.top + (FILTER_H - 12.0) / 2.0,
                field.left + 19.0,
                field.bottom - (FILTER_H - 12.0) / 2.0,
            ),
            palette.text_dim,
        ))
        .child(input::render_access(
            UiRect::new(
                field.left + 22.0,
                field.top + 1.0,
                field.right - 4.0,
                field.bottom - 1.0,
            ),
            input_id,
            input_focus,
            InputBinding::new_access(state.input.clone(), |input| input, |input| input),
            AccessInputOptions {
                label: "Filter languages",
                placeholder: "Filter",
                style: InputStyle {
                    text: theme::mono(palette.text_bright, theme::SMALL + 1.0),
                    placeholder: theme::mono(palette.text_faint, theme::SMALL + 1.0),
                    caret: palette.accent,
                    selection: mix(palette.surface, palette.accent, 0.45),
                },
                on_submit: None,
                on_cancel: None,
                on_blur: None,
            },
        ));

    if shown(&frame) {
        let list_top = y + MORE_HEADER_H;
        let rows = &frame.more[..frame.more.len().min(MAX_ROWS)];
        surface = surface.child(offer_rows(card, list_top, rows, &state));
        if let Some(note) = more_note(&frame) {
            let at = list_top + rows.len() as f32 * ROW_H;
            surface = surface.child(text(
                UiRect::new(card.left + PAD_X, at, card.right - PAD_X, at + NOTE_H),
                note,
                theme::mono(palette.text_dim, theme::SMALL + 1.0),
            ));
        }
    }
    y += more_height(&frame);

    // Footer: the name of the badge under the pointer, else the keys; and a way on.
    let foot = UiRect::new(card.left + 1.0, y, card.right - 1.0, card.bottom - 1.0);
    surface = surface.child(divider(card, foot.top));
    let more_state = state.clone();
    let link = "Open Extensions page";
    let link_w = measure(link, theme::SMALL, 400) + 8.0;
    surface = surface
        .child(text(
            UiRect::new(
                foot.left + PAD_X,
                foot.top + 1.0,
                foot.right - PAD_X - link_w - 12.0,
                foot.bottom,
            ),
            frame
                .hover
                .clone()
                .unwrap_or_else(|| "Esc Close".to_owned()),
            theme::mono(palette.text_dim, theme::SMALL),
        ))
        .child(
            panel(
                UiRect::new(
                    foot.right - PAD_X - link_w,
                    foot.top + 1.0,
                    foot.right - PAD_X + 4.0,
                    foot.bottom,
                ),
                VisualStyle::default(),
            )
            .event_policy(EventPolicy::INTERACTIVE)
            .cursor(CursorIcon::Pointer)
            .semantics(Semantics::new(SemanticRole::Button).name(link))
            .on_click(move || more_state.run(Command::OpenExtensions))
            .child(text(
                UiRect::new(
                    foot.right - PAD_X - link_w,
                    foot.top + 1.0,
                    foot.right - PAD_X,
                    foot.bottom,
                ),
                link,
                theme::mono(palette.accent, theme::SMALL),
            )),
        );
    overlay.child(surface)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn badge(text: &str) -> Badge {
        Badge {
            name: text.into(),
            text: text.into(),
            key: text.to_lowercase(),
        }
    }

    #[test]
    fn badges_wrap_to_the_next_line_when_the_row_is_full() {
        let badges: Vec<Badge> = ["RS", "MD", "JSON", "SH", "YML"]
            .iter()
            .map(|t| badge(t))
            .collect();
        let (places, height) = flow(&badges, 120.0);
        assert_eq!(places.len(), 5);
        assert_eq!(places[0], (0.0, 0.0));
        assert!(places.iter().any(|(_, y)| *y > 0.0), "{places:?}");
        assert!(height >= BADGE_H * 2.0);
        // A wide enough row keeps them all on one line.
        let (places, height) = flow(&badges, 10_000.0);
        assert!(places.iter().all(|(_, y)| *y == 0.0));
        assert_eq!(height, BADGE_H);
    }

    fn row(extension: &str, name: &str, text: &str, note: &str) -> Row {
        Row {
            extension: extension.into(),
            name: name.into(),
            text: text.into(),
            key: name.to_lowercase(),
            note: note.into(),
            installing: false,
        }
    }

    /// The panel rendered for real, with what the pointer did recorded as words.
    fn rendered(
        frame: Frame,
    ) -> (
        lgui::session::UiSession,
        lgui::application::ApplicationContext,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::UiScale;
        use lgui::session::UiSession;
        use std::sync::Mutex;
        let rect = UiRect::new(0.0, 0.0, 800.0, 700.0);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        let view: AppView = Arc::new(move |cx| {
            let input = cx.state_with(InputState::default);
            let id = cx.use_stable_id();
            let focus = cx.focus_handle(id.clone());
            let frame = frame.clone();
            let record = record.clone();
            let binding = Binding::new(
                StateAccess::from(input),
                move || frame.clone(),
                move |command| {
                    record.lock().unwrap().push(match command {
                        Command::Close => "close".to_owned(),
                        Command::ToggleMore => "toggle".to_owned(),
                        Command::Hover(Some(name)) => format!("hover {name}"),
                        Command::Hover(None) => "hover none".to_owned(),
                        Command::Install(extension) => format!("install {extension}"),
                        Command::OpenExtensions => "extensions".to_owned(),
                    });
                },
            );
            render(rect, binding, focus, id)
        });
        let context = ApplicationContext::empty(Default::default());
        let fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        std::mem::forget(fonts);
        let mut session = UiSession::new();
        session.render_view(&view, rect, UiScale::ONE);
        (session, context, seen)
    }

    fn centre_of(session: &lgui::session::UiSession, label: &str) -> (f32, f32) {
        let bounds = session
            .tree()
            .nodes()
            .iter()
            .find(|node| node.text.as_deref() == Some(label))
            .unwrap_or_else(|| panic!("`{label}` is not drawn"))
            .paint_bounds;
        (
            (bounds.left + bounds.right) / 2.0,
            (bounds.top + bounds.bottom) / 2.0,
        )
    }

    fn click(
        session: &mut lgui::session::UiSession,
        context: &lgui::application::ApplicationContext,
        at: (f32, f32),
    ) {
        use lgui::core::{InputEvent, Point, PointerButton, PointerData};
        let pointer = PointerData::mouse(Point::new(at.0, at.1));
        for input in [
            InputEvent::PointerDown {
                pointer,
                button: PointerButton::Left,
            },
            InputEvent::PointerUp {
                pointer,
                button: PointerButton::Left,
            },
        ] {
            let output = session.handle_input(input);
            lgui::core::dispatch_runtime_output(
                output,
                context,
                &lgui::window::WindowId::new("language-menu-test"),
                |action| session.handle_default_action(action),
                |_| {},
            );
        }
    }

    fn sample() -> Frame {
        Frame {
            active: vec![badge("RS")],
            all: vec![badge("RS"), badge("MD")],
            recommended: vec![row("loog.lang.yaml", "YAML", "YAML", "2 open files")],
            more: vec![
                row("loog.lang.go", "Go", "GO", ""),
                row("loog.lang.lua", "Lua", "LUA", ""),
            ],
            more_total: 2,
            more_open: true,
            registry_ready: true,
            ..Frame::default()
        }
    }

    #[test]
    fn the_four_groups_are_drawn_and_install_reports_its_extension() {
        let (mut session, context, seen) = rendered(sample());
        for label in [
            "ACTIVE · 1",
            "ALL · 2",
            "RECOMMENDED · 1",
            "MORE · 2",
            "YAML",
            "2 open files",
            "Go",
            "Lua",
            "Esc Close",
            "Open Extensions page",
        ] {
            centre_of(&session, label);
        }
        let installs: Vec<_> = session
            .tree()
            .nodes()
            .iter()
            .filter(|node| node.text.as_deref() == Some("Install"))
            .map(|node| {
                let b = node.paint_bounds;
                ((b.left + b.right) / 2.0, (b.top + b.bottom) / 2.0)
            })
            .collect();
        assert_eq!(installs.len(), 3, "one per language to install");
        click(&mut session, &context, installs[0]);
        assert_eq!(*seen.lock().unwrap(), ["install loog.lang.yaml"]);
        let link = centre_of(&session, "Open Extensions page");
        click(&mut session, &context, link);
        assert_eq!(seen.lock().unwrap().last().unwrap(), "extensions");
    }

    #[test]
    fn the_last_groups_label_and_its_filter_sit_on_the_same_line() {
        for open in [true, false] {
            let mut frame = sample();
            frame.more_open = open;
            let (session, _context, _seen) = rendered(frame);
            let label = centre_of(&session, "MORE · 2").1;
            let filter = centre_of(&session, "Filter").1;
            assert!(
                (label - filter).abs() < 1.5,
                "label at {label}, filter at {filter}"
            );
        }
    }

    #[test]
    fn the_last_group_stays_folded_until_opened_and_its_header_toggles_it() {
        let mut frame = sample();
        frame.more_open = false;
        let (mut session, context, seen) = rendered(frame);
        assert!(
            !session
                .tree()
                .nodes()
                .iter()
                .any(|node| node.text.as_deref() == Some("Go")),
            "folded rows are not drawn"
        );
        let header = centre_of(&session, "MORE · 2");
        click(&mut session, &context, (header.0 - 40.0, header.1));
        assert_eq!(*seen.lock().unwrap(), ["toggle"]);
    }

    #[test]
    fn a_language_being_installed_shows_that_and_cannot_be_pressed_again() {
        let mut frame = sample();
        frame.recommended[0].installing = true;
        let (session, _context, _seen) = rendered(frame);
        centre_of(&session, "Installing…");
        let installs = session
            .tree()
            .nodes()
            .iter()
            .filter(|node| node.text.as_deref() == Some("Install"))
            .count();
        assert_eq!(installs, 2);
    }

    #[test]
    fn an_empty_group_takes_a_line_for_its_note() {
        assert_eq!(badges_height(&[], 500.0), HEADER_H + NOTE_H + SECTION_PAD);
    }

    #[test]
    fn the_last_group_is_closed_until_opened_or_filtered() {
        let mut frame = Frame {
            registry_ready: true,
            more_total: 3,
            ..Frame::default()
        };
        let closed = more_height(&frame);
        frame.more_open = true;
        assert!(more_height(&frame) > closed);
        frame.more_open = false;
        frame.filtering = true;
        assert!(more_height(&frame) > closed);
    }

    #[test]
    fn a_language_has_the_same_color_every_time_and_the_theme_color_when_it_has_one() {
        assert_eq!(tint("yaml"), tint("yaml"));
        assert_eq!(tint("rust"), theme::c().badge.rust);
    }
}
