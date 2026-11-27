//! Local view interaction and application command callbacks.
use super::state::{SearchPanelState, SearchUiState};
use crate::features::search::FileMatches;
use std::sync::Arc;
pub const FILE_ROW_H: f32 = 24.0;
pub const MATCH_ROW_H: f32 = 20.0;
#[derive(Clone)]
pub enum Command {
    Refresh,
    Cancel,
    Clear,
    ResetFilters,
    /// Fold or open the whole section.
    ToggleOpen,
    SplitStart,
    /// The pointer's vertical position while the divider is dragged.
    Split(f32),
    SplitEnd,
    Select {
        file: Arc<FileMatches>,
        index: usize,
        point: (f32, f32),
    },
}
#[derive(Clone)]
pub struct Commands(Arc<dyn Fn(Command) + Send + Sync>);
impl Commands {
    pub fn new(dispatch: impl Fn(Command) + Send + Sync + 'static) -> Self {
        Self(Arc::new(dispatch))
    }
    pub fn dispatch(&self, command: Command) {
        (self.0)(command);
    }
}
pub fn toggle_filters(search: &mut SearchUiState) {
    let closing_focused = search.filters_open && (search.include.focused || search.exclude.focused);
    search.filters_open = !search.filters_open;
    if !search.filters_open {
        for input in [&mut search.include, &mut search.exclude] {
            input.focused = false;
            input.preedit.clear();
            input.preedit_cursor = None;
        }
        if closing_focused {
            search.focus_request += 1;
        }
    }
}

pub fn selected(app: &SearchPanelState) -> Option<(Arc<FileMatches>, usize)> {
    let (path, index) = app.ui.selected.as_ref()?;
    app.model
        .status()
        .results
        .files
        .iter()
        .find(|file| &file.path == path)
        .filter(|file| *index < file.matches.len())
        .map(|file| (file.clone(), *index))
}

pub fn move_selection(app: &mut SearchPanelState, delta: isize) {
    let mut total = 0;
    let mut current = None;
    for file in &app.model.status().results.files {
        if app.ui.collapsed.contains(&file.path) {
            continue;
        }
        if let Some((path, index)) = &app.ui.selected
            && path == &file.path
            && *index < file.matches.len()
        {
            current = Some(total + index);
        }
        total += file.matches.len();
    }
    if total == 0 {
        return;
    }
    let mut next = current.map_or_else(
        || if delta < 0 { total - 1 } else { 0 },
        |index| index.saturating_add_signed(delta).min(total - 1),
    );
    let mut top = 0.0;
    for file in &app.model.status().results.files {
        top += FILE_ROW_H;
        if !app.ui.collapsed.contains(&file.path) {
            if next < file.matches.len() {
                app.ui.selected = Some((file.path.clone(), next));
                top += next as f32 * MATCH_ROW_H;
                break;
            }
            top += file.matches.len() as f32 * MATCH_ROW_H;
            next -= file.matches.len();
        }
    }
    // Initial query focus has not measured the result viewport yet. The result
    // focus callback reveals the selection using its actual height.
    if app.ui.viewport_height > 0.0 {
        reveal_at(app, top);
    }
    app.ui.results_focus_request += 1;
}

fn reveal_at(app: &mut SearchPanelState, top: f32) {
    let height = app.ui.viewport_height.max(MATCH_ROW_H);
    if top < app.ui.scroll_y {
        app.ui.scroll_y = top;
    } else if top + MATCH_ROW_H > app.ui.scroll_y + height {
        app.ui.scroll_y = (top + MATCH_ROW_H - height).max(0.0);
    }
}

pub fn reveal_selection(app: &mut SearchPanelState) {
    let Some((selected_path, index)) = app.ui.selected.as_ref() else {
        return;
    };
    let mut top = 0.0;
    for file in &app.model.status().results.files {
        top += FILE_ROW_H;
        if app.ui.collapsed.contains(&file.path) {
            continue;
        }
        if &file.path == selected_path && *index < file.matches.len() {
            top += *index as f32 * MATCH_ROW_H;
            reveal_at(app, top);
            return;
        }
        top += file.matches.len() as f32 * MATCH_ROW_H;
    }
}
