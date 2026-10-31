//! Keymap page shown in its own editor tab: every action with its key
//! bindings, a filter, and a shortcut to the user's `keymap.json`.
//!
//! The page is read-only; bindings are customized in `keymap.json`, which
//! reloads as soon as it is saved.

use lgui::core::{CursorIcon, EventPolicy, UiFocusHandle, UiId, WheelUnit, clip};
use lgui::prelude::{Element, TextAlign, UiRect, VisualStyle, group, panel, text};

use crate::features::keymap::action::{ACTIONS, Action};
use crate::features::keymap::bindings::Keymap;
use crate::ui::components::input::{self, AccessInputOptions, InputBinding, InputStyle};
use crate::ui::keymap_binding::{Binding, Command};
use crate::ui::tabs::{TEXT_MARGIN, measure};
use crate::ui::theme;
use std::sync::Arc;

const MAX_PAGE_W: f32 = 880.0;
const PAGE_PAD_X: f32 = 32.0;
const PAGE_PAD_TOP: f32 = 36.0;
const HEADER_H: f32 = 64.0;
const TOOLBAR_H: f32 = 30.0;
const TOOLBAR_GAP: f32 = 16.0;
const ROW_H: f32 = 28.0;
const SCROLL_STEP: f32 = 24.0;
const BUTTON_PAD_X: f32 = 12.0;
/// Column starts and widths as fractions of the table width.
const COLUMNS: [(f32, f32); 4] = [(0.0, 0.40), (0.40, 0.22), (0.62, 0.28), (0.90, 0.10)];

/// One line of the shortcut table.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ShortcutRow {
    action: String,
    description: &'static str,
    keys: String,
    context: String,
    source: &'static str,
}

impl ShortcutRow {
    /// Whether every whitespace-separated term of `query` appears somewhere
    /// in the row, ignoring case.
    fn matches(&self, query: &str) -> bool {
        let haystack = format!(
            "{} {} {} {} {}",
            self.description, self.action, self.keys, self.context, self.source
        )
        .to_lowercase();
        query
            .to_lowercase()
            .split_whitespace()
            .all(|term| haystack.contains(term))
    }
}

/// Every action in catalog order with each of its active bindings; actions
/// without a binding get one row so they stay discoverable.
fn shortcut_rows(keymap: &Keymap) -> Vec<ShortcutRow> {
    let mut rows = Vec::new();
    for meta in ACTIONS {
        let start = rows.len();
        for binding in keymap
            .active_bindings()
            .filter(|binding| binding.action.name() == meta.name)
        {
            let action = match binding.action {
                Action::ActivateItem(index) => format!("{} {index}", meta.name),
                Action::VimKeys(keys) => format!("{} {}", meta.name, keys.keys()),
                Action::RunExtension(command) => format!("{} {}", meta.name, command.id()),
                _ => meta.name.to_owned(),
            };
            rows.push(ShortcutRow {
                action,
                description: meta.description,
                keys: binding.label(),
                context: binding.context_label(),
                source: binding.source.label(),
            });
        }
        if rows.len() == start {
            rows.push(ShortcutRow {
                action: meta.name.to_owned(),
                description: meta.description,
                keys: "—".to_owned(),
                context: String::new(),
                source: "",
            });
        }
    }
    rows
}

fn content_height(rows: usize) -> f32 {
    PAGE_PAD_TOP + HEADER_H + TOOLBAR_H + TOOLBAR_GAP + (rows + 2) as f32 * ROW_H
}

