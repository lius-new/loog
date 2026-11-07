//! Shared displayed-row geometry for paint, pointer input, caret and scrolling.
use super::{hints, view};
use crate::ui::fonts;
use lgui::prelude::UiRect;
use loog_editor_core::text::{Assoc, DisplayMap, DisplayRow, Selection, TextBuffer};
use std::{cell::RefCell, collections::VecDeque, ops::Range, sync::Arc};

struct Cached {
    source: Arc<String>,
    tab: usize,
    columns: Option<usize>,
    folds: Vec<Range<usize>>,
    map: Arc<DisplayMap>,
}
thread_local! {static CACHE: RefCell<VecDeque<Cached>> = const { RefCell::new(VecDeque::new()) };}
pub fn cached(
    buffer: &TextBuffer,
    columns: Option<usize>,
    folds: &[Range<usize>],
) -> Arc<DisplayMap> {
    if columns.is_none() && folds.is_empty() || buffer.len() > 1024 * 1024 {
        return Arc::new(DisplayMap::new(buffer, columns, folds));
    }
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let source = buffer.text_snapshot();
        if let Some(index) = cache.iter().position(|c| {
            Arc::ptr_eq(&c.source, &source)
                && c.tab == buffer.options().tab_width
                && c.columns == columns
                && c.folds == folds
        }) {
            let entry = cache.remove(index).unwrap();
            let map = entry.map.clone();
            cache.push_back(entry);
            return map;
        }
        let map = Arc::new(DisplayMap::new(buffer, columns, folds));
        cache.push_back(Cached {
            source,
            tab: buffer.options().tab_width,
            columns,
            folds: folds.to_vec(),
            map: map.clone(),
        });
        while cache.len() > 8
            || cache
                .iter()
                .map(|c| c.source.len() + c.map.len() * std::mem::size_of::<DisplayRow>())
                .sum::<usize>()
                > 8 * 1024 * 1024
        {
            cache.pop_front();
        }
        map
    })
}
pub fn metrics(
    rect: UiRect,
    left: f32,
    buffer: &TextBuffer,
    map: &DisplayMap,
) -> view::ScrollMetrics {
    let content_h = (map.len() as f32 * fonts::editor().line_h).max(fonts::editor().line_h);
    let natural_w = if map.wrapped() {
        0.0
    } else {
        buffer.max_line_display_width() as f32 * fonts::editor().char_w + 32.0
    };
    let mut viewport_w = (rect.right - left).max(1.0);
    let mut viewport_h = rect.height().max(1.0);
    for _ in 0..2 {
        viewport_w = (rect.right
            - left
            - if content_h > viewport_h {
                view::SCROLLBAR_SIZE
            } else {
                0.0
            })
        .max(1.0);
        viewport_h = (rect.height()
            - if natural_w > viewport_w {
                view::SCROLLBAR_SIZE
            } else {
                0.0
            })
        .max(1.0);
    }
    let content_w = natural_w.max(viewport_w);
    view::ScrollMetrics {
        viewport_w,
        viewport_h,
        content_w,
        content_h,
        max_x: (content_w - viewport_w).max(0.0),
        max_y: (content_h - viewport_h).max(0.0),
    }
}
pub fn window(
    buffer: &TextBuffer,
    map: &DisplayMap,
    row: usize,
    sx: f32,
    width: f32,
) -> Range<usize> {
    let source = map.row(buffer, row);
    if map.wrapped() {
        source.range
    } else {
        view::line_window(buffer, source.line, sx, width)
    }
}
pub fn point_row(map: &DisplayMap, rect: UiRect, sy: f32, y: f32) -> usize {
    (((y - rect.top + sy) / fonts::editor().line_h)
        .floor()
        .max(0.0) as usize)
        .min(map.len().saturating_sub(1))
}
pub fn caret(
    buffer: &TextBuffer,
    map: &DisplayMap,
    selection: Selection,
    left: f32,
    top: f32,
    sx: f32,
    width: f32,
) -> (f32, f32) {
    let _tabs = view::TabScope::new(buffer.options().tab_width);
    let at = selection.cursor;
    let row = map.caret_row(buffer, at, selection.wrap_affinity);
    let source = map.row(buffer, row);
    let mut range = window(buffer, map, row, sx, width);
    if at < range.start || at > range.end {
        // Retain the original bounded off-screen caret path for unwrapped long lines.
        let (_, col) = buffer.line_col(at);
        let x = view::document_caret(
            buffer,
            source.line,
            col,
            left - source.column as f32 * fonts::editor().char_w,
            top,
            left + width,
            (sx, width),
        );
        return (x, top + row as f32 * fonts::editor().line_h);
    }
    range.end = range.end.max(range.start);
    let initial = buffer.display_column(range.start);
    let origin = left + (initial - source.column) as f32 * fonts::editor().char_w;
    let y = top + row as f32 * fonts::editor().line_h;
    let drawn = hints::drawn(buffer.snapshot_identity());
    let inlays = drawn
        .as_ref()
        .map(|h| hints::in_window(h, buffer.text(), range.clone()))
        .unwrap_or_default();
    let layout = view::layout_line_hinted(
        &buffer.text()[range.clone()],
        &inlays,
        origin,
        y,
        left + width,
        initial,
    );
    let x = view::caret_x(
        layout.as_ref(),
        buffer.text()[range.start..at.min(range.end)]
            .chars()
            .count(),
    )
    .unwrap_or(
        left + buffer.display_column(at).saturating_sub(source.column) as f32
            * fonts::editor().char_w,
    );
    (x, y)
}
#[allow(clippy::too_many_arguments)]
pub fn hit(
    buffer: &TextBuffer,
    map: &DisplayMap,
    rect: UiRect,
    left: f32,
    metrics: view::ScrollMetrics,
    sx: f32,
    sy: f32,
    x: f32,
    y: f32,
) -> (usize, Assoc) {
    let _tabs = view::TabScope::new(buffer.options().tab_width);
    let row = point_row(map, rect, sy, y);
    let source = map.row(buffer, row);
    let range = window(buffer, map, row, sx, metrics.viewport_w);
    let initial = buffer.display_column(range.start);
    let origin = left + (initial - source.column) as f32 * fonts::editor().char_w;
    let line = &buffer.text()[range.clone()];
    let top = rect.top + row as f32 * fonts::editor().line_h;
    let drawn = hints::drawn(buffer.snapshot_identity());
    let inlays = drawn
        .as_ref()
        .map(|h| hints::in_window(h, buffer.text(), range.clone()))
        .unwrap_or_default();
    let column = view::layout_line_hinted(
        line,
        &inlays,
        origin,
        top,
        left + metrics.content_w,
        initial,
    )
    .map(|layout| view::hit_column(&layout, x + sx, y + sy))
    .unwrap_or_else(|| view::fallback_hit_column(line, x + sx - origin));
    let at = buffer.grapheme_floor(
        range.start
            + line
                .char_indices()
                .nth(column)
                .map_or(line.len(), |(at, _)| at),
    );
    (at, map.affinity_at(buffer, row, at))
}
