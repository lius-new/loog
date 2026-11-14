//! Moving between the conflicts Git reports in the open file.

use lgui::prelude::{State, UiRect};

use super::language_ui::target;
use crate::app::context::AppContext;
use crate::domain::identity::PaneId;
use crate::features::git::GitStoreSnapshot;
use crate::features::git::conflict::{ConflictRegion, Resolution, resolution_edit};
use loog_editor_core::text::{Edit, EditKind, Selection};

/// The conflict that starts after (or, going backwards, before) one-based
/// `line`, wrapping around.
pub(crate) fn next_region(
    regions: &[ConflictRegion],
    line: usize,
    forward: bool,
) -> Option<&ConflictRegion> {
    if forward {
        regions
            .iter()
            .find(|region| region.start > line)
            .or_else(|| regions.first())
    } else {
        regions
            .iter()
            .rev()
            .find(|region| region.start < line)
            .or_else(|| regions.last())
    }
}

/// Moves the cursor to the next or previous conflict of the open file.
pub(crate) fn go_to(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    forward: bool,
    rect: UiRect,
) -> bool {
    let app = state.get();
    let Some(target) = target(&app, app.workspace.active_pane()) else {
        return false;
    };
    let Some(path) = crate::features::git::location::git_path(&target.resource) else {
        return false;
    };
    let regions = store.get().conflicts.get(&path).cloned();
    let Some(regions) = regions.filter(|regions| !regions.is_empty()) else {
        state.update(|app| app.show_toast("No conflicts in this file"));
        return true;
    };
    let Some(document) = app.workspace.documents.get(&target.document) else {
        return false;
    };
    let line = document.buffer.line_of(target.cursor) + 1;
    let Some(region) = next_region(&regions, line, forward) else {
        return false;
    };
    let offset = document.buffer.line_start(region.start - 1);
    state.update(|app| {
        if let Some(mut editor) = app.workspace.active_editor_mut() {
            editor.set_cursor(offset);
        }
        crate::app::editor::reveal_cursor(app, rect);
    });
    true
}

/// Keeps one side of `region` in the pane's file as a single undo step. The
/// region is where Git reported it, moved for unsaved edits; if the text no
/// longer has its markers there, nothing changes.
pub(crate) fn resolve(
    state: &AppContext,
    pane: PaneId,
    rect: UiRect,
    region: &ConflictRegion,
    resolution: Resolution,
) {
    state.update(|app| {
        let Some(mut editor) = app.workspace.editor_mut(pane) else {
            return;
        };
        let text = editor.buffer().text_snapshot();
        let Some((range, replacement)) = resolution_edit(&text, region, resolution) else {
            app.show_toast("The file changed since Git looked. Save it and try again");
            return;
        };
        let start = range.start;
        let end = start + replacement.len();
        let applied = editor.edit(
            EditKind::Other,
            vec![Edit::replace(range, replacement)],
            |_, _| Selection::caret(end),
        );
        drop(editor);
        if applied.is_ok() {
            crate::app::editor::reveal_cursor(app, rect);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(start: usize) -> ConflictRegion {
        ConflictRegion {
            start,
            current: start + 1..start + 2,
            separator: start + 2,
            incoming: start + 3..start + 4,
            end: start + 4,
        }
    }

    const FILE: &str = "keep\n<<<<<<< HEAD\nX\n=======\nY\n>>>>>>> topic\nafter\n";

    fn open_file(text: &str) -> AppContext {
        let state = crate::app::test_support::live(crate::app::state::AppState::new());
        state.update(|app| {
            app.workspace.open_path("f.txt".into(), text.into());
        });
        state
    }

    fn text(state: &AppContext) -> String {
        let app = state.get();
        app.workspace
            .editor(app.workspace.active_pane())
            .unwrap()
            .buffer()
            .text_snapshot()
            .to_string()
    }

    fn conflict() -> ConflictRegion {
        ConflictRegion {
            start: 2,
            current: 3..4,
            separator: 4,
            incoming: 5..6,
            end: 6,
        }
    }

    fn resolve_in(state: &AppContext, region: &ConflictRegion, resolution: Resolution) {
        let pane = state.get().workspace.active_pane();
        resolve(
            state,
            pane,
            UiRect::new(0.0, 0.0, 400.0, 300.0),
            region,
            resolution,
        );
    }

    #[test]
    fn accepting_a_side_is_one_undo_step() {
        let state = open_file(FILE);
        resolve_in(&state, &conflict(), Resolution::Incoming);
        assert_eq!(text(&state), "keep\nY\nafter\n");
        state.update(|app| {
            app.workspace.active_editor_mut().unwrap().undo();
        });
        assert_eq!(text(&state), FILE);
    }

    #[test]
    fn a_conflict_that_moved_changes_nothing() {
        let state = open_file(FILE);
        let moved = ConflictRegion {
            start: 3,
            ..conflict()
        };
        resolve_in(&state, &moved, Resolution::Current);
        assert_eq!(text(&state), FILE);
    }

    #[test]
    fn the_next_conflict_is_after_the_cursor_and_wraps() {
        let regions = [region(3), region(20)];
        assert_eq!(next_region(&regions, 1, true).map(|r| r.start), Some(3));
        assert_eq!(next_region(&regions, 3, true).map(|r| r.start), Some(20));
        assert_eq!(next_region(&regions, 20, true).map(|r| r.start), Some(3));
    }

    #[test]
    fn the_previous_conflict_is_before_the_cursor_and_wraps() {
        let regions = [region(3), region(20)];
        assert_eq!(next_region(&regions, 25, false).map(|r| r.start), Some(20));
        assert_eq!(next_region(&regions, 20, false).map(|r| r.start), Some(3));
        assert_eq!(next_region(&regions, 3, false).map(|r| r.start), Some(20));
    }
}
