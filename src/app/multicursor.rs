//! View-local selection commands share the editor's displayed-row map.
use super::state::AppState;
use lgui::prelude::UiRect;
use loog_editor_core::text::{DisplayMap, Selection, TextBuffer};
pub fn prepare(app: &mut AppState) {
    super::editor_vim::start_multi(app);
    super::language_ui::snippet_session::end(app);
    super::language_ui::dismiss(app);
    app.editor.language.completion = None;
    app.editor.language.completion_timer = None;
    app.editor.drag = None;
}
pub fn primary_only(app: &mut AppState) {
    if let Some(mut editor) = app.workspace.active_editor_mut()
        && editor.has_secondary()
    {
        let primary = editor.selection_state();
        editor.set_selection(primary);
    }
}
pub fn command(app: &mut AppState, next: bool, down: bool, rect: UiRect) {
    prepare(app);
    let map = super::editing::active_projection(app, rect);
    let Some(mut editor) = app.workspace.active_editor_mut() else {
        return;
    };
    let mut states = editor.selections();
    if states.len() >= 256 {
        return;
    }
    if next {
        let primary = states[0];
        let Some(range) = primary.range() else {
            let r = editor.word_range(primary.cursor);
            editor.set_selection(Selection::spanning(r.start, r.end));
            return;
        };
        let needle = editor.text()[range.clone()].to_owned();
        if needle.is_empty() {
            return;
        }
        let whole_word = editor.word_range(range.start) == range;
        let start = states
            .iter()
            .filter_map(|s| s.range())
            .map(|r| r.end)
            .max()
            .unwrap();
        let candidates = editor.text()[start..]
            .match_indices(&needle)
            .map(|(at, _)| at + start)
            .chain(
                editor.text()[..start]
                    .match_indices(&needle)
                    .map(|(at, _)| at),
            );
        let found = candidates
            .filter(|&at| {
                editor.is_grapheme_boundary(at) && editor.is_grapheme_boundary(at + needle.len())
            })
            .find(|&at| {
                (!whole_word || editor.word_range(at) == (at..at + needle.len()))
                    && states.iter().all(|s| {
                        s.range()
                            .is_none_or(|r| r.end <= at || r.start >= at + needle.len())
                    })
            });
        if let Some(at) = found {
            let selection = Selection::spanning(at, at + needle.len());
            editor.set_selections(selection, states);
        }
    } else {
        let s = states[0];
        let row = map.caret_row(&editor, s.cursor, s.wrap_affinity);
        let target = if down {
            (row + 1).min(map.len() - 1)
        } else {
            row.saturating_sub(1)
        };
        if target == row {
            return;
        }
        let col = editor
            .display_column(s.cursor)
            .saturating_sub(map.row(&editor, row).column);
        let at = map.offset_at(&editor, target, col);
        if states.iter().any(|s| s.cursor == at) {
            return;
        }
        let mut added = Selection::caret(at);
        added.wrap_affinity = map.affinity_at(&editor, target, at);
        if let Some(anchor) = s.anchor
            && map.caret_row(&editor, anchor, s.wrap_affinity) == row
        {
            let col = editor
                .display_column(anchor)
                .saturating_sub(map.row(&editor, row).column);
            added.anchor = Some(map.offset_at(&editor, target, col));
        }
        states[0].preferred_column = None;
        editor.set_selections(added, states);
    }
    super::editor::reveal_cursor(app, rect);
}
#[cfg(test)]
pub fn rectangle(
    buffer: &TextBuffer,
    map: &DisplayMap,
    origin: usize,
    head: usize,
) -> (Selection, Vec<Selection>) {
    let a = map.row_of(buffer, origin);
    let b = map.row_of(buffer, head);
    let ca = buffer
        .display_column(origin)
        .saturating_sub(map.row(buffer, a).column);
    let cb = buffer
        .display_column(head)
        .saturating_sub(map.row(buffer, b).column);
    rectangle_at(buffer, map, (a, ca), (b, cb))
}
pub fn rectangle_at(
    buffer: &TextBuffer,
    map: &DisplayMap,
    (a, ca): (usize, usize),
    (b, cb): (usize, usize),
) -> (Selection, Vec<Selection>) {
    let make = |row| {
        let mut s = Selection::spanning(
            map.offset_at(buffer, row, ca),
            map.offset_at(buffer, row, cb),
        );
        s.wrap_affinity = map.affinity_at(buffer, row, s.cursor);
        s
    };
    let primary = make(b);
    let mut others = Vec::new();
    for row in a.min(b)..=a.max(b) {
        if row != b {
            others.push(make(row));
            if others.len() == 255 {
                break;
            }
        }
    }
    (primary, others)
}
