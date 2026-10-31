//! Extensions page shown in its own editor tab: what is installed, what the
//! marketplace offers and what has an update on the left, and the selected
//! extension, what it is and what it adds, on the right.
//!
//! The page only presents. Every button sends a semantic command and leaves
//! the decision to the app layer.

mod content;
mod detail;
mod widgets;

use lgui::core::{UiFocusHandle, UiId, clip};
use lgui::prelude::{Element, TextAlign, UiRect, VisualStyle, panel, scroll_view};
use std::sync::Arc;

use self::widgets::{Ids, Kind, Look, line};
use crate::ui::components::input::{self, AccessInputOptions, InputBinding, InputStyle};
use crate::ui::extensions_binding::{
    Action, Binding, CATEGORIES, Command, Entry, ExtensionsUiState, FooterAction, Frame, Source,
    StateFilter,
};
use crate::ui::tabs::{ellipsize, ellipsize_sans, icon, measure, measure_sans};
use crate::ui::theme;

const LIST_MIN_W: f32 = 240.0;
const LIST_MAX_W: f32 = 340.0;
const SOURCE_H: f32 = 36.0;
const SEARCH_H: f32 = 28.0;
const CHIP_H: f32 = 22.0;
const FOOTER_H: f32 = 32.0;
const ROW_H: f32 = 62.0;
const GROUP_H: f32 = 26.0;
const TILE: f32 = 36.0;
const MARGIN: f32 = 12.0;

/// The entries the list shows for `ui`, in order.
fn visible<'a>(frame: &'a Frame, ui: &ExtensionsUiState, query: &str) -> Vec<&'a Entry> {
    match ui.source {
        Source::Installed => frame
            .installed
            .iter()
            .filter(|entry| entry.matches(query))
            .filter(|entry| match (ui.filter, &entry.installed) {
                (StateFilter::All, _) => true,
                (StateFilter::Enabled, Some(installed)) => installed.enabled,
                (StateFilter::Disabled, Some(installed)) => !installed.enabled,
                _ => false,
            })
            .collect(),
        Source::Marketplace => frame
            .marketplace
            .iter()
            .filter(|entry| entry.matches(query))
            .filter(|entry| {
                ui.category
                    .is_none_or(|category| entry.categories.iter().any(|have| have == category))
            })
            .collect(),
        Source::Updates => frame
            .installed
            .iter()
            .filter(|entry| entry.installed.as_ref().is_some_and(|i| i.update.is_some()))
            .filter(|entry| entry.matches(query))
            .collect(),
    }
}

/// The entry the detail shows: the selected one when the list still has it,
/// otherwise the first.
fn current<'a>(visible: &[&'a Entry], selected: Option<&str>) -> Option<&'a Entry> {
    selected
        .and_then(|id| visible.iter().copied().find(|entry| entry.id == id))
        .or_else(|| visible.first().copied())
}

pub fn render(
    rect: UiRect,
    state: Binding,
    search_id: UiId,
    search_focus: UiFocusHandle,
) -> Element {
    let frame = state.frame();
    let ui = state.ui.get();
    let query = ui.search.text().trim().to_owned();
    let c = theme::c();
    let mut page = panel(rect, VisualStyle::filled(c.bg));

    if frame.installed.is_empty() && ui.source == Source::Installed && query.is_empty() {
        return clip(rect, 0.0, 0.0).child(page.child(empty(rect, &state)));
    }

    let list_w = (rect.width() * 0.32)
        .clamp(LIST_MIN_W, LIST_MAX_W)
        .min(rect.width());
    let list = UiRect::new(rect.left, rect.top, rect.left + list_w, rect.bottom);
    let entries = visible(&frame, &ui, &query);
    let selected = current(&entries, ui.selected.as_deref());

    page = page.child(list_column(
        list,
        &frame,
        &ui,
        &query,
        &entries,
        selected.map(|entry| entry.id.as_str()),
        &state,
        search_id,
        search_focus,
    ));
    let detail_area = UiRect::new(list.right, rect.top, rect.right, rect.bottom);
    page = match selected {
        Some(entry) => page.child(detail::render(
            detail_area,
            entry,
            ui.source == Source::Marketplace,
            if ui.source == Source::Marketplace
                && ui.tab == crate::ui::extensions_binding::DetailTab::Runtime
            {
                crate::ui::extensions_binding::DetailTab::Details
            } else {
                ui.tab
            },
            &state,
        )),
        None => page.child(nothing_selected(detail_area)),
    };
    clip(rect, 0.0, 0.0).child(page)
}