pub fn render(
    rect: UiRect,
    state: Binding,
    search_id: UiId,
    search_focus: UiFocusHandle,
) -> Element {
    let app = state.frame();
    let query = state.ui.get().search.text().to_owned();
    let rows = shortcut_rows(&app.keymap)
        .into_iter()
        .filter(|row| row.matches(&query))
        .collect::<Vec<_>>();
    let max_scroll = (content_height(rows.len()) - rect.height()).max(0.0);
    let scroll_y = app.scroll.1.clamp(0.0, max_scroll);

    let page_w = (rect.width() - PAGE_PAD_X * 2.0).clamp(0.0, MAX_PAGE_W);
    let page_left = rect.left + (rect.width() - page_w) / 2.0;
    let page_right = page_left + page_w;
    let mut top = rect.top + PAGE_PAD_TOP - scroll_y;

    let wheel_state = state.clone();
    let mut page = panel(rect, VisualStyle::filled(theme::c().bg))
        .event_policy(EventPolicy::INTERACTIVE)
        .on_wheel(move |cx, delta| {
            let step = match delta.unit {
                WheelUnit::Lines => delta.y * SCROLL_STEP * 3.0,
                WheelUnit::Pixels => delta.y,
            };
            wheel_state.run(Command::Scroll {
                delta: step,
                max: max_scroll,
            });
            cx.stop_propagation();
        });

    let mut content = group(rect);
    let title_w = measure("Keymap", 18.0, 700);
    content = content.child(text(
        UiRect::new(
            page_left,
            top,
            page_left + title_w + TEXT_MARGIN,
            top + 26.0,
        ),
        "Keymap",
        theme::mono_bold(theme::c().text_bright, 18.0),
    ));
    let location = app.location;
    content = content.child(text(
        UiRect::new(page_left, top + 28.0, page_right, top + 48.0),
        location,
        theme::sans(theme::c().text_dim, theme::UI_SIZE),
    ));
    top += HEADER_H;

    // Toolbar: filter on the left, Edit keymap.json on the right.
    let button_label = "Edit keymap.json";
    let button_w = measure(button_label, theme::UI_SIZE, 400) + BUTTON_PAD_X * 2.0;
    let button = UiRect::new(page_right - button_w, top, page_right, top + TOOLBAR_H);
    let search = UiRect::new(
        page_left,
        top,
        (button.left - 12.0).max(page_left),
        top + TOOLBAR_H,
    );
    content = content.child(
        theme::bordered(search, theme::c().surface, theme::c().border, 4.0, 1.0).child(
            search_input(
                UiRect::new(
                    search.left + 4.0,
                    search.top + 1.0,
                    search.right - 4.0,
                    search.bottom - 1.0,
                ),
                state.clone(),
                search_focus,
                search_id,
            ),
        ),
    );
    let mut button_text = theme::sans(theme::c().text, theme::UI_SIZE);
    button_text.align = TextAlign::Center;
    let open_state = state.clone();
    content = content.child(
        theme::bordered(button, theme::c().surface, theme::c().border, 4.0, 1.0)
            .key("keymap-open-file")
            .event_policy(EventPolicy::INTERACTIVE)
            .cursor(CursorIcon::Pointer)
            .on_click(move || {
                open_state.run(Command::OpenUserFile);
            })
            .child(text(button, button_label, button_text)),
    );
    top += TOOLBAR_H + TOOLBAR_GAP;

    content = content.child(table(
        UiRect::new(page_left, top, page_right, top),
        &rows,
        &query,
    ));

    page = page.child(content);
    clip(rect, 0.0, 0.0).child(page)
}

