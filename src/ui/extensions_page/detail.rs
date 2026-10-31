//! The detail pane: the selected extension's identity and actions, its tabs,
//! and the body of the tab that is open.
use lgui::prelude::{Element, TextAlign, UiRect, VisualStyle, panel};

use super::content::{self, Body};
use super::widgets::{self, Ids, Kind, Look, Tone, line};
use crate::ui::extensions_binding::{Action, Binding, Command, DetailTab, Entry};
use crate::ui::tabs::{ellipsize_sans, measure, measure_sans};
use crate::ui::theme;

const PAD_X: f32 = 32.0;
const PAD_TOP: f32 = 26.0;
const TILE: f32 = 72.0;
const BUTTON_H: f32 = 28.0;
const TABS_H: f32 = 38.0;

/// The tabs of a detail.
pub(super) fn tabs() -> Vec<(DetailTab, &'static str)> {
    vec![
        (DetailTab::Details, "Details"),
        (DetailTab::Contributions, "Contributions"),
        (DetailTab::Changelog, "Changelog"),
    ]
}

/// The state shown at the top right of the head, with its colour.
fn state_label(entry: &Entry) -> (&'static str, Tone) {
    match &entry.installed {
        None => ("Not installed", Tone::Quiet),
        Some(installed) if !installed.enabled => ("Disabled", Tone::Quiet),
        Some(_) if entry.failed() => ("Could not start", Tone::Bad),
        Some(_) => ("Enabled", Tone::Ok),
    }
}

struct HeadButton {
    label: &'static str,
    icon: Option<&'static str>,
    kind: Kind,
    command: Option<Command>,
}

fn actions(entry: &Entry) -> Vec<HeadButton> {
    let id = entry.id.clone();
    let act = |action| Some(Command::Act(action, id.clone()));
    let Some(installed) = &entry.installed else {
        return Vec::new();
    };
    let mut buttons = Vec::new();
    if installed.enabled {
        buttons.push(HeadButton {
            label: "Disable",
            icon: None,
            kind: Kind::Plain,
            command: act(Action::Disable),
        });
    } else {
        buttons.push(HeadButton {
            label: "Enable",
            icon: None,
            kind: Kind::Primary,
            command: act(Action::Enable),
        });
    }
    if entry.failed() {
        buttons.push(HeadButton {
            label: "Reload",
            icon: Some("refresh"),
            kind: Kind::Primary,
            command: act(Action::Reload),
        });
    }
    if !installed.bundled {
        buttons.push(HeadButton {
            label: "Uninstall",
            icon: None,
            kind: Kind::Plain,
            command: act(Action::Uninstall),
        });
    }
    buttons
}

