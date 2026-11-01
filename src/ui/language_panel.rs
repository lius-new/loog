//! Language servers, presented from supplied state and commands.
//!
//! One flat list. The active file's servers come first, each on a card of its
//! own; the rest follow as compact rows. A server's state shows as the colour of
//! its dot and status text, never as a background.
use lgui::core::{CursorIcon, EventPolicy, SemanticRole, Semantics, UiEventContext, UiId};
use lgui::prelude::{Color, Element, TextAlign, UiRect, VisualStyle, panel, text};
use std::sync::Arc;

use super::components::{loading_indicator, progress_ring};
use super::tabs::{ellipsize_sans, icon, measure_sans as measure};
use super::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Quiet,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub level: Level,
    pub cancel: Option<TaskTarget>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskTarget {
    pub instance: u64,
    pub run: u64,
    pub task: u64,
}

/// Where a section sits in the list, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    /// Serves the active file: drawn as a card.
    Current,
    Other,
    Download,
}

/// What a server is doing, which picks its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Ready,
    /// Starting, installing or downloading.
    Busy,
    /// Its pack has not been downloaded.
    Missing,
    /// Stopped, or an install failed.
    Failed,
    Off,
}

/// The indicator at the right edge of a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    None,
    Percent(u8),
    Spinner,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub group: Group,
    /// The server or resource name.
    pub title: String,
    /// Languages and size, or the reason it stopped.
    pub subtitle: String,
    pub subtitle_level: Level,
    /// The state in words, drawn at the right.
    pub state: String,
    pub tone: Tone,
    pub progress: Progress,
    /// Notices and tasks under the subtitle.
    pub lines: Vec<Line>,
    /// The server whose pack this section can install, with the action's label.
    pub install: Option<(String, &'static str)>,
    /// The instance this section can restart.
    pub restart: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    /// Ordered by group.
    pub sections: Vec<Section>,
}

pub enum Event {
    Close,
    /// Restart every server.
    Restart,
    /// Restart one instance.
    RestartServer(u64),
    /// Install the extension with this id.
    Install(String),
    CancelTask(TaskTarget),
}
pub type Commands = Arc<dyn Fn(Event) + Send + Sync>;

const WIDTH: f32 = 380.0;
const PAD: f32 = 8.0;
const INSET: f32 = 12.0;
const CARD_H: f32 = 56.0;
const ROW_H: f32 = 46.0;
const LINE_H: f32 = 18.0;
const GAP: f32 = 2.0;
const FOOTER_H: f32 = 36.0;
const MARGIN: f32 = 8.0;
const DOT: f32 = 7.0;
const RING: f32 = 18.0;
/// Approximate advance of the small monospace face.
const MONO_W: f32 = 6.4;
const EMPTY: &str = "No language servers or downloads are active.";

pub(crate) fn tone_colour(tone: Tone) -> Color {
    let c = theme::c();
    match tone {
        Tone::Ready => c.diff.add_mark,
        Tone::Busy => c.accent,
        Tone::Missing => c.warning,
        Tone::Failed => c.error,
        Tone::Off => c.text_dim,
    }
}

fn level_colour(level: Level, quiet: Color) -> Color {
    match level {
        Level::Quiet => quiet,
        Level::Warning => theme::c().warning,
        Level::Error => theme::c().error,
    }
}

fn top_height(section: &Section) -> f32 {
    if section.group == Group::Current {
        CARD_H
    } else {
        ROW_H
    }
}

fn section_height(section: &Section) -> f32 {
    top_height(section) + section.lines.len() as f32 * LINE_H + GAP
}

/// The card's height for a frame: its sections, then the footer.
fn height(frame: &Frame) -> f32 {
    let body: f32 = if frame.sections.is_empty() {
        LINE_H
    } else {
        frame.sections.iter().map(section_height).sum()
    };
    PAD + body + FOOTER_H
}

