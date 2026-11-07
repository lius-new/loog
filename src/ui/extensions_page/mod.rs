//! Extensions page shown in its own editor tab: the installed extensions on the
//! left, and the selected extension, what it is and what it adds, on the right.
//!
//! The page only presents. Every button sends a semantic command and leaves
//! the decision to the app layer.

mod content;
mod detail;
mod scroll;
mod widgets;

use lgui::core::{UiFocusHandle, UiId, clip};
use lgui::prelude::{Element, TextAlign, UiRect, VisualStyle, panel};
use std::sync::Arc;

use self::widgets::{Ids, Kind, Look, line};
use crate::ui::components::input::InputBinding;
use crate::ui::components::search_field;
use crate::ui::extensions_binding::{
    Binding, Command, Entry, ExtensionsUiState, Frame, RegistryStatus,
};
use crate::ui::tabs::{ellipsize_sans, icon, measure, measure_sans};
use crate::ui::theme;

const LIST_MIN_W: f32 = 240.0;
const LIST_MAX_W: f32 = 340.0;
const SEARCH_TOP: f32 = 12.0;
const SEARCH_H: f32 = search_field::HEIGHT;
const ROW_H: f32 = 62.0;
const TILE: f32 = 36.0;
const MARGIN: f32 = 12.0;

/// The entries the list shows for `ui`, in order.
///
/// Without a search, what is installed. A search looks through what is installed
/// and then through what the registry offers.
fn visible<'a>(frame: &'a Frame, query: &str) -> Vec<&'a Entry> {
    if query.is_empty() {
        return frame.installed.iter().collect();
    }
    frame
        .installed
        .iter()
        .chain(&frame.available)
        .filter(|entry| entry.matches(query))
        .collect()
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
    let page = panel(rect, VisualStyle::filled(c.bg));

    let list_w = (rect.width() * 0.32)
        .clamp(LIST_MIN_W, LIST_MAX_W)
        .min(rect.width());
    let default_ratio = list_w / (rect.width() - 4.0).max(1.0);
    let reader = state.ui.clone();
    let writer = state.ui.clone();
    let ratio = lgui::WidgetValue::new(
        move || reader.get().split_ratio.unwrap_or(default_ratio),
        move |ratio| writer.update(|ui| ui.split_ratio = Some(ratio)),
    );
    let entries = visible(&frame, &query);
    let selected = current(&entries, ui.selected.as_deref());

    let page = page.child(lgui::split(
        rect,
        lgui::core::Axis::Horizontal,
        ratio,
        (LIST_MIN_W, 300.0),
        |list| {
            list_column(
                list,
                &ui,
                &query,
                &entries,
                selected.map(|entry| entry.id.as_str()),
                &frame.registry,
                &state,
                search_id,
                search_focus,
            )
        },
        |detail_area| match selected {
            Some(entry) => detail::render(
                detail_area,
                entry,
                ui.tab,
                detail::Context {
                    registry: &frame.registry,
                    asks: frame.asks.get(&entry.id).map(Vec::as_slice).unwrap_or(&[]),
                    confirming: ui.confirming.as_deref() == Some(entry.id.as_str()),
                },
                &state,
            ),
            // Nothing installed and nothing searched for: the page invites finding something.
            None if frame.installed.is_empty() && query.is_empty() => empty(detail_area, &state),
            None => nothing_selected(detail_area),
        },
    ));
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
        "Search for extensions in the box on the left, or import a .loogext package.",
        widgets::aligned(theme::sans(c.text_muted, theme::UI_SIZE), TextAlign::Center),
        19.0,
    );
    for element in text {
        root = root.child(element);
    }
    let y = top + 130.0 + height + 16.0;
    let import = "Import Extension";
    let import_w = widgets::button_width(import, false, theme::UI_SIZE);
    let open = state.clone();
    root = root.child(widgets::button(
        &mut ids,
        "ext.empty.import".to_owned(),
        UiRect::new(
            middle - import_w / 2.0,
            y,
            middle + import_w / 2.0,
            y + 28.0,
        ),
        import,
        None,
        Kind::Primary,
        move || open.run(Command::Import),
    ));
    root
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
    ui: &ExtensionsUiState,
    query: &str,
    entries: &[&Entry],
    selected: Option<&str>,
    registry: &RegistryStatus,
    state: &Binding,
    search_id: UiId,
    search_focus: UiFocusHandle,
) -> Element {
    let c = theme::c();
    let mut ids = Ids::new("ext.list");
    let mut column = panel(area, VisualStyle::filled(c.sidebar));

    // Search.
    let search = UiRect::new(
        area.left + MARGIN,
        area.top + SEARCH_TOP,
        area.right - MARGIN,
        area.top + SEARCH_TOP + SEARCH_H,
    );
    let cancel = state.clone();
    let mut options = search_field::Options::new("Search extensions");
    options.trailing_width = if registry.refreshing { 28.0 } else { 4.0 };
    options.on_cancel = Some(Arc::new(move || cancel.run(Command::ClearSearch)));
    column = column.child(search_field::render(
        search,
        search_id,
        search_focus,
        InputBinding::new_access(state.ui.clone(), |ui| &ui.search, |ui| &mut ui.search),
        options,
    ));
    let y = search.bottom + 8.0;
    let ring = UiRect::new(
        search.right - 22.0,
        search.top + 4.0,
        search.right - 6.0,
        search.bottom - 4.0,
    );
    column = column.child(
        lgui::presence(
            ring,
            registry.refreshing,
            lgui::VisibilityTransition::from_theme(theme::widget_theme().tokens()),
            lgui::progress_ring(
                ring,
                if registry.refreshing { None } else { Some(0.0) },
                c.accent,
            ),
        )
        .z_index(1),
    );

    // Rows.
    let viewport = UiRect::new(area.left, y, area.right, area.bottom);
    column = column.child(rows(
        &mut ids, viewport, ui, query, entries, selected, registry, state,
    ));
    column.child(widgets::rule(UiRect::new(
        area.right - 1.0,
        area.top,
        area.right,
        area.bottom,
    )))
}

