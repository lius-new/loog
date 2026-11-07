//! The central code viewport: gutter, syntax-highlighted source, cursor and
//! editor-specific overlay scrollbars.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use lgui::core::{
    CursorIcon, EventPolicy, KeyState, PointerButton, SemanticRole, Semantics, UiElement,
    UiEventKind, UiEventPayload, UiFocusHandle, UiId, WheelUnit, clip,
};
use lgui::core::{PathStyle, Point, Stroke, UiPath, UiPathCommand, path};
use lgui::prelude::{Color, Element, UiRect, VisualStyle, group, panel, selection_region, text};
use lgui::text::{self, TextLayout, TextLayoutRequest};

use super::gutter;
use super::projection;
use super::syntax;
use crate::domain::identity::PaneId;
use crate::domain::line_change::LineChange;
use crate::features::git::conflict::{ConflictPart, ConflictRegion, Resolution};
use crate::features::language_service::diagnostics::{Diagnostic, Severity};
use crate::ui::editor_binding::{Binding, Event, PeekPlacement, ViewState};
use crate::ui::fonts;
use crate::ui::git::action_button;
use crate::ui::theme;
use loog_editor_core::text::TextBuffer;
use loog_editor_core::vim::CursorShape;

pub(crate) use crate::ui::components::scrollbar::SIZE as SCROLLBAR_SIZE;
use crate::ui::components::scrollbar::{
    INSET as SCROLLBAR_INSET, MIN_THUMB_LENGTH, thumb as scrollbar_thumb, track as scrollbar_track,
};
const TRAILING_CODE_SPACE: f32 = 32.0;
/// The shortest change mark on the scrollbar, so one line stays visible.
const MIN_MARK_LENGTH: f32 = 3.0;

pub(crate) use crate::ui::editor_binding::Metrics as ScrollMetrics;

pub(crate) use crate::ui::editor_binding::OverviewMark;