/// The card opens above `anchor`, the status-bar item of the language server which
/// triggered it, with its bottom-right corner at that item.
pub fn render(
    viewport: UiRect,
    anchor: Option<UiRect>,
    frame: &Frame,
    commands: Commands,
) -> Element {
    let h = height(frame).min(viewport.bottom - viewport.top - theme::STATUS_H - 2.0 * MARGIN);
    let right = anchor
        .map_or(viewport.right - MARGIN, |a| a.right)
        .min(viewport.right - MARGIN)
        .max(viewport.left + MARGIN + WIDTH);
    let bottom = viewport.bottom - theme::STATUS_H - MARGIN;
    let card = UiRect::new(right - WIDTH, bottom - h, right, bottom);
    let policy = EventPolicy {
        focus: false,
        ..EventPolicy::INTERACTIVE
    };
    let close = commands.clone();
    let overlay = panel(viewport, VisualStyle::default())
        .event_policy(policy)
        .on_pointer_down_with_button(move |cx, p, _| {
            if !card.contains(p.point) {
                close(Event::Close);
            }
            cx.stop_propagation();
        });
    let mut body =
        theme::bordered(card, theme::c().surface, theme::c().border, 6.0, 1.0).event_policy(policy);
    let mut y = card.top + PAD;
    if frame.sections.is_empty() {
        body = body.child(text(
            UiRect::new(card.left + PAD + INSET, y, card.right - PAD, y + LINE_H),
            EMPTY,
            theme::sans(theme::c().text_muted, theme::SMALL),
        ));
    }
    for (index, section) in frame.sections.iter().enumerate() {
        let strip = UiRect::new(
            card.left + PAD,
            y,
            card.right - PAD,
            y + section_height(section) - GAP,
        );
        body = body.child(section_row(index, section, strip, &commands, policy));
        y += section_height(section);
    }
    // Footer: restart everything, retry downloads.
    let footer = UiRect::new(card.left, card.bottom - FOOTER_H, card.right, card.bottom);
    body = body.child(
        panel(
            UiRect::new(footer.left, footer.top, footer.right, footer.top + 0.5),
            VisualStyle::filled(theme::c().border),
        )
        .event_policy(EventPolicy::NONE),
    );
    let label = "Restart all servers";
    let width = measure(label, theme::SMALL, 400) + 8.0;
    let restart = commands.clone();
    body = body.child(link(
        UiRect::new(
            footer.left + PAD + INSET,
            footer.top,
            footer.left + PAD + INSET + width,
            footer.bottom,
        ),
        label,
        theme::c().text_soft,
        TextAlign::Left,
        policy,
        move || restart(Event::Restart),
    ));
    overlay.child(body)
}