pub(super) fn render(area: UiRect, entry: &Entry, tab: DetailTab, state: &Binding) -> Element {
    let c = theme::c();
    let mut ids = Ids::new("ext.detail");
    let mut root = panel(area, VisualStyle::filled(c.bg));
    let top = area.top + PAD_TOP;
    let tile_rect = UiRect::new(area.left + PAD_X, top, area.left + PAD_X + TILE, top + TILE);
    root = root.child(widgets::extension_tile(&mut ids, tile_rect, entry, false));
    let left = tile_rect.right + 20.0;
    let right = (area.right - PAD_X).max(left + 80.0);

    // The state, at the top right.
    let (state_text, state_tone) = state_label(entry);
    let state_w = measure_sans(state_text, theme::SMALL + 1.0, 400) + 4.0;
    let ink = widgets::tone(state_tone);
    root = root
        .child(panel(
            UiRect::new(
                right - state_w - 14.0,
                top + 11.0,
                right - state_w - 8.0,
                top + 17.0,
            ),
            VisualStyle::filled(ink).radius(3.0),
        ))
        .child(line(
            UiRect::new(right - state_w - 4.0, top + 4.0, right + 8.0, top + 24.0),
            state_text,
            theme::sans(ink, theme::SMALL + 1.0),
        ));

    // Name, then who made it.
    let title_w = (right - left - state_w - 28.0).max(60.0);
    root = root.child(line(
        UiRect::new(left, top, left + title_w, top + 30.0),
        ellipsize_sans(&entry.name, title_w, 22.0, 600),
        theme::sans_semibold(c.text_bright, 22.0),
    ));
    let mut y = top + 36.0;
    let publisher_w = measure_sans(&entry.publisher, theme::UI_SIZE, 400) + 6.0;
    root = root.child(line(
        UiRect::new(left, y, left + publisher_w, y + 18.0),
        entry.publisher.clone(),
        theme::sans(c.text_muted, theme::UI_SIZE),
    ));
    let version = format!("v{}", entry.version);
    let version_w = measure(&version, theme::SMALL, 400) + 14.0;
    let badge = UiRect::new(
        left + publisher_w + 10.0,
        y,
        left + publisher_w + 10.0 + version_w,
        y + 18.0,
    );
    root = root
        .child(theme::bordered(badge, c.bg, c.border, 4.0, 1.0))
        .child(line(
            badge,
            version,
            widgets::aligned(theme::mono(c.text_dim, theme::SMALL), TextAlign::Center),
        ))
        .child(line(
            UiRect::new(badge.right + 10.0, y, right, y + 18.0),
            entry.id.clone(),
            theme::mono(c.text_dim, theme::SMALL),
        ));
    y += 24.0;

    root = root.child(line(
        UiRect::new(left, y, right, y + 22.0),
        ellipsize_sans(&entry.tagline, right - left, theme::UI_SIZE + 2.0, 400),
        theme::sans(c.text, theme::UI_SIZE + 2.0),
    ));
    y += 32.0;

    // Actions.
    let mut button_left = left;
    for (index, action) in actions(entry).into_iter().enumerate() {
        let width = widgets::button_width(action.label, action.icon.is_some(), theme::UI_SIZE);
        let rect = UiRect::new(button_left, y, button_left + width, y + BUTTON_H);
        let run = state.clone();
        let command = action.command;
        root = root.child(widgets::button(
            &mut ids,
            format!("ext.head.{}.{index}", entry.id),
            rect,
            action.label,
            action.icon,
            action.kind,
            move || {
                if let Some(command) = command.clone() {
                    run.run(command);
                }
            },
        ));
        button_left = rect.right + 8.0;
    }
    y += BUTTON_H;

    // Tabs, over a rule the selected one's underline covers.
    let tabs_top = (y + 22.0).max(tile_rect.bottom + 22.0);
    let tabs_bottom = tabs_top + TABS_H;
    root = root.child(widgets::rule(UiRect::new(
        area.left,
        tabs_bottom - 1.0,
        area.right,
        tabs_bottom,
    )));
    let mut tab_left = area.left + PAD_X;
    for (kind, label) in tabs() {
        let count = (kind == DetailTab::Contributions).then(|| entry.contribution_count());
        let label_w = measure_sans(label, theme::UI_SIZE, 400);
        let count_text = count.map(|count| count.to_string()).unwrap_or_default();
        let count_w = if count.is_some() {
            measure(&count_text, theme::SMALL, 400) + 6.0
        } else {
            0.0
        };
        let rect = UiRect::new(
            tab_left - 4.0,
            tabs_top,
            tab_left + label_w + count_w + 4.0,
            tabs_bottom,
        );
        let selected = kind == tab;
        let run = state.clone();
        root = root.child(
            widgets::pad(
                format!("ext.tab.{}.{label}", entry.id),
                rect,
                Look {
                    fill: None,
                    hover: c.bg,
                    border: None,
                    radius: 0.0,
                },
            )
            .child(line(
                UiRect::new(
                    tab_left,
                    tabs_top,
                    tab_left + label_w + 8.0,
                    tabs_bottom - 2.0,
                ),
                label,
                theme::sans(
                    if selected {
                        c.text_bright
                    } else {
                        c.text_muted
                    },
                    theme::UI_SIZE,
                ),
            ))
            .child(line(
                UiRect::new(
                    tab_left + label_w + 6.0,
                    tabs_top,
                    tab_left + label_w + count_w + 12.0,
                    tabs_bottom - 2.0,
                ),
                count_text,
                theme::mono(c.text_dim, theme::SMALL),
            ))
            .on_click(move || run.run(Command::SetTab(kind))),
        );
        if selected {
            root = root.child(panel(
                UiRect::new(rect.left, tabs_bottom - 2.0, rect.right, tabs_bottom),
                VisualStyle::filled(c.accent),
            ));
        }
        tab_left = rect.right + 22.0;
    }
    let region = UiRect::new(area.left, tabs_bottom, area.right, area.bottom);
    root.child(content::render(
        region,
        &mut ids,
        Body { entry, tab, state },
    ))
}
