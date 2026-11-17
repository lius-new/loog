use super::*;

#[test]
fn corner_radius_maps_to_the_closest_dwm_preference() {
    assert_eq!(
        corner_preference(0, WindowMode::Windowed),
        CornerPreference::DoNotRound
    );
    assert_eq!(
        corner_preference(4, WindowMode::Windowed),
        CornerPreference::RoundSmall
    );
    assert_eq!(
        corner_preference(8, WindowMode::Windowed),
        CornerPreference::Round
    );
}

#[test]
fn fullscreen_windows_disable_dwm_rounding() {
    assert_eq!(
        corner_preference(8, WindowMode::Fullscreen),
        CornerPreference::DoNotRound
    );
}

#[test]
fn resize_directions_map_to_the_matching_hit_test_codes() {
    assert_eq!(hit_test_for_resize(ResizeDirection::North), HTTOP);
    assert_eq!(hit_test_for_resize(ResizeDirection::South), HTBOTTOM);
    assert_eq!(hit_test_for_resize(ResizeDirection::East), HTRIGHT);
    assert_eq!(hit_test_for_resize(ResizeDirection::West), HTLEFT);
    assert_eq!(hit_test_for_resize(ResizeDirection::NorthEast), HTTOPRIGHT);
    assert_eq!(hit_test_for_resize(ResizeDirection::NorthWest), HTTOPLEFT);
    assert_eq!(
        hit_test_for_resize(ResizeDirection::SouthEast),
        HTBOTTOMRIGHT
    );
    assert_eq!(
        hit_test_for_resize(ResizeDirection::SouthWest),
        HTBOTTOMLEFT
    );
}

#[test]
fn outer_resize_frame_hit_tests_every_edge_and_corner() {
    let rect = Rect {
        left: 100,
        top: 200,
        right: 500,
        bottom: 600,
    };
    let border = 8;

    assert_eq!(
        resize_hit_test(rect, 100, 200, border, border),
        Some(HTTOPLEFT)
    );
    assert_eq!(
        resize_hit_test(rect, 499, 200, border, border),
        Some(HTTOPRIGHT)
    );
    assert_eq!(
        resize_hit_test(rect, 100, 599, border, border),
        Some(HTBOTTOMLEFT)
    );
    assert_eq!(
        resize_hit_test(rect, 499, 599, border, border),
        Some(HTBOTTOMRIGHT)
    );
    assert_eq!(
        resize_hit_test(rect, 100, 400, border, border),
        Some(HTLEFT)
    );
    assert_eq!(
        resize_hit_test(rect, 499, 400, border, border),
        Some(HTRIGHT)
    );
    assert_eq!(resize_hit_test(rect, 300, 200, border, border), Some(HTTOP));
    assert_eq!(
        resize_hit_test(rect, 300, 599, border, border),
        Some(HTBOTTOM)
    );
    assert_eq!(resize_hit_test(rect, 300, 400, border, border), None);
}

#[test]
fn resize_hit_test_excludes_points_outside_the_native_window_rect() {
    let rect = Rect {
        left: -500,
        top: 100,
        right: -100,
        bottom: 500,
    };

    assert_eq!(resize_hit_test(rect, -501, 300, 8, 8), None);
    assert_eq!(resize_hit_test(rect, -99, 300, 8, 8), None);
    assert_eq!(resize_hit_test(rect, -300, 99, 8, 8), None);
    assert_eq!(resize_hit_test(rect, -300, 500, 8, 8), None);
}