/// No extension is installed: the whole page invites finding one.
fn empty(rect: UiRect, state: &Binding) -> Element {
    let c = theme::c();
    let mut ids = Ids::new("ext.empty");
    let middle = (rect.left + rect.right) / 2.0;
    let top = rect.top + (rect.height() * 0.28).max(40.0);
    let mut root = panel(rect, VisualStyle::default());
    root = root
        .child(widgets::tile(
            &mut ids,
            UiRect::new(middle - 36.0, top, middle + 36.0, top + 72.0),
            "extensions",
            c.accent.0,
            false,
        ))
        .child(line(
            UiRect::new(middle - 240.0, top + 92.0, middle + 240.0, top + 120.0),
            "No extensions installed",
            widgets::aligned(theme::sans_semibold(c.text_bright, 20.0), TextAlign::Center),
        ));
    let (text, height) = widgets::paragraph(
        middle - 215.0,
        top + 130.0,
        430.0,
        "Import a .loomext package to install an extension, or find extensions in the Marketplace.",
        widgets::aligned(theme::sans(c.text_muted, theme::UI_SIZE), TextAlign::Center),
        19.0,
    );
    for element in text {
        root = root.child(element);
    }
    let y = top + 130.0 + height + 16.0;
    let browse = "Browse the Marketplace";
    let import = "Import Extension";
    let browse_w = widgets::button_width(browse, false, theme::UI_SIZE);
    let import_w = widgets::button_width(import, false, theme::UI_SIZE);
    let left = middle - (browse_w + 8.0 + import_w) / 2.0;
    let go = state.clone();
    let open = state.clone();
    root = root
        .child(widgets::button(
            &mut ids,
            "ext.empty.browse".to_owned(),
            UiRect::new(left, y, left + browse_w, y + 28.0),
            browse,
            None,
            Kind::Primary,
            move || go.run(Command::SetSource(Source::Marketplace)),
        ))
        .child(widgets::button(
            &mut ids,
            "ext.empty.import".to_owned(),
            UiRect::new(
                left + browse_w + 8.0,
                y,
                left + browse_w + 8.0 + import_w,
                y + 28.0,
            ),
            import,
            None,
            Kind::Plain,
            move || open.run(Command::Footer(FooterAction::Import)),
        ));
    root.into()
}

fn nothing_selected(area: UiRect) -> Element {
    let c = theme::c();
    panel(area, VisualStyle::filled(c.bg)).child(line(
        UiRect::new(area.left, area.top, area.right, area.bottom),
        "Select an extension to see its details.",
        widgets::aligned(theme::sans(c.text_dim, theme::UI_SIZE), TextAlign::Center),
    ))
}