/// A server's row: a card for the active file's servers, a compact row otherwise.
fn section_row(
    index: usize,
    section: &Section,
    strip: UiRect,
    commands: &Commands,
    policy: EventPolicy,
) -> Element {
    let current = section.group == Group::Current;
    let colour = tone_colour(section.tone);
    let mut row = if current {
        theme::bordered(
            strip,
            theme::c().active_line,
            theme::c().active_line,
            6.0,
            0.0,
        )
    } else {
        panel(strip, VisualStyle::default())
    }
    .event_policy(policy);
    let top_h = top_height(section);
    let middle = strip.top + top_h / 2.0;
    let left = strip.left + INSET;
    let text_left = left + DOT + 8.0;

    // The right edge, from the outside in: indicator or restart icon, the state in
    // words, then the action.
    let mut edge = strip.right - INSET;
    match section.progress {
        Progress::None => {}
        Progress::Percent(percent) => {
            let ring = UiRect::new(edge - RING, middle - RING / 2.0, edge, middle + RING / 2.0);
            row = row.child(progress_ring::render(
                UiId::owned(format!("language-panel.ring.{index}")),
                ring,
                percent,
                colour,
            ));
            edge -= RING + 8.0;
        }
        Progress::Spinner => {
            let ring = UiRect::new(edge - RING, middle - RING / 2.0, edge, middle + RING / 2.0);
            row = row.child(loading_indicator::render(
                ring,
                colour,
                format!("language-panel.spinner.{index}"),
            ));
            edge -= RING + 8.0;
        }
    }
    let restart_icon = section
        .restart
        .filter(|_| current && section.install.is_none());
    if let Some(instance) = restart_icon {
        let button = UiRect::new(edge - 24.0, middle - 12.0, edge, middle + 12.0);
        let restart = commands.clone();
        row = row.child(
            panel(button, VisualStyle::default())
                .event_policy(policy)
                .cursor(CursorIcon::Pointer)
                .semantics(
                    Semantics::new(SemanticRole::Button).name(format!("Restart {}", section.title)),
                )
                .on_click(move |cx: &mut UiEventContext| {
                    restart(Event::RestartServer(instance));
                    cx.stop_propagation();
                })
                .child(icon(
                    UiId::owned(format!("language-panel.restart.{index}")),
                    "refresh",
                    UiRect::new(
                        button.left + 4.0,
                        button.top + 4.0,
                        button.right - 4.0,
                        button.bottom - 4.0,
                    ),
                    theme::c().text_soft,
                )),
        );
        edge -= 24.0 + 6.0;
    }
    // Installing or restarting replaces the state words; the rest keep them.
    let replaced = section.install.is_some() || (section.restart.is_some() && !current);
    if !section.state.is_empty() && !replaced {
        let width = measure(&section.state, theme::SMALL, 400) + 6.0;
        row = row.child(link_text(
            UiRect::new(edge - width, middle - 9.0, edge, middle + 9.0),
            section.state.clone(),
            theme::c().text_soft,
        ));
        edge -= width + 8.0;
    }
    // The action: a button for the active file's card, a text link elsewhere.
    let action = section
        .install
        .as_ref()
        .map(|(server, label)| (*label, Event::Install(server.clone())))
        .or_else(|| {
            section
                .restart
                .filter(|_| !current)
                .map(|instance| ("Restart", Event::RestartServer(instance)))
        });
    if let Some((label, event)) = action {
        let width = measure(label, theme::SMALL, 400);
        let commands = commands.clone();
        let click = move || match &event {
            Event::Install(server) => commands(Event::Install(server.clone())),
            Event::RestartServer(instance) => commands(Event::RestartServer(*instance)),
            _ => {}
        };
        if current && section.install.is_some() {
            let button = UiRect::new(edge - width - 24.0, middle - 13.0, edge, middle + 13.0);
            row = row.child(
                panel(button, VisualStyle::filled(theme::c().surface).radius(4.0))
                    .event_policy(policy)
                    .cursor(CursorIcon::Pointer)
                    .on_click(move |cx: &mut UiEventContext| {
                        click();
                        cx.stop_propagation();
                    })
                    .child(text(
                        UiRect::new(button.left + 12.0, button.top, button.right, button.bottom),
                        label,
                        theme::sans(theme::c().text_soft, theme::SMALL),
                    )),
            );
            edge = button.left - 8.0;
        } else {
            let hit = UiRect::new(edge - width - 8.0, middle - 11.0, edge, middle + 11.0);
            row = row.child(link(
                hit,
                label,
                theme::c().text_soft,
                TextAlign::Right,
                policy,
                click,
            ));
            edge = hit.left - 4.0;
        }
    }

    // The left side: dot, name, then the subtitle.
    let name_w = (edge - text_left - 8.0).max(0.0);
    let name_size = if current {
        theme::UI_SIZE
    } else {
        theme::SMALL + 1.0
    };
    let name_top = middle
        - if section.subtitle.is_empty() {
            9.0
        } else {
            17.0
        };
    row = row
        .child(
            panel(
                UiRect::new(
                    left,
                    name_top + 9.0 - DOT / 2.0,
                    left + DOT,
                    name_top + 9.0 + DOT / 2.0,
                ),
                VisualStyle::filled(colour).radius(DOT / 2.0),
            )
            .event_policy(EventPolicy::NONE),
        )
        .child(text(
            UiRect::new(text_left, name_top, text_left + name_w, name_top + 18.0),
            ellipsize_sans(
                &section.title,
                name_w,
                name_size,
                if current { 600 } else { 400 },
            ),
            if current {
                theme::sans_semibold(theme::c().text_bright, name_size)
            } else {
                theme::sans(theme::c().text, name_size)
            },
        ));
    if !section.subtitle.is_empty() {
        let quiet = if current {
            theme::c().text_soft
        } else {
            theme::c().text_dim
        };
        row = row.child(text(
            UiRect::new(
                text_left,
                name_top + 18.0,
                text_left + name_w,
                name_top + 34.0,
            ),
            ellipsize_sans(&section.subtitle, name_w, theme::SMALL, 400),
            theme::sans(level_colour(section.subtitle_level, quiet), theme::SMALL),
        ));
    }
    let mut y = strip.top + top_h;
    let text_w = strip.right - INSET - text_left;
    for line in &section.lines {
        // Right-aligned at the row's edge, like the other actions.
        let cancel_w = if line.cancel.is_some() {
            measure("Cancel", theme::SMALL, 400) + 8.0 + 8.0
        } else {
            0.0
        };
        let shown = clip(&line.text, ((text_w - cancel_w) / MONO_W) as usize);
        let line_rect = UiRect::new(
            text_left,
            y - 8.0,
            strip.right - INSET - cancel_w,
            y + LINE_H - 8.0,
        );
        row = row.child(text(
            line_rect,
            shown,
            theme::mono(
                level_colour(line.level, theme::c().text_muted),
                theme::SMALL,
            ),
        ));
        if let Some(target) = line.cancel {
            let button = UiRect::new(
                strip.right - INSET - cancel_w + 8.0,
                y - 8.0,
                strip.right - INSET,
                y + LINE_H - 8.0,
            );
            let cancel = commands.clone();
            row = row.child(link(
                button,
                "Cancel",
                theme::c().text_soft,
                TextAlign::Right,
                policy,
                move || cancel(Event::CancelTask(target)),
            ));
        }
        y += LINE_H;
    }
    row
}

