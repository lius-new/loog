//! The detail pane: the selected extension's identity and actions, its tabs,
//! and the body of the tab that is open.
use lgui::prelude::{Element, TextAlign, UiRect, VisualStyle, panel};

use super::content::{self, Body};
use super::widgets::{self, Ids, Kind, Tone, line};
use crate::ui::extensions_binding::{Action, Binding, Command, DetailTab, Entry, RegistryStatus};
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
    label: String,
    icon: Option<&'static str>,
    kind: Kind,
    command: Option<Command>,
}

/// What the detail needs to know besides the extension itself.
#[derive(Clone, Copy)]
pub(super) struct Context<'a> {
    pub registry: &'a RegistryStatus,
    /// What installing or updating it asks for, in words.
    pub asks: &'a [String],
    /// The user is being asked whether to accept `asks`.
    pub confirming: bool,
}

fn button(label: impl Into<String>, kind: Kind, command: Option<Command>) -> HeadButton {
    HeadButton {
        label: label.into(),
        icon: None,
        kind,
        command,
    }
}

fn actions(entry: &Entry, context: Context) -> Vec<HeadButton> {
    let id = entry.id.clone();
    let act = |action| Some(Command::Act(action, id.clone()));
    let busy = context.registry.installing.contains(&entry.id);
    let Some(installed) = &entry.installed else {
        return if busy {
            vec![button("Installing…", Kind::Plain, None)]
        } else if context.confirming {
            vec![
                button("Accept and install", Kind::Primary, act(Action::Install)),
                button("Cancel", Kind::Plain, Some(Command::CancelInstall)),
            ]
        } else {
            vec![button("Install", Kind::Primary, act(Action::Install))]
        };
    };
    let mut buttons = Vec::new();
    if let Some(update) = &installed.update {
        if busy {
            buttons.push(button("Updating…", Kind::Plain, None));
        } else if context.confirming {
            buttons.push(button(
                "Accept and update",
                Kind::Primary,
                act(Action::Update),
            ));
            buttons.push(button("Cancel", Kind::Plain, Some(Command::CancelInstall)));
        } else {
            buttons.push(button(
                format!("Update to {}", update.version),
                Kind::Primary,
                act(Action::Update),
            ));
        }
    }
    if !installed.bundled {
        buttons.push(if installed.pinned {
            button("Unpin", Kind::Plain, act(Action::Unpin))
        } else {
            button("Pin Version", Kind::Plain, act(Action::Pin))
        });
        if let Some(previous) = &installed.previous {
            buttons.push(button(
                format!("Go Back to {previous}"),
                Kind::Plain,
                act(Action::Rollback),
            ));
        }
    }
    if installed.enabled {
        buttons.push(button("Disable", Kind::Plain, act(Action::Disable)));
    } else {
        buttons.push(button("Enable", Kind::Primary, act(Action::Enable)));
    }
    if entry.failed() {
        buttons.push(HeadButton {
            label: "Reload".to_owned(),
            icon: Some("refresh"),
            kind: Kind::Primary,
            command: act(Action::Reload),
        });
    }
    if !installed.bundled {
        buttons.push(button("Uninstall", Kind::Plain, act(Action::Uninstall)));
    }
    buttons
}

pub(super) fn render(
    area: UiRect,
    entry: &Entry,
    tab: DetailTab,
    context: Context,
    state: &Binding,
) -> Element {
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
    for (index, action) in actions(entry, context).into_iter().enumerate() {
        let width = widgets::button_width(&action.label, action.icon.is_some(), theme::UI_SIZE);
        let rect = UiRect::new(button_left, y, button_left + width, y + BUTTON_H);
        let run = state.clone();
        let command = action.command;
        root = root.child(widgets::button(
            &mut ids,
            format!("ext.head.{}.{index}", entry.id),
            rect,
            &action.label,
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

    // What accepting would let the extension do, when the user is being asked.
    if context.confirming && !context.asks.is_empty() {
        y += 12.0;
        let title = "Installing this lets it:";
        let mut lines = vec![line(
            UiRect::new(left + 12.0, y + 8.0, right - 12.0, y + 26.0),
            title,
            theme::sans_semibold(c.text_bright, theme::UI_SIZE),
        )];
        let mut inner = y + 30.0;
        for ask in context.asks {
            let (text, height) = widgets::paragraph(
                left + 12.0,
                inner,
                right - left - 24.0,
                &format!("• {ask}"),
                theme::sans(c.text, theme::UI_SIZE),
                19.0,
            );
            lines.extend(text);
            inner += height + 2.0;
        }
        let box_rect = UiRect::new(left, y, right, inner + 8.0);
        root = root.child(theme::bordered(
            box_rect,
            widgets::blend(c.bg, c.accent, 0.06),
            widgets::blend(c.border, c.accent, 0.4),
            6.0,
            1.0,
        ));
        for element in lines {
            root = root.child(element);
        }
        y = box_rect.bottom;
    }

    // Tabs, over a rule the selected one's underline covers.
    let tabs_top = (y + 22.0).max(tile_rect.bottom + 22.0);
    let tabs_bottom = tabs_top + TABS_H;
    root = root.child(widgets::rule(UiRect::new(
        area.left,
        tabs_bottom - 1.0,
        area.right,
        tabs_bottom,
    )));
    let current = state.clone();
    let update = state.clone();
    let index = lgui::WidgetValue::new(
        move || match current.ui.get().tab {
            DetailTab::Details => 0,
            DetailTab::Contributions => 1,
            DetailTab::Changelog => 2,
        },
        move |index| {
            update.run(Command::SetTab(match index {
                1 => DetailTab::Contributions,
                2 => DetailTab::Changelog,
                _ => DetailTab::Details,
            }))
        },
    );
    let labels = tabs()
        .into_iter()
        .map(|(kind, label)| {
            if kind == DetailTab::Contributions {
                format!("{label} ({})", entry.contribution_count())
            } else {
                label.to_owned()
            }
        })
        .collect::<Vec<_>>();
    // The portable control owns header selection and confines arrow keys to it.
    root.child(lgui::tabs(
        UiRect::new(area.left, tabs_top, area.right, area.bottom),
        labels.into(),
        index,
        |_, region| content::render(region, &mut ids, Body { entry, tab, state }),
    ))
}