/// The overview ruler marks of a file's Git `changes`, keyed by 1-based
/// line; adjacent lines with the same change form one mark.
pub(crate) fn change_marks(
    changes: &BTreeMap<usize, LineChange>,
    line_count: usize,
) -> Vec<OverviewMark> {
    let mut marks: Vec<(usize, usize, LineChange)> = Vec::new();
    for (&line, &change) in changes {
        let line = line.clamp(1, line_count.max(1));
        match marks.last_mut() {
            Some((_, end, last)) if *last == change && *end + 1 >= line => *end = line,
            _ => marks.push((line, line, change)),
        }
    }
    marks
        .into_iter()
        .map(|(first, last, change)| OverviewMark {
            first: first - 1,
            lines: last + 1 - first,
            change,
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ThumbGeometry {
    pub(crate) start: f32,
    pub(crate) length: f32,
}

/// Render the editor of `pane`'s active file into `rect`.
pub(crate) fn render(
    pane: PaneId,
    rect: UiRect,
    state: Binding,
    editor_id: UiId,
    editor_focus: UiFocusHandle,
) -> Element {
    let s = state.get();
    let focused_pane = s.focused_pane;

    let ime_rect = ime_cursor_rect(&s, rect);
    // Vim's Normal and Visual modes take keys as commands, so the input
    // method stays off until a mode accepts text.
    let role = if !s.accepts_text {
        SemanticRole::Group
    } else {
        SemanticRole::TextInput
    };
    let mut root = Element::new(move |cx| {
        UiElement::panel(editor_id, rect, VisualStyle::filled(theme::c().bg))
            .ime_cursor_rect(ime_rect)
            .semantics(Semantics::new(role).name("Code editor"))
            .children(cx.children)
    })
    .event_policy(EventPolicy::INTERACTIVE);

    if let Some(document) = &s.document {
        let id = document.id;
        let buffer = document.editor();
        let _tabs = TabScope::new(buffer.options().tab_width);
        let projection = document.projection.clone();
        let line_count = buffer.line_count();
        let (cursor_line, cursor_col) = buffer.line_col();
        let cursor_row =
            projection.caret_row(&buffer, buffer.cursor(), document.selection.wrap_affinity);
        let code_top = rect.top;
        let code_left =
            rect.left + fonts::editor().gutter_w_for_lines(line_count) + theme::CODE_PAD;
        let metrics = projection::metrics(rect, code_left, &buffer, &projection);
        let (stored_x, stored_y) = document.scroll;
        let scroll_x = stored_x.clamp(0.0, metrics.max_x);
        let scroll_y = stored_y.clamp(0.0, metrics.max_y);
        let visible_rows = visible_line_range(scroll_y, rect.height(), projection.len());
        let visible_lines = if visible_rows.is_empty() {
            0..0
        } else {
            projection.source_line(visible_rows.start)
                ..projection.source_line(visible_rows.end - 1) + 1
        };
        let mut decorations = s
            .line_changes
            .range(visible_lines.start + 1..visible_lines.end + 1)
            .map(|(line, change)| {
                let decoration = match change {
                    LineChange::Added => gutter::GutterDecoration::GitAdded,
                    LineChange::Modified => gutter::GutterDecoration::GitModified,
                    LineChange::Deleted => gutter::GutterDecoration::GitDeleted,
                };
                (*line, vec![decoration])
            })
            .collect::<BTreeMap<_, _>>();
        let conflict_lines = conflict_lines(&s.conflicts, &visible_lines);
        for &(line, part) in &conflict_lines {
            decorations.insert(line, vec![conflict_decoration(part)]);
        }
        let has_vertical = metrics.max_y > 0.0;
        let has_horizontal = metrics.max_x > 0.0;

        super::hints::publish(document.buffer.snapshot_identity(), &s.inlay_hints);
        let st_pointer = state.clone();
        let pointer_focus = editor_focus.clone();
        root = root.on_pointer_down_with_button(move |cx, pointer, button| {
            let point = pointer.point;
            let over_vertical_scrollbar = has_vertical && point.x >= rect.right - SCROLLBAR_SIZE;
            let over_horizontal_scrollbar =
                has_horizontal && point.y >= rect.bottom - SCROLLBAR_SIZE;
            if !matches!(button, PointerButton::Left | PointerButton::Right)
                || over_vertical_scrollbar
                || over_horizontal_scrollbar
            {
                return;
            }

            pointer_focus.focus();
            st_pointer.send(Event::PointerDown {
                document: id,
                point: (point.x, point.y),
                button,
                code_left,
                metrics,
            });
            cx.stop_propagation();
        });

        let st_drag = state.clone();
        root = root.on_pointer_drag(move |cx, pointer| {
            st_drag.send(Event::PointerDrag((pointer.point.x, pointer.point.y)));
            cx.stop_propagation();
        });
        let st_up = state.clone();
        root = root.on_pointer_up(move |_, _| st_up.send(Event::PointerUp));

        let st_wheel = state.clone();
        root = root.on_wheel(move |cx, delta| {
            let (step_x, step_y) = match delta.unit {
                WheelUnit::Lines => (
                    delta.x * fonts::editor().line_h * 3.0,
                    delta.y * fonts::editor().line_h * 3.0,
                ),
                WheelUnit::Pixels => (delta.x, delta.y),
            };
            st_wheel.send(Event::Wheel {
                x: step_x,
                y: step_y,
                metrics,
            });
            cx.stop_propagation();
        });

        // Default cursors inherit from ancestors in lgui. Keep the text cursor
        // on the content subtree so sibling scrollbars retain the arrow cursor.
        // Content runs under the overlay scrollbars, whose tracks are transparent.
        let mut content = clip(rect, 0.0, 0.0)
            .key(format!("editor-content-{}", pane.get()))
            .cursor(if s.link.is_some() {
                CursorIcon::Pointer
            } else {
                CursorIcon::Text
            });
        if !conflict_lines.is_empty() {
            let mut rows = group(rect);
            for &(line, part) in &conflict_lines {
                let at = buffer.line_start(line - 1);
                if projection.hidden(&buffer, at) {
                    continue;
                }
                let first = projection.row_of(&buffer, at);
                let after = if line < buffer.line_count() {
                    projection.row_of(&buffer, buffer.line_start(line))
                } else {
                    projection.len()
                };
                let top = code_top + first as f32 * fonts::editor().line_h;
                rows = rows.child(panel(
                    UiRect::new(
                        code_left,
                        top,
                        rect.right,
                        top + (after - first).max(1) as f32 * fonts::editor().line_h,
                    ),
                    VisualStyle::filled(conflict_row_color(part)),
                ));
            }
            content = content.child(clip(rect, 0.0, -scroll_y).child(rows));
        }
        if visible_lines.contains(&cursor_line) {
            let highlight_y = code_top + cursor_row as f32 * fonts::editor().line_h;
            let highlight = panel(
                UiRect::new(
                    code_left,
                    highlight_y,
                    rect.right,
                    highlight_y + fonts::editor().line_h,
                ),
                VisualStyle::filled(theme::c().active_line),
            );
            content = content.child(clip(rect, 0.0, -scroll_y).child(highlight));
        }

        let gutter_viewport = UiRect::new(
            rect.left,
            code_top,
            code_left - theme::CODE_PAD,
            rect.bottom,
        );
        let gutter_content = UiRect::new(
            gutter_viewport.left,
            code_top,
            gutter_viewport.right - theme::GUTTER_FOLD_W - theme::GUTTER_FOLD_PAD,
            code_top + metrics.content_h,
        );
        let gutter_hover = s
            .editor_gutter_hover
            .filter(|(hovered_pane, _)| *hovered_pane == pane)
            .map(|(_, line)| line);
        let fold_viewport = UiRect::new(
            gutter_viewport.right - theme::GUTTER_FOLD_W - theme::GUTTER_FOLD_PAD,
            gutter_viewport.top,
            gutter_viewport.right - theme::GUTTER_FOLD_PAD,
            gutter_viewport.bottom,
        );
        let fold_rows: Vec<_> = visible_rows
            .clone()
            .filter_map(|row| {
                let source = projection.row(&buffer, row);
                if source.continuation {
                    return None;
                }
                document
                    .folds
                    .iter()
                    .find(|f| f.line == source.line)
                    .map(|marker| (row, marker))
            })
            .collect();
        let fold_column =
            super::fold::Column::new(id, fold_rows.iter().map(|(_, marker)| marker.start));
        let mut fold_controls = fold_column
            .render(gutter_viewport, fold_viewport.left)
            .child(
                clip(gutter_viewport, 0.0, -scroll_y).child(gutter::render_rows(
                    gutter_content,
                    visible_rows.clone().filter_map(|row| {
                        let source = projection.row(&buffer, row);
                        (!source.continuation).then_some((source.line, row))
                    }),
                    &decorations,
                    gutter_hover,
                )),
            );
        for (row, marker) in fold_rows {
            let y = code_top + row as f32 * fonts::editor().line_h - scroll_y;
            fold_controls = fold_controls.child(super::fold::button(
                UiRect::new(
                    fold_viewport.left,
                    y,
                    fold_viewport.right,
                    y + fonts::editor().line_h,
                ),
                format!(
                    "{} code at line {}",
                    if marker.collapsed { "Unfold" } else { "Fold" },
                    marker.line + 1
                ),
                super::fold::Content::Chevron {
                    collapsed: marker.collapsed,
                    column: fold_column.clone(),
                    start: marker.start,
                },
                state.clone(),
                editor_focus.clone(),
                id,
                document.stamp,
                marker.start,
            ));
        }
        // The hovered change bar opens its peek, like a link.
        if let Some(run) = gutter::hovered_run(&decorations, gutter_hover)
            && let Some(line) = gutter_hover
            && s.has_repository
        {
            let top = (code_top
                + projection.row_of(&buffer, buffer.line_start(*run.start() - 1)) as f32
                    * fonts::editor().line_h
                - scroll_y)
                .max(gutter_viewport.top);
            let bottom = (code_top
                + (if *run.end() < buffer.line_count() {
                    projection.row_of(&buffer, buffer.line_start(*run.end()))
                } else {
                    projection.len()
                }) as f32
                    * fonts::editor().line_h
                - scroll_y)
                .min(gutter_viewport.bottom);
            let st_peek = state.clone();
            let change_id = fold_column.change_id();
            let change_rect = UiRect::new(
                gutter_viewport.left + theme::GUTTER_ACTION_W,
                top,
                fold_viewport.left,
                bottom,
            );
            fold_controls = fold_controls.child(
                Element::new(move |cx| {
                    UiElement::panel(change_id.clone(), change_rect, VisualStyle::default())
                        .children(cx.children)
                })
                .key(format!("gutter-change-{}", pane.get()))
                .event_policy(EventPolicy::INTERACTIVE)
                .cursor(CursorIcon::Pointer)
                .semantics(Semantics::new(SemanticRole::Button).name("Show change"))
                .on_pointer_down(move |cx, _pointer| {
                    cx.stop_propagation();
                    st_peek.send(Event::Peek { line });
                }),
            );
        }
        let st_gutter_hover = state.clone();
        let gutter_projection = projection.clone();
        if focused_pane
            && visible_lines.contains(&cursor_line)
            && let Some(revision) = s.lightbulb
        {
            let top = code_top + cursor_row as f32 * fonts::editor().line_h - scroll_y;
            let st_actions = state.clone();
            fold_controls = fold_controls.child(super::lightbulb::render(
                UiRect::new(
                    gutter_viewport.left,
                    top,
                    gutter_viewport.left + theme::GUTTER_ACTION_W,
                    top + fonts::editor().line_h,
                ),
                fold_column.actions_id(),
                move || st_actions.send(Event::CodeActions { revision }),
            ));
        }
        root = root.on_event_capture(UiEventKind::PointerMove, move |_cx, payload| {
            if let UiEventPayload::PointerMove { pointer } = payload {
                let point = pointer.point;
                let hovered = gutter_viewport.contains(point).then(|| {
                    let line =
                        ((point.y - code_top + scroll_y) / fonts::editor().line_h).floor() as usize;
                    (pane, gutter_projection.source_line(line) + 1)
                });
                let hovered = hovered.filter(|(_, line)| *line <= line_count + 1);
                st_gutter_hover.send(Event::GutterHover(hovered));
            }
        });

        let code_viewport = UiRect::new(code_left, code_top, rect.right, rect.bottom);
        let layout_right = code_left + metrics.content_w;
        let mut code = group(UiRect::new(
            code_left,
            code_top,
            layout_right,
            code_top + metrics.content_h,
        ));

        // Vim draws its own visual selection; other views show the core one.
        let selections = &s.selections;
        let matches = &s.search_matches;
        let windows = visible_rows
            .clone()
            .map(|row| {
                let source = projection.row(&buffer, row);
                crate::features::syntax::Window {
                    line: buffer.line_range(source.line),
                    range: projection::window(
                        &buffer,
                        &projection,
                        row,
                        scroll_x,
                        metrics.viewport_w,
                    ),
                }
            })
            .collect();
        let ready = state.highlights(document, windows);
        // Where each underlined diagnostic is on screen, least serious first.
        let mut diagnostic_hits: Vec<(UiRect, Range<usize>)> = Vec::new();
        for row_index in visible_rows.clone() {
            let source_row = projection.row(&buffer, row_index);
            let line_index = source_row.line;
            let y = code_top + row_index as f32 * fonts::editor().line_h;
            let window = projection::window(
                &buffer,
                &projection,
                row_index,
                scroll_x,
                metrics.viewport_w,
            );
            let line = &buffer.text()[window.clone()];
            let initial_column = buffer.display_column(window.start);
            let line_left =
                code_left + (initial_column - source_row.column) as f32 * fonts::editor().char_w;
            let line_hints = super::hints::in_window(&s.inlay_hints, buffer.text(), window.clone());
            let layout = layout_line_hinted(
                line,
                &line_hints,
                line_left,
                y,
                layout_right,
                initial_column,
            );
            let (start, end) = (window.start, window.end);
            let next = if window.end < buffer.line_end(line_index) {
                window.end
            } else {
                buffer.line_range_with_break(line_index).end
            };
            // The part of `range` on this line, as a highlight rectangle.
            let line_rect = |range: &Range<usize>| {
                if range.start >= next || range.end <= start || range.start > end {
                    return None;
                }
                let from = range.start.max(start).min(end);
                let to = range.end.min(end).max(start);
                let left = caret_x(layout.as_ref(), buffer.text()[start..from].chars().count())
                    .unwrap_or_else(|| {
                        code_left
                            + (buffer.display_column(from) - source_row.column) as f32
                                * fonts::editor().char_w
                    });
                let mut right = caret_x(layout.as_ref(), buffer.text()[start..to].chars().count())
                    .unwrap_or_else(|| {
                        code_left
                            + (buffer.display_column(to) - source_row.column) as f32
                                * fonts::editor().char_w
                    });
                if range.end > end && next > end {
                    right += fonts::editor().char_w;
                }
                Some(UiRect::new(
                    left,
                    y,
                    right.max(left + 2.0),
                    y + fonts::editor().line_h,
                ))
            };
            // Hints (a compiler's "add `;` here") are not underlined: they only
            // repeat a nearby error. Of the rest, the more serious is drawn last so
            // it stays on top where two share a place.
            for severity in [Severity::Information, Severity::Warning, Severity::Error] {
                for diagnostic in s.diagnostics.iter().filter(|d| d.severity == severity) {
                    if let Some(r) = line_rect(&diagnostic.range) {
                        code = code.child(squiggle(r, severity_colour(severity)));
                        let on_screen = UiRect::new(
                            r.left - scroll_x,
                            r.top - scroll_y,
                            r.right - scroll_x,
                            r.bottom - scroll_y,
                        );
                        diagnostic_hits.push((on_screen, diagnostic.range.clone()));
                    }
                }
            }
            if let Some(link) = &s.link
                && let Some(r) = line_rect(link)
            {
                code = code.child(panel(
                    UiRect::new(r.left, r.bottom - 3.0, r.right, r.bottom - 2.0),
                    VisualStyle::filled(theme::c().accent),
                ));
            }
            for found in s.symbol_highlights.iter() {
                if let Some(r) = line_rect(found) {
                    code = code.child(panel(r, VisualStyle::filled(theme::c().accent).alpha(38)));
                }
            }
            for found in matches {
                if let Some(r) = line_rect(found) {
                    code = code.child(panel(r, VisualStyle::filled(theme::c().warning).alpha(45)));
                }
            }
            for selection in selections {
                if let Some(r) = line_rect(selection) {
                    code = code.child(panel(r, VisualStyle::filled(theme::c().accent).alpha(55)));
                }
            }
            if buffer.options().indent_guides {
                let indentation = buffer
                    .line(line_index)
                    .chars()
                    .take(4096)
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .count();
                let end = buffer.offset_at_char_column(line_index, indentation);
                let columns = buffer.display_column(end);
                let step = if buffer.options().use_tabs {
                    buffer.options().tab_width
                } else {
                    buffer.options().indent_size
                };
                for column in (step..=columns).step_by(step) {
                    let x = code_left
                        + (column as f32 - source_row.column as f32) * fonts::editor().char_w;
                    if x >= code_left + scroll_x && x <= rect.right + scroll_x {
                        code = code.child(
                            panel(
                                UiRect::new(x, y, x + 1.0, y + fonts::editor().line_h),
                                VisualStyle::filled(theme::c().border).alpha(100),
                            )
                            .semantics(Semantics::new(SemanticRole::Group).name("Indent guide")),
                        );
                    }
                }
            }
            if buffer.options().match_brackets {
                if let Some((a, b)) = buffer.matching_brackets(buffer.cursor()) {
                    for at in [a, b] {
                        if let Some(r) = line_rect(&(at..at + 1)) {
                            code = code.child(
                                panel(r, VisualStyle::filled(theme::c().accent).alpha(70))
                                    .semantics(
                                        Semantics::new(SemanticRole::Group)
                                            .name("Matching bracket"),
                                    ),
                            );
                        }
                    }
                }
            }
            if buffer.options().show_whitespace {
                for (column, ch) in line.chars().enumerate() {
                    if ch != ' ' && ch != '\t' {
                        continue;
                    }
                    let x = caret_x(layout.as_ref(), column).unwrap_or(line_left);
                    let mark = if ch == '\t' { "→" } else { "·" };
                    code = code.child(text(
                        UiRect::new(x, y, x + fonts::editor().char_w, y + fonts::editor().line_h),
                        mark,
                        fonts::editor().style(theme::c().text_faint),
                    ));
                }
                if end == buffer.line_end(line_index) && next > end {
                    let x = caret_x(layout.as_ref(), line.chars().count()).unwrap_or(line_left);
                    code = code.child(text(
                        UiRect::new(x, y, x + fonts::editor().char_w, y + fonts::editor().line_h),
                        "¬",
                        fonts::editor().style(theme::c().text_faint),
                    ));
                }
            }

            // Hints are drawn in the room the layout left for them.
            for (n, (_, hint)) in line_hints.iter().enumerate() {
                if let Some(hint_x) = layout.as_ref().and_then(|l| l.hint_x(n)) {
                    let width = hint.chars().count() as f32 * fonts::editor().char_w;
                    code = code.child(text(
                        UiRect::new(hint_x, y, hint_x + width + 4.0, y + fonts::editor().line_h),
                        (*hint).to_owned(),
                        fonts::editor().style(theme::c().text_faint),
                    ));
                }
            }
            if window.end == buffer.line_end(line_index) {
                if let Some(marker) = document
                    .folds
                    .iter()
                    .find(|f| f.line == line_index && f.collapsed)
                {
                    let count = buffer
                        .line_of(marker.end.saturating_sub(1))
                        .saturating_sub(marker.line);
                    let glyph = format!(" ⋯ {count} lines ");
                    let width = glyph.chars().count() as f32 * fonts::editor().char_w;
                    let edge = caret_x(layout.as_ref(), line.chars().count()).unwrap_or(line_left);
                    let left = (edge + 4.0)
                        .min(code_left + metrics.viewport_w - width)
                        .max(code_left);
                    code = code.child(super::fold::button(
                        UiRect::new(left, y, left + width, y + fonts::editor().line_h),
                        "Expand folded lines".into(),
                        super::fold::Content::Placeholder(glyph),
                        state.clone(),
                        editor_focus.clone(),
                        id,
                        document.stamp,
                        marker.start,
                    ));
                }
            }
            let mut x = caret_x(layout.as_ref(), 0).unwrap_or(line_left);
            let mut char_offset = 0;
            let mut display_column = initial_column;
            let hint_positions: Vec<usize> = line_hints.iter().map(|(at, _)| *at).collect();
            let spans = super::hints::split_spans(
                syntax::highlight_window(
                    &buffer,
                    window.clone(),
                    ready
                        .iter()
                        .find(|tokens| tokens.range == window)
                        .map(|tokens| tokens.spans.as_ref()),
                ),
                &hint_positions,
            );
            let spans = match &s.link {
                Some(link) => tint_range(spans, window.start, link, theme::c().accent),
                None => spans,
            };
            for (span, color) in spans {
                char_offset += span.chars().count();
                let (display, _, column) = expand_tabs(&span, display_column);
                let next_x = caret_x(layout.as_ref(), char_offset).unwrap_or_else(|| {
                    x + (column - display_column) as f32 * fonts::editor().char_w
                });
                let span_rect = UiRect::new(x, y, next_x.max(x) + 4.0, y + fonts::editor().line_h);
                display_column = column;
                code = code.child(text(span_rect, display, fonts::editor().style(color)));
                x = next_x;
            }
        }

        // Only the editor that owns keyboard focus shows a caret.
        let editor_focused = s.focused && focused_pane;
        if editor_focused && visible_lines.contains(&cursor_line) {
            let cursor_y = code_top + cursor_row as f32 * fonts::editor().line_h;
            let (cursor_x, _) = projection::caret(
                &buffer,
                &projection,
                document.selection,
                code_left,
                code_top,
                scroll_x,
                metrics.viewport_w,
            );
            let shape = s.cursor_shape;
            let cell = [cursor_x, cursor_y];
            let (cursor_rect, cursor_style) = if shape == CursorShape::Bar {
                (
                    super::caret::bar(cell),
                    VisualStyle::filled(theme::c().accent),
                )
            } else {
                // Block cursors cover the character under the cursor.
                let chars = buffer
                    .grapheme_at(buffer.cursor())
                    .filter(|grapheme| !grapheme.starts_with(['\r', '\n']))
                    .map_or(0, |grapheme| grapheme.chars().count());
                let right = (chars > 0)
                    .then(|| {
                        Some(
                            projection::caret(
                                &buffer,
                                &projection,
                                loog_editor_core::text::Selection {
                                    cursor: buffer
                                        .offset_at_char_column(cursor_line, cursor_col + chars),
                                    wrap_affinity: loog_editor_core::text::Assoc::Before,
                                    ..Default::default()
                                },
                                code_left,
                                code_top,
                                scroll_x,
                                metrics.viewport_w,
                            )
                            .0,
                        )
                    })
                    .flatten()
                    .unwrap_or(cursor_x + fonts::editor().char_w)
                    .max(cursor_x + 2.0);
                let top = match shape {
                    CursorShape::Underline => cursor_y + fonts::editor().line_h - 3.0,
                    CursorShape::HalfBlock => cursor_y + fonts::editor().line_h / 2.0,
                    _ => cursor_y + 1.0,
                };
                (
                    UiRect::new(
                        cursor_x,
                        top,
                        right,
                        cursor_y + fonts::editor().line_h - 1.0,
                    ),
                    VisualStyle::filled(theme::c().accent).alpha(140),
                )
            };
            // Every shape shares one caret element so mode switches keep the motion.
            // Selections (core or Vim visual) are painted immediately, so the caret
            // must not lag behind their edge.
            code = code.child(super::caret::render(
                cursor_rect,
                cell,
                cursor_style,
                super::caret::CaretContext {
                    pane,
                    document: id,
                    viewport: rect,
                    scroll: (scroll_x, scroll_y),
                },
                s.smooth_caret
                    && !(s.dragging && s.caret_dragging)
                    && selections.is_empty()
                    && s.preedit_for(pane, id).is_none(),
            ));
            for selection in &document.secondary {
                let (x, y) = projection::caret(
                    &buffer,
                    &projection,
                    *selection,
                    code_left,
                    code_top,
                    scroll_x,
                    metrics.viewport_w,
                );
                if y + fonts::editor().line_h >= scroll_y + rect.top && y <= scroll_y + rect.bottom
                {
                    code = code.child(panel(
                        UiRect::new(x, y + 1.0, x + 2.0, y + fonts::editor().line_h - 1.0),
                        VisualStyle::filled(theme::c().accent),
                    ));
                }
            }
            if let Some(session) = s.preedit_for(pane, id) {
                let width = layout_line(&session.text, 0.0, 0.0, metrics.content_w)
                    .map_or(80.0, |l| l.width)
                    .max(2.0);
                let r = UiRect::new(
                    cursor_x,
                    cursor_y,
                    cursor_x + width + 4.0,
                    cursor_y + fonts::editor().line_h,
                );
                code = code.child(
                    panel(r, VisualStyle::filled(theme::c().surface))
                        .child(text(
                            r,
                            session.text.clone(),
                            fonts::editor().style(theme::c().text_bright),
                        ))
                        .child(panel(
                            UiRect::new(r.left, r.bottom - 2.0, r.right, r.bottom - 1.0),
                            VisualStyle::filled(theme::c().accent),
                        )),
                );
            }
        }

        content = content.child(clip(code_viewport, -scroll_x, -scroll_y).child(code));
        // Above the code layer, which would otherwise take the pointer and
        // its text cursor away from the buttons.
        content = content.child(conflict_actions(
            &projection,
            &buffer,
            &s.conflicts,
            &visible_lines,
            rect,
            code_top,
            scroll_y,
            pane,
            &state,
        ));
        root = root
            .child(content)
            .child(clip(gutter_viewport, 0.0, 0.0).child(fold_controls));

        // The pointer over an underline opens its message, as in VS Code and
        // IntelliJ. Where two overlap, the more serious one wins.
        let hover_area = UiRect::new(
            code_viewport.left,
            code_viewport.top,
            code_viewport.right - if has_vertical { SCROLLBAR_SIZE } else { 0.0 },
            code_viewport.bottom - if has_horizontal { SCROLLBAR_SIZE } else { 0.0 },
        );
        let hovered_diagnostic = s
            .editor_diagnostic_hover
            .as_ref()
            .filter(|(hovered_pane, _)| *hovered_pane == pane)
            .and_then(|(_, range)| diagnostic_hits.iter().rev().find(|(_, r)| r == range))
            .cloned();
        // The card: a server hover text with the diagnostics under it, or just the
        // diagnostic the pointer is on.
        let mut card_anchor: Option<UiRect> = None;
        let card = if let Some(hover) = &s.hover {
            let anchor = anchor_rect_in(
                &buffer,
                &projection,
                &hover.anchor,
                (code_left, code_top, layout_right),
                (scroll_x, scroll_y),
                metrics.viewport_w,
            );
            card_anchor = anchor;
            let found: Vec<&Diagnostic> = s
                .diagnostics
                .iter()
                .filter(|d| {
                    d.range.start < hover.anchor.end
                        && hover.anchor.start < d.range.end.max(d.range.start + 1)
                })
                .collect();
            anchor.and_then(|anchor| {
                info_card_actions(
                    &found,
                    Some(&hover.docs),
                    anchor,
                    hover_area,
                    s.quickfix.as_ref(),
                    Some(state.clone()),
                )
            })
        } else if let Some((anchor, range)) = &hovered_diagnostic {
            card_anchor = Some(*anchor);
            let found: Vec<_> = s.diagnostics.iter().filter(|d| d.range == *range).collect();
            info_card_actions(
                &found,
                None,
                *anchor,
                hover_area,
                s.quickfix.as_ref(),
                Some(state.clone()),
            )
        } else {
            None
        };
        // The popup list, drawn over the code; the pointer on it leaves hover alone.
        let choice_revision = s.snippet_choices.as_ref().map(|(revision, _)| *revision);
        let popup = s
            .snippet_choices
            .as_ref()
            .map(|(_, view)| view)
            .or(s.popup.as_ref())
            .map(|view| {
                let anchor = view.anchor.and_then(|offset| {
                    anchor_rect_in(
                        &buffer,
                        &projection,
                        &(offset..offset),
                        (code_left, code_top, layout_right),
                        (scroll_x, scroll_y),
                        metrics.viewport_w,
                    )
                });
                let send = state.clone();
                super::popup::render(
                    view,
                    anchor,
                    hover_area,
                    std::sync::Arc::new(move |event| {
                        send.send(match choice_revision {
                            Some(revision) => Event::SnippetChoice { revision, event },
                            None => Event::Popup(event),
                        })
                    }),
                )
            });
        let signature = s.signature.as_ref().and_then(|(anchor, view)| {
            let rect = anchor_rect_in(
                &buffer,
                &projection,
                &(*anchor..*anchor),
                (code_left, code_top, layout_right),
                (scroll_x, scroll_y),
                metrics.viewport_w,
            )?;
            let send = state.clone();
            let revision = view.revision;
            let (links, selections) = (state.clone(), state.clone());
            Some(super::signature::render(
                view,
                rect,
                hover_area,
                std::sync::Arc::new(move |forward| {
                    send.send(Event::Signature { revision, forward });
                }),
                super::docs::Actions {
                    open_link: std::sync::Arc::new(move |url| links.send(Event::OpenLink(url))),
                    select: std::sync::Arc::new(move |text| {
                        selections.send(Event::SignatureSelect { revision, text })
                    }),
                },
            ))
        });
        let popup_rect = popup.as_ref().map(|(rect, _)| *rect);
        let signature_rect = signature.as_ref().map(|(rect, _)| *rect);
        let card_rect = card
            .as_ref()
            .map(|(rect, _)| *rect)
            .filter(|_| popup_rect.is_none());
        // The strip between the word and its card: the pointer crossing it on
        // the way to the card must not dismiss the card.
        let card_bridge = card_rect.zip(card_anchor).and_then(|(card, anchor)| {
            let (left, right) = (card.left.min(anchor.left), card.right.max(anchor.right));
            if card.top >= anchor.bottom {
                Some(UiRect::new(left, anchor.bottom, right, card.top))
            } else if card.bottom <= anchor.top {
                Some(UiRect::new(left, card.bottom, right, anchor.top))
            } else {
                None
            }
        });
        let st_diagnostic_hover = state.clone();
        let dragging = s.dragging;
        root = root.on_event_capture(UiEventKind::PointerMove, move |_cx, payload| {
            if let UiEventPayload::PointerMove { pointer } = payload {
                let point = pointer.point;
                // On the card itself nothing changes: it must stay to be read.
                if card_rect.is_some_and(|rect| rect.contains(point))
                    || card_bridge.is_some_and(|rect| rect.contains(point))
                    || popup_rect.is_some_and(|rect| rect.contains(point))
                    || signature_rect.is_some_and(|rect| rect.contains(point))
                {
                    return;
                }
                if !dragging && hover_area.contains(point) {
                    st_diagnostic_hover.send(Event::HoverMove {
                        point: (point.x, point.y),
                        code_left,
                        metrics,
                    });
                } else {
                    st_diagnostic_hover.send(Event::HoverLeave);
                }
                let hovered = (!dragging && hover_area.contains(point))
                    .then(|| {
                        diagnostic_hits
                            .iter()
                            .rev()
                            .find(|(r, _)| r.contains(point))
                    })
                    .flatten()
                    .map(|(_, range)| (pane, range.clone()));
                st_diagnostic_hover.send(Event::DiagnosticHover(hovered));
            }
        });
        // A drag that began on the card keeps it, wherever the pointer goes.
        let st_press = state.clone();
        root = root.on_event_capture(UiEventKind::PointerDown, move |_cx, payload| {
            if let UiEventPayload::PointerDown { pointer, .. } = payload
                && card_rect.is_some_and(|rect| rect.contains(pointer.point))
            {
                st_press.send(Event::HoverPress(true));
            }
        });
        let st_release = state.clone();
        root = root.on_event_capture(UiEventKind::PointerUp, move |_cx, _payload| {
            st_release.send(Event::HoverPress(false));
        });
        if s.peek_visible {
            if let Some(overlay) = state.overlay(PeekPlacement {
                rect: UiRect::new(rect.left, code_top, rect.right, rect.bottom),
                code_left,
                scroll_y,
                metrics,
            }) {
                root = root.child(overlay);
            }
        }

        let emphasized = s.editor_hovered == Some(pane)
            || (focused_pane
                && (s.editor_vertical_scrollbar_dragging
                    || s.editor_horizontal_scrollbar_dragging));
        if !s.overview_marks.is_empty() || !s.conflicts.is_empty() {
            root = root.child(overview_ruler(
                rect,
                metrics,
                has_horizontal,
                fonts::editor().line_h,
                s.overview_marks
                    .iter()
                    .map(|mark| {
                        let (first, lines) =
                            displayed_span(&buffer, &projection, mark.first, mark.lines);
                        (first, lines, change_color(mark.change))
                    })
                    // Conflicts come last so they draw over the additions Git
                    // sees in their lines.
                    .chain(s.conflicts.iter().map(|region| {
                        let (first, lines) = displayed_span(
                            &buffer,
                            &projection,
                            region.start - 1,
                            region.lines().len(),
                        );
                        (first, lines, theme::c().warning)
                    })),
            ));
        }
        if !s.diagnostics.is_empty() {
            root = root.child(diagnostic_ruler(
                rect,
                metrics,
                has_horizontal,
                fonts::editor().line_h,
                s.diagnostics
                    .iter()
                    .filter(|d| d.severity != Severity::Hint)
                    .map(|d| (projection.row_of(&buffer, d.range.start), d.severity)),
            ));
        }
        if has_vertical {
            root = root.child(vertical_scrollbar(
                pane,
                rect,
                metrics,
                scroll_y,
                has_horizontal,
                emphasized,
                state.clone(),
            ));
        }
        if has_horizontal {
            root = root.child(horizontal_scrollbar(
                pane,
                rect,
                code_left,
                metrics,
                scroll_x,
                has_vertical,
                emphasized,
                state.clone(),
            ));
        }
        if has_vertical && has_horizontal {
            root = root.child(scrollbar_corner(pane, rect));
        }
        if let Some((_, card)) = card
            && popup.is_none()
        {
            root = root.child(card);
        }
        if let Some((_, signature)) = signature {
            root = root.child(signature);
        }
        if let Some((_, popup)) = popup {
            root = root.child(popup);
        }
        if let Some(peek) = &s.language_peek {
            root = root.child(super::peek::render(peek, rect, state.clone()));
        }
    }

    let st_input = state.clone();
    root = root.on_input(move |ctx, input| {
        let input = input.to_string();
        st_input.send(Event::Text(input));
        ctx.stop_propagation();
    });

    let st_ime = state.clone();
    root = root.on_composition_update(move |_, value, cursor| {
        st_ime.send(Event::Composition {
            text: value.to_owned(),
            cursor,
        })
    });
    let st_ime_end = state.clone();
    root = root.on_composition_end(move |_| st_ime_end.send(Event::CompositionEnd));

    // Bound keys are dispatched from the keymap (`key_actions`) before they
    // reach the editor. Unbound shortcut chords must still not type text.
    root = root.on_key_down(move |ctx, event| {
        if event.state == KeyState::Down
            && (event.modifiers.ctrl() || event.modifiers.meta())
            && !event.modifiers.alt()
        {
            ctx.prevent_default();
        }
    });

    root.on_focus({
        let state = state.clone();
        move |_| state.send(Event::Focus)
    })
    .on_blur(move |_| state.send(Event::Blur))
}

/// Git change marks along the editor's right edge, independent of the
/// scrollbar, which draws over them. Lines map to the ruler as they do to
/// the scroll range, so a file that fits shows each mark beside its line.
pub(crate) fn overview_ruler(
    rect: UiRect,
    metrics: ScrollMetrics,
    has_horizontal: bool,
    row_height: f32,
    marks: impl IntoIterator<Item = (usize, usize, Color)>,
) -> Element {
    let corner = if has_horizontal { SCROLLBAR_SIZE } else { 0.0 };
    let lane = UiRect::new(
        rect.right - SCROLLBAR_SIZE + 1.0,
        rect.top,
        rect.right - SCROLLBAR_SIZE + 5.0,
        rect.bottom - corner,
    );
    let scale = lane.height() / metrics.content_h.max(metrics.viewport_h);
    let mut ruler = group(lane);
    let mut last: Option<(f32, f32, Color)> = None;
    let flush = |ruler: Element, (top, bottom, color): (f32, f32, Color)| {
        ruler.child(panel(
            UiRect::new(lane.left, top, lane.right, bottom),
            VisualStyle::filled(color),
        ))
    };
    for (first, lines, color) in marks {
        let top = (lane.top + first as f32 * row_height * scale).min(lane.bottom - MIN_MARK_LENGTH);
        let bottom =
            (lane.top + (first + lines) as f32 * row_height * scale).max(top + MIN_MARK_LENGTH);
        // Marks closer than a pixel draw as one, so huge files stay cheap.
        match &mut last {
            Some((_, end, previous)) if *previous == color && top <= *end + 1.0 => {
                *end = end.max(bottom)
            }
            _ => {
                if let Some(previous) = last.replace((top, bottom, color)) {
                    ruler = flush(ruler, previous);
                }
            }
        }
    }
    if let Some(previous) = last {
        ruler = flush(ruler, previous);
    }
    ruler
}

fn displayed_span(
    buffer: &TextBuffer,
    map: &loog_editor_core::text::DisplayMap,
    first: usize,
    lines: usize,
) -> (usize, usize) {
    let start = map.row_of(
        buffer,
        buffer.line_start(first.min(buffer.line_count() - 1)),
    );
    let end = if first + lines < buffer.line_count() {
        map.row_of(buffer, buffer.line_start(first + lines))
    } else {
        map.len()
    };
    (start, end.saturating_sub(start).max(1))
}
pub(crate) fn change_color(change: LineChange) -> Color {
    match change {
        LineChange::Added => theme::c().diff.add_mark,
        LineChange::Modified => theme::c().diff.mod_mark,
        LineChange::Deleted => theme::c().diff.del_mark,
    }
}

/// The one-based lines of `regions` inside `visible` (zero-based), each with
/// its part of the conflict.
fn conflict_lines(
    regions: &[ConflictRegion],
    visible: &Range<usize>,
) -> Vec<(usize, ConflictPart)> {
    let mut lines = Vec::new();
    for region in regions {
        for line in region.lines() {
            if visible.contains(&(line - 1))
                && let Some(part) = region.part_of(line)
            {
                lines.push((line, part));
            }
        }
    }
    lines
}

/// What each action bar offers, in order.
const CONFLICT_ACTIONS: [(Resolution, &str); 3] = [
    (Resolution::Current, "Accept Current"),
    (Resolution::Incoming, "Accept Incoming"),
    (Resolution::Both, "Accept Both"),
];
const CONFLICT_ACTION_GAP: f32 = 4.0;

/// The buttons that keep one side of each conflict, on its opening line at the
/// right edge where the marker text rarely reaches.
fn conflict_actions(
    projection: &loog_editor_core::text::DisplayMap,
    buffer: &TextBuffer,
    regions: &[ConflictRegion],
    visible: &Range<usize>,
    rect: UiRect,
    code_top: f32,
    scroll_y: f32,
    pane: PaneId,
    state: &Binding,
) -> Element {
    let mut bar = group(rect);
    let widths: Vec<f32> = CONFLICT_ACTIONS
        .iter()
        .map(|(_, label)| action_button::width(label))
        .collect();
    for region in regions {
        if !visible.contains(&(region.start - 1)) {
            continue;
        }
        let line_h = fonts::editor().line_h;
        let top = code_top
            + projection.row_of(buffer, buffer.line_start(region.start - 1)) as f32 * line_h
            - scroll_y
            + 2.0;
        let mut right = rect.right - SCROLLBAR_SIZE - 8.0;
        for ((resolution, label), width) in CONFLICT_ACTIONS.iter().zip(&widths).rev() {
            let st = state.clone();
            let region = region.clone();
            let resolution = *resolution;
            bar = bar.child(action_button::render(
                UiRect::new(right - width, top, right, top + line_h - 4.0),
                format!("conflict-{}-{}-{label}", pane.get(), region.start),
                label,
                format!("{label} for the conflict at line {}", region.start),
                move || {
                    st.send(Event::ResolveConflict {
                        region: region.clone(),
                        resolution,
                    })
                },
            ));
            right -= width + CONFLICT_ACTION_GAP;
        }
    }
    bar
}

fn conflict_decoration(part: ConflictPart) -> gutter::GutterDecoration {
    match part {
        ConflictPart::Current => gutter::GutterDecoration::ConflictCurrent,
        ConflictPart::Incoming => gutter::GutterDecoration::ConflictIncoming,
        ConflictPart::Marker => gutter::GutterDecoration::ConflictMarker,
    }
}

fn conflict_row_color(part: ConflictPart) -> Color {
    match part {
        ConflictPart::Current => theme::c().diff.add_bg,
        ConflictPart::Incoming => theme::c().conflict.incoming_bg,
        ConflictPart::Marker => theme::c().conflict.marker_bg,
    }
}

fn severity_colour(severity: Severity) -> Color {
    match severity {
        Severity::Error => theme::c().error,
        Severity::Warning => theme::c().warning,
        Severity::Information | Severity::Hint => theme::c().accent,
    }
}

/// `spans` (which start at byte `start`) with the part inside `range` in `tint`.
fn tint_range(
    spans: Vec<(String, Color)>,
    start: usize,
    range: &Range<usize>,
    tint: Color,
) -> Vec<(String, Color)> {
    let mut out = Vec::with_capacity(spans.len() + 2);
    let mut at = start;
    for (text, color) in spans {
        let end = at + text.len();
        if end <= range.start || at >= range.end {
            out.push((text, color));
        } else {
            let from = range.start.saturating_sub(at).min(text.len());
            let to = (range.end - at).min(text.len());
            if text.is_char_boundary(from) && text.is_char_boundary(to) {
                for (part, colour) in [
                    (&text[..from], color),
                    (&text[from..to], tint),
                    (&text[to..], color),
                ] {
                    if !part.is_empty() {
                        out.push((part.to_owned(), colour));
                    }
                }
            } else {
                out.push((text, color));
            }
        }
        at = end;
    }
    out
}

/// Half the period and the height of a diagnostic's wavy underline.
const SQUIGGLE_STEP: f32 = 2.0;
const SQUIGGLE_HEIGHT: f32 = 2.5;

/// A wavy underline along the bottom of `r`, the way VS Code and IntelliJ mark
/// a problem; a straight bar reads as a link or a cursor. It is at least one
/// period wide so a one-character range still shows a wave.
fn squiggle(r: UiRect, colour: Color) -> Element {
    let right = r.right.max(r.left + 2.0 * SQUIGGLE_STEP);
    let low = r.bottom - 1.0;
    let high = low - SQUIGGLE_HEIGHT;
    let mut commands = vec![UiPathCommand::MoveTo(Point::new(r.left, low))];
    let mut x = r.left;
    let mut up = true;
    while x < right {
        x = (x + SQUIGGLE_STEP).min(right);
        commands.push(UiPathCommand::LineTo(Point::new(
            x,
            if up { high } else { low },
        )));
        up = !up;
    }
    path(
        UiRect::new(r.left, high - 1.0, right, low + 1.0),
        UiPath::new(commands),
        PathStyle {
            fill: None,
            fill_alpha: 0,
            stroke: Some(Stroke::new(colour, 1.0, 255)),
        },
    )
}

/// Where the text `range` starts, on screen: the part of it on its first line.
pub(crate) fn anchor_rect(
    buffer: &TextBuffer,
    range: &Range<usize>,
    (code_left, code_top, layout_right): (f32, f32, f32),
    (scroll_x, scroll_y): (f32, f32),
    viewport_w: f32,
) -> Option<UiRect> {
    let map = loog_editor_core::text::DisplayMap::new(buffer, None, &[]);
    anchor_rect_in(
        buffer,
        &map,
        range,
        (code_left, code_top, layout_right),
        (scroll_x, scroll_y),
        viewport_w,
    )
}

pub(crate) fn anchor_rect_in(
    buffer: &TextBuffer,
    map: &loog_editor_core::text::DisplayMap,
    range: &Range<usize>,
    (code_left, code_top, _layout_right): (f32, f32, f32),
    (scroll_x, scroll_y): (f32, f32),
    viewport_w: f32,
) -> Option<UiRect> {
    if range.start > buffer.len()
        || !buffer.text().is_char_boundary(range.start)
        || map.hidden(buffer, range.start)
    {
        return None;
    }
    let row = map.row(buffer, map.row_of(buffer, range.start));
    let end = range.end.min(row.range.end).max(range.start);
    let (x0, y) = projection::caret(
        buffer,
        map,
        loog_editor_core::text::Selection::caret(range.start),
        code_left,
        code_top,
        scroll_x,
        viewport_w,
    );
    let x1 = projection::caret(
        buffer,
        map,
        loog_editor_core::text::Selection {
            cursor: end,
            wrap_affinity: loog_editor_core::text::Assoc::Before,
            ..Default::default()
        },
        code_left,
        code_top,
        scroll_x,
        viewport_w,
    )
    .0;
    let left = x0 - scroll_x;
    let top = y - scroll_y;
    Some(UiRect::new(
        left,
        top,
        (x1 - scroll_x).max(left + 2.0),
        top + fonts::editor().line_h,
    ))
}

/// Diagnostic marks along the editor's right edge, beside the Git lane, so
/// problems off screen can be found from the scrollbar. A line's most serious
/// problem is drawn last, over the others.
fn diagnostic_ruler(
    rect: UiRect,
    metrics: ScrollMetrics,
    has_horizontal: bool,
    row_height: f32,
    marks: impl IntoIterator<Item = (usize, Severity)>,
) -> Element {
    let corner = if has_horizontal { SCROLLBAR_SIZE } else { 0.0 };
    let lane = UiRect::new(
        rect.right - 5.0,
        rect.top,
        rect.right - 1.0,
        rect.bottom - corner,
    );
    let scale = lane.height() / metrics.content_h.max(metrics.viewport_h);
    let mut marks: Vec<_> = marks.into_iter().collect();
    marks.sort_by_key(|&(line, severity)| (std::cmp::Reverse(severity), line));
    marks.dedup();
    let mut ruler = group(lane);
    for (line, severity) in marks {
        let top = (lane.top + line as f32 * row_height * scale).min(lane.bottom - MIN_MARK_LENGTH);
        let bottom = (lane.top + (line + 1) as f32 * row_height * scale).max(top + MIN_MARK_LENGTH);
        ruler = ruler.child(panel(
            UiRect::new(lane.left, top, lane.right, bottom),
            VisualStyle::filled(severity_colour(severity)),
        ));
    }
    ruler
}

const CARD_PAD: f32 = 12.0;
const CARD_ROW: f32 = 18.0;
const CARD_MAX_W: f32 = 640.0;

/// The gap between a hover card and the underline it explains.
const CARD_GAP: f32 = 4.0;

/// One line of a diagnostic message as drawn: how tall it is, whether it wrapped,
/// and whether the origin after the message goes on a row below it.
struct MessageRow {
    height: f32,
    wrapped: bool,
    origin_below: bool,
}

/// Where a `width` × `height` hover card for `anchor` opens: below the
/// underline, or above it when the space below is too short; starting at the
/// underline's left edge, or ending at its right edge when the space to the
/// right is too narrow. It stays inside `area` either way.
pub(crate) fn card_origin(anchor: UiRect, area: UiRect, width: f32, height: f32) -> (f32, f32) {
    let below = anchor.bottom + CARD_GAP;
    let above = anchor.top - CARD_GAP - height;
    let top = if below + height <= area.bottom || above < area.top {
        below
    } else {
        above
    };
    let left = if anchor.left + width <= area.right - CARD_GAP {
        anchor.left
    } else {
        anchor.right - width
    };
    let top = top.min(area.bottom - height).max(area.top);
    let left = left
        .min(area.right - width - CARD_GAP)
        .max(area.left + CARD_GAP);
    (left, top)
}

/// The hover card at `anchor`: the diagnostics there (each one's message beside
/// a mark of its severity, with the reporting tool and code faint after it, as
/// VS Code shows `rustc(E0425)`), then what the language servers say about the
/// text, as Markdown. It is placed by [`card_origin`]. `None` when there is
/// nothing to show.
#[cfg(test)]
pub(crate) fn info_card(
    found: &[&Diagnostic],
    docs: Option<&crate::ui::markdown::card::Document>,
    anchor: UiRect,
    area: UiRect,
) -> Option<(UiRect, Element)> {
    info_card_actions(found, docs, anchor, area, None, None)
}

fn info_card_actions(
    found: &[&Diagnostic],
    docs: Option<&crate::ui::markdown::card::Document>,
    anchor: UiRect,
    area: UiRect,
    fixes: Option<&crate::ui::editor_binding::QuickFixView>,
    binding: Option<Binding>,
) -> Option<(UiRect, Element)> {
    let docs = docs.filter(|blocks| !blocks.is_empty());
    if found.is_empty() && docs.is_none() {
        return None;
    }
    let mut found = found.to_vec();
    found.sort_by_key(|d| d.severity);
    let size = theme::UI_SIZE;
    let measure = |s: &str| crate::ui::tabs::measure(s, size, 400);
    let mark_w = 14.0;
    // One entry per diagnostic: its message lines and the faint origin.
    let entries: Vec<(Severity, Vec<&str>, Option<String>)> = found
        .iter()
        .map(|d| {
            let origin = match (&d.source, &d.code) {
                (Some(source), Some(code)) => Some(format!("{source}({code})")),
                (Some(source), None) => Some(source.clone()),
                (None, Some(code)) => Some(code.clone()),
                (None, None) => None,
            };
            let lines = d.message.lines().filter(|l| !l.trim().is_empty()).collect();
            (d.severity, lines, origin)
        })
        .collect();
    let mut natural = 0.0f32;
    for (_, lines, origin) in &entries {
        for (i, line) in lines.iter().enumerate() {
            let mut w = measure(line);
            if i + 1 == lines.len()
                && let Some(origin) = origin
            {
                w += 8.0 + measure(origin);
            }
            natural = natural.max(w);
        }
    }
    let fixes = fixes.filter(|v| found.iter().any(|d| d.range == v.range));
    if let Some(fixes) = fixes {
        for label in &fixes.labels {
            natural = natural.max(measure(label) + 20.0);
        }
    }
    let diagnostics_w = if entries.is_empty() {
        0.0
    } else {
        natural + mark_w
    };
    let max_width = CARD_MAX_W.min((area.width() - 8.0).max(80.0));
    let documentation = docs.map(|blocks| super::docs::layout(blocks, max_width - 2.0 * CARD_PAD));
    // Sized from the placeholder it would open as a short card below the text
    // and then jump above it once the real height is known.
    if documentation
        .as_ref()
        .is_some_and(|layout| !layout.measured)
    {
        return None;
    }
    let width = (diagnostics_w.max(documentation.as_ref().map_or(0.0, |l| l.width))
        + 2.0 * CARD_PAD)
        .min(max_width);
    // A message line wider than the card wraps onto more lines; when only the
    // origin after the last line would not fit, it takes a row of its own.
    let text_w = (width - 2.0 * CARD_PAD - mark_w).max(40.0);
    let message_style = theme::sans(theme::c().text_bright, size);
    let wrapped_style = message_style
        .wrap(lgui::prelude::TextWrap::Words)
        .vertical_align(text::TextVerticalAlign::Top);
    let rows: Vec<Vec<MessageRow>> = entries
        .iter()
        .map(|(_, lines, origin)| {
            lines
                .iter()
                .enumerate()
                .map(|(i, line)| {
                    let line_w = measure(line);
                    let origin_w = origin
                        .as_ref()
                        .filter(|_| i + 1 == lines.len())
                        .map_or(0.0, |origin| 8.0 + measure(origin));
                    if line_w > text_w {
                        let height = text::layout(&TextLayoutRequest::for_style(
                            line,
                            UiRect::new(0.0, 0.0, text_w, 10_000.0),
                            &wrapped_style,
                        ))
                        .map_or(CARD_ROW, |laid_out| laid_out.height.max(CARD_ROW));
                        MessageRow {
                            height,
                            wrapped: true,
                            origin_below: origin_w > 0.0,
                        }
                    } else {
                        MessageRow {
                            height: CARD_ROW,
                            wrapped: false,
                            origin_below: line_w + origin_w > text_w && origin_w > 0.0,
                        }
                    }
                })
                .collect()
        })
        .collect();
    let diagnostics_h = if entries.is_empty() {
        0.0
    } else {
        rows.iter()
            .map(|lines| {
                lines
                    .iter()
                    .map(|row| row.height + if row.origin_below { CARD_ROW } else { 0.0 })
                    .sum::<f32>()
                    .max(CARD_ROW)
            })
            .sum::<f32>()
            + (entries.len() - 1) as f32 * 9.0
    };
    let separator = if entries.is_empty() || documentation.is_none() {
        0.0
    } else {
        9.0
    };
    let docs_h = documentation.as_ref().map_or(0.0, |l| l.height);
    let fixes_h = fixes.map_or(0.0, |v| v.labels.len() as f32 * CARD_ROW + 9.0);
    let height = (diagnostics_h + separator + docs_h + fixes_h + 2.0 * CARD_PAD)
        .min((area.height() * 0.7).max(CARD_ROW * 3.0));
    let (left, top) = card_origin(anchor, area, width, height);
    let card_rect = UiRect::new(left, top, left + width, top + height);
    let body = UiRect::new(
        card_rect.left + CARD_PAD,
        card_rect.top + CARD_PAD,
        card_rect.right - CARD_PAD,
        card_rect.bottom - CARD_PAD,
    );
    let mut content = clip(body, 0.0, 0.0);
    // The selectable text of the card, in reading order: the diagnostics, then the docs.
    let mut texts: Vec<Element> = Vec::new();
    let mut y = body.top;
    for (index, (severity, lines, origin)) in entries.iter().enumerate() {
        if index > 0 {
            content = content.child(panel(
                UiRect::new(body.left, y + 4.0, body.right, y + 5.0),
                VisualStyle::filled(theme::c().border),
            ));
            y += 9.0;
        }
        let dot_top = y + (CARD_ROW - 8.0) / 2.0;
        content = content.child(panel(
            UiRect::new(body.left, dot_top, body.left + 8.0, dot_top + 8.0),
            VisualStyle::filled(severity_colour(*severity)).radius(2.0),
        ));
        let text_left = body.left + mark_w;
        for (i, line) in lines.iter().enumerate() {
            let row = &rows[index][i];
            texts.push(if row.wrapped {
                text(
                    UiRect::new(text_left, y, text_left + text_w, y + row.height),
                    (*line).to_owned(),
                    wrapped_style,
                )
                .into()
            } else {
                text(
                    UiRect::new(text_left, y, text_left + measure(line) + 6.0, y + CARD_ROW),
                    (*line).to_owned(),
                    message_style,
                )
                .into()
            });
            if i + 1 == lines.len()
                && let Some(origin) = origin
            {
                // After the line, or on a row of its own when the line leaves no room.
                let (after, origin_top) = if row.origin_below {
                    (text_left, y + row.height)
                } else {
                    (text_left + measure(line) + 8.0, y)
                };
                let r = UiRect::new(
                    after,
                    origin_top,
                    body.right.max(after),
                    origin_top + CARD_ROW,
                );
                let href = found[index].documentation_url.as_deref();
                if let Some(href) =
                    href.filter(|u| u.starts_with("https://") || u.starts_with("http://"))
                    && let Some(binding) = binding.as_ref()
                {
                    let send = binding.clone();
                    let url = href.to_owned();
                    content = content.child(
                        panel(r, VisualStyle::default())
                            .event_policy(EventPolicy {
                                focus: false,
                                ..EventPolicy::INTERACTIVE
                            })
                            .cursor(CursorIcon::Pointer)
                            .semantics(
                                Semantics::new(SemanticRole::Button)
                                    .name("Open diagnostic documentation"),
                            )
                            .on_pointer_down_with_button(|cx, _, _| cx.stop_propagation())
                            .on_click(move |cx: &mut lgui::core::UiEventContext| {
                                cx.stop_propagation();
                                send.send(Event::DiagnosticLink(url.clone()));
                            })
                            .child(text(
                                r,
                                origin.clone(),
                                theme::sans(theme::c().accent, size),
                            )),
                    );
                } else {
                    content = content.child(text(
                        r,
                        origin.clone(),
                        theme::sans(theme::c().text_faint, size),
                    ));
                }
            }
            y += row.height + if row.origin_below { CARD_ROW } else { 0.0 };
        }
        if lines.is_empty() {
            y += CARD_ROW;
        }
    }
    // Links and selection act the same in every card over the code.
    let interaction = binding.as_ref().map(|binding| {
        let (links, selections) = (binding.clone(), binding.clone());
        super::docs::Actions {
            open_link: std::sync::Arc::new(move |url| links.send(Event::OpenLink(url))),
            select: std::sync::Arc::new(move |text| selections.send(Event::HoverSelect(text))),
        }
        .interaction()
    });
    let docs_interaction = interaction.clone();
    let on_select = interaction.and_then(|i| i.on_select);
    if let Some(fixes) = fixes
        && let Some(binding) = binding
    {
        y += 9.0;
        for (index, label) in fixes.labels.iter().enumerate() {
            let r = UiRect::new(body.left, y, body.right, y + CARD_ROW);
            let send = binding.clone();
            let revision = fixes.revision;
            content = content.child(
                panel(r, VisualStyle::default())
                    .event_policy(EventPolicy {
                        focus: false,
                        ..EventPolicy::INTERACTIVE
                    })
                    .cursor(CursorIcon::Pointer)
                    .semantics(
                        Semantics::new(SemanticRole::Button).name(format!("Quick fix: {label}")),
                    )
                    .on_pointer_down_with_button(|cx, _, _| cx.stop_propagation())
                    .on_click(move |cx: &mut lgui::core::UiEventContext| {
                        cx.stop_propagation();
                        send.send(Event::QuickFix { revision, index });
                    })
                    .child(text(r, label.clone(), theme::sans(theme::c().accent, size))),
            );
            y += CARD_ROW;
        }
    }
    if let Some(layout) = &documentation {
        if separator > 0.0 {
            content = content.child(panel(
                UiRect::new(body.left, y + 4.0, body.right, y + 5.0),
                VisualStyle::filled(theme::c().border),
            ));
            y += separator;
        }
        let documentation = super::docs::render(
            layout,
            body.left,
            y,
            docs_interaction.as_ref(),
            (body.bottom - y).max(1.0),
        );
        content = content.child(documentation);
    }
    // Metadata uses its own text selection; the shared reader controls the
    // documentation selection, links and copy independently.
    let mut layers = clip(body, 0.0, 0.0);
    match on_select {
        Some(on_select) => {
            layers = layers.child(
                selection_region(body)
                    .selection_color(theme::c().accent)
                    .on_select(move |_cx, text| on_select(text))
                    .children(texts),
            );
        }
        None => layers = layers.children(texts),
    }
    let layers = layers.child(content);
    Some((
        card_rect,
        theme::bordered(card_rect, theme::c().surface, theme::c().border, 4.0, 1.0)
            .key("language-hover-card")
            .child(layers),
    ))
}

pub(crate) fn vertical_scrollbar(
    pane: PaneId,
    rect: UiRect,
    metrics: ScrollMetrics,
    scroll: f32,
    has_horizontal: bool,
    emphasized: bool,
    state: Binding,
) -> Element {
    let corner = if has_horizontal { SCROLLBAR_SIZE } else { 0.0 };
    let track = UiRect::new(
        rect.right - SCROLLBAR_SIZE,
        rect.top,
        rect.right,
        rect.bottom - corner,
    );
    let geometry = thumb_geometry(
        track.top,
        track.height(),
        metrics.viewport_h,
        metrics.content_h,
        scroll,
    );
    let travel = track.height() - geometry.length;
    let alpha = if emphasized { 0xb8 } else { 0x68 };

    let st_track_down = state.clone();
    let st_track_move = state.clone();
    let st_track_up = state.clone();
    let track_hit = scrollbar_track(track)
        .key(format!("editor-vertical-scrollbar-track-{}", pane.get()))
        .event_policy(EventPolicy::INTERACTIVE)
        .on_pointer_down(move |_cx, pointer| {
            let offset = geometry.length / 2.0;
            let thumb_start = (pointer.point.y - track.top - offset).clamp(0.0, travel);
            let next = scroll_from_thumb(thumb_start, travel, metrics.max_y);
            st_track_down.send(Event::ScrollbarDown {
                vertical: true,
                offset,
                position: Some(next),
            });
        })
        .on_pointer_drag(move |_cx, pointer| {
            st_track_move.send(Event::ScrollbarDrag {
                vertical: true,
                pointer: pointer.point.y,
                start: track.top,
                travel,
                max: metrics.max_y,
            });
        })
        .on_pointer_up(move |_cx, _pointer| {
            st_track_up.send(Event::ScrollbarUp { vertical: true });
        });

    let thumb_rect = UiRect::new(
        track.left + SCROLLBAR_INSET,
        geometry.start,
        track.right - SCROLLBAR_INSET,
        geometry.start + geometry.length,
    );
    let st_thumb_down = state.clone();
    let st_thumb_move = state.clone();
    let st_thumb_up = state;
    let thumb = scrollbar_thumb(thumb_rect, alpha)
        .key(format!("editor-vertical-scrollbar-thumb-{}", pane.get()))
        .event_policy(EventPolicy::INTERACTIVE)
        .on_pointer_down(move |_cx, pointer| {
            st_thumb_down.send(Event::ScrollbarDown {
                vertical: true,
                offset: pointer.point.y - geometry.start,
                position: None,
            });
        })
        .on_pointer_drag(move |_cx, pointer| {
            st_thumb_move.send(Event::ScrollbarDrag {
                vertical: true,
                pointer: pointer.point.y,
                start: track.top,
                travel,
                max: metrics.max_y,
            });
        })
        .on_pointer_up(move |_cx, _pointer| {
            st_thumb_up.send(Event::ScrollbarUp { vertical: true });
        });

    group(track)
        .key(format!("editor-vertical-scrollbar-{}", pane.get()))
        .child(track_hit)
        .child(thumb)
}

/// The square where both scrollbars meet. It keeps the arrow cursor over the
/// content that runs beneath it and swallows clicks there.
pub(crate) fn scrollbar_corner(pane: PaneId, rect: UiRect) -> Element {
    scrollbar_track(UiRect::new(
        rect.right - SCROLLBAR_SIZE,
        rect.bottom - SCROLLBAR_SIZE,
        rect.right,
        rect.bottom,
    ))
    .key(format!("editor-scrollbar-corner-{}", pane.get()))
    .event_policy(EventPolicy::INTERACTIVE)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn horizontal_scrollbar(
    pane: PaneId,
    rect: UiRect,
    code_left: f32,
    metrics: ScrollMetrics,
    scroll: f32,
    has_vertical: bool,
    emphasized: bool,
    state: Binding,
) -> Element {
    let corner = if has_vertical { SCROLLBAR_SIZE } else { 0.0 };
    let track = UiRect::new(
        code_left,
        rect.bottom - SCROLLBAR_SIZE,
        rect.right - corner,
        rect.bottom,
    );
    let geometry = thumb_geometry(
        track.left,
        track.width(),
        metrics.viewport_w,
        metrics.content_w,
        scroll,
    );
    let travel = track.width() - geometry.length;
    let alpha = if emphasized { 0xb8 } else { 0x68 };

    let st_track_down = state.clone();
    let st_track_move = state.clone();
    let st_track_up = state.clone();
    let track_hit = scrollbar_track(track)
        .key(format!("editor-horizontal-scrollbar-track-{}", pane.get()))
        .event_policy(EventPolicy::INTERACTIVE)
        .on_pointer_down(move |_cx, pointer| {
            let offset = geometry.length / 2.0;
            let thumb_start = (pointer.point.x - track.left - offset).clamp(0.0, travel);
            let next = scroll_from_thumb(thumb_start, travel, metrics.max_x);
            st_track_down.send(Event::ScrollbarDown {
                vertical: false,
                offset,
                position: Some(next),
            });
        })
        .on_pointer_drag(move |_cx, pointer| {
            st_track_move.send(Event::ScrollbarDrag {
                vertical: false,
                pointer: pointer.point.x,
                start: track.left,
                travel,
                max: metrics.max_x,
            });
        })
        .on_pointer_up(move |_cx, _pointer| {
            st_track_up.send(Event::ScrollbarUp { vertical: false });
        });

    let thumb_rect = UiRect::new(
        geometry.start,
        track.top + SCROLLBAR_INSET,
        geometry.start + geometry.length,
        track.bottom - SCROLLBAR_INSET,
    );
    let st_thumb_down = state.clone();
    let st_thumb_move = state.clone();
    let st_thumb_up = state;
    let thumb = scrollbar_thumb(thumb_rect, alpha)
        .key(format!("editor-horizontal-scrollbar-thumb-{}", pane.get()))
        .event_policy(EventPolicy::INTERACTIVE)
        .on_pointer_down(move |_cx, pointer| {
            st_thumb_down.send(Event::ScrollbarDown {
                vertical: false,
                offset: pointer.point.x - geometry.start,
                position: None,
            });
        })
        .on_pointer_drag(move |_cx, pointer| {
            st_thumb_move.send(Event::ScrollbarDrag {
                vertical: false,
                pointer: pointer.point.x,
                start: track.left,
                travel,
                max: metrics.max_x,
            });
        })
        .on_pointer_up(move |_cx, _pointer| {
            st_thumb_up.send(Event::ScrollbarUp { vertical: false });
        });

    group(track)
        .key(format!("editor-horizontal-scrollbar-{}", pane.get()))
        .child(track_hit)
        .child(thumb)
}

/// Keep long-line shaping bounded horizontally as well as vertically.
pub(crate) fn line_window(
    buffer: &TextBuffer,
    line: usize,
    scroll_x: f32,
    width: f32,
) -> Range<usize> {
    let char_w = fonts::editor().char_w.max(1.0);
    let left = (scroll_x / char_w).floor() as usize;
    let right = ((scroll_x + width) / char_w).ceil() as usize;
    // Stable column bands let small scrolls reuse the same shaped fragment.
    // Bounds depend on the viewport, never on file size or a byte threshold.
    let left = left.saturating_sub(16) / 32 * 32;
    let right = right.saturating_add(16).div_ceil(32) * 32;
    buffer
        .display_column_range(line, left, right)
        .unwrap_or_else(|| {
            let end = buffer.line_end(line);
            end..end
        })
}

pub(crate) fn document_caret(
    buffer: &TextBuffer,
    line: usize,
    column: usize,
    left: f32,
    top: f32,
    right: f32,
    viewport: (f32, f32),
) -> f32 {
    let _tabs = TabScope::new(buffer.options().tab_width);
    let offset = buffer.offset_at_char_column(line, column);
    let visual = buffer.display_column(offset);
    let mut range = line_window(buffer, line, viewport.0, viewport.1);
    // Revealing an off-screen caret still uses a bounded fragment. On-screen
    // geometry uses exactly the same fragment as paint and pointer hit testing.
    if offset < range.start || offset > range.end {
        range = line_window(
            buffer,
            line,
            (visual as f32 * fonts::editor().char_w - viewport.1 / 2.0).max(0.0),
            viewport.1,
        );
    }
    let initial = buffer.display_column(range.start);
    let origin = left + initial as f32 * fonts::editor().char_w;
    let drawn = super::hints::drawn(buffer.snapshot_identity());
    let line_hints = drawn
        .as_ref()
        .map(|hints| super::hints::in_window(hints, buffer.text(), range.clone()))
        .unwrap_or_default();
    let layout = layout_line_hinted(
        &buffer.text()[range.clone()],
        &line_hints,
        origin,
        top,
        right,
        initial,
    );
    caret_x(
        layout.as_ref(),
        buffer.text()[range.start..offset].chars().count(),
    )
    .unwrap_or(left + visual as f32 * fonts::editor().char_w)
}

pub(crate) fn scroll_metrics(rect: UiRect, code_left: f32, buffer: &TextBuffer) -> ScrollMetrics {
    let mut viewport_w = (rect.right - code_left).max(1.0);
    let mut viewport_h = rect.height().max(1.0);
    let natural_w =
        buffer.max_line_display_width() as f32 * fonts::editor().char_w + TRAILING_CODE_SPACE;
    let content_h =
        (buffer.line_count() as f32 * fonts::editor().line_h).max(fonts::editor().line_h);
    // A scrollbar may make the other axis overflow; reserve both before revealing the caret.
    for _ in 0..2 {
        viewport_w = (rect.right
            - code_left
            - if content_h > viewport_h {
                SCROLLBAR_SIZE
            } else {
                0.0
            })
        .max(1.0);
        viewport_h = (rect.height()
            - if natural_w > viewport_w {
                SCROLLBAR_SIZE
            } else {
                0.0
            })
        .max(1.0);
    }
    let content_w = natural_w.max(viewport_w);
    ScrollMetrics {
        viewport_w,
        viewport_h,
        content_w,
        content_h,
        max_x: (content_w - viewport_w).max(0.0),
        max_y: (content_h - viewport_h).max(0.0),
    }
}

pub(crate) fn visual_columns(line: &str) -> usize {
    line.graphemes(true).fold(0, |column, g| {
        column
            + if g == "\t" {
                tab_size() - column % tab_size()
            } else {
                UnicodeWidthStr::width(g)
            }
    })
}

pub(crate) fn visible_line_range(
    scroll_y: f32,
    viewport_h: f32,
    line_count: usize,
) -> Range<usize> {
    let first = ((scroll_y / fonts::editor().line_h).floor() as usize).saturating_sub(1);
    let last =
        (((scroll_y + viewport_h) / fonts::editor().line_h).ceil() as usize + 1).min(line_count);
    first.min(last)..last
}

pub(crate) fn thumb_geometry(
    track_start: f32,
    track_length: f32,
    viewport_length: f32,
    content_length: f32,
    scroll: f32,
) -> ThumbGeometry {
    let length = (track_length * viewport_length / content_length.max(1.0))
        .max(MIN_THUMB_LENGTH)
        .min(track_length);
    let travel = (track_length - length).max(0.0);
    let max_scroll = (content_length - viewport_length).max(0.0);
    let position = if max_scroll > 0.0 {
        travel * (scroll.clamp(0.0, max_scroll) / max_scroll)
    } else {
        0.0
    };
    ThumbGeometry {
        start: track_start + position,
        length,
    }
}

pub(crate) fn scroll_from_thumb(thumb_start: f32, travel: f32, max_scroll: f32) -> f32 {
    if travel <= 0.0 {
        0.0
    } else {
        (thumb_start / travel * max_scroll).clamp(0.0, max_scroll)
    }
}

/// Layout uses expanded tabs, while all buffer positions remain source character indices.
#[derive(Clone)]
pub(crate) struct LineLayout {
    pub(crate) layout: std::sync::Arc<TextLayout>,
    pub(crate) source_to_display: std::sync::Arc<[usize]>,
    pub(crate) width: f32,
    origin: (f32, f32),
    /// Where inlay hints sit in the layout: display index and length.
    pub(crate) hints: std::sync::Arc<[(usize, usize)]>,
}

impl LineLayout {
    /// The x position where the `n`th hint of the line starts.
    pub(crate) fn hint_x(&self, n: usize) -> Option<f32> {
        let (start, _) = *self.hints.get(n)?;
        self.layout
            .caret_rect(start)
            .map(|rect| rect.left + self.origin.0)
    }
}

type LayoutKey = (u32, i32, usize, u32, usize);
struct LayoutEntry {
    layout: LineLayout,
    used: u64,
    bytes: usize,
}
#[derive(Default)]
struct LayoutCache {
    entries: HashMap<LayoutKey, HashMap<String, LayoutEntry>>,
    len: usize,
    clock: u64,
    bytes: usize,
    hits: usize,
    misses: usize,
    evictions: usize,
}
const LAYOUT_BUDGET: usize = 8 * 1024 * 1024;
impl LayoutCache {
    fn trim(&mut self) {
        while self.bytes > LAYOUT_BUDGET || self.len > 2048 {
            let Some((key, line)) = self
                .entries
                .iter()
                .flat_map(|(key, lines)| {
                    lines
                        .iter()
                        .map(move |(line, entry)| (*key, line, entry.used))
                })
                .min_by_key(|(_, _, used)| *used)
                .map(|(key, line, _)| (key, line.clone()))
            else {
                break;
            };
            let lines = self.entries.get_mut(&key).unwrap();
            self.bytes -= lines.remove(&line).unwrap().bytes;
            self.len -= 1;
            self.evictions += 1;
            if lines.is_empty() {
                self.entries.remove(&key);
            }
        }
    }
}
thread_local! { static TAB_SIZE: std::cell::Cell<usize> = const { std::cell::Cell::new(4) }; }
pub(super) struct TabScope(usize);
impl TabScope {
    pub(super) fn new(width: usize) -> Self {
        Self(TAB_SIZE.with(|s| s.replace(width.clamp(1, 16))))
    }
}
impl Drop for TabScope {
    fn drop(&mut self) {
        TAB_SIZE.with(|s| s.set(self.0));
    }
}
fn tab_size() -> usize {
    TAB_SIZE.with(|s| s.get())
}
thread_local! {
    static LINE_LAYOUTS: RefCell<LayoutCache> = RefCell::new(LayoutCache::default());
}
#[cfg(test)]
pub(crate) fn layout_cache_stats() -> (usize, usize, usize, usize, usize) {
    LINE_LAYOUTS.with(|cache| {
        let cache = cache.borrow();
        (
            cache.hits,
            cache.misses,
            cache.evictions,
            cache.bytes,
            cache.len,
        )
    })
}

#[cfg(test)]
pub(crate) fn clear_layout_cache() {
    LINE_LAYOUTS.with(|cache| *cache.borrow_mut() = LayoutCache::default());
}
/// Expand tabs at four-column stops for editor and diff display.
pub(crate) fn expand_tabs(line: &str, initial_column: usize) -> (String, Vec<usize>, usize) {
    let mut result = String::new();
    let mut mapping = vec![0];
    let mut column = initial_column;
    let mut display_chars = 0;
    for grapheme in line.graphemes(true) {
        if grapheme == "\t" {
            let spaces = tab_size() - column % tab_size();
            result.extend(std::iter::repeat_n(' ', spaces));
            column += spaces;
            display_chars += spaces;
            mapping.push(display_chars);
        } else {
            for ch in grapheme.chars() {
                result.push(ch);
                display_chars += 1;
                mapping.push(display_chars);
            }
            column += UnicodeWidthStr::width(grapheme);
        }
    }
    (result, mapping, column)
}
pub(crate) fn layout_line(line: &str, left: f32, top: f32, right: f32) -> Option<LineLayout> {
    layout_line_at_column(line, left, top, right, 0)
}

pub(crate) fn layout_line_at_column(
    line: &str,
    left: f32,
    top: f32,
    right: f32,
    column: usize,
) -> Option<LineLayout> {
    layout_line_hinted(line, &[], left, top, right, column)
}

/// Like `layout_line_at_column`, with inlay hints (character index in `line`, text)
/// laid out in the line: what follows each hint moves right by its width.
pub(crate) fn layout_line_hinted(
    line: &str,
    hints: &[(usize, &str)],
    left: f32,
    top: f32,
    _right: f32,
    column: usize,
) -> Option<LineLayout> {
    let decorated = (!hints.is_empty()).then(|| super::hints::decorate(line, hints));
    // A line with hints is a different line: the cache tells them apart.
    let cache_text: std::borrow::Cow<str> = match &decorated {
        None => std::borrow::Cow::Borrowed(line),
        Some(made) => {
            let signature: String = hints
                .iter()
                .map(|(at, text)| format!("{at}:{};", text.chars().count()))
                .collect();
            std::borrow::Cow::Owned(format!("\u{1}{signature}\u{1}{}", made.text))
        }
    };
    let line = cache_text.as_ref();
    let font = fonts::editor();
    let key = (
        font.size.to_bits(),
        font.weight,
        font.families.as_ptr() as usize,
        font.line_h.to_bits(),
        tab_size() * 32 + column % tab_size(),
    );
    if let Some(mut layout) = LINE_LAYOUTS.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.clock += 1;
        let used = cache.clock;
        let found = cache
            .entries
            .get_mut(&key)
            .and_then(|entries| entries.get_mut(line))
            .map(|entry| {
                entry.used = used;
                entry.layout.clone()
            });
        if found.is_some() {
            cache.hits += 1;
        } else {
            cache.misses += 1;
        }
        found
    }) {
        layout.origin = (left, top);
        return Some(layout);
    }
    let (display, source_to_display, hint_ranges) = match &decorated {
        None => {
            let (display, mapping, _) = expand_tabs(line, column);
            (display, mapping, Vec::new())
        }
        Some(made) => {
            let (display, mapping, _) = expand_tabs(&made.text, column);
            let source: Vec<usize> = made.positions.iter().map(|at| mapping[*at]).collect();
            let ranges = made
                .hints
                .iter()
                .map(|(start, length)| (mapping[*start], mapping[start + length] - mapping[*start]))
                .collect();
            (display, source, ranges)
        }
    };
    let bounds = UiRect::new(0.0, 0.0, 16_777_216.0, font.line_h);
    let mut request = TextLayoutRequest::single_line(&display, bounds, font.size, font.weight);
    request.font_families = font.families;
    let layout = text::layout(&request).map(|layout| LineLayout {
        width: layout.width,
        layout: std::sync::Arc::new(layout),
        source_to_display: source_to_display.into(),
        origin: (0.0, 0.0),
        hints: hint_ranges.into(),
    });
    if line.len() <= 8192
        && let Some(layout) = &layout
    {
        LINE_LAYOUTS.with(|cache| {
            let mut cache = cache.borrow_mut();
            cache.clock += 1;
            let used = cache.clock;
            let bytes = line.len()
                + layout.source_to_display.len()
                    * (std::mem::size_of::<usize>() + 2 * std::mem::size_of::<UiRect>())
                + std::mem::size_of_val(layout.layout.clusters())
                + std::mem::size_of_val(layout.layout.lines())
                + std::mem::size_of::<LayoutEntry>();
            cache.entries.entry(key).or_default().insert(
                line.to_owned(),
                LayoutEntry {
                    layout: layout.clone(),
                    used,
                    bytes,
                },
            );
            cache.len += 1;
            cache.bytes += bytes;
            cache.trim();
        });
    }
    layout.map(|mut layout| {
        layout.origin = (left, top);
        layout
    })
}
pub(crate) fn caret_x(layout: Option<&LineLayout>, char_index: usize) -> Option<f32> {
    let layout = layout?;
    let index = *layout
        .source_to_display
        .get(char_index)
        .or_else(|| layout.source_to_display.last())?;
    layout
        .layout
        .caret_rect(index)
        .map(|rect| rect.left + layout.origin.0)
}
pub(crate) fn hit_column(layout: &LineLayout, x: f32, y: f32) -> usize {
    let index = layout
        .layout
        .hit_test(x - layout.origin.0, y - layout.origin.1)
        .index;
    layout
        .source_to_display
        .iter()
        .enumerate()
        .min_by_key(|(_, i)| i.abs_diff(index))
        .map_or(0, |(i, _)| i)
}
/// Display columns before the `chars`-th character, with tabs at four columns.
pub(crate) fn display_columns(line: &str, chars: usize) -> usize {
    line.chars().take(chars).fold(0, |column, ch| {
        column
            + if ch == '\t' {
                tab_size() - column % tab_size()
            } else {
                UnicodeWidthChar::width(ch).unwrap_or(0)
            }
    })
}
pub(crate) fn fallback_hit_column(line: &str, x: f32) -> usize {
    let mut column = 0;
    let mut chars = 0;
    for g in line.graphemes(true) {
        let width = if g == "\t" {
            tab_size() - column % tab_size()
        } else {
            UnicodeWidthStr::width(g)
        };
        if x < (column as f32 + width as f32 / 2.0) * fonts::editor().char_w {
            return chars;
        }
        column += width;
        chars += g.chars().count();
    }
    chars
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn cursor_offset_from_point(
    buffer: &TextBuffer,
    rect: UiRect,
    code_left: f32,
    metrics: ScrollMetrics,
    scroll_x: f32,
    scroll_y: f32,
    point_x: f32,
    point_y: f32,
) -> usize {
    let _tabs = TabScope::new(buffer.options().tab_width);
    let line = line_index_from_point(point_y, rect.top, scroll_y, buffer.line_count());
    let content_x = point_x + scroll_x;
    let range = line_window(buffer, line, scroll_x, metrics.viewport_w);
    let initial = buffer.display_column(range.start);
    let left = code_left + initial as f32 * fonts::editor().char_w;
    let line_text = &buffer.text()[range.clone()];
    let content_y = point_y + scroll_y;
    let line_top = rect.top + line as f32 * fonts::editor().line_h;
    let drawn = super::hints::drawn(buffer.snapshot_identity());
    let line_hints = drawn
        .as_ref()
        .map(|hints| super::hints::in_window(hints, buffer.text(), range.clone()))
        .unwrap_or_default();
    let char_index = layout_line_hinted(
        line_text,
        &line_hints,
        left,
        line_top,
        code_left + metrics.content_w,
        initial,
    )
    .map(|layout| hit_column(&layout, content_x, content_y))
    .unwrap_or_else(|| fallback_hit_column(line_text, content_x - left));
    buffer.grapheme_floor(
        range.start
            + line_text
                .char_indices()
                .nth(char_index)
                .map(|(offset, _)| offset)
                .unwrap_or(line_text.len()),
    )
}

pub(crate) fn line_index_from_point(
    point_y: f32,
    viewport_top: f32,
    scroll_y: f32,
    line_count: usize,
) -> usize {
    if line_count == 0 {
        return 0;
    }
    (((point_y - viewport_top + scroll_y) / fonts::editor().line_h)
        .floor()
        .max(0.0) as usize)
        .min(line_count - 1)
}

pub(crate) fn ime_cursor_rect(app: &ViewState, rect: UiRect) -> UiRect {
    let Some(document) = &app.document else {
        return rect;
    };
    let buffer = document.editor();
    let (sx, sy) = document.scroll;
    let left =
        rect.left + fonts::editor().gutter_w_for_lines(buffer.line_count()) + theme::CODE_PAD;
    let metrics = projection::metrics(rect, left, &buffer, &document.projection);
    let (mut x, top) = projection::caret(
        &buffer,
        &document.projection,
        document.selection,
        left,
        rect.top,
        sx,
        metrics.viewport_w,
    );
    x -= sx;
    let top = top - sy;
    let pane = app.pane;
    if let Some(session) = app.preedit_for(pane, document.id) {
        let index = session
            .cursor
            .as_ref()
            .map_or(session.text.len(), |r| r.start)
            .min(session.text.len());
        let prefix = session.text.get(..index).unwrap_or("");
        x += layout_line(prefix, 0.0, 0.0, 100000.0).map_or(0.0, |l| l.width);
    }
    let x = x.clamp(left, rect.right.max(left));
    let top = top.clamp(
        rect.top,
        (rect.bottom - fonts::editor().line_h).max(rect.top),
    );
    UiRect::new(x, top, x + 2.0, top + fonts::editor().line_h)
}

/// Lines in one page of an editor of this size, for page movements.
pub(crate) fn page_lines(rect: UiRect) -> usize {
    (rect.height() / fonts::editor().line_h).floor().max(1.0) as usize
}

#[cfg(test)]
mod card_placement_tests {
    use super::*;

    const AREA: UiRect = UiRect {
        left: 0.0,
        top: 0.0,
        right: 800.0,
        bottom: 600.0,
    };

    #[test]
    fn a_link_tints_only_its_own_bytes_across_span_boundaries() {
        let (a, b) = (Color(0x010203), Color(0x040506));
        let tint = Color(0x090909);
        let spans = vec![("let ".to_owned(), a), ("héllo_world".to_owned(), b)];
        // "héllo" is bytes 4..10 of the line, which starts at byte 100.
        let out = tint_range(spans, 100, &(104..110), tint);
        assert_eq!(
            out,
            vec![
                ("let ".to_owned(), a),
                ("héllo".to_owned(), tint),
                ("_world".to_owned(), b)
            ]
        );
        let untouched = tint_range(vec![("abc".to_owned(), a)], 0, &(5..8), tint);
        assert_eq!(untouched, vec![("abc".to_owned(), a)]);
    }

    #[test]
    fn a_card_opens_below_and_to_the_right_of_its_underline() {
        let anchor = UiRect::new(100.0, 200.0, 140.0, 220.0);
        assert_eq!(card_origin(anchor, AREA, 300.0, 80.0), (100.0, 224.0));
    }

    #[test]
    fn a_card_opens_above_when_the_space_below_is_too_short() {
        let anchor = UiRect::new(100.0, 540.0, 140.0, 560.0);
        assert_eq!(card_origin(anchor, AREA, 300.0, 80.0), (100.0, 456.0));
    }

    #[test]
    fn a_card_ends_at_the_underline_when_the_space_to_the_right_is_too_narrow() {
        let anchor = UiRect::new(600.0, 200.0, 700.0, 220.0);
        assert_eq!(card_origin(anchor, AREA, 300.0, 80.0), (400.0, 224.0));
    }

    #[test]
    fn a_card_that_fits_neither_way_stays_inside_the_editor() {
        let anchor = UiRect::new(10.0, 290.0, 30.0, 310.0);
        let (left, top) = card_origin(anchor, AREA, 900.0, 700.0);
        assert_eq!(left, CARD_GAP.max(AREA.right - 900.0 - CARD_GAP));
        assert_eq!(top, AREA.top);
    }
}
