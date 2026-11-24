use lgui::core::Point;
use lgui::prelude::{Element, UiRect};

use crate::ui::components::divider::{VerticalPlacement, vertical_pixel};
use crate::ui::theme;

pub(super) struct TreeGuides {
    viewport: UiRect,
    scroll: f32,
    first_x: f32,
    indent: f32,
}

impl TreeGuides {
    pub(super) fn new(viewport: UiRect, scroll: f32, first_x: f32, indent: f32) -> Self {
        Self {
            viewport,
            scroll,
            first_x,
            indent,
        }
    }

    /// Join adjacent rows that share a guide into a single span.
    pub(super) fn append(
        &self,
        content: &mut Vec<Element>,
        rows: impl Iterator<Item = (f32, usize)>,
        bottom: f32,
    ) {
        let mut starts = Vec::new();
        let placement = VerticalPlacement::Scrolled(0.0, self.scroll);
        for (top, depth) in rows.chain(std::iter::once((bottom, 0))) {
            let keep = depth.min(starts.len());
            for (offset, start) in starts.drain(keep..).enumerate() {
                content.push(self.line(start, top, keep + offset, placement));
            }
            starts.resize(depth, top);
        }
    }

    /// Restore the ancestor guides over the pinned row's opaque background.
    pub(super) fn pinned(&self, mut row: Element, rect: UiRect, depth: usize) -> Element {
        let placement = VerticalPlacement::Overlay(0.0, self.scroll);
        for level in 0..depth {
            row = row.child(self.line(rect.top, rect.bottom, level, placement));
        }
        row
    }

    fn line(&self, top: f32, bottom: f32, level: usize, placement: VerticalPlacement) -> Element {
        vertical_pixel(
            Point::new(self.first_x + level as f32 * self.indent, top),
            bottom,
            self.viewport,
            placement,
            theme::c().border,
        )
    }
}