/// The scrolling rows of the list, or the message that stands in for them.
fn rows(
    ids: &mut Ids,
    viewport: UiRect,
    _ui: &ExtensionsUiState,
    query: &str,
    entries: &[&Entry],
    selected: Option<&str>,
    registry: &RegistryStatus,
    state: &Binding,
) -> Element {
    if entries.is_empty() {
        return message(ids, viewport, query, registry, state);
    }
    let owner = query.to_owned();
    let reader = state.ui.clone();
    let writer = state.ui.clone();
    let write_owner = owner.clone();
    let offset = lgui::WidgetValue::new(
        move || reader.get().list.at(&owner),
        move |offset| {
            writer.update(|ui| {
                ui.list.owner = write_owner.clone();
                ui.list.offset = offset;
            })
        },
    );
    lgui::virtual_list(
        viewport,
        Arc::new(lgui::RowHeights::uniform(entries.len(), ROW_H)),
        offset,
        |index, rect| {
            let entry = entries[index];
            let mut ids = Ids::new(format!("ext.row.{}", entry.id));
            row(
                &mut ids,
                UiRect::new(
                    rect.left + 6.0,
                    rect.top,
                    rect.right - 6.0,
                    rect.bottom - 2.0,
                ),
                entry,
                selected == Some(entry.id.as_str()),
                !query.is_empty(),
                state,
            )
            .key(&entry.id)
        },
    )
    .key("ext.rows")
}

