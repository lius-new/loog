//! The language services panel: a card like the highlight panel's, with the
//! language services that are installed and how each is doing, and the ones to
//! install.
//!
//! The view draws a `Frame` the app prepares and reports what the user did as a
//! `Command`; it knows nothing about services or the registry.
use std::sync::Arc;

use lgui::core::{CursorIcon, EventPolicy, SemanticRole, Semantics};
use lgui::prelude::{Color, Element, ShadowStyle, UiRect, VisualStyle, panel, text};

use crate::features::language_services_menu::State;
use crate::ui::highlight_menu::{centered, divider, header, mark};
use crate::ui::tabs::measure;
use crate::ui::theme;

const CARD_W: f32 = 560.0;
const SIDE_MARGIN: f32 = 12.0;
const PAD_X: f32 = 14.0;
const HEADER_H: f32 = 28.0;
const ROW_H: f32 = 32.0;
const NOTE_H: f32 = 24.0;
const FOOT_H: f32 = 28.0;
const BUTTON_H: f32 = 24.0;
const RESTART_W: f32 = 76.0;
const SWITCH_W: f32 = 64.0;
const INSTALL_W: f32 = 76.0;
const GAP: f32 = 6.0;

/// An installed language service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub extension: String,
    pub name: String,
    /// The short mark of the language that stands for it, and what its color follows.
    pub text: String,
    pub key: String,
    pub state: State,
    pub note: String,
}

/// A language service that can be installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub extension: String,
    pub name: String,
    pub text: String,
    pub key: String,
    pub note: String,
    pub installing: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub installed: Vec<Row>,
    pub recommended: Vec<Offer>,
    /// There is a registry index to offer services from.
    pub registry_ready: bool,
}

pub enum Command {
    Close,
    Restart(String),
    /// Turn a service on (`true`) or off.
    Switch(String, bool),
    Install(String),
    OpenExtensions,
}

#[derive(Clone)]
pub struct Binding {
    read: Arc<dyn Fn() -> Frame + Send + Sync>,
    execute: Arc<dyn Fn(Command) + Send + Sync>,
}

