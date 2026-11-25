//! Pure horizontal layout for the editor tab strip.

use std::ops::Range;

#[derive(Clone, Copy, Debug)]
pub struct TabLayoutInput<'a> {
    pub natural_widths: &'a [f32],
    pub viewport_width: f32,
    pub scroll_x: f32,
    pub active_index: Option<usize>,
    pub reveal_active: bool,
    pub min_tab_width: f32,
    pub max_tab_width: f32,
    pub gap: f32,
    pub padding: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TabLayoutItem {
    pub index: usize,
    pub left: f32,
    pub right: f32,
    pub width: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TabLayoutResult {
    pub items: Vec<TabLayoutItem>,
    pub viewport_width: f32,
    pub max_scroll_x: f32,
    pub scroll_x: f32,
}

pub fn calculate(input: TabLayoutInput<'_>) -> TabLayoutResult {
    let viewport_width = finite_non_negative(input.viewport_width);
    let min_tab_width = finite_non_negative(input.min_tab_width);
    let max_tab_width = finite_non_negative(input.max_tab_width).max(min_tab_width);
    let gap = finite_non_negative(input.gap);
    let padding = finite_non_negative(input.padding);

    let mut left = padding;
    let mut items = Vec::with_capacity(input.natural_widths.len());
    for (index, natural_width) in input.natural_widths.iter().copied().enumerate() {
        let natural_width = finite_non_negative(natural_width);
        let width = if input.active_index == Some(index) {
            natural_width.max(min_tab_width)
        } else {
            natural_width.clamp(min_tab_width, max_tab_width)
        };
        let right = left + width;
        items.push(TabLayoutItem {
            index,
            left,
            right,
            width,
        });
        left = right + gap;
    }

    let content_width = items.last().map_or(0.0, |item| item.right + padding);
    let max_scroll_x = (content_width - viewport_width).max(0.0);
    let mut scroll_x = finite_non_negative(input.scroll_x).clamp(0.0, max_scroll_x);

    if input.reveal_active
        && let Some(item) = input.active_index.and_then(|index| items.get(index))
    {
        if item.width + padding * 2.0 > viewport_width || item.left < scroll_x + padding {
            scroll_x = (item.left - padding).max(0.0);
        } else if item.right > scroll_x + viewport_width - padding {
            scroll_x = item.right + padding - viewport_width;
        }
        scroll_x = scroll_x.clamp(0.0, max_scroll_x);
    }

    TabLayoutResult {
        items,
        viewport_width,
        max_scroll_x,
        scroll_x,
    }
}

/// Final index of the dragged tab `source` for a pointer in content
/// coordinates: how many other tabs have their center left of the pointer.
/// Crossing another tab's center is what changes the reorder target.
pub fn reorder_slot(items: &[TabLayoutItem], source: usize, pointer_x: f32) -> Option<usize> {
    if source >= items.len() {
        return None;
    }
    let pointer_x = if pointer_x.is_finite() {
        pointer_x
    } else {
        0.0
    };
    let crossed = items.partition_point(|item| (item.left + item.right) / 2.0 < pointer_x);
    Some(crossed - usize::from(source < crossed))
}

/// Indices of the tabs that intersect the viewport, widened by `overscan` on
/// both sides. Items are ordered by position, so the bounds are found with
/// binary searches and the cost stays independent of the open tab count.
pub fn visible_range(
    items: &[TabLayoutItem],
    scroll_x: f32,
    viewport_width: f32,
    overscan: f32,
) -> Range<usize> {
    let overscan = finite_non_negative(overscan);
    let left = finite_non_negative(scroll_x) - overscan;
    let right = finite_non_negative(scroll_x) + finite_non_negative(viewport_width) + overscan;
    let start = items.partition_point(|item| item.right < left);
    let end = items.partition_point(|item| item.left <= right);
    start..end.max(start)
}

fn finite_non_negative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: f32 = 120.0;
    const MAX: f32 = 220.0;
    const GAP: f32 = 4.0;
    const PAD: f32 = 8.0;

    fn layout(
        widths: &[f32],
        viewport_width: f32,
        scroll_x: f32,
        active_index: Option<usize>,
        reveal_active: bool,
    ) -> TabLayoutResult {
        calculate(TabLayoutInput {
            natural_widths: widths,
            viewport_width,
            scroll_x,
            active_index,
            reveal_active,
            min_tab_width: MIN,
            max_tab_width: MAX,
            gap: GAP,
            padding: PAD,
        })
    }

    #[test]
    fn empty_strip_has_no_content_or_scroll() {
        let result = layout(&[], 500.0, 80.0, None, false);

        assert!(result.items.is_empty());
        assert_eq!(result.max_scroll_x, 0.0);
        assert_eq!(result.scroll_x, 0.0);
    }

    #[test]
    fn one_and_two_tabs_use_bounded_natural_widths() {
        let one = layout(&[80.0], 500.0, 0.0, Some(0), false);
        assert_eq!(one.items[0].width, MIN);
        assert_eq!(one.items[0].right, PAD + MIN);

        let two = layout(&[180.0, 400.0], 500.0, 0.0, Some(0), false);
        assert_eq!(two.items[0].width, 180.0);
        assert_eq!(two.items[1].width, MAX);
        assert_eq!(two.items[1].left, PAD + 180.0 + GAP);
    }

    #[test]
    fn twenty_tabs_overflow_without_overlapping() {
        let widths = vec![160.0; 20];
        let result = layout(&widths, 600.0, 0.0, Some(0), false);

        assert!(result.max_scroll_x > 0.0);
        assert!(
            result
                .items
                .windows(2)
                .all(|pair| pair[0].right + GAP == pair[1].left)
        );
    }

    #[test]
    fn one_hundred_tabs_remain_finite_and_scrollable() {
        let widths = vec![f32::INFINITY; 100];
        let result = layout(&widths, 320.0, f32::INFINITY, Some(99), false);

        assert_eq!(result.items.len(), 100);
        assert!(result.max_scroll_x.is_finite());
        assert_eq!(result.scroll_x, 0.0);
    }

    #[test]
    fn revealing_last_tab_scrolls_it_into_a_narrow_viewport() {
        let widths = vec![160.0; 20];
        let result = layout(&widths, 240.0, 0.0, Some(19), true);
        let active = result.items[19];

        assert!(result.scroll_x > 0.0);
        assert!(active.left >= result.scroll_x);
        assert!(active.right <= result.scroll_x + result.viewport_width);
    }

    #[test]
    fn shrinking_content_clamps_stale_scroll() {
        let result = layout(&[160.0, 160.0], 500.0, 10_000.0, Some(1), false);

        assert_eq!(result.max_scroll_x, 0.0);
        assert_eq!(result.scroll_x, 0.0);
    }

    #[test]
    fn active_tab_uses_its_full_natural_width_while_inactive_tabs_stay_bounded() {
        let result = layout(&[480.0, 480.0], 800.0, 0.0, Some(0), false);

        assert_eq!(result.items[0].width, 480.0);
        assert_eq!(result.items[1].width, MAX);
    }

    #[test]
    fn revealing_an_active_tab_wider_than_the_viewport_aligns_its_start() {
        let result = layout(&[160.0, 600.0], 300.0, 0.0, Some(1), true);

        assert_eq!(result.items[1].width, 600.0);
        assert_eq!(result.scroll_x, result.items[1].left - PAD);
    }

    #[test]
    fn reorder_slot_changes_only_after_crossing_tab_centers() {
        let result = layout(&[120.0, 120.0, 120.0], 500.0, 0.0, Some(0), false);

        let centre = |i: usize| (result.items[i].left + result.items[i].right) / 2.0;
        // Dragging the first tab right: nothing changes until a centre is crossed.
        assert_eq!(reorder_slot(&result.items, 0, 0.0), Some(0));
        assert_eq!(reorder_slot(&result.items, 0, centre(1) - 1.0), Some(0));
        assert_eq!(reorder_slot(&result.items, 0, centre(1) + 1.0), Some(1));
        assert_eq!(reorder_slot(&result.items, 0, centre(2) - 1.0), Some(1));
        assert_eq!(reorder_slot(&result.items, 0, 10_000.0), Some(2));
        // Dragging the last tab left uses the same rule.
        assert_eq!(reorder_slot(&result.items, 2, 10_000.0), Some(2));
        assert_eq!(reorder_slot(&result.items, 2, centre(1) + 1.0), Some(2));
        assert_eq!(reorder_slot(&result.items, 2, centre(1) - 1.0), Some(1));
        assert_eq!(reorder_slot(&result.items, 2, centre(0) - 1.0), Some(0));
        assert_eq!(reorder_slot(&[], 0, 10.0), None);
    }

    #[test]
    fn visible_range_covers_only_tabs_near_the_viewport() {
        let widths = vec![160.0; 500];
        let result = layout(&widths, 600.0, 0.0, Some(250), true);
        let range = visible_range(&result.items, result.scroll_x, result.viewport_width, 200.0);

        assert!(range.contains(&250));
        assert!(range.len() < 10);
        for item in &result.items[range.clone()] {
            assert!(item.right >= result.scroll_x - 200.0);
            assert!(item.left <= result.scroll_x + result.viewport_width + 200.0);
        }
        if range.start > 0 {
            assert!(result.items[range.start - 1].right < result.scroll_x - 200.0);
        }
        if range.end < result.items.len() {
            assert!(result.items[range.end].left > result.scroll_x + 600.0 + 200.0);
        }
    }

    #[test]
    fn visible_range_handles_empty_and_unscrolled_strips() {
        assert_eq!(visible_range(&[], 0.0, 600.0, 200.0), 0..0);

        let result = layout(&[160.0, 160.0], 600.0, 0.0, None, false);
        assert_eq!(visible_range(&result.items, 0.0, 600.0, 0.0), 0..2);
        assert_eq!(visible_range(&result.items, f32::NAN, 0.0, 0.0), 0..0);
        assert_eq!(visible_range(&result.items, f32::NAN, 0.0, PAD), 0..1);
    }
}