fn table(area: UiRect, rows: &[ShortcutRow], query: &str) -> Element {
    let width = area.width();
    let cell = |top: f32, column: usize| {
        let (start, span) = COLUMNS[column];
        UiRect::new(
            area.left + width * start,
            top,
            area.left + width * (start + span) - 8.0,
            top + ROW_H,
        )
    };
    let mut top = area.top;
    let mut table = group(UiRect::new(
        area.left,
        area.top,
        area.right,
        area.top + (rows.len() + 1) as f32 * ROW_H,
    ));
    let heading = theme::mono(theme::c().text_dim, theme::SMALL).tracking(0.8);
    for (column, title) in ["ACTION", "KEYSTROKE", "CONTEXT", "SOURCE"]
        .into_iter()
        .enumerate()
    {
        table = table.child(text(cell(top, column), title, heading));
    }
    table = table.child(panel(
        UiRect::new(area.left, top + ROW_H - 1.0, area.right, top + ROW_H),
        VisualStyle::filled(theme::c().border),
    ));
    top += ROW_H;

    if rows.is_empty() {
        table = table.child(text(
            UiRect::new(area.left, top, area.right, top + ROW_H),
            format!("No actions or bindings match “{query}”."),
            theme::sans(theme::c().text_dim, theme::UI_SIZE),
        ));
    }
    for row in rows {
        table = table
            .child(text(
                cell(top, 0),
                row.description,
                theme::sans(theme::c().text, theme::UI_SIZE),
            ))
            .child(text(
                cell(top, 1),
                row.keys.clone(),
                theme::mono_bold(theme::c().text_soft, theme::SMALL),
            ))
            .child(text(
                cell(top, 2),
                if row.context.is_empty() {
                    row.action.clone()
                } else {
                    format!("{} · {}", row.context, row.action)
                },
                theme::mono(theme::c().text_muted, theme::SMALL),
            ))
            .child(text(
                cell(top, 3),
                row.source,
                theme::mono(
                    if row.source == "User" {
                        theme::c().accent
                    } else {
                        theme::c().text_dim
                    },
                    theme::SMALL,
                ),
            ))
            .child(panel(
                UiRect::new(area.left, top + ROW_H - 1.0, area.right, top + ROW_H),
                VisualStyle::filled(theme::c().active_line),
            ));
        top += ROW_H;
    }
    table
}

fn search_input(rect: UiRect, state: Binding, focus: UiFocusHandle, id: UiId) -> Element {
    let cancel = state.clone();
    input::render_access(
        rect,
        id,
        focus,
        InputBinding::new_access(state.ui, |ui| &ui.search, |ui| &mut ui.search),
        AccessInputOptions {
            label: "Filter key bindings",
            placeholder: "Filter by action, keystroke or context",
            style: InputStyle {
                text: theme::mono(theme::c().text, theme::SMALL),
                placeholder: theme::mono(theme::c().text_faint, theme::SMALL),
                caret: theme::c().text_faint,
                selection: theme::c().accent,
            },
            on_submit: None,
            on_cancel: Some(Arc::new(move || cancel.run(Command::ClearSearch))),
            on_blur: None,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::keymap::bindings::BindingSource;
    use crate::features::keymap::keymap_file::{build, default_keymap, parse};

    #[test]
    fn rows_list_every_action_and_reflect_overrides() {
        let rows = shortcut_rows(&default_keymap());
        for meta in ACTIONS {
            assert!(
                rows.iter().any(|row| row.action.starts_with(meta.name)),
                "{} is missing",
                meta.name
            );
        }
        assert!(rows.iter().any(|row| row.action == "pane::ActivateItem 7"));
        let unbound = rows
            .iter()
            .find(|row| row.action == "workspace::CloneRepository")
            .unwrap();
        assert_eq!(unbound.keys, "—");

        let user = parse(
            r#"[{ "context": "Editor", "bindings": { "secondary-y": null, "alt-r": "editor::Redo" } }]"#,
            BindingSource::User,
        )
        .unwrap();
        let rows = shortcut_rows(&build(user.bindings));
        let redo = rows
            .iter()
            .filter(|row| row.action == "editor::Redo")
            .collect::<Vec<_>>();
        assert_eq!(redo.len(), 2, "{redo:?}");
        assert!(redo.iter().any(|row| row.source == "User"));
    }

    #[test]
    fn filter_matches_every_term_across_columns() {
        let rows = shortcut_rows(&default_keymap());
        let undo = rows
            .iter()
            .find(|row| row.action == "editor::Undo")
            .unwrap();
        assert!(undo.matches(""));
        assert!(undo.matches("UNDO"));
        assert!(undo.matches("editor undo"));
        assert!(undo.matches("default"));
        assert!(!undo.matches("undo terminal"));
        let terminal = rows.iter().filter(|row| row.matches("terminal::")).count();
        assert!(terminal >= 3);
    }
}