impl Binding {
    pub fn new(
        read: impl Fn() -> Frame + Send + Sync + 'static,
        execute: impl Fn(Command) + Send + Sync + 'static,
    ) -> Self {
        Self {
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

fn card_height(frame: &Frame) -> f32 {
    let installed = if frame.installed.is_empty() {
        NOTE_H
    } else {
        frame.installed.len() as f32 * ROW_H
    };
    let mut height = 1.0 + HEADER_H + installed + 4.0;
    if !frame.recommended.is_empty() {
        height += 1.0 + HEADER_H + frame.recommended.len() as f32 * ROW_H + 4.0;
    }
    height + 1.0 + FOOT_H
}

fn state_color(state: State) -> Color {
    let palette = theme::c();
    match state {
        State::Ready => palette.git.added,
        State::Working => palette.accent,
        State::Failed => palette.error,
        State::Starting => palette.warning,
        State::Idle | State::Stopped => palette.text_faint,
    }
}

/// A bordered button; one that cannot be pressed is drawn dim and does nothing.
fn button(
    rect: UiRect,
    label: &str,
    name: String,
    on_click: Option<Box<dyn Fn() + Send + Sync>>,
) -> Element {
    let palette = theme::c();
    let color = if on_click.is_some() {
        palette.text_soft
    } else {
        palette.text_faint
    };
    let mut element = theme::bordered(rect, palette.sidebar, palette.border, 4.0, 1.0);
    if let Some(on_click) = on_click {
        element = element
            .event_policy(EventPolicy::INTERACTIVE)
            .cursor(CursorIcon::Pointer)
            .semantics(Semantics::new(SemanticRole::Button).name(name))
            .on_click(on_click);
    }
    element.child(text(
        rect,
        label.to_owned(),
        centered(color, theme::SMALL + 1.0, 400),
    ))
}

fn row_frame(card: UiRect, y: f32) -> (UiRect, f32, UiRect) {
    let rect = UiRect::new(card.left + 1.0, y, card.right - 1.0, y + ROW_H);
    let middle = (rect.top + rect.bottom) / 2.0;
    let mark_rect = UiRect::new(
        rect.left + PAD_X,
        middle - 9.0,
        rect.left + PAD_X + 38.0,
        middle + 9.0,
    );
    (rect, middle, mark_rect)
}

fn installed_rows(card: UiRect, mut y: f32, rows: &[Row], state: &Binding) -> Element {
    let palette = theme::c();
    let mut layer = lgui::prelude::group(card);
    for row in rows {
        let (rect, middle, mark_rect) = row_frame(card, y);
        let switch = UiRect::new(
            rect.right - PAD_X - SWITCH_W,
            middle - BUTTON_H / 2.0,
            rect.right - PAD_X,
            middle + BUTTON_H / 2.0,
        );
        let restart = UiRect::new(
            switch.left - GAP - RESTART_W,
            switch.top,
            switch.left - GAP,
            switch.bottom,
        );
        let mut element = panel(rect, VisualStyle::default())
            .child(mark(mark_rect, &row.text, &row.key, theme::SMALL))
            .child(text(
                UiRect::new(
                    mark_rect.right + 10.0,
                    rect.top,
                    restart.left - 8.0,
                    rect.bottom,
                ),
                row.name.clone(),
                theme::mono(palette.text_soft, theme::UI_SIZE),
            ));
        // The note, such as why it failed, else how it is doing.
        let note = if row.note.is_empty() {
            row.state.label().to_owned()
        } else {
            row.note.clone()
        };
        let width = measure(&note, theme::SMALL + 1.0, 400) + 4.0;
        let room = (restart.left - 12.0) - (mark_rect.right + 10.0 + 120.0);
        element = element.child(text(
            UiRect::new(
                restart.left - 12.0 - width.min(room.max(0.0)),
                rect.top,
                restart.left - 12.0,
                rect.bottom,
            ),
            note,
            theme::mono_right(state_color(row.state), theme::SMALL + 1.0),
        ));
        let can_restart = !matches!(row.state, State::Starting | State::Stopped);
        let extension = row.extension.clone();
        let on_restart = state.clone();
        let restart_button = button(
            restart,
            "Restart",
            format!("Restart {}", row.name),
            can_restart.then(|| {
                Box::new(move || on_restart.run(Command::Restart(extension.clone())))
                    as Box<dyn Fn() + Send + Sync>
            }),
        );
        let turn_on = !row.state.is_on();
        let extension = row.extension.clone();
        let on_switch = state.clone();
        let switch_button = button(
            switch,
            if turn_on { "Start" } else { "Stop" },
            format!("{} {}", if turn_on { "Start" } else { "Stop" }, row.name),
            (row.state != State::Starting).then(|| {
                Box::new(move || on_switch.run(Command::Switch(extension.clone(), turn_on)))
                    as Box<dyn Fn() + Send + Sync>
            }),
        );
        layer = layer.child(element.child(restart_button).child(switch_button));
        y += ROW_H;
    }
    layer
}

fn offer_rows(card: UiRect, mut y: f32, rows: &[Offer], state: &Binding) -> Element {
    let palette = theme::c();
    let mut layer = lgui::prelude::group(card);
    for row in rows {
        let (rect, middle, mark_rect) = row_frame(card, y);
        let install = UiRect::new(
            rect.right - PAD_X - INSTALL_W,
            middle - BUTTON_H / 2.0,
            rect.right - PAD_X,
            middle + BUTTON_H / 2.0,
        );
        let mut element = panel(rect, VisualStyle::default())
            .child(mark(mark_rect, &row.text, &row.key, theme::SMALL))
            .child(text(
                UiRect::new(
                    mark_rect.right + 10.0,
                    rect.top,
                    install.left - 8.0,
                    rect.bottom,
                ),
                row.name.clone(),
                theme::mono(palette.text_soft, theme::UI_SIZE),
            ));
        if !row.note.is_empty() {
            let width = measure(&row.note, theme::SMALL + 1.0, 400) + 4.0;
            element = element.child(text(
                UiRect::new(
                    install.left - 12.0 - width,
                    rect.top,
                    install.left - 12.0,
                    rect.bottom,
                ),
                row.note.clone(),
                theme::mono_right(palette.text_faint, theme::SMALL + 1.0),
            ));
        }
        let extension = row.extension.clone();
        let on_install = state.clone();
        let action = button(
            install,
            if row.installing {
                "Installing…"
            } else {
                "Install"
            },
            format!("Install {}", row.name),
            (!row.installing).then(|| {
                Box::new(move || on_install.run(Command::Install(extension.clone())))
                    as Box<dyn Fn() + Send + Sync>
            }),
        );
        layer = layer.child(element.child(action));
        y += ROW_H;
    }
    layer
}

pub fn render(viewport: UiRect, state: Binding) -> Element {
    let palette = theme::c();
    let frame = state.frame();
    let card_w = CARD_W.min((viewport.width() - SIDE_MARGIN * 2.0).max(0.0));
    let card_h = card_height(&frame);
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
    let mut surface = theme::bordered(card, palette.surface, palette.border, 8.0, 1.0)
        .shadow(
            ShadowStyle::new(Color::BLACK)
                .alpha(palette.shadow_alpha)
                .offset(0.0, 2.0)
                .blur(3.0),
        )
        .event_policy(EventPolicy::INTERACTIVE);

    let mut y = card.top + 1.0;
    surface = surface.child(header(
        card,
        y,
        format!("INSTALLED · {}", frame.installed.len()),
    ));
    y += HEADER_H;
    if frame.installed.is_empty() {
        surface = surface.child(text(
            UiRect::new(card.left + PAD_X, y, card.right - PAD_X, y + NOTE_H),
            "No language service is installed.",
            theme::mono(palette.text_dim, theme::SMALL + 1.0),
        ));
        y += NOTE_H;
    } else {
        surface = surface.child(installed_rows(card, y, &frame.installed, &state));
        y += frame.installed.len() as f32 * ROW_H;
    }
    y += 4.0;

    if !frame.recommended.is_empty() {
        surface = surface.child(divider(card, y));
        y += 1.0;
        surface = surface.child(header(
            card,
            y,
            format!("RECOMMENDED · {}", frame.recommended.len()),
        ));
        y += HEADER_H;
        surface = surface.child(offer_rows(card, y, &frame.recommended, &state));
        y += frame.recommended.len() as f32 * ROW_H + 4.0;
    }

    // Footer: the keys, and a way on.
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
            "Esc Close",
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

    fn row(name: &str, state: State) -> Row {
        Row {
            extension: format!("loog.lsp.{name}"),
            name: name.into(),
            text: "RS".into(),
            key: "rust".into(),
            state,
            note: String::new(),
        }
    }

    #[test]
    fn the_card_grows_with_its_rows_and_with_the_recommended_group() {
        let empty = card_height(&Frame::default());
        let mut frame = Frame {
            installed: vec![row("a", State::Ready), row("b", State::Idle)],
            ..Frame::default()
        };
        let installed = card_height(&frame);
        assert!(installed > empty);
        frame.recommended.push(Offer {
            extension: "loog.lsp.c".into(),
            name: "c".into(),
            text: "TS".into(),
            key: "typescript".into(),
            note: String::new(),
            installing: false,
        });
        assert!(card_height(&frame) > installed + ROW_H);
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
        let view: AppView = Arc::new(move |_| {
            let frame = frame.clone();
            let record = record.clone();
            render(
                rect,
                Binding::new(
                    move || frame.clone(),
                    move |command| {
                        record.lock().unwrap().push(match command {
                            Command::Close => "close".to_owned(),
                            Command::Restart(extension) => format!("restart {extension}"),
                            Command::Switch(extension, on) => format!("switch {extension} {on}"),
                            Command::Install(extension) => format!("install {extension}"),
                            Command::OpenExtensions => "extensions".to_owned(),
                        });
                    },
                ),
            )
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

    /// Where the `index`th node drawing `label` is, in the middle.
    fn centre_of(session: &lgui::session::UiSession, label: &str, index: usize) -> (f32, f32) {
        let bounds = session
            .tree()
            .nodes()
            .iter()
            .filter(|node| node.text.as_deref() == Some(label))
            .nth(index)
            .unwrap_or_else(|| panic!("`{label}` #{index} is not drawn"))
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
                &lgui::window::WindowId::new("language-services-test"),
                |action| session.handle_default_action(action),
                |_| {},
            );
        }
    }

    fn sample() -> Frame {
        Frame {
            installed: vec![
                row("analyzer", State::Ready),
                row("taplo", State::Stopped),
                Row {
                    note: "exited with code 101".into(),
                    ..row("pyright", State::Failed)
                },
            ],
            recommended: vec![Offer {
                extension: "loog.lsp.typescript".into(),
                name: "TypeScript Language Server".into(),
                text: "TS".into(),
                key: "typescript".into(),
                note: "1 open file".into(),
                installing: false,
            }],
            registry_ready: true,
        }
    }

    #[test]
    fn the_groups_are_drawn_and_each_button_reports_its_service() {
        let (mut session, context, seen) = rendered(sample());
        for label in [
            "INSTALLED · 3",
            "RECOMMENDED · 1",
            "analyzer",
            "exited with code 101",
            "Esc Close",
            "Open Extensions page",
        ] {
            centre_of(&session, label, 0);
        }
        // A running service is stopped, a stopped one started, a failed one started again.
        let stop = centre_of(&session, "Stop", 0);
        click(&mut session, &context, stop);
        let start = centre_of(&session, "Start", 0);
        click(&mut session, &context, start);
        let retry = centre_of(&session, "Start", 1);
        click(&mut session, &context, retry);
        let restart = centre_of(&session, "Restart", 0);
        click(&mut session, &context, restart);
        let install = centre_of(&session, "Install", 0);
        click(&mut session, &context, install);
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "switch loog.lsp.analyzer false",
                "switch loog.lsp.taplo true",
                "switch loog.lsp.pyright true",
                "restart loog.lsp.analyzer",
                "install loog.lsp.typescript",
            ]
        );
    }

    #[test]
    fn a_stopped_service_cannot_be_restarted_and_a_starting_one_cannot_be_pressed() {
        let frame = Frame {
            installed: vec![
                row("taplo", State::Stopped),
                row("analyzer", State::Starting),
            ],
            ..Frame::default()
        };
        let (mut session, context, seen) = rendered(frame);
        let restart = centre_of(&session, "Restart", 0);
        click(&mut session, &context, restart);
        let restart = centre_of(&session, "Restart", 1);
        click(&mut session, &context, restart);
        let switch = centre_of(&session, "Stop", 0);
        click(&mut session, &context, switch);
        assert!(
            seen.lock().unwrap().is_empty(),
            "{:?}",
            seen.lock().unwrap()
        );
    }

    #[test]
    fn a_service_being_installed_says_so_and_cannot_be_pressed_again() {
        let mut frame = sample();
        frame.recommended[0].installing = true;
        let (mut session, context, seen) = rendered(frame);
        let at = centre_of(&session, "Installing…", 0);
        click(&mut session, &context, at);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn a_failed_service_is_not_drawn_in_the_color_of_a_running_one() {
        assert_ne!(state_color(State::Failed), state_color(State::Ready));
        assert_ne!(state_color(State::Starting), state_color(State::Ready));
        assert_ne!(state_color(State::Working), state_color(State::Ready));
    }
}