/// Right-aligned small text.
fn link_text(rect: UiRect, label: String, colour: Color) -> Element {
    let mut style = theme::sans(colour, theme::SMALL);
    style.align = TextAlign::Right;
    text(rect, label, style).into()
}

/// Small text that acts on click.
fn link(
    rect: UiRect,
    label: &'static str,
    colour: Color,
    align: TextAlign,
    policy: EventPolicy,
    on_click: impl Fn() + Send + Sync + 'static,
) -> Element {
    let mut style = theme::sans(colour, theme::SMALL);
    style.align = align;
    panel(rect, VisualStyle::default())
        .event_policy(policy)
        .cursor(CursorIcon::Pointer)
        .on_click(move |cx: &mut UiEventContext| {
            on_click();
            cx.stop_propagation();
        })
        .child(text(rect, label, style))
}

/// `value` cut to `max` characters, with an ellipsis when something was left out.
fn clip(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_owned();
    }
    let kept: String = value.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(group: Group, lines: usize) -> Section {
        Section {
            group,
            title: "a".into(),
            subtitle: String::new(),
            subtitle_level: Level::Quiet,
            state: "Ready".into(),
            tone: Tone::Ready,
            progress: Progress::None,
            lines: (0..lines)
                .map(|_| Line {
                    text: "x".into(),
                    level: Level::Quiet,
                    cancel: None,
                })
                .collect(),
            install: None,
            restart: None,
        }
    }

    fn frame(sections: Vec<Section>) -> Frame {
        Frame {
            sections,
        }
    }

    #[test]
    fn the_panel_grows_with_its_lines() {
        let one = frame(vec![section(Group::Current, 1)]);
        let two = frame(vec![section(Group::Current, 2)]);
        assert_eq!(height(&two) - height(&one), LINE_H);
        assert!(height(&Frame::default()) > FOOTER_H);
    }

    #[test]
    fn the_active_files_card_is_taller_than_a_plain_row() {
        let card = frame(vec![section(Group::Current, 0)]);
        let row = frame(vec![section(Group::Other, 0)]);
        assert_eq!(height(&card) - height(&row), CARD_H - ROW_H);
    }
}
