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
        if rect.bottom - self.scroll <= self.viewport.top
            || rect.top - self.scroll >= self.viewport.bottom
        {
            return row;
        }
        let placement = VerticalPlacement::Overlay(0.0, self.scroll);
        let visible_levels = ((self.viewport.right - self.first_x) / self.indent)
            .ceil()
            .max(0.0) as usize;
        for level in 0..depth.min(visible_levels) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use lgui::application::AppView;
    use lgui::core::UiScale;
    use lgui::prelude::{VisualStyle, panel};
    use lgui::session::UiSession;
    use std::sync::Arc;

    #[test]
    fn pinned_guides_cull_offscreen_rows_and_levels_in_deep_trees() {
        let viewport = UiRect::new(0.0, 0.0, 100.0, 60.0);
        for (top, expected) in [(-20.0, 0), (80.0, 0), (10.0, 9)] {
            let row = UiRect::new(0.0, top, 100.0, top + 20.0);
            let view: AppView = Arc::new(move |_| {
                TreeGuides::new(viewport, 0.0, 10.0, 10.0).pinned(
                    panel(row, VisualStyle::filled(theme::c().sidebar)),
                    row,
                    100_000,
                )
            });
            let mut session = UiSession::new();
            session.render_view(&view, viewport, UiScale::ONE);
            assert!(
                session.tree().nodes().len() <= 12,
                "no offscreen placeholder nodes"
            );
            assert_eq!(
                session
                    .tree()
                    .nodes()
                    .iter()
                    .filter(|node| node.style.fill == Some(theme::c().border))
                    .count(),
                expected
            );
        }
    }
}