#[allow(clippy::too_many_arguments)]
fn list_column(
    area: UiRect,
    frame: &Frame,
    ui: &ExtensionsUiState,
    query: &str,
    entries: &[&Entry],
    selected: Option<&str>,
    state: &Binding,
    search_id: UiId,
    search_focus: UiFocusHandle,
) -> Element {
    let c = theme::c();
    let mut ids = Ids::new("ext.list");
    let mut column = panel(area, VisualStyle::filled(c.sidebar));

    // Where the list comes from.
    let updates = frame
        .installed
        .iter()
        .filter(|entry| entry.installed.as_ref().is_some_and(|i| i.update.is_some()))
        .count();
    let sources = [
        (
            Source::Installed,
            "Installed",
            Some(frame.installed.len()),
            false,
        ),
        (Source::Marketplace, "Marketplace", None, false),
        (Source::Updates, "Updates", Some(updates), updates > 0),
    ];
    let tab_w = (area.width() - 2.0 * MARGIN) / sources.len() as f32;
    for (index, (source, label, count, hot)) in sources.into_iter().enumerate() {
        let rect = UiRect::new(
            area.left + MARGIN + tab_w * index as f32,
            area.top,
            area.left + MARGIN + tab_w * (index + 1) as f32,
            area.top + SOURCE_H,
        );
        let selected = ui.source == source;
        let label_w = measure_sans(label, theme::UI_SIZE, 400);
        let count_text = count.map(|count| count.to_string()).unwrap_or_default();
        let count_w = if count.is_some() {
            measure(&count_text, theme::SMALL, 400) + 5.0
        } else {
            0.0
        };
        let start = rect.left + (rect.width() - label_w - count_w) / 2.0;
        let run = state.clone();
        column = column.child(
            widgets::pad(
                format!("ext.source.{label}"),
                rect,
                Look {
                    fill: None,
                    hover: c.active_line,
                    border: None,
                    radius: 4.0,
                },
            )
            .child(line(
                UiRect::new(start, rect.top, start + label_w + 8.0, rect.bottom - 2.0),
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
                    start + label_w + 5.0,
                    rect.top,
                    rect.right,
                    rect.bottom - 2.0,
                ),
                count_text,
                theme::mono(if hot { c.accent } else { c.text_dim }, theme::SMALL),
            ))
            .on_click(move || run.run(Command::SetSource(source))),
        );
        if selected {
            column = column.child(panel(
                UiRect::new(
                    rect.left + 6.0,
                    rect.bottom - 2.0,
                    rect.right - 6.0,
                    rect.bottom,
                ),
                VisualStyle::filled(c.accent),
            ));
        }
    }
    column = column.child(widgets::rule(UiRect::new(
        area.left,
        area.top + SOURCE_H,
        area.right,
        area.top + SOURCE_H + 1.0,
    )));

    // Search.
    let search = UiRect::new(
        area.left + MARGIN,
        area.top + SOURCE_H + 10.0,
        area.right - MARGIN,
        area.top + SOURCE_H + 10.0 + SEARCH_H,
    );
    column = column
        .child(theme::bordered(search, c.bg, c.border, 5.0, 1.0))
        .child(icon(
            ids.next(),
            "search",
            UiRect::new(
                search.left + 8.0,
                search.top + 7.0,
                search.left + 22.0,
                search.bottom - 7.0,
            ),
            c.text_dim,
        ))
        .child(search_input(
            UiRect::new(
                search.left + 26.0,
                search.top + 1.0,
                search.right - 4.0,
                search.bottom - 1.0,
            ),
            state.clone(),
            search_focus,
            search_id,
            match ui.source {
                Source::Installed => "Search installed extensions",
                Source::Marketplace => "Search the Marketplace",
                Source::Updates => "Filter updates",
            },
        ));
    let mut y = search.bottom + 8.0;

    // Filter chips.
    let chips: Vec<(String, bool, Command)> = match ui.source {
        Source::Installed => [
            ("All", StateFilter::All),
            ("Enabled", StateFilter::Enabled),
            ("Disabled", StateFilter::Disabled),
        ]
        .into_iter()
        .map(|(label, filter)| {
            (
                label.to_owned(),
                ui.filter == filter,
                Command::SetFilter(filter),
            )
        })
        .collect(),
        Source::Marketplace => std::iter::once(("All", None))
            .chain(
                CATEGORIES
                    .iter()
                    .map(|category| (*category, Some(*category))),
            )
            .map(|(label, category)| {
                (
                    label.to_owned(),
                    ui.category == category,
                    Command::SetCategory(category),
                )
            })
            .collect(),
        Source::Updates => Vec::new(),
    };
    if !chips.is_empty() {
        let mut left = area.left + MARGIN;
        for (label, on, command) in chips {
            let width = measure_sans(&label, theme::SMALL + 1.0, 400) + 20.0;
            if left + width > area.right - MARGIN {
                break;
            }
            let rect = UiRect::new(left, y, left + width, y + CHIP_H);
            let run = state.clone();
            let look = Look {
                fill: on.then(|| widgets::blend(c.sidebar, c.accent, 0.14)),
                hover: if on {
                    widgets::blend(c.sidebar, c.accent, 0.2)
                } else {
                    c.active_line
                },
                border: Some(if on { c.accent } else { c.border }),
                radius: CHIP_H / 2.0,
            };
            column = column.child(
                widgets::pad(format!("ext.chip.{label}"), rect, look)
                    .child(line(
                        rect,
                        label.clone(),
                        widgets::aligned(
                            theme::sans(
                                if on { c.text_bright } else { c.text_muted },
                                theme::SMALL + 1.0,
                            ),
                            TextAlign::Center,
                        ),
                    ))
                    .on_click(move || run.run(command.clone())),
            );
            left = rect.right + 6.0;
        }
        y += CHIP_H + 8.0;
    }

    // Rows.
    let footer = UiRect::new(area.left, area.bottom - FOOTER_H, area.right, area.bottom);
    let viewport = UiRect::new(area.left, y, area.right, footer.top);
    column = column.child(rows(
        &mut ids, viewport, ui, query, entries, selected, state,
    ));

    // Footer.
    column = column.child(widgets::rule(UiRect::new(
        footer.left,
        footer.top,
        footer.right,
        footer.top + 1.0,
    )));
    let (label, action) = match ui.source {
        Source::Installed => ("Import Extension", FooterAction::Import),
        Source::Marketplace => ("Refresh", FooterAction::RefreshMarketplace),
        Source::Updates => ("Update all", FooterAction::UpdateAll),
    };
    let label_style = theme::sans(c.text_dim, theme::SMALL + 1.0);
    let text = match ui.source {
        Source::Installed => String::new(),
        Source::Marketplace => "Marketplace".to_owned(),
        Source::Updates => format!("{updates} update(s)"),
    };
    column = column.child(line(
        UiRect::new(
            footer.left + MARGIN,
            footer.top,
            footer.right,
            footer.bottom,
        ),
        text,
        label_style,
    ));
    {
        let run = state.clone();
        let width = measure_sans(label, theme::SMALL + 1.0, 400) + 12.0;
        column = column.child(widgets::link(
            "ext.footer.right".to_owned(),
            UiRect::new(
                footer.right - 8.0 - width,
                footer.top + 4.0,
                footer.right - 8.0,
                footer.bottom - 4.0,
            ),
            label,
            TextAlign::Right,
            move || run.run(Command::Footer(action)),
        ));
    }
    column.child(widgets::rule(UiRect::new(
        area.right - 1.0,
        area.top,
        area.right,
        area.bottom,
    )))
}

