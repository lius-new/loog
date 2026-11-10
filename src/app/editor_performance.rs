//! Opt-in comparisons using real Skia layout; input text is never logged.
use super::{context::AppContext, state::AppState};
use crate::{
    app::{runtime::AppRuntime, syntax::Highlighter, test_support::UiTasks},
    features::syntax::{Source, Window},
};
use crate::{features::git::GitStoreSnapshot, ui::editor::view};
use lgui::{
    application::{AppView, ApplicationContext},
    core::{UiId, UiScale},
    prelude::UiRect,
    session::UiSession,
};
use std::{
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};

fn wait_ready(state: &AppContext, queue: &UiTasks, rect: UiRect, verify: bool) {
    let app = state.get();
    let pane = app.workspace.active_pane();
    let id = app.workspace.pane_document(pane).unwrap();
    let doc = app.workspace.documents.get(&id).unwrap();
    let left = rect.left
        + crate::ui::fonts::editor().gutter_w_for_lines(doc.buffer.line_count())
        + crate::ui::theme::CODE_PAD;
    let metrics = view::scroll_metrics(rect, left, &doc.buffer);
    let (x, y) = app.workspace.active_scroll();
    let x = x.clamp(0.0, metrics.max_x);
    let y = y.clamp(0.0, metrics.max_y);
    let windows: Vec<_> = view::visible_line_range(y, rect.height(), doc.buffer.line_count())
        .map(|line| Window {
            line: doc.buffer.line_range(line),
            range: view::line_window(&doc.buffer, line, x, metrics.viewport_w),
        })
        .collect();
    let worker = state.services().unwrap().highlighter().unwrap().worker;
    let limit = Instant::now() + Duration::from_secs(20);
    loop {
        let ready = worker.query(
            pane,
            doc.stamp(id),
            Source::new(&doc.buffer),
            doc.meta.lang.clone(),
            windows.clone(),
        );
        if ready.len() == windows.len() && ready.iter().all(|tokens| tokens.complete) {
            // Check the colours against a second, independent parse of the same
            // text, including windows deep inside minified strings and comments.
            if verify {
                let mut reference = crate::infrastructure::language_packs::index();
                let expected = reference
                    .prepare(
                        id,
                        Source::new(&doc.buffer),
                        doc.meta.lang.clone(),
                        &windows,
                        &AtomicBool::new(false),
                    )
                    .expect("the reference parse is not cancelled");
                for ((actual, expected), window) in ready.iter().zip(&expected).zip(&windows) {
                    assert_eq!(
                        merged(actual.spans.as_ref(), &window.range),
                        merged(expected.spans.as_ref(), &window.range)
                    );
                }
            }
            break;
        }
        assert!(Instant::now() < limit, "highlighting did not complete");
        std::thread::sleep(Duration::from_millis(1));
    }
    queue.run_all();
}

/// Spans cut to `range` with equal neighbours joined: a window answered from
/// several cache pages can split a run that one query returns whole, and a token
/// that crosses the window keeps its full range.
fn merged(
    spans: &[crate::features::syntax::TokenSpan],
    range: &std::ops::Range<usize>,
) -> Vec<crate::features::syntax::TokenSpan> {
    let mut out: Vec<crate::features::syntax::TokenSpan> = Vec::new();
    for span in spans {
        let (start, end) = (
            span.range.start.max(range.start),
            span.range.end.min(range.end),
        );
        if start >= end {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.class == span.class && last.range.end == start => {
                last.range.end = end;
            }
            _ => out.push(crate::features::syntax::TokenSpan {
                range: start..end,
                class: span.class,
            }),
        }
    }
    out
}

