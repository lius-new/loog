//! The line-number gutter with clickable breakpoint dots.

use std::collections::BTreeMap;
use std::ops::{Range, RangeInclusive};

use lgui::core::ellipse;
use lgui::prelude::{Color, Element, UiRect, VisualStyle, group, text};

use crate::ui::fonts;
use crate::ui::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GutterDecoration {
    Breakpoint,
    GitAdded,
    GitModified,
    GitDeleted,
    ConflictCurrent,
    ConflictIncoming,
    ConflictMarker,
    Diagnostic,
}

/// A Git change bar's width, and its width while the pointer is on it.
const CHANGE_BAR_W: f32 = 3.0;
const HOVERED_CHANGE_BAR_W: f32 = 6.0;

fn conflict_color(
    decorations: &BTreeMap<usize, Vec<GutterDecoration>>,
    line: usize,
) -> Option<Color> {
    decorations
        .get(&line)?
        .iter()
        .find_map(|value| match value {
            GutterDecoration::ConflictCurrent => Some(theme::c().diff.add_mark),
            GutterDecoration::ConflictIncoming => Some(theme::c().diff.mod_mark),
            GutterDecoration::ConflictMarker => Some(theme::c().warning),
            _ => None,
        })
}

fn git_change(
    decorations: &BTreeMap<usize, Vec<GutterDecoration>>,
    line: usize,
) -> Option<GutterDecoration> {
    decorations.get(&line)?.iter().copied().find(|value| {
        matches!(
            value,
            GutterDecoration::GitAdded
                | GutterDecoration::GitModified
                | GutterDecoration::GitDeleted
        )
    })
}

/// The 1-based lines of the change bar under `hovered`: it and its
/// neighbors with the same change.
pub(crate) fn hovered_run(
    decorations: &BTreeMap<usize, Vec<GutterDecoration>>,
    hovered: Option<usize>,
) -> Option<RangeInclusive<usize>> {
    let hovered = hovered?;
    let change = git_change(decorations, hovered)?;
    let mut first = hovered;
    while first > 1 && git_change(decorations, first - 1) == Some(change) {
        first -= 1;
    }
    let mut last = hovered;
    while git_change(decorations, last + 1) == Some(change) {
        last += 1;
    }
    Some(first..=last)
}

/// Render only `visible_lines` while preserving their absolute document rows.
/// `hovered` is the 1-based line under the pointer, whose change bar widens.
pub fn render(
    rect: UiRect,
    visible_lines: Range<usize>,
    decorations: &BTreeMap<usize, Vec<GutterDecoration>>,
    hovered: Option<usize>,
) -> Element {
    render_rows(
        rect,
        visible_lines.map(|line| (line, line)),
        decorations,
        hovered,
    )
}
pub fn render_rows(
    rect: UiRect,
    rows: impl IntoIterator<Item = (usize, usize)>,
    decorations: &BTreeMap<usize, Vec<GutterDecoration>>,
    hovered: Option<usize>,
) -> Element {
    let mut g = group(rect);
    let hovered_run = hovered_run(decorations, hovered);

    for (i, row) in rows {
        let y = rect.top + row as f32 * fonts::editor().line_h;
        let line_decorations = decorations.get(&(i + 1));
        let has_bp =
            line_decorations.is_some_and(|values| values.contains(&GutterDecoration::Breakpoint));

        let color: Color = if has_bp {
            theme::c().error
        } else {
            theme::c().text_faint
        };
        let num_rect = UiRect::new(
            rect.left + theme::GUTTER_ACTION_W,
            y,
            rect.right - theme::GUTTER_NUMBER_PAD,
            y + fonts::editor().line_h,
        );
        g = g.child(text(
            num_rect,
            (i + 1).to_string(),
            fonts::editor().right(color),
        ));

        if has_bp {
            let dot = UiRect::new(
                num_rect.right - 1.0,
                y + 8.0,
                num_rect.right + 5.0,
                y + 14.0,
            );
            g = g.child(ellipse(dot, VisualStyle::filled(theme::c().error)));
        }
        // A conflict's bar replaces the Git change bar: Git sees its lines as
        // plain additions.
        if let Some(color) = conflict_color(decorations, i + 1) {
            g = g.child(lgui::prelude::panel(
                UiRect::new(
                    rect.right - CHANGE_BAR_W,
                    y,
                    rect.right,
                    y + fonts::editor().line_h,
                ),
                VisualStyle::filled(color),
            ));
        } else if let Some(change) = git_change(decorations, i + 1) {
            let color = match change {
                GutterDecoration::GitAdded => theme::c().diff.add_mark,
                GutterDecoration::GitModified => theme::c().diff.mod_mark,
                _ => theme::c().diff.del_mark,
            };
            let width = if hovered_run
                .as_ref()
                .is_some_and(|run| run.contains(&(i + 1)))
            {
                HOVERED_CHANGE_BAR_W
            } else {
                CHANGE_BAR_W
            };
            // Neighbouring lines of the same change join into one continuous
            // bar; only the ends of a run are inset.
            let joins_above = i > 0 && git_change(decorations, i) == Some(change);
            let joins_below = git_change(decorations, i + 2) == Some(change);
            let line_h = fonts::editor().line_h;
            g = g.child(lgui::prelude::panel(
                UiRect::new(
                    rect.right - width,
                    if joins_above { y } else { y + 2.0 },
                    rect.right,
                    if joins_below {
                        y + line_h
                    } else {
                        y + line_h - 2.0
                    },
                ),
                VisualStyle::filled(color),
            ));
        }
    }

    g
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hovering_a_change_bar_selects_its_run_of_the_same_change() {
        let decorations = BTreeMap::from([
            (2, vec![GutterDecoration::GitAdded]),
            (
                3,
                vec![GutterDecoration::Breakpoint, GutterDecoration::GitAdded],
            ),
            (4, vec![GutterDecoration::GitAdded]),
            (5, vec![GutterDecoration::GitModified]),
            (7, vec![GutterDecoration::Breakpoint]),
        ]);
        assert_eq!(hovered_run(&decorations, Some(3)), Some(2..=4));
        assert_eq!(hovered_run(&decorations, Some(5)), Some(5..=5));
        assert_eq!(hovered_run(&decorations, Some(7)), None);
        assert_eq!(hovered_run(&decorations, Some(1)), None);
        assert_eq!(hovered_run(&decorations, None), None);
    }
}