fn search_input(
    rect: UiRect,
    state: Binding,
    focus: UiFocusHandle,
    id: UiId,
    label: &'static str,
) -> Element {
    let cancel = state.clone();
    input::render_access(
        rect,
        id,
        focus,
        InputBinding::new_access(state.ui, |ui| &ui.search, |ui| &mut ui.search),
        AccessInputOptions {
            label,
            placeholder: label,
            style: InputStyle {
                text: theme::sans(theme::c().text, theme::UI_SIZE),
                placeholder: theme::sans(theme::c().text_faint, theme::UI_SIZE),
                caret: theme::c().text_faint,
                selection: theme::c().accent,
            },
            on_submit: None,
            on_cancel: Some(Arc::new(move || cancel.run(Command::ClearSearch))),
            on_blur: None,
        },
    )
}

/// The scrolling rows of the list, or the message that stands in for them.
fn rows(
    ids: &mut Ids,
    viewport: UiRect,
    ui: &ExtensionsUiState,
    query: &str,
    entries: &[&Entry],
    selected: Option<&str>,
    state: &Binding,
) -> Element {
    let c = theme::c();
    if entries.is_empty() {
        return message(ids, viewport, ui, query, state);
    }
    let mut items: Vec<Element> = Vec::new();
    let mut y = viewport.top;
    if ui.source == Source::Marketplace {
        let title = if query.is_empty() {
            "POPULAR"
        } else {
            "RESULTS"
        };
        let style = theme::mono(c.text_dim, theme::SMALL).tracking(0.8);
        items.push(line(
            UiRect::new(viewport.left + 16.0, y, viewport.right - 16.0, y + GROUP_H),
            title,
            style,
        ));
        items.push(line(
            UiRect::new(viewport.left + 16.0, y, viewport.right - 16.0, y + GROUP_H),
            entries.len().to_string(),
            widgets::aligned(style, TextAlign::Right),
        ));
        y += GROUP_H;
    }
    for entry in entries {
        let rect = UiRect::new(
            viewport.left + 6.0,
            y,
            viewport.right - 6.0,
            y + ROW_H - 2.0,
        );
        items.push(row(
            ids,
            rect,
            entry,
            ui.source,
            selected == Some(entry.id.as_str()),
            state,
        ));
        y += ROW_H;
    }
    let content_h = y - viewport.top + 8.0;
    Element::from(
        scroll_view(viewport, (viewport.width(), content_h))
            .vertical()
            .thumb_color(c.scrollbar)
            .children(items),
    )
    .key(format!("ext.rows.{:?}", ui.source))
}