/// What stands in for an empty list.
fn message(
    ids: &mut Ids,
    viewport: UiRect,
    query: &str,
    registry: &RegistryStatus,
    state: &Binding,
) -> Element {
    let c = theme::c();
    let (title, body, action): (String, String, Option<(String, Command)>) = if query.is_empty() {
        (
            "Nothing installed".to_owned(),
            "Type above to search the extensions Loog offers.".to_owned(),
            None,
        )
    } else {
        // The registry is searched too; say when it could not be.
        match (&registry.problem, registry.loaded, registry.refreshing) {
            (_, false, true) => (
                format!("No installed extension matches “{query}”"),
                "Still looking in the extension registry…".to_owned(),
                None,
            ),
            (Some(problem), false, _) => (
                format!("No installed extension matches “{query}”"),
                format!("Could not reach the extension registry: {problem}"),
                Some(("Try again".to_owned(), Command::Refresh)),
            ),
            (_, false, false) => (
                format!("No installed extension matches “{query}”"),
                "The extension registry has not been read.".to_owned(),
                Some(("Look for extensions".to_owned(), Command::Refresh)),
            ),
            _ => (
                format!("No extension matches “{query}”"),
                "Try another word.".to_owned(),
                Some(("Clear search".to_owned(), Command::ClearSearch)),
            ),
        }
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
        &body,
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
    panel(viewport, VisualStyle::default()).children(items)
}

/// One entry of the list.
fn row(
    ids: &mut Ids,
    rect: UiRect,
    entry: &Entry,
    selected: bool,
    searching: bool,
    state: &Binding,
) -> Element {
    let c = theme::c();
    let enabled = entry
        .installed
        .as_ref()
        .is_none_or(|installed| installed.enabled);
    let quiet = !enabled;
    let select = {
        let run = state.clone();
        let id = entry.id.clone();
        move || run.run(Command::Select(id.clone()))
    };
    let mut row = widgets::pad(
        format!("ext.row.{}", entry.id),
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
    // Among search results an installed extension is marked with a pill, so it
    // stands out from the ones that can be installed.
    if searching && entry.installed.is_some() {
        let style = theme::sans_semibold(c.accent, theme::SMALL);
        let width = measure_sans("Installed", theme::SMALL, 600) + 16.0;
        let pill = UiRect::new(edge - width, rect.top + 9.0, edge, rect.top + 25.0);
        row = row.child(
            theme::bordered(
                pill,
                widgets::blend(c.sidebar, c.accent, 0.16),
                c.accent,
                pill.height() / 2.0,
                1.0,
            )
            .child(line(
                pill,
                "Installed",
                widgets::aligned(style, TextAlign::Center),
            )),
        );
        edge -= width + 8.0;
    }
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
    let update = entry.installed.as_ref().and_then(|i| i.update.as_ref());
    row = row
        .child(line(
            UiRect::new(left, top, left + name_w + 8.0, top + 16.0),
            name,
            name_style,
        ))
        .child(line(
            UiRect::new(left + name_w + 6.0, top, left + width, top + 16.0),
            entry.version.clone(),
            theme::mono(c.text_dim, theme::SMALL),
        ))
        .child(line(
            UiRect::new(left, top + 17.0, left + width + 6.0, top + 32.0),
            ellipsize_sans(&entry.tagline, width, theme::SMALL + 1.0, 400),
            theme::sans(c.text_muted, theme::SMALL + 1.0),
        ));
    // The status of the extension, after who made it.
    let meta_rect = UiRect::new(left, top + 32.0, left + width + 6.0, top + 46.0);
    // Among search results an installed extension says so.
    let mut notes: Vec<String> = Vec::new();
    if quiet {
        notes.push("Disabled".to_owned());
    }
    match update {
        Some(update) => notes.push(format!("Update to {}", update.version)),
        None if entry.installed.as_ref().is_some_and(|i| i.pinned) => {
            notes.push("Pinned".to_owned())
        }
        None => {}
    }
    let ink = if update.is_some() {
        c.accent
    } else {
        c.text_dim
    };
    let meta = std::iter::once(entry.publisher.as_str())
        .chain(notes.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" · ");
    row.child(line(
        meta_rect,
        ellipsize_sans(&meta, width, theme::SMALL + 1.0, 400),
        theme::sans(ink, theme::SMALL + 1.0),
    ))
}

#[cfg(test)]
mod tests;