#[test]
#[ignore = "navigation rendering regression; optional local input stays in memory"]
fn editor_navigation_highlight_continuity() {
    assert!(
        std::env::var_os("LOOG_TEST_LANGUAGE_PACKS").is_some(),
        "provide prebuilt language packages through LOOG_TEST_LANGUAGE_PACKS for this syntax benchmark"
    );
    use crate::features::syntax::TokenSpan;
    use std::{collections::HashMap, path::PathBuf};
    let mut cases = vec![
        (
            PathBuf::from("navigation.rs"),
            "fn example(){let text=\"中e\u{301}\";value(123);} ".repeat(200),
        ),
        (
            PathBuf::from("navigation.txt"),
            "Label123 中e\u{301} \"value\" 456 /* note */ ".repeat(200),
        ),
    ];
    if let Some(path) = std::env::var_os("LOOG_EDITOR_PROFILE_FILE") {
        let source = std::fs::read_to_string(&path).expect("read local navigation input");
        cases.push((PathBuf::from(path), source));
    }
    let context = ApplicationContext::empty(Default::default());
    let _fonts = lgui::backend::install_text_environment(
        &context,
        lgui::render_skia::skia_text_system_handle(),
    );
    for (path, source) in cases {
        let bytes = source.len();
        let mut app = AppState::new();
        app.workspace.open_path(path, source);
        app.editor.focused = true;
        let initial = Arc::new(Mutex::new(Some(app)));
        let output = Arc::new(Mutex::new(None));
        let exposed = output.clone();
        let runtime = Arc::new(AppRuntime::default());
        let view_runtime = runtime.clone();
        let queue = UiTasks::default();
        let application = queue.application();
        let known = Arc::new(Mutex::new(HashMap::<usize, Vec<TokenSpan>>::new()));
        let rect = UiRect::new(0.0, 0.0, 800.0, 400.0);
        let ui: AppView = Arc::new(move |cx| {
            let state =
                AppContext::detached(cx.state_with(|| initial.lock().unwrap().take().unwrap()))
                    .with_runtime(&view_runtime);
            if view_runtime.highlighter().is_none() {
                view_runtime
                    .install_highlighter(Highlighter::new(state.clone(), application.clone()));
            }
            *exposed.lock().unwrap() = Some(state.clone());
            let git = cx.state_with(GitStoreSnapshot::default);
            let pane = state.get().workspace.active_pane();
            let binding = super::editor_view::binding(state, git, pane, rect);
            let provider = binding.clone();
            let previous = known.clone();
            let binding = binding.with_highlights(move |document, windows| {
                let ready = provider.highlights(document, windows.clone());
                let mut previous = previous.lock().unwrap();
                for window in windows.iter().filter(|window| !window.range.is_empty()) {
                    let actual = ready.iter().find(|tokens| tokens.range == window.range);
                    if let Some(old) = previous.get(&window.line.start) {
                        for token in old.iter().filter(|token| {
                            token.range.end > window.range.start
                                && token.range.start < window.range.end
                        }) {
                            // The same text keeps its class, wherever the page boundaries now
                            // fall; a span may be cut or joined differently.
                            let overlap = token.range.start.max(window.range.start)
                                ..token.range.end.min(window.range.end);
                            let class_at = |offset: usize| {
                                actual.and_then(|tokens| {
                                    tokens.spans.iter().find(|s| s.range.contains(&offset))
                                })
                            };
                            for offset in [overlap.start, overlap.end - 1] {
                                assert_eq!(
                                    class_at(offset).map(|span| span.class),
                                    Some(token.class),
                                    "caret scrolling erased a previously known semantic span"
                                );
                            }
                        }
                    }
                }
                previous.clear();
                for window in windows {
                    if let Some(tokens) = ready.iter().find(|tokens| tokens.range == window.range) {
                        previous.insert(window.line.start, tokens.spans.to_vec());
                    }
                }
                ready
            });
            let id = UiId::new("navigation-continuity");
            let focus = cx.focus_handle(id.clone());
            view::render(pane, rect, binding, id, focus)
        });
        let mut session = UiSession::new();
        session.render_view(&ui, rect, UiScale::ONE);
        let state: AppContext = output.lock().unwrap().clone().unwrap();
        wait_ready(&state, &queue, rect, true);
        session.render_view(&ui, rect, UiScale::ONE);
        for column in (0..1800).step_by(8) {
            state.update(|app| {
                let offset = app
                    .workspace
                    .active_editor()
                    .unwrap()
                    .offset_at_display_column(0, column);
                app.workspace
                    .active_editor_mut()
                    .unwrap()
                    .set_cursor(offset);
                super::editor::reveal_cursor(app, rect);
            });
            session.render_view(&ui, rect, UiScale::ONE);
            queue.run_all();
        }
        assert!(state.get().workspace.active_scroll().0 > 0.0);
        wait_ready(&state, &queue, rect, true);
        session.render_view(&ui, rect, UiScale::ONE);
        println!(
            "EDITOR_NAVIGATION {}",
            serde_json::json!({"bytes":bytes,"frames":225,"known_highlights_preserved":true})
        );
    }
}