/// What stands in for an empty list.
fn message(
    ids: &mut Ids,
    viewport: UiRect,
    ui: &ExtensionsUiState,
    query: &str,
    state: &Binding,
) -> Element {
    let c = theme::c();
    let (title, body, action): (String, &str, Option<(String, Command)>) =
        match (ui.source, query.is_empty()) {
            (Source::Installed, false) => (
                format!("No installed extension matches “{query}”"),
                "It may not be installed yet.",
                Some((
                    format!("Search the Marketplace for “{query}”"),
                    Command::SearchMarketplace,
                )),
            ),
            (Source::Installed, true) => (
                format!(
                    "Nothing {}",
                    if ui.filter == StateFilter::Enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                ),
                "Change the filter to see the others.",
                None,
            ),
            (Source::Marketplace, _) => (
                format!(
                    "No extensions found{}",
                    if query.is_empty() {
                        String::new()
                    } else {
                        format!(" for “{query}”")
                    }
                ),
                "Try another word or a category.",
                Some(("Clear search".to_owned(), Command::ClearSearch)),
            ),
            (Source::Updates, _) => (
                "Everything is up to date".to_owned(),
                "Updates for installed extensions appear here.",
                None,
            ),
        };
    let width = viewport.width() - 44.0;
    let center = widgets::aligned(
        theme::sans_semibold(c.text_bright, theme::UI_SIZE + 1.0),
        TextAlign::Center,
    );
    let mut items: Vec<Element> = Vec::new();
    let mut y = viewport.top + 26.0;
    let (lines, height) = widgets::paragraph(viewport.left + 22.0, y, width, &title, center, 19.0);
    items.extend(lines);
    y += height + 4.0;
    let (lines, height) = widgets::paragraph(
        viewport.left + 22.0,
        y,
        width,
        body,
        widgets::aligned(theme::sans(c.text_muted, theme::UI_SIZE), TextAlign::Center),
        19.0,
    );
    items.extend(lines);
    y += height + 12.0;
    if let Some((label, command)) = action {
        let button_w = widgets::button_width(&label, false, theme::UI_SIZE).min(width);
        let middle = (viewport.left + viewport.right) / 2.0;
        let run = state.clone();
        items.push(widgets::button(
            ids,
            "ext.list.message".to_owned(),
            UiRect::new(
                middle - button_w / 2.0,
                y,
                middle + button_w / 2.0,
                y + 28.0,
            ),
            &ellipsize_sans(&label, button_w - 24.0, theme::UI_SIZE, 500),
            None,
            Kind::Plain,
            move || run.run(command.clone()),
        ));
    }
    panel(viewport, VisualStyle::default())
        .children(items)
        .into()
}

