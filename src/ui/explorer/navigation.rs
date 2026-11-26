//! First-letter navigation changes tree selection without opening documents.
use super::commands::Commands;
use super::source::{self, Entry};
use super::state::{ExplorerPanelState, ExplorerReveal};
use crate::ui::components::state_access::StateAccess;
use lgui::core::{KeyState, LogicalKey, NamedKey, UiEventKind, UiEventPayload, UiFocusHandle};
use lgui::prelude::{Element, UiRect};
use std::collections::HashSet;

struct Row {
    entry: Entry,
    covered: usize,
}

fn rows(panel: &ExplorerPanelState) -> Vec<Row> {
    fn visit(
        entry: Entry,
        depth: usize,
        preceding: usize,
        panel: &ExplorerPanelState,
        result: &mut Vec<Row>,
        visited: &mut HashSet<String>,
    ) {
        if !visited.insert(entry.key.clone()) {
            return;
        }
        let children = if entry.directory && entry.expanded(panel) {
            entry.children(panel, depth)
        } else {
            vec![]
        };
        result.push(Row {
            entry,
            covered: preceding + depth,
        });
        for child in children {
            visit(child, depth + 1, preceding, panel, result, visited);
        }
    }
    let mut result = vec![];
    let mut visited = HashSet::new();
    for (preceding, root) in source::roots(panel).into_iter().enumerate() {
        visit(root, 0, preceding, panel, &mut result, &mut visited);
    }
    result
}

fn enabled(panel: &ExplorerPanelState) -> bool {
    panel.ui.menu.is_none()
        && panel.ui.resource_menu.is_none()
        && panel.model.create_request().is_none()
        && panel.model.rename_request().is_none()
        && panel.resources.edit.is_none()
        && !panel.ui.create_input.focused
}

fn jump(
    panel: &mut ExplorerPanelState,
    initial: &str,
    active: Option<&loog_workspace_api::ResourceRef>,
) -> bool {
    let rows = rows(panel);
    if rows.is_empty() {
        return false;
    }
    let current = panel
        .ui
        .keyboard_selection
        .as_deref()
        .or_else(|| {
            panel
                .ui
                .selected_directory
                .as_ref()
                .and_then(|selected| selected.path.to_str())
        })
        .or_else(|| {
            panel
                .ui
                .selection_anchor
                .as_ref()
                .and_then(|path| path.to_str())
        });
    let current = rows
        .iter()
        .position(|row| Some(row.entry.key.as_str()) == current)
        .or_else(|| {
            rows.iter().position(|row| {
                row.entry
                    .resource
                    .as_ref()
                    .is_some_and(|resource| Some(resource) == active)
            })
        });
    let start = current.map_or(0, |index| (index + 1) % rows.len());
    let initial = initial.to_lowercase();
    let found = (0..rows.len())
        .map(|offset| (start + offset) % rows.len())
        .find(|&index| {
            rows[index].entry.resource.is_some()
                && rows[index].entry.name.to_lowercase().starts_with(&initial)
        });
    let Some(index) = found else {
        return false;
    };
    panel.ui.keyboard_selection = Some(rows[index].entry.key.clone());
    panel.ui.reveal = Some(ExplorerReveal {
        row: index,
        covered: rows[index].covered,
    });
    true
}

pub(super) fn attach(
    element: Element,
    content: UiRect,
    state: StateAccess<ExplorerPanelState>,
    commands: Commands,
    focus: UiFocusHandle,
    active: Option<loog_workspace_api::ResourceRef>,
) -> Element {
    let click = state.clone();
    let key = state.clone();
    let key_focus = focus.clone();
    element
        .on_event_capture(UiEventKind::PointerDown, move |_, payload| {
            if let UiEventPayload::PointerDown { pointer, .. } = payload
                && pointer.point.y >= content.top
                && pointer.point.y < content.bottom
                && enabled(&click.get())
            {
                click.update(|panel| panel.ui.keyboard_selection = None);
                focus.focus();
            }
        })
        .on_key_down(move |cx, event| {
            if event.state != KeyState::Down
                || event.is_composing
                || event.modifiers.ctrl()
                || event.modifiers.alt()
                || event.modifiers.meta()
                || !enabled(&key.get())
            {
                return;
            }
            if event.key == LogicalKey::Named(NamedKey::Enter) {
                let panel = key.get();
                let command = rows(&panel)
                    .into_iter()
                    .find(|row| Some(&row.entry.key) == panel.ui.keyboard_selection.as_ref())
                    .and_then(|row| row.entry.activate());
                if let Some(command) = command {
                    commands.dispatch(command);
                    cx.prevent_default();
                    cx.stop_propagation();
                }
            } else if let LogicalKey::Character(value) = &event.key
                && value.chars().count() == 1
                && value
                    .chars()
                    .all(|ch| !ch.is_control() && !ch.is_whitespace())
            {
                if key.try_update(|panel| jump(panel, value, active.as_ref())) {
                    // Virtualized rows can unmount when the jump scrolls them
                    // away. Keep subsequent letters on the persistent tree.
                    key_focus.focus();
                }
                cx.prevent_default();
                cx.stop_propagation();
            }
        })
        .on_blur(move |_| {
            state.try_update(|panel| panel.ui.keyboard_selection.take().is_some());
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::explorer::Listing;
    use std::{path::PathBuf, sync::Arc};

    #[test]
    fn initials_cycle_case_insensitively_and_skip_collapsed_children() {
        let mut panel = ExplorerPanelState::default();
        let root = PathBuf::from("project");
        let folder = root.join("folder");
        panel.model.restore(vec![root.clone()], vec![]);
        panel.model.insert_listing(
            "project".into(),
            Arc::new(Listing::ordered(
                Arc::from("project"),
                [
                    ("folder", true),
                    ("Alpha.rs", false),
                    ("apple.rs", false),
                    ("Éclair.rs", false),
                ],
            )),
        );
        panel.model.insert_listing(
            folder.to_string_lossy().into_owned(),
            Arc::new(Listing::ordered(
                Arc::from(folder.to_string_lossy().as_ref()),
                [("another.rs", false)],
            )),
        );
        panel.model.set_expanded("project", true);
        for name in ["Alpha.rs", "apple.rs", "Alpha.rs"] {
            assert!(jump(&mut panel, "a", None));
            assert_eq!(
                panel.ui.keyboard_selection.as_deref(),
                root.join(name).to_str()
            );
        }
        assert!(jump(&mut panel, "A", None));
        assert_eq!(
            panel.ui.keyboard_selection.as_deref(),
            root.join("apple.rs").to_str()
        );
        assert!(jump(&mut panel, "é", None));
        assert_eq!(
            panel.ui.keyboard_selection.as_deref(),
            root.join("Éclair.rs").to_str()
        );
        let selected = panel.ui.keyboard_selection.clone();
        assert!(!jump(&mut panel, "q", None));
        assert_eq!(panel.ui.keyboard_selection, selected);
        panel.model.set_expanded(&folder.to_string_lossy(), true);
        assert!(jump(&mut panel, "a", None));
        assert_eq!(
            panel.ui.keyboard_selection.as_deref(),
            folder.join("another.rs").to_str()
        );
        assert_eq!(panel.ui.reveal, Some(ExplorerReveal { row: 2, covered: 2 }));
    }
}
