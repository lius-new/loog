use crate::features::git::diff_selection::DiffSide;
use crate::features::git::diff_selection_tests::overview;
use crate::ui::diff_editor::{code_left, row_h};
use crate::ui::diff_selection::{Geometry, point_at};
use crate::ui::editor::view::display_columns;
use crate::ui::{fonts, theme};
use lgui::prelude::UiRect;

#[test]
fn pointer_positions_resolve_to_files_rows_and_characters() {
    let document = overview();
    let body = UiRect::new(0.0, 34.0, 800.0, 600.0);
    let geometry = Geometry {
        body,
        split: false,
        ratio: 0.5,
        scroll_x: 0.0,
        scroll_y: 0.0,
    };
    let layout = document.changes_layout(false, &Default::default());
    let text_left = code_left(body, false) + theme::CODE_PAD;
    let row_y = |row: usize| body.top + (row as f32 + 0.5) * row_h();
    let first_body = layout.sections[0].body_top;
    let (side, point, inside) = point_at(
        &document,
        Some(&layout),
        geometry,
        None,
        (
            text_left + 3.2 * fonts::editor().char_w,
            row_y(first_body + 1),
        ),
    )
    .unwrap();
    assert_eq!((side, inside), (DiffSide::New, true));
    assert_eq!((point.section, point.row, point.column), (0, 1, 3));
    // A header row pulls in to the start of its file's body.
    let (_, header, inside) = point_at(
        &document,
        Some(&layout),
        geometry,
        None,
        (text_left, row_y(0)),
    )
    .unwrap();
    assert!(!inside && (header.section, header.row, header.column) == (0, 0, 0));
    // Below everything pulls in to the end of the last row.
    let (_, tail, inside) =
        point_at(&document, Some(&layout), geometry, None, (text_left, 590.0)).unwrap();
    assert!(!inside && (tail.section, tail.row, tail.column) == (1, 1, 6));
}

#[test]
fn tabs_and_wide_characters_advance_the_highlight() {
    assert_eq!(display_columns("\tab", 1), 4);
    assert_eq!(display_columns("a\tb", 2), 4);
    assert_eq!(display_columns("你a", 1), 2);
}

#[test]
fn split_pointer_maps_each_compact_side_back_to_its_selection_row() {
    use crate::features::git::{DiffTarget, diff::parse_unified, diff_document::DiffDocument};
    use crate::ui::diff_split::Columns;
    let document = DiffDocument::from_unified("repo".into(), "a.txt".into(), DiffTarget::IndexToWorktree,
        parse_unified("diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,5 @@\n start\n-old\n+new\n+extra\n+more\n end\n").unwrap());
    let body = UiRect::new(0.0, 0.0, 800.0, row_h());
    let geometry = Geometry {
        body,
        split: true,
        ratio: 0.5,
        scroll_x: 0.0,
        scroll_y: 3.0 * row_h(),
    };
    let columns = Columns::new(body, 0.5);
    for (side, expected_row) in [(DiffSide::Old, 4), (DiffSide::New, 3)] {
        let x = columns.code(side).left + theme::CODE_PAD + fonts::editor().char_w * 1.2;
        let (actual_side, point, inside) =
            point_at(&document, None, geometry, None, (x, row_h() * 0.5)).unwrap();
        assert_eq!(actual_side, side);
        assert!(inside);
        assert_eq!((point.row, point.column), (expected_row, 1));
    }
    let (_, _, inside) = point_at(&document, None, geometry, None, (400.0, row_h() * 0.5)).unwrap();
    assert!(
        !inside,
        "central line numbers and controls must not start a text selection"
    );
}