/// One entry of the list.
fn row(
    ids: &mut Ids,
    rect: UiRect,
    entry: &Entry,
    source: Source,
    selected: bool,
    state: &Binding,
) -> Element {
    let c = theme::c();
    let enabled = entry
        .installed
        .as_ref()
        .is_none_or(|installed| installed.enabled);
    let quiet = !enabled && source != Source::Marketplace;
    let select = {
        let run = state.clone();
        let id = entry.id.clone();
        move || run.run(Command::Select(id.clone()))
    };
    let mut row = widgets::pad(
        format!("ext.row.{:?}.{}", source, entry.id),
        rect,
        Look {
            fill: selected.then(|| widgets::blend(c.sidebar, c.accent, 0.09)),
            hover: if selected {
                widgets::blend(c.sidebar, c.accent, 0.12)
            } else {
                c.active_line
            },
            border: selected.then(|| widgets::blend(c.border, c.accent, 0.3)),
            radius: 7.0,
        },
    )
    .semantics(
        lgui::core::Semantics::new(lgui::core::SemanticRole::Button).name(entry.name.clone()),
    )
    .on_click(select);

    let tile = UiRect::new(
        rect.left + 10.0,
        rect.top + (rect.height() - TILE) / 2.0,
        rect.left + 10.0 + TILE,
        rect.top + (rect.height() + TILE) / 2.0,
    );
    row = row.child(widgets::extension_tile(ids, tile, entry, quiet));

    // The right edge, from the outside in.
    let mut edge = rect.right - 10.0;
    let middle = rect.top + rect.height() / 2.0;
    match source {
        Source::Installed => {
            if let Some(installed) = &entry.installed {
                if installed.enabled && installed.error.is_some() {
                    row = row.child(icon(
                        ids.next(),
                        "warning",
                        UiRect::new(edge - 15.0, middle - 7.5, edge, middle + 7.5),
                        c.error,
                    ));
                    edge -= 21.0;
                }
                if installed.update.is_some() {
                    row = row.child(icon(
                        ids.next(),
                        "update-available",
                        UiRect::new(edge - 15.0, middle - 7.5, edge, middle + 7.5),
                        c.accent,
                    ));
                    edge -= 21.0;
                }
            }
        }
        Source::Marketplace => {
            if entry.installed.is_some() {
                let label = "Installed";
                let width = measure_sans(label, theme::SMALL + 1.0, 400);
                row = row
                    .child(icon(
                        ids.next(),
                        "check",
                        UiRect::new(
                            edge - width - 18.0,
                            middle - 6.0,
                            edge - width - 6.0,
                            middle + 6.0,
                        ),
                        c.diff.add_mark,
                    ))
                    .child(line(
                        UiRect::new(edge - width - 2.0, middle - 9.0, edge + 4.0, middle + 9.0),
                        label,
                        theme::sans(c.diff.add_mark, theme::SMALL + 1.0),
                    ));
                edge -= width + 24.0;
            } else {
                let run = state.clone();
                let id = entry.id.clone();
                let width = widgets::button_width("Install", false, theme::SMALL + 1.0);
                row = row.child(widgets::button(
                    ids,
                    format!("ext.row.install.{}", entry.id),
                    UiRect::new(edge - width, middle - 11.0, edge, middle + 11.0),
                    "Install",
                    None,
                    Kind::Plain,
                    move || run.run(Command::Act(Action::Install, id.clone())),
                ));
                edge -= width + 8.0;
            }
        }
        Source::Updates => {
            let run = state.clone();
            let id = entry.id.clone();
            let width = widgets::button_width("Update", false, theme::SMALL + 1.0);
            row = row.child(widgets::button(
                ids,
                format!("ext.row.update.{}", entry.id),
                UiRect::new(edge - width, middle - 11.0, edge, middle + 11.0),
                "Update",
                None,
                Kind::Primary,
                move || run.run(Command::Act(Action::Update, id.clone())),
            ));
            edge -= width + 8.0;
        }
    }

    // The text: name and version, a line about it, and who made it.
    let left = tile.right + 10.0;
    let width = (edge - left).max(40.0);
    let name_style = if quiet {
        theme::sans_semibold(c.text_dim, theme::UI_SIZE)
    } else {
        theme::sans_semibold(c.text_bright, theme::UI_SIZE)
    };
    let version_w = measure(&entry.version, theme::SMALL, 400) + 4.0;
    let name = ellipsize_sans(
        &entry.name,
        (width - version_w - 6.0).max(30.0),
        theme::UI_SIZE,
        600,
    );
    let name_w = measure_sans(&name, theme::UI_SIZE, 600);
    let top = rect.top + 9.0;
    let (second, third) = match source {
        Source::Updates => {
            let update = entry.installed.as_ref().and_then(|i| i.update.as_ref());
            (
                update.map_or_else(String::new, |u| {
                    format!("{} → {}", entry.version, u.version)
                }),
                update.map_or_else(String::new, |u| u.notes.clone()),
            )
        }
        _ => (entry.tagline.clone(), String::new()),
    };
    row = row
        .child(line(
            UiRect::new(left, top, left + name_w + 8.0, top + 16.0),
            name,
            name_style,
        ))
        .child(line(
            UiRect::new(left + name_w + 6.0, top, left + width, top + 16.0),
            if source == Source::Updates {
                String::new()
            } else {
                entry.version.clone()
            },
            theme::mono(c.text_dim, theme::SMALL),
        ))
        .child(line(
            UiRect::new(left, top + 17.0, left + width + 6.0, top + 32.0),
            ellipsize_sans(&second, width, theme::SMALL + 1.0, 400),
            theme::sans(c.text_muted, theme::SMALL + 1.0),
        ));
    let meta_rect = UiRect::new(left, top + 32.0, left + width + 6.0, top + 46.0);
    row = match source {
        Source::Installed => row.child(line(
            meta_rect,
            if quiet {
                format!("{} · Disabled", entry.publisher)
            } else {
                entry.publisher.clone()
            },
            theme::sans(c.text_dim, theme::SMALL + 1.0),
        )),
        Source::Marketplace => {
            let market = entry.market.as_ref();
            row.child(line(
                meta_rect,
                format!(
                    "{}   ↓ {}   ★ {}",
                    entry.publisher,
                    market.map_or("—", |m| m.downloads.as_str()),
                    market.map_or("—", |m| m.rating.as_str())
                ),
                theme::sans(c.text_dim, theme::SMALL + 1.0),
            ))
        }
        Source::Updates => row.child(line(
            meta_rect,
            ellipsize(&third, width, theme::SMALL + 1.0, 400),
            theme::sans(c.text_dim, theme::SMALL + 1.0),
        )),
    };
    row
}

#[cfg(test)]
mod tests;