#[test]
#[ignore = "real text layout benchmark; optional LOOG_EDITOR_PROFILE_FILE stays in memory"]
fn editor_viewport_benchmark() {
    assert!(
        std::env::var_os("LOOG_TEST_LANGUAGE_PACKS").is_some(),
        "provide prebuilt language packages through LOOG_TEST_LANGUAGE_PACKS for this syntax benchmark"
    );
    let js = "const item={name:\"hello\\\"world\",value:12345};function get(a){return a.value+item.value;}";
    let mut cases = vec![
        ("minified-3k.js", (js.repeat(36) + "\n").repeat(50)),
        ("minified-1m.js", js.repeat(12_000)),
    ];
    if let Some(path) = std::env::var_os("LOOG_EDITOR_PROFILE_FILE") {
        let label = if crate::domain::language::from_path(std::path::Path::new(&path)).name()
            == "javascript"
        {
            "local-input.js"
        } else {
            "local-input.txt"
        };
        cases.insert(
            0,
            (
                label,
                std::fs::read_to_string(path).expect("read local profiling input"),
            ),
        );
    }
    let context = ApplicationContext::empty(Default::default());
    let _fonts = lgui::backend::install_text_environment(
        &context,
        lgui::render_skia::skia_text_system_handle(),
    );
    for (name, source) in cases {
        view::clear_layout_cache();
        let bytes = source.len();
        let prepare = Instant::now();
        let mut app = AppState::new();
        app.workspace.open_path(name.into(), source);
        app.editor.focused = true;
        let prepare_ms = prepare.elapsed().as_secs_f64() * 1000.0;
        let initial = Arc::new(Mutex::new(Some(app)));
        let exposed = Arc::new(Mutex::new(None));
        let output = exposed.clone();
        let runtime = Arc::new(AppRuntime::default());
        let view_runtime = runtime.clone();
        let queue = UiTasks::default();
        let application = queue.application();
        let rect = UiRect::new(0.0, 0.0, 1000.0, 600.0);
        let ui: AppView = Arc::new(move |cx| {
            let state =
                AppContext::detached(cx.state_with(|| initial.lock().unwrap().take().unwrap()))
                    .with_runtime(&view_runtime);
            if view_runtime.highlighter().is_none() {
                view_runtime
                    .install_highlighter(Highlighter::new(state.clone(), application.clone()));
            }
            let git = cx.state_with(GitStoreSnapshot::default);
            *output.lock().unwrap() = Some(state.clone());
            let id = UiId::new("editor-viewport-bench");
            let focus = cx.focus_handle(id.clone());
            view::render(
                state.get().workspace.active_pane(),
                rect,
                super::editor_view::binding(
                    state,
                    git,
                    crate::domain::identity::PaneId::new(1),
                    rect,
                ),
                id,
                focus,
            )
        });
        let mut session = UiSession::new();
        let cold = Instant::now();
        session.render_view(&ui, rect, UiScale::ONE);
        let cold_ms = cold.elapsed().as_secs_f64() * 1000.0;
        let nodes = session.tree().nodes().len();
        let submitted_chars: usize = session
            .tree()
            .nodes()
            .iter()
            .filter_map(|node| node.text.as_deref())
            .map(|text| text.chars().count())
            .sum();
        let state: AppContext = exposed.lock().unwrap().clone().unwrap();
        wait_ready(&state, &queue, rect, false);
        let initial_ready_ms = cold.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        session.render_view(&ui, rect, UiScale::ONE);
        let initial_highlighted_frame_ms = start.elapsed().as_secs_f64() * 1000.0;
        wait_ready(&state, &queue, rect, true);
        let mut frames = Vec::new();
        let mut highlighted_frames = Vec::new();
        for i in 0..20 {
            state.update(|app| app.workspace.set_active_scroll(i as f32 * 12.0, 0.0));
            let start = Instant::now();
            session.render_view(&ui, rect, UiScale::ONE);
            frames.push(start.elapsed().as_secs_f64() * 1000.0);
            wait_ready(&state, &queue, rect, false);
            let start = Instant::now();
            session.render_view(&ui, rect, UiScale::ONE);
            highlighted_frames.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        // Far horizontal jump: background prefix scanning must produce the same
        // tokens as a fresh scan while the first UI frame remains bounded.
        let far_x = {
            let app = state.get();
            let editor = app.workspace.active_editor().unwrap();
            editor.line_display_width(0) as f32 * crate::ui::fonts::editor().char_w - 1000.0
        }
        .max(0.0);
        state.update(|app| app.workspace.set_active_scroll(far_x, 0.0));
        let start = Instant::now();
        session.render_view(&ui, rect, UiScale::ONE);
        let jump_frame_ms = start.elapsed().as_secs_f64() * 1000.0;
        let ready = Instant::now();
        wait_ready(&state, &queue, rect, false);
        let jump_ready_ms = ready.elapsed().as_secs_f64() * 1000.0;
        wait_ready(&state, &queue, rect, true);
        session.render_view(&ui, rect, UiScale::ONE);
        let worker = runtime.highlighter().unwrap().worker;
        let before_edit = worker.stats();
        state.update(|app| {
            app.workspace
                .active_editor_mut()
                .unwrap()
                .replace(0..0, " ", loog_editor_core::text::EditKind::Other)
                .unwrap();
        });
        let start = Instant::now();
        session.render_view(&ui, rect, UiScale::ONE);
        let edit_frame_ms = start.elapsed().as_secs_f64() * 1000.0;
        wait_ready(&state, &queue, rect, true);
        let edit_parsed_bytes = worker.stats().tree.parsed_bytes - before_edit.tree.parsed_bytes;
        // Check the actual input's position mapping without logging any text.
        let app = state.get();
        let buffer = app.workspace.active_editor().unwrap();
        let left = crate::ui::fonts::editor().gutter_w_for_lines(buffer.line_count())
            + crate::ui::theme::CODE_PAD;
        let metrics = view::scroll_metrics(rect, left, &buffer);
        for line in [0, buffer.line_count() / 2, buffer.line_count() - 1] {
            let visual = buffer.line_display_width(line) / 2;
            let offset = buffer.offset_at_display_column(line, visual);
            let column = buffer.buffer().line_col(offset).1;
            let sx = (visual as f32 * crate::ui::fonts::editor().char_w - metrics.viewport_w / 2.0)
                .max(0.0);
            let x = view::document_caret(
                &buffer,
                line,
                column,
                left,
                0.0,
                left + metrics.content_w,
                (sx, metrics.viewport_w),
            );
            let hit = view::cursor_offset_from_point(
                &buffer,
                rect,
                left,
                metrics,
                sx,
                line as f32 * crate::ui::fonts::editor().line_h,
                x - sx + 0.05,
                crate::ui::fonts::editor().line_h / 2.0,
            );
            assert_eq!(
                hit, offset,
                "position mapping must round-trip for the profiling input"
            );
        }
        frames.sort_by(f64::total_cmp);
        highlighted_frames.sort_by(f64::total_cmp);
        let stats = worker.stats();
        let (layout_hits, layout_misses, layout_evictions, layout_bytes, _) =
            view::layout_cache_stats();
        println!(
            "EDITOR_VIEWPORT {}",
            serde_json::json!({"case":name,"bytes":bytes,
            "prepare_ms":prepare_ms,"cold_ms":cold_ms,"scroll_median_ms":frames[10],
            "scroll_max_ms":frames[19],"nodes":nodes,"submitted_chars":submitted_chars,
            "initial_ready_ms":initial_ready_ms,"initial_highlighted_frame_ms":initial_highlighted_frame_ms,
            "highlighted_scroll_median_ms":highlighted_frames[10],"highlighted_scroll_max_ms":highlighted_frames[19],
            "jump_frame_ms":jump_frame_ms,"jump_ready_ms":jump_ready_ms,"edit_frame_ms":edit_frame_ms,"edit_parsed_bytes":edit_parsed_bytes,
            "parsed_bytes":stats.tree.parsed_bytes,"full_parses":stats.tree.full_parses,"incremental_parses":stats.tree.incremental_parses,
            "parse_ms":stats.tree.parse_time.as_secs_f64() * 1000.0,"query_ms":stats.tree.query_time.as_secs_f64() * 1000.0,
            "provisional_windows":stats.provisional.windows,
            "syntax_cache_hits":stats.cache_hits,"syntax_cache_bytes":stats.cache_bytes,"syntax_evictions":stats.evictions,
            "layout_hits":layout_hits,"layout_misses":layout_misses,"layout_evictions":layout_evictions,"layout_bytes":layout_bytes})
        );
    }
}
