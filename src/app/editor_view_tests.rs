use super::*;
use crate::app::context::AppContext;
use crate::domain::identity::PaneId;
use crate::features::git::{GitStoreSnapshot, LineChange};
use crate::model::text::TextBuffer;
use lgui::core::{CursorIcon, KeyState, PointerButton, UiId};
use lgui::core::{KeyModifiers, KeyboardEvent, LogicalKey};
use lgui::prelude::State;
use lgui::prelude::{Element, group};
use lgui::services::{Clipboard, ClipboardError};
use std::collections::BTreeMap;
use std::sync::Mutex;

#[test]
fn layout_cache_reuses_geometry_across_scroll_and_resize_with_correct_hit_testing() {
    let context = lgui::application::ApplicationContext::empty(Default::default());
    let _fonts = lgui::backend::install_text_environment(
        &context,
        lgui::render_skia::skia_text_system_handle(),
    );
    clear_layout_cache();
    let text = "ab\t中e\u{301}😀";
    let a = layout_line_at_column(text, 30.0, 40.0, 800.0, 0).unwrap();
    let b = layout_line_at_column(text, -200.0, 180.0, 420.0, 4).unwrap();
    assert!(std::sync::Arc::ptr_eq(&a.layout, &b.layout));
    for index in [0, 1, 2, 3, 4, 6, 7] {
        let x = caret_x(Some(&a), index).unwrap();
        let shifted = caret_x(Some(&b), index).unwrap();
        assert!((shifted - x + 230.0).abs() < 0.01);
        assert_eq!(hit_column(&a, x + 0.01, 45.0), index);
        assert_eq!(hit_column(&b, shifted + 0.01, 185.0), index);
    }
    let different_tabs = layout_line_at_column(text, 30.0, 40.0, 800.0, 1).unwrap();
    assert!(!std::sync::Arc::ptr_eq(&a.layout, &different_tabs.layout));
    assert_eq!(layout_cache_stats().0, 1);
}

#[test]
fn font_weight_changes_refresh_layout_and_keep_carets_and_hits_aligned() {
    use lgui::text::{TextLayoutRequest, layout};
    let context = lgui::application::ApplicationContext::empty(Default::default());
    let _text = lgui::backend::install_text_environment(
        &context,
        lgui::render_skia::skia_text_system_handle(),
    );
    clear_layout_cache();
    let source = "ab\t中e\u{301}😀";
    let mut layouts = Vec::new();
    for weight in [400, 700, 900, 400] {
        let font = fonts::FontSettings {
            weight,
            ..fonts::FontSettings::EDITOR
        };
        fonts::set(&font, &fonts::FontSettings::TERMINAL);
        let metrics = fonts::editor();
        assert_eq!(metrics.style(theme::c().text).weight, i32::from(weight));
        assert_eq!(
            metrics.bold(theme::c().text).weight,
            i32::from(weight.max(700))
        );
        assert_eq!(fonts::terminal().style(theme::c().text).weight, 400);
        let bounds = UiRect::new(0.0, 0.0, 16_777_216.0, metrics.line_h);
        let mut digits = TextLayoutRequest::single_line(
            "0000000000000000",
            bounds,
            metrics.size,
            i32::from(weight),
        );
        digits.font_families = metrics.families;
        assert!((metrics.char_w - layout(&digits).unwrap().width / 16.0).abs() < 0.01);
        let (display, _, _) = expand_tabs(source, 0);
        let mut request =
            TextLayoutRequest::single_line(&display, bounds, metrics.size, i32::from(weight));
        request.font_families = metrics.families;
        let expected = layout(&request).unwrap();
        let measured = layout_line(source, 30.0, 40.0, 800.0).unwrap();
        assert!((measured.width - expected.width).abs() < 0.01);
        for index in [0, 1, 2, 3, 4, 6, 7] {
            let x = caret_x(Some(&measured), index).unwrap();
            assert_eq!(hit_column(&measured, x + 0.01, 45.0), index);
        }
        layouts.push(measured.layout);
    }
    assert!(!std::sync::Arc::ptr_eq(&layouts[0], &layouts[1]));
    assert!(!std::sync::Arc::ptr_eq(&layouts[1], &layouts[2]));
    assert!(std::sync::Arc::ptr_eq(&layouts[0], &layouts[3]));
    fonts::set(&fonts::FontSettings::EDITOR, &fonts::FontSettings::TERMINAL);
}

#[test]
fn layout_cache_evicts_individual_cold_entries_without_clearing_hot_geometry() {
    let context = lgui::application::ApplicationContext::empty(Default::default());
    let _fonts = lgui::backend::install_text_environment(
        &context,
        lgui::render_skia::skia_text_system_handle(),
    );
    clear_layout_cache();
    let hot = layout_line("hot retained line", 0.0, 0.0, 800.0).unwrap();
    for i in 0..2100 {
        let text = format!("{i}:{}", "cold entry ".repeat(10));
        layout_line(&text, 0.0, 0.0, 800.0).unwrap();
        let reused = layout_line("hot retained line", 0.0, 0.0, 800.0).unwrap();
        assert!(std::sync::Arc::ptr_eq(&hot.layout, &reused.layout));
    }
    let (_, _, evictions, bytes, len) = layout_cache_stats();
    assert!(evictions > 0);
    assert!(bytes <= 8 * 1024 * 1024);
    assert!(len <= 2048);
}

#[test]
fn multi_megabyte_single_line_windows_and_clicks_are_bounded() {
    let buffer = TextBuffer::new("a".repeat(8 * 1024 * 1024));
    let rect = UiRect::new(0.0, 0.0, 800.0, 400.0);
    let left = 80.0;
    let metrics = scroll_metrics(rect, left, &buffer);
    let char_w = fonts::editor().char_w;
    for column in [0, 1000, 1_000_000, 8 * 1024 * 1024 - 100] {
        let sx = column as f32 * char_w;
        let window = line_window(&buffer, 0, sx, 720.0);
        assert!(
            window.len() < 256,
            "only viewport text is submitted for shaping"
        );
        assert!(window.contains(&column));
        let offset = cursor_offset_from_point(
            &buffer,
            rect,
            left,
            metrics,
            sx,
            0.0,
            left + char_w * 4.0,
            10.0,
        );
        assert_eq!(offset, column + 4);
    }
    assert_eq!(
        buffer.len(),
        8 * 1024 * 1024,
        "windowing must preserve all file contents"
    );
}

#[test]
fn long_unicode_windows_remain_on_grapheme_boundaries_near_the_end() {
    let buffer = TextBuffer::new("中e\u{301}👩‍💻\t😀".repeat(100_000));
    let columns = buffer.line_display_width(0);
    for column in [0, columns / 2, columns - 20] {
        let window = line_window(&buffer, 0, column as f32 * fonts::editor().char_w, 700.0);
        assert!(window.len() < 4096);
        assert!(buffer.is_grapheme_boundary(window.start));
        assert!(buffer.is_grapheme_boundary(window.end));
    }
}

#[test]
fn viewport_windows_bound_medium_and_long_lines_without_a_byte_threshold() {
    let width = 720.0;
    let char_w = fonts::editor().char_w;
    for len in [512, 3245, 4095, 4096, 4097, 100_000] {
        let buffer = TextBuffer::new("x".repeat(len));
        for column in [0, len / 2, len - 30] {
            let scroll = column as f32 * char_w;
            let range = line_window(&buffer, 0, scroll, width);
            assert!(range.len() <= (width / char_w).ceil() as usize + 192);
            assert!(range.contains(&column));
        }
        assert_eq!(buffer.len(), len);
    }
}

#[test]
fn real_layout_keeps_scrolled_carets_hits_ime_and_selection_in_agreement() {
    use lgui::{
        application::{AppView, ApplicationContext},
        core::UiScale,
        session::UiSession,
    };
    use std::sync::Arc;
    let context = ApplicationContext::empty(Default::default());
    let _fonts = lgui::backend::install_text_environment(
        &context,
        lgui::render_skia::skia_text_system_handle(),
    );
    fonts::set(&fonts::FontSettings::EDITOR, &fonts::FontSettings::TERMINAL);
    for source in [
        "const item={value:123};function get(a){return a.value;}".repeat(65),
        "中e\u{301}👩‍💻\tabc123 ".repeat(180),
    ] {
        let mut app = document(&source);
        let document = app.workspace.active_document().unwrap();
        if source.starts_with("const ") {
            app.workspace
                .documents
                .get_mut(&document)
                .unwrap()
                .meta
                .lang = crate::domain::language::builtin("javascript");
        }
        let asynchronous = app.workspace.documents[&document]
            .meta
            .lang
            .grammar
            .is_some();
        app.editor.focused = true;
        let rect = UiRect::new(0.0, 0.0, 800.0, 400.0);
        let left = fonts::editor().gutter_w() + theme::CODE_PAD;
        let char_w = fonts::editor().char_w;
        let buffer = app.workspace.active_editor().unwrap();
        let start = buffer.offset_at_display_column(0, 1000);
        let end = buffer.offset_at_display_column(0, 1020);
        app.workspace
            .active_editor_mut()
            .unwrap()
            .select_range(start..end);
        app.workspace.set_active_scroll(970.0 * char_w, 0.0);
        let initial = Arc::new(Mutex::new(Some(app)));
        let exposed = Arc::new(Mutex::new(None));
        let output = exposed.clone();
        let runtime = Arc::new(crate::app::runtime::AppRuntime::default());
        let view_runtime = runtime.clone();
        let tasks = crate::app::test_support::UiTasks::default();
        let application = tasks.application();
        let view: AppView = Arc::new(move |cx| {
            let state =
                AppContext::detached(cx.state_with(|| initial.lock().unwrap().take().unwrap()))
                    .with_runtime(&view_runtime);
            if view_runtime.markdown().is_none() {
                view_runtime.install_markdown(crate::app::markdown_service::MarkdownService::new(
                    state.clone(),
                    application.clone(),
                    Arc::new(TestClipboard::default()),
                ));
            }
            if view_runtime.highlighter().is_none() {
                view_runtime.install_highlighter(crate::app::syntax::Highlighter::new(
                    state.clone(),
                    application.clone(),
                ));
            }
            let git = cx.state_with(GitStoreSnapshot::default);
            *output.lock().unwrap() = Some(state.clone());
            let id = UiId::new("bounded-editor-test");
            let focus = cx.focus_handle(id.clone());
            render(PaneId::new(1), rect, state, git, id, focus)
        });
        let mut session = UiSession::new();
        session.render_view(&view, rect, UiScale::ONE);
        let state: AppContext = exposed.lock().unwrap().clone().unwrap();
        let before_ime = session
            .tree()
            .node(&UiId::new("bounded-editor-test"))
            .unwrap()
            .ime_cursor_rect;
        let before_selection = session
            .tree()
            .nodes()
            .iter()
            .find(|node| {
                node.style == lgui::prelude::VisualStyle::filled(theme::c().accent).alpha(55)
            })
            .unwrap()
            .layout_rect;
        if asynchronous {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while tasks.pending_ids().is_empty() {
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
            tasks.run_all();
            assert!(state.get().editor.syntax_revision > 0);
        } else {
            assert!(tasks.pending_ids().is_empty());
            assert_eq!(state.get().editor.syntax_revision, 0);
        }
        session.render_view(&view, rect, UiScale::ONE);
        assert_eq!(
            session
                .tree()
                .node(&UiId::new("bounded-editor-test"))
                .unwrap()
                .ime_cursor_rect,
            before_ime
        );
        assert_eq!(
            session
                .tree()
                .nodes()
                .iter()
                .find(|node| node.style
                    == lgui::prelude::VisualStyle::filled(theme::c().accent).alpha(55))
                .unwrap()
                .layout_rect,
            before_selection
        );
        let app = state.get();
        let buffer = app.workspace.active_editor().unwrap();
        let sx = app.workspace.active_scroll().0;
        let metrics = scroll_metrics(rect, left, &buffer);
        let range = line_window(&buffer, 0, sx, metrics.viewport_w);
        let initial_column = buffer.display_column(range.start);
        let origin = left + initial_column as f32 * char_w;
        let layout = layout_line_at_column(
            &buffer.text()[range.clone()],
            origin,
            0.0,
            left + metrics.content_w,
            initial_column,
        )
        .unwrap();
        for offset in [start, end] {
            let chars = buffer.text()[range.start..offset].chars().count();
            let x = caret_x(Some(&layout), chars).unwrap();
            let hit = cursor_offset_from_point(
                &buffer,
                rect,
                left,
                metrics,
                sx,
                0.0,
                x - sx + 0.05,
                fonts::editor().line_h / 2.0,
            );
            assert_eq!(hit, offset);
        }
        let cursor = buffer.cursor();
        let x = caret_x(
            Some(&layout),
            buffer.text()[range.start..cursor].chars().count(),
        )
        .unwrap();
        let ime = session
            .tree()
            .node(&UiId::new("bounded-editor-test"))
            .unwrap()
            .ime_cursor_rect
            .unwrap();
        assert!((ime.left - (x - sx)).abs() < 0.01);
        let selection = session
            .tree()
            .nodes()
            .iter()
            .find(|node| {
                node.style == lgui::prelude::VisualStyle::filled(theme::c().accent).alpha(55)
            })
            .unwrap();
        let expected_left = caret_x(
            Some(&layout),
            buffer.text()[range.start..start].chars().count(),
        )
        .unwrap();
        let expected_right = caret_x(
            Some(&layout),
            buffer.text()[range.start..end].chars().count(),
        )
        .unwrap();
        assert!((selection.layout_rect.width() - (expected_right - expected_left)).abs() < 0.01);
        assert_eq!(buffer.selected_text(), Some(&source[start..end]));
        drop(app);
        state.update(|app| {
            let mut editor = app.workspace.active_editor_mut().unwrap();
            let end = editor.len();
            editor.select_range(0..end);
        });
        session.render_view(&view, rect, UiScale::ONE);
        let selection = session
            .tree()
            .nodes()
            .iter()
            .find(|node| {
                node.style == lgui::prelude::VisualStyle::filled(theme::c().accent).alpha(55)
            })
            .unwrap();
        assert!(
            selection.layout_rect.width() >= metrics.viewport_w,
            "a selection spanning both crop edges must cover the entire visible row"
        );
        assert_eq!(
            state
                .get()
                .workspace
                .active_editor()
                .unwrap()
                .selected_text(),
            Some(source.as_str())
        );
    }
}

#[test]
#[ignore = "manual large-file rendering performance baseline"]
fn large_file_interaction_baseline() {
    use lgui::{application::AppView, core::UiScale, session::UiSession};
    use std::sync::Arc;
    let rect = UiRect::new(0.0, 0.0, 1000.0, 600.0);
    for (name, contents) in [
        ("8 MiB single line", "a".repeat(8 * 1024 * 1024)),
        ("Unicode single line", "中文e\u{301}😀\t".repeat(500_000)),
        (
            "many short lines",
            "{\"record\": \"中文\", \"value\": 12345}\n".repeat(500_000),
        ),
    ] {
        let prepare = std::time::Instant::now();
        let mut app = document(&contents);
        app.editor.focused = true;
        let prepare = prepare.elapsed();
        let exposed = Arc::new(Mutex::new(None));
        let output = exposed.clone();
        let initial = Arc::new(Mutex::new(Some(app)));
        let view: AppView = Arc::new(move |cx| {
            let state =
                AppContext::detached(cx.state_with(|| initial.lock().unwrap().take().unwrap()));
            let git = cx.state(GitStoreSnapshot::default());
            *output.lock().unwrap() = Some(state.clone());
            let id = UiId::new("large-file-benchmark");
            let focus = cx.focus_handle(id.clone());
            render(PaneId::new(1), rect, state, git, id, focus)
        });
        let mut session = UiSession::new();
        session.render_view(&view, rect, UiScale::ONE);
        let state: AppContext = exposed.lock().unwrap().clone().unwrap();
        let start = std::time::Instant::now();
        for i in 0..100 {
            state.update(|app| {
                let buffer = app.workspace.active_editor().unwrap();
                let left = rect.left + fonts::editor().gutter_w() + theme::CODE_PAD;
                let metrics = scroll_metrics(rect, left, &buffer);
                let sx = metrics.max_x * (i as f32 / 100.0);
                let offset = cursor_offset_from_point(
                    &buffer,
                    rect,
                    left,
                    metrics,
                    sx,
                    0.0,
                    left + 20.0,
                    10.0,
                );
                app.workspace
                    .active_editor_mut()
                    .unwrap()
                    .set_cursor(offset);
                app.workspace.set_active_scroll(sx, 0.0);
            });
            session.render_view(&view, rect, UiScale::ONE);
        }
        eprintln!(
            "{name}: {:.2} MiB, prepare {:?}, 100 viewport renders and clicks {:?}",
            contents.len() as f64 / (1024.0 * 1024.0),
            prepare,
            start.elapsed()
        );
        let edits = std::time::Instant::now();
        for _ in 0..20 {
            state.update(|app| {
                let mut editor = app.workspace.active_editor_mut().unwrap();
                let end = editor.len();
                editor.set_cursor(end);
                editor
                    .replace_selection("x", crate::model::text::EditKind::Other)
                    .unwrap();
            });
            session.render_view(&view, rect, UiScale::ONE);
        }
        eprintln!(
            "{name}: 20 end-of-file edits and renders {:?}",
            edits.elapsed()
        );
        state.update(|app| {
            assert!(app.workspace.is_view_dirty(app.workspace.active().unwrap()));
            for _ in 0..20 {
                assert!(app.workspace.active_editor_mut().unwrap().undo().is_some());
            }
            assert!(!app.workspace.is_view_dirty(app.workspace.active().unwrap()));
        });
        assert_eq!(
            state.get().workspace.active_editor().unwrap().text(),
            contents
        );
    }
}

#[test]
fn overview_marks_join_adjacent_lines_of_the_same_change() {
    let changes = BTreeMap::from([
        (1, LineChange::Added),
        (2, LineChange::Added),
        (3, LineChange::Modified),
        (5, LineChange::Modified),
        (6, LineChange::Modified),
        (10, LineChange::Deleted),
        // A deletion after the last line marks the end of the file.
        (11, LineChange::Deleted),
    ]);
    let runs = change_marks(&changes, 10)
        .iter()
        .map(|mark| (mark.first, mark.lines, mark.change))
        .collect::<Vec<_>>();
    assert_eq!(
        runs,
        [
            (0, 2, LineChange::Added),
            (2, 1, LineChange::Modified),
            (4, 2, LineChange::Modified),
            (9, 1, LineChange::Deleted),
        ]
    );
}

#[derive(Default)]
struct TestClipboard {
    text: Mutex<Option<String>>,
    fail: bool,
}
impl Clipboard for TestClipboard {
    fn read_text(&self) -> Result<Option<String>, ClipboardError> {
        if self.fail {
            Err(ClipboardError::AccessDenied)
        } else {
            Ok(self.text.lock().unwrap().clone())
        }
    }
    fn write_text(&self, text: &str) -> Result<(), ClipboardError> {
        if self.fail {
            return Err(ClipboardError::AccessDenied);
        }
        *self.text.lock().unwrap() = Some(text.to_owned());
        Ok(())
    }
}
fn document(contents: &str) -> AppState {
    let mut app = AppState::new();
    app.workspace
        .open_path("editing-test.txt".into(), contents.into());
    app
}

/// Exercise real lgui hit testing, focus and event dispatch without opening a native window.
#[test]
fn editor_events_route_drag_selection_shortcuts_and_ime_commit() {
    use lgui::application::{AppView, ApplicationContext};
    use lgui::core::{ImeEvent, InputEvent, Point, PointerData, UiScale, dispatch_runtime_output};
    use lgui::session::UiSession;
    use std::sync::Arc;
    let exposed = Arc::new(Mutex::new(None::<AppContext>));
    let output = exposed.clone();
    let viewport = UiRect::new(0.0, 0.0, 500.0, 300.0);
    let app = ApplicationContext::empty(Default::default());
    let view_app = app.clone();
    let view: AppView = Arc::new(move |cx| {
        let state = AppContext::detached(cx.state(document("hello world\nsecond")));
        let git_store = cx.state(GitStoreSnapshot::default());
        *output.lock().unwrap() = Some(state.clone());
        let id = cx.use_stable_id();
        let focus = cx.focus_handle(id.clone());
        // Shortcuts reach the editor through the keymap, as in the app root.
        let env = crate::app::key_actions::KeyEnv {
            state: state.clone(),
            git_store: git_store.clone(),
            application: view_app.clone(),
            editor_focus: focus.clone(),
            editor_rect: viewport,
            terminal: None,
        };
        crate::app::key_actions::attach(
            group(viewport).child(render(
                PaneId::new(1),
                viewport,
                state,
                git_store,
                id,
                focus,
            )),
            env,
        )
    });
    let mut session = UiSession::new();
    session.render_view(&view, viewport, UiScale::ONE);
    let state = exposed.lock().unwrap().clone().unwrap();
    let mut send = |input| {
        let events = session.handle_input(input);
        let mut prevented = false;
        dispatch_runtime_output(
            events,
            &app,
            &lgui::window::WindowId::new("editor-test"),
            |action| session.handle_default_action(action),
            |cx| prevented |= cx.default_prevented(),
        );
        session.render_view(&view, viewport, UiScale::ONE);
        prevented
    };
    let left = fonts::editor().gutter_w() + theme::CODE_PAD;
    let pointer =
        |column: f32| PointerData::mouse(Point::new(left + column * fonts::editor().char_w, 5.0));
    send(InputEvent::PointerDown {
        pointer: pointer(0.0),
        button: PointerButton::Left,
    });
    send(InputEvent::PointerMove(pointer(5.0)));
    send(InputEvent::PointerUp {
        pointer: pointer(5.0),
        button: PointerButton::Left,
    });
    assert_eq!(
        state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .selected_text(),
        Some("hello")
    );
    let key = |key, modifiers| {
        InputEvent::Keyboard(KeyboardEvent {
            state: KeyState::Down,
            key,
            modifiers,
            ..Default::default()
        })
    };
    let shortcut = if cfg!(target_os = "macos") {
        KeyModifiers::META
    } else {
        KeyModifiers::CONTROL
    };
    assert!(send(key(
        LogicalKey::Named(lgui::core::NamedKey::Tab),
        KeyModifiers::empty()
    )));
    assert_eq!(
        state.get().workspace.active_editor().unwrap().text(),
        "  hello world\nsecond"
    );
    send(key(LogicalKey::Character("z".into()), shortcut));
    assert_eq!(
        state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .selected_text(),
        Some("hello")
    );
    send(InputEvent::Ime(ImeEvent::Preedit {
        text: "nihao".into(),
        cursor: Some(0..5),
    }));
    assert_eq!(
        state.get().workspace.active_editor().unwrap().text(),
        "hello world\nsecond"
    );
    send(InputEvent::Ime(ImeEvent::Commit("你好".into())));
    assert_eq!(
        state.get().workspace.active_editor().unwrap().text(),
        "你好 world\nsecond"
    );
    assert!(state.get().editor.ime.is_none());
    send(key(LogicalKey::Character("z".into()), shortcut));
    assert_eq!(
        state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .selected_text(),
        Some("hello")
    );
}

#[test]
fn smooth_caret_keeps_input_and_ime_logical_and_snaps_during_drag_or_context_changes() {
    use lgui::application::{AppView, ApplicationContext};
    use lgui::core::{
        ImeEvent, InputEvent, NamedKey, Point, PointerData, UiScale, dispatch_runtime_output,
    };
    use lgui::session::UiSession;
    use std::sync::Arc;

    let exposed = Arc::new(Mutex::new(None::<AppContext>));
    let output = exposed.clone();
    let viewport = UiRect::new(0.0, 0.0, 500.0, 300.0);
    let application = ApplicationContext::empty(Default::default());
    let view_app = application.clone();
    let view: AppView = Arc::new(move |cx| {
        let mut initial = document(&format!("hello world\n{}", "line\n".repeat(40)));
        initial.editor.focused = true;
        let state = AppContext::detached(cx.state(initial));
        let git = cx.state(GitStoreSnapshot::default());
        *output.lock().unwrap() = Some(state.clone());
        let id = UiId::new("smooth-editor-test");
        let focus = cx.focus_handle(id.clone());
        let env = crate::app::key_actions::KeyEnv {
            state: state.clone(),
            git_store: git.clone(),
            application: view_app.clone(),
            editor_focus: focus.clone(),
            editor_rect: viewport,
            terminal: None,
        };
        crate::app::key_actions::attach(
            group(viewport).child(render(PaneId::new(1), viewport, state, git, id, focus)),
            env,
        )
    });
    let mut session = UiSession::new();
    session.render_view(&view, viewport, UiScale::ONE);
    let state = exposed.lock().unwrap().clone().unwrap();
    let send = |session: &mut UiSession, input| {
        dispatch_runtime_output(
            session.handle_input(input),
            &application,
            &lgui::window::WindowId::new("smooth-test"),
            |action| session.handle_default_action(action),
            |_| {},
        );
        session.render_view(&view, viewport, UiScale::ONE);
    };
    let caret = |session: &UiSession| {
        let node = session.tree().node(&UiId::new("editor-caret-1")).unwrap();
        let layer = session.tree().node(node.parent.as_ref().unwrap()).unwrap();
        let transform = layer.compositing_layer.unwrap().transform;
        node.layout_rect
            .translate(transform.translation_x(), transform.translation_y())
    };
    // UiSession has no native text service. Use the editor's existing
    // fallback metrics for the painted target, and check IME geometry separately.
    let logical = |state: &AppContext| {
        let app = state.get();
        let buffer = app.workspace.active_editor().unwrap();
        let (line, col) = buffer.line_col();
        let left = viewport.left + fonts::editor().gutter_w() + theme::CODE_PAD;
        let layout = layout_line(
            buffer.line(line),
            left,
            line as f32 * fonts::editor().line_h,
            viewport.right,
        );
        (caret_x(layout.as_ref(), col).unwrap_or(
            left + buffer.display_column(buffer.cursor()) as f32 * fonts::editor().char_w,
        ) - app.workspace.active_scroll().0)
            .round()
    };
    let left = fonts::editor().gutter_w() + theme::CODE_PAD;
    let pointer =
        |column| PointerData::mouse(Point::new(left + column * fonts::editor().char_w, 5.0));
    send(
        &mut session,
        InputEvent::PointerDown {
            pointer: pointer(0.0),
            button: PointerButton::Left,
        },
    );
    send(
        &mut session,
        InputEvent::PointerUp {
            pointer: pointer(0.0),
            button: PointerButton::Left,
        },
    );
    let start = caret(&session);
    send(&mut session, InputEvent::TextInput("中".into()));
    assert!(
        state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .text()
            .starts_with("中hello")
    );
    assert_eq!(
        state.get().workspace.active_editor().unwrap().cursor(),
        "中".len()
    );
    assert_eq!(caret(&session), start, "only the visual caret should lag");
    assert!(logical(&state) > start.left);
    assert_eq!(
        session
            .tree()
            .node(&UiId::new("smooth-editor-test"))
            .unwrap()
            .ime_cursor_rect
            .unwrap()
            .left
            .round(),
        super::ime_cursor_rect(&state.get(), viewport).left.round()
    );
    session.advance(0.0);
    session.advance(40.0);
    session.render_view(&view, viewport, UiScale::ONE);
    assert!(caret(&session).left > start.left && caret(&session).left < logical(&state));
    let displayed = caret(&session);
    send(
        &mut session,
        InputEvent::Keyboard(KeyboardEvent {
            state: KeyState::Down,
            key: LogicalKey::Named(NamedKey::ArrowRight),
            ..Default::default()
        }),
    );
    assert_eq!(
        caret(&session),
        displayed,
        "navigation retargets without jumping"
    );
    session.advance(0.0);
    session.advance(80.0);
    session.render_view(&view, viewport, UiScale::ONE);
    assert_eq!(caret(&session).left, logical(&state));

    let before_click = caret(&session);
    send(
        &mut session,
        InputEvent::PointerDown {
            pointer: pointer(8.0),
            button: PointerButton::Left,
        },
    );
    assert_eq!(
        caret(&session),
        before_click,
        "single-click placement should animate"
    );
    send(&mut session, InputEvent::PointerMove(pointer(10.0)));
    assert!(
        state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .selection()
            .is_some()
    );
    assert_eq!(
        caret(&session).left,
        logical(&state),
        "drag selection must track immediately"
    );
    send(
        &mut session,
        InputEvent::PointerUp {
            pointer: pointer(10.0),
            button: PointerButton::Left,
        },
    );
    send(&mut session, InputEvent::TextInput("x".into()));
    send(
        &mut session,
        InputEvent::Ime(ImeEvent::Preedit {
            text: "ni".into(),
            cursor: Some(0..0),
        }),
    );
    assert_eq!(
        caret(&session).left,
        logical(&state),
        "composition cancels visual motion"
    );
    send(&mut session, InputEvent::Ime(ImeEvent::Commit("你".into())));
    assert!(state.get().editor.ime.is_none());
    state.update(|app| app.preferences.values_mut().smooth_caret = false);
    session.render_view(&view, viewport, UiScale::ONE);
    assert_eq!(
        caret(&session).left,
        logical(&state),
        "turning off motion snaps an active animation"
    );
    state.update(|app| {
        app.preferences.values_mut().smooth_caret = true;
        app.workspace.active_editor_mut().unwrap().set_cursor(0);
    });
    session.render_view(&view, viewport, UiScale::ONE);
    state.update(|app| app.workspace.set_active_scroll(0.0, 1.0));
    session.render_view(&view, viewport, UiScale::ONE);
    assert_eq!(
        caret(&session).left,
        logical(&state),
        "scrolling snaps even while moving"
    );
    state.update(|app| app.editor.focused = false);
    session.render_view(&view, viewport, UiScale::ONE);
    assert!(session.tree().node(&UiId::new("editor-caret-1")).is_none());
    state.update(|app| {
        app.editor.focused = true;
        app.workspace.active_editor_mut().unwrap().set_cursor(4);
    });
    session.render_view(&view, viewport, UiScale::ONE);
    assert_eq!(
        caret(&session).left,
        logical(&state),
        "focus restoration starts at the logical caret"
    );
    state.update(|app| {
        app.workspace
            .open_path("other.txt".into(), "other file".into());
    });
    session.render_view(&view, viewport, UiScale::ONE);
    assert_eq!(
        caret(&session).left,
        logical(&state),
        "switching files snaps immediately"
    );
}

#[test]
fn conflict_buttons_take_the_pointer_cursor_over_the_code() {
    use crate::features::git::conflict::ConflictRegion;
    use lgui::application::AppView;
    use lgui::core::{Point, UiScale};
    use lgui::session::UiSession;
    use std::sync::Arc;

    let viewport = UiRect::new(0.0, 0.0, 700.0, 300.0);
    let view: AppView = Arc::new(move |cx| {
        let state = AppContext::detached(
            cx.state(document("a\n<<<<<<< x\nL\n=======\nR\n>>>>>>> y\nafter\n")),
        );
        let id = cx.use_stable_id();
        let focus = cx.focus_handle(id.clone());
        let git_store = cx.state(GitStoreSnapshot {
            conflicts: BTreeMap::from([(
                "editing-test.txt".into(),
                Arc::from(vec![ConflictRegion {
                    start: 2,
                    current: 3..4,
                    separator: 4,
                    incoming: 5..6,
                    end: 6,
                }]),
            )])
            .into(),
            ..Default::default()
        });
        group(viewport).child(render(
            PaneId::new(1),
            viewport,
            state,
            git_store,
            id,
            focus,
        ))
    });
    let mut session = UiSession::new();
    session.render_view(&view, viewport, UiScale::ONE);

    // The last button ends 8px left of the scrollbar, on the opening line.
    let line_h = crate::ui::fonts::editor().line_h;
    let button = Point::new(
        viewport.right - crate::ui::editor::view::SCROLLBAR_SIZE - 8.0 - 6.0,
        line_h + 2.0 + line_h / 2.0,
    );
    assert_eq!(
        session.tree().cursor_at(button),
        Some(CursorIcon::Pointer),
        "the text layer must not cover the buttons"
    );
    // Beside the buttons the code keeps its text cursor.
    assert_eq!(
        session.tree().cursor_at(Point::new(150.0, 10.0 * line_h)),
        Some(CursorIcon::Text)
    );
}

#[test]
fn scrollbar_release_survives_editor_child_reordering() {
    use lgui::application::{AppView, ApplicationContext};
    use lgui::core::{
        InputEvent, Point, PointerData, UiEventKind, UiEventPayload, UiScale,
        dispatch_runtime_output,
    };
    use lgui::session::UiSession;
    use std::sync::Arc;

    let exposed = Arc::new(Mutex::new(None::<AppContext>));
    let output = exposed.clone();
    let viewport = UiRect::new(0.0, 0.0, 500.0, 300.0);
    let view: AppView = Arc::new(move |cx| {
        let state = AppContext::detached(
            cx.state(document(&format!("{}\n", "line".repeat(100)).repeat(100))),
        );
        *output.lock().unwrap() = Some(state.clone());
        let id = cx.use_stable_id();
        let focus = cx.focus_handle(id.clone());
        let git_store = cx.state(GitStoreSnapshot::default());

        let drag_state = state.clone();
        let up_state = state.clone();
        group(viewport)
            .on_event_capture(UiEventKind::PointerMove, move |_cx, payload| {
                if let UiEventPayload::PointerMove { pointer } = payload {
                    drag_state.try_update(|app| {
                        drag_scrollbars(app, viewport, pointer.point.x, pointer.point.y)
                    });
                }
            })
            .on_event_capture(UiEventKind::PointerUp, move |_cx, _payload| {
                up_state.try_update(finish_scrollbar_drag);
            })
            .child(render(
                PaneId::new(1),
                viewport,
                state,
                git_store,
                id,
                focus,
            ))
    });
    let mut session = UiSession::new();
    session.render_view(&view, viewport, UiScale::ONE);
    let state = exposed.lock().unwrap().clone().unwrap();
    let app = ApplicationContext::empty(Default::default());
    let thumb_point = Point::new(viewport.right - 4.0, 10.0);
    let thumb_id = session.tree().hit_test(thumb_point).unwrap().id;
    let idle_style = session.tree().node(&thumb_id).unwrap().style;
    assert_eq!(session.tree().cursor_at(thumb_point), None);
    assert_eq!(
        session
            .tree()
            .cursor_at(Point::new(viewport.right - 1.0, 100.0)),
        None
    );
    assert_eq!(
        session
            .tree()
            .cursor_at(Point::new(150.0, viewport.bottom - 4.0)),
        None
    );
    assert_eq!(
        session
            .tree()
            .cursor_at(Point::new(viewport.right - 4.0, viewport.bottom - 4.0)),
        None
    );
    assert_eq!(
        session.tree().cursor_at(Point::new(150.0, 10.0)),
        Some(CursorIcon::Text)
    );
    let events = session.handle_input(InputEvent::PointerMove(PointerData::mouse(thumb_point)));
    dispatch_runtime_output(
        events,
        &app,
        &lgui::window::WindowId::new("scrollbar-test"),
        |action| session.handle_default_action(action),
        |_| {},
    );
    session.render_view(&view, viewport, UiScale::ONE);
    let hover_style = session.tree().node(&thumb_id).unwrap().style;
    assert_ne!(hover_style, idle_style);
    assert_eq!(hover_style.fill, Some(theme::c().text_soft));
    let mut send = |input| {
        let events = session.handle_input(input);
        dispatch_runtime_output(
            events,
            &app,
            &lgui::window::WindowId::new("scrollbar-test"),
            |action| session.handle_default_action(action),
            |_| {},
        );
        session.render_view(&view, viewport, UiScale::ONE);
    };
    let pointer = |x, y| PointerData::mouse(Point::new(x, y));

    send(InputEvent::PointerDown {
        pointer: pointer(viewport.right - 4.0, 10.0),
        button: PointerButton::Left,
    });
    send(InputEvent::PointerMove(pointer(100.0, 260.0)));
    assert!(state.get().editor.editor_vertical_scrollbar_dragging);
    assert!(state.get().workspace.active_scroll().1 > 0.0);

    send(InputEvent::PointerUp {
        pointer: pointer(100.0, 260.0),
        button: PointerButton::Left,
    });
    assert!(!state.get().editor.editor_vertical_scrollbar_dragging);
    let released_scroll = state.get().workspace.active_scroll().1;

    send(InputEvent::PointerMove(pointer(100.0, 20.0)));
    assert_eq!(state.get().workspace.active_scroll().1, released_scroll);
}

#[test]
fn clipboard_cut_paste_and_undo_preserve_unicode_and_selection() {
    let mut app = document("你好 world");
    let clipboard = TestClipboard::default();
    app.workspace
        .active_editor_mut()
        .unwrap()
        .select_range(0..6);
    apply_command(&mut app, Command::Copy, &clipboard).unwrap();
    assert_eq!(clipboard.read_text().unwrap().as_deref(), Some("你好"));
    apply_command(&mut app, Command::Cut, &clipboard).unwrap();
    assert_eq!(app.workspace.active_editor().unwrap().text(), " world");
    apply_command(&mut app, Command::Undo, &clipboard).unwrap();
    assert_eq!(
        app.workspace.active_editor().unwrap().selected_text(),
        Some("你好")
    );
    clipboard.write_text("再见").unwrap();
    apply_command(&mut app, Command::Paste, &clipboard).unwrap();
    assert_eq!(app.workspace.active_editor().unwrap().text(), "再见 world");
}

#[test]
fn clipboard_failure_and_empty_clipboard_never_destroy_selection() {
    let mut app = document("keep this");
    app.workspace.active_editor_mut().unwrap().select_all();
    let unavailable = TestClipboard {
        fail: true,
        ..Default::default()
    };
    for command in [Command::Cut, Command::Copy, Command::Paste] {
        assert!(apply_command(&mut app, command, &unavailable).is_err());
        assert_eq!(
            app.workspace.active_editor().unwrap().selected_text(),
            Some("keep this")
        );
    }
    apply_command(&mut app, Command::Paste, &TestClipboard::default()).unwrap();
    assert_eq!(
        app.workspace.active_editor().unwrap().selected_text(),
        Some("keep this")
    );
    assert!(!app.workspace.active_editor().unwrap().can_undo());
}

#[test]
fn editing_commands_promote_a_preview_while_navigation_does_not() {
    let mut app = AppState::new();
    let preview = app
        .workspace
        .test_preview_view("preview.txt".into(), "hello".into());
    let clipboard = TestClipboard::default();

    apply_command(
        &mut app,
        Command::Move(normal::Movement::Right, false),
        &clipboard,
    )
    .unwrap();
    assert_eq!(app.workspace.preview(), Some(preview));
    apply_command(&mut app, Command::Undo, &clipboard).unwrap();
    assert_eq!(app.workspace.preview(), Some(preview));

    clipboard.write_text("!").unwrap();
    apply_command(&mut app, Command::Paste, &clipboard).unwrap();
    assert_ne!(app.workspace.preview(), Some(preview));
    assert_eq!(app.workspace.active_editor().unwrap().text(), "h!ello");
}

#[test]
fn a_failed_cut_does_not_promote_or_modify_a_preview() {
    let mut app = AppState::new();
    let preview = app
        .workspace
        .test_preview_view("preview.txt".into(), "hello".into());
    app.workspace.active_editor_mut().unwrap().select_all();
    let unavailable = TestClipboard {
        fail: true,
        ..Default::default()
    };

    assert!(apply_command(&mut app, Command::Cut, &unavailable).is_err());
    assert_eq!(app.workspace.preview(), Some(preview));
    assert_eq!(app.workspace.active_editor().unwrap().text(), "hello");
}

#[test]
fn tab_expansion_keeps_source_indices_and_visual_columns_in_sync() {
    let (display, mapping, width) = expand_tabs("a\t中\tb", 0);
    assert_eq!(display, "a   中  b");
    assert_eq!(mapping, vec![0, 1, 4, 5, 7, 8]);
    assert_eq!(width, 9);
    assert_eq!(
        fallback_hit_column("a\t中\tb", 5.9 * fonts::editor().char_w),
        3
    );
    assert_eq!(fallback_hit_column("👩‍💻x", 1.5 * fonts::editor().char_w), 3);
}

#[test]
fn held_pointer_scrolls_and_extends_selection_without_mouse_movement() {
    let mut app = document(&(0..100).map(|i| format!("line {i}\n")).collect::<String>());
    let rect = UiRect::new(0.0, 0.0, 400.0, 100.0);
    app.editor.drag = Some(DragSelection {
        document: app.workspace.active().unwrap(),
        origin: 0..0,
        unit: SelectionUnit::Character,
        point: (100.0, 150.0),
    });
    assert!(drag_scroll_tick(&mut app, rect));
    let first = app.workspace.active_scroll().1;
    let first_selection = app.workspace.active_editor().unwrap().selection().unwrap();
    assert!(drag_scroll_tick(&mut app, rect));
    assert!(app.workspace.active_scroll().1 > first);
    assert!(
        app.workspace
            .active_editor()
            .unwrap()
            .selection()
            .unwrap()
            .end
            > first_selection.end
    );
    app.editor.drag = None;
    assert!(!drag_scroll_tick(&mut app, rect));
}

#[test]
fn scrollbar_drag_uses_window_pointer_outside_the_scrollbar_strip() {
    let mut app = document(&(0..100).map(|i| format!("line {i}\n")).collect::<String>());
    let rect = UiRect::new(0.0, 0.0, 400.0, 100.0);
    app.editor.editor_vertical_scrollbar_dragging = true;
    app.editor.editor_vertical_scrollbar_drag_offset = 4.0;

    assert!(drag_scrollbars(&mut app, rect, 40.0, 500.0));
    let bottom = app.workspace.active_scroll().1;
    assert!(bottom > 0.0);

    assert!(drag_scrollbars(&mut app, rect, 40.0, rect.top));
    assert_eq!(app.workspace.active_scroll().1, 0.0);
    assert!(finish_scrollbar_drag(&mut app));
    assert!(!app.editor.editor_vertical_scrollbar_dragging);
    assert!(!finish_scrollbar_drag(&mut app));

    let mut app = document(&"x".repeat(1_000));
    app.editor.editor_horizontal_scrollbar_dragging = true;
    app.editor.editor_horizontal_scrollbar_drag_offset = 4.0;

    assert!(drag_scrollbars(&mut app, rect, 1_000.0, 40.0));
    assert!(app.workspace.active_scroll().0 > 0.0);
    assert!(drag_scrollbars(&mut app, rect, rect.left, 40.0));
    assert_eq!(app.workspace.active_scroll().0, 0.0);
    assert!(finish_scrollbar_drag(&mut app));
    assert!(!app.editor.editor_horizontal_scrollbar_dragging);
}

#[test]
fn gutter_drag_selects_whole_lines_in_both_directions() {
    let mut app = document("one\ntwo\nthree");
    let rect = UiRect::new(0.0, 0.0, 400.0, 300.0);
    app.editor.drag = Some(DragSelection {
        document: app.workspace.active().unwrap(),
        origin: 4..8,
        unit: SelectionUnit::Line,
        point: (5.0, 1.0),
    });
    update_drag_selection(&mut app, rect);
    assert_eq!(
        app.workspace.active_editor().unwrap().selected_text(),
        Some("one\ntwo\n")
    );
    app.editor.drag.as_mut().unwrap().point.1 = fonts::editor().line_h * 2.0 + 1.0;
    update_drag_selection(&mut app, rect);
    assert_eq!(
        app.workspace.active_editor().unwrap().selected_text(),
        Some("two\nthree")
    );
}

#[test]
fn switching_documents_cancels_drag_and_keeps_undo_histories_independent() {
    let mut app = document("first");
    let first = app.workspace.active().unwrap();
    normal::insert(&mut app.workspace.active_editor_mut().unwrap(), "A");
    app.editor.drag = Some(DragSelection {
        document: first,
        origin: 0..0,
        unit: SelectionUnit::Character,
        point: (100.0, 500.0),
    });
    app.workspace
        .test_open_view("second.txt".into(), "second".into());
    normal::insert(&mut app.workspace.active_editor_mut().unwrap(), "B");
    assert!(drag_scroll_tick(
        &mut app,
        UiRect::new(0.0, 0.0, 400.0, 100.0)
    ));
    assert!(app.editor.drag.is_none());
    app.workspace.active_editor_mut().unwrap().undo();
    assert_eq!(app.workspace.active_editor().unwrap().text(), "second");
    app.workspace.set_active(first);
    assert_eq!(app.workspace.active_editor().unwrap().text(), "Afirst");
    app.workspace.active_editor_mut().unwrap().undo();
    assert_eq!(app.workspace.active_editor().unwrap().text(), "first");
}

#[test]
fn a_late_composition_commit_lands_in_the_view_it_started_in() {
    let rect = UiRect::new(0.0, 0.0, 400.0, 300.0);
    let mut app = document("first");
    let first = app.workspace.active().unwrap();
    let left = app.workspace.active_pane();
    update_composition(&mut app, "ni", None);
    assert!(app.editor.preedit_for(left, first).is_some());

    let second = app
        .workspace
        .test_open_view("second.txt".into(), "second".into());
    assert!(app.editor.preedit_for(left, second).is_none());
    insert_text(&mut app, "你", rect);
    assert_eq!(app.workspace.active(), Some(second));
    assert_eq!(app.workspace.active_editor().unwrap().text(), "second");
    app.workspace.set_active(first);
    assert_eq!(app.workspace.active_editor().unwrap().text(), "你first");
    assert!(app.editor.ime.is_none());

    // Plain typing without a composition goes to the active view.
    insert_text(&mut app, "x", rect);
    assert_eq!(app.workspace.active_editor().unwrap().text(), "你xfirst");
}

#[test]
fn a_commit_for_a_closed_view_is_dropped() {
    let rect = UiRect::new(0.0, 0.0, 400.0, 300.0);
    let mut app = document("first");
    let first = app.workspace.active().unwrap();
    update_composition(&mut app, "ni", None);
    app.workspace
        .test_open_view("second.txt".into(), "second".into());
    app.workspace.test_close_document(first);
    insert_text(&mut app, "你", rect);
    assert_eq!(app.workspace.active_editor().unwrap().text(), "second");
}

#[test]
fn an_emptied_composition_moves_to_the_view_that_starts_the_next_one() {
    let mut app = document("first");
    let left = app.workspace.active_pane();
    let first = app.workspace.active().unwrap();
    update_composition(&mut app, "ni", None);
    update_composition(&mut app, "", None);
    let right = app
        .workspace
        .split(left, crate::model::pane_layout::Direction::Right)
        .unwrap();
    let copied = app.workspace.active().unwrap();
    assert_ne!(copied, first);
    update_composition(&mut app, "hao", None);
    assert!(app.editor.preedit_for(right, copied).is_some());
    assert!(app.editor.preedit_for(left, first).is_none());
}

#[test]
fn p1_late_ime_commit_targets_its_source_view_even_after_a_pane_move() {
    let mut app = document("abc");
    let left = app.workspace.active_pane();
    let source = app.workspace.active().unwrap();
    update_composition(&mut app, "ni", None);
    let right = app
        .workspace
        .split(left, crate::model::pane_layout::Direction::Right)
        .unwrap();
    let copied = app.workspace.active().unwrap();
    app.workspace.editor_mut(right).unwrap().set_cursor(3);
    app.workspace.move_item(left, source, right, 0);
    app.workspace.activate(right, copied);
    insert_text(&mut app, "你", UiRect::new(0.0, 0.0, 600.0, 400.0));
    assert_eq!(
        app.workspace.view(source).unwrap().source.selection.cursor,
        3
    );
    assert_eq!(
        app.workspace.view(copied).unwrap().source.selection.cursor,
        6
    );
    assert_eq!(app.workspace.active_editor().unwrap().text(), "你abc");
    assert_eq!(app.workspace.active(), Some(copied));
}

#[test]
fn p1_ime_commit_is_rejected_after_source_reader_source_round_trip() {
    use crate::domain::view::Presentation;
    let mut app = document("abc");
    let source = app.workspace.active().unwrap();
    update_composition(&mut app, "ni", None);
    app.workspace
        .set_presentation(source, Presentation::Markdown);
    app.workspace.set_presentation(source, Presentation::Source);
    insert_text(&mut app, "你", UiRect::new(0.0, 0.0, 600.0, 400.0));
    assert_eq!(app.workspace.active_editor().unwrap().text(), "abc");
    assert!(!app.workspace.active_editor().unwrap().can_undo());
}

#[test]
fn keyboard_reveal_scrolls_to_the_document_end() {
    let mut app = document(&"long line\n".repeat(100));
    app.workspace
        .active_editor_mut()
        .unwrap()
        .select_to(usize::MAX, false);
    reveal_cursor(&mut app, UiRect::new(0.0, 0.0, 400.0, 100.0));
    assert_eq!(
        app.workspace.active_scroll().1,
        101.0 * fonts::editor().line_h - 100.0
    );
}

#[test]
fn visible_range_keeps_only_rows_around_the_viewport() {
    assert_eq!(visible_line_range(240.0, 120.0, 100), 9..16);
}

#[test]
fn thumb_geometry_maps_the_full_scroll_range_to_the_track() {
    let start = thumb_geometry(10.0, 100.0, 100.0, 400.0, 0.0);
    let end = thumb_geometry(10.0, 100.0, 100.0, 400.0, 300.0);

    assert_eq!(
        start,
        ThumbGeometry {
            start: 10.0,
            length: 28.0
        }
    );
    assert_eq!(
        end,
        ThumbGeometry {
            start: 82.0,
            length: 28.0
        }
    );
}

#[test]
fn visual_columns_expand_tabs_and_wide_characters() {
    assert_eq!(visual_columns("a\tb"), 5);
    assert_eq!(visual_columns("a界b"), 4);
}

#[test]
fn pointer_line_accounts_for_vertical_scroll_and_clamps_to_document() {
    assert_eq!(
        line_index_from_point(100.0, 100.0, fonts::editor().line_h * 2.0, 10),
        2
    );
    assert_eq!(line_index_from_point(50.0, 100.0, 0.0, 10), 0);
    assert_eq!(line_index_from_point(900.0, 100.0, 0.0, 10), 9);
}

fn render(
    pane: crate::domain::identity::PaneId,
    rect: UiRect,
    state: AppContext,
    git: State<GitStoreSnapshot>,
    id: UiId,
    focus: lgui::core::UiFocusHandle,
) -> Element {
    let binding = crate::app::editor_view::binding(state, git, pane, rect);
    crate::ui::editor::view::render(pane, rect, binding, id, focus)
}

#[test]
fn editor_menu_executes_its_intent_and_keeps_editor_focus() {
    use lgui::application::{AppView, ApplicationContext};
    use lgui::core::{InputEvent, Point, PointerData, UiScale, dispatch_runtime_output};
    use lgui::session::UiSession;
    use std::sync::Arc;
    let exposed = Arc::new(Mutex::new(None::<AppContext>));
    let output = exposed.clone();
    let viewport = UiRect::new(0.0, 0.0, 500.0, 300.0);
    let app = ApplicationContext::empty(Default::default());
    let view: AppView = Arc::new(move |cx| {
        let state = AppContext::detached(cx.state(document("hello")));
        let git = cx.state(GitStoreSnapshot::default());
        *output.lock().unwrap() = Some(state.clone());
        let id = cx.use_stable_id();
        let focus = cx.focus_handle(id.clone());
        let scene = state.get();
        let mut root = group(viewport).child(render(
            PaneId::new(1),
            viewport,
            state.clone(),
            git,
            id,
            focus.clone(),
        ));
        if let Some(pos) = scene.editor.menu {
            root = root.child(crate::app::editor_view::menu(
                viewport, viewport, pos, state, focus,
            ));
        }
        root
    });
    let mut session = UiSession::new();
    session.render_view(&view, viewport, UiScale::ONE);
    let state = exposed.lock().unwrap().clone().unwrap();
    let mut click = |point, button| {
        for input in [
            InputEvent::PointerDown {
                pointer: PointerData::mouse(point),
                button,
            },
            InputEvent::PointerUp {
                pointer: PointerData::mouse(point),
                button,
            },
        ] {
            let events = session.handle_input(input);
            dispatch_runtime_output(
                events,
                &app,
                &lgui::window::WindowId::new("menu-test"),
                |action| session.handle_default_action(action),
                |_| {},
            );
            session.render_view(&view, viewport, UiScale::ONE);
        }
    };
    click(Point::new(100.0, 10.0), PointerButton::Right);
    assert_eq!(state.get().editor.menu, Some((100.0, 10.0)));
    // The final row is Select All; menu rows retain focus and emit a core command.
    click(
        Point::new(120.0, 10.0 + 4.0 + 5.0 * 28.0 + 14.0),
        PointerButton::Left,
    );
    let value = state.get();
    assert!(value.editor.menu.is_none());
    assert_eq!(
        value.workspace.active_editor().unwrap().selected_text(),
        Some("hello")
    );
    assert!(value.editor.focused);
}

/// A server's diagnostics are drawn as underlines in the real editor view, from the
/// server process to the scene tree.
#[test]
fn diagnostics_from_a_server_are_underlined_in_the_editor() {
    use lgui::{
        core::{InputEvent, Point, PointerData, UiScale, dispatch_runtime_output},
        session::UiSession,
    };
    let LanguageHarness {
        mut session,
        view,
        context,
        state,
        tasks,
        runtime,
        rect,
    } = LanguageHarness::new("fn a() {}\nsecond line\n", &["--diagnostics"]);
    // Make the services see the document, then wait for the server's answer.
    state.update(|app| app.editor.syntax_revision = app.editor.syntax_revision.wrapping_add(1));
    // The underline is a wave stroked in the error colour.
    let squiggle = |session: &UiSession| {
        session
            .tree()
            .nodes()
            .iter()
            .find(|node| {
                node.path.is_some()
                    && node
                        .path_style
                        .stroke
                        .is_some_and(|stroke| stroke.color == theme::c().error)
            })
            .map(|node| node.paint_bounds)
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let underline = loop {
        tasks.run_all();
        session.render_view(&view, rect, UiScale::ONE);
        if let Some(bounds) = squiggle(&session) {
            break bounds;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no underline was drawn; infos {:?}; services {}; docs {:?}",
            runtime
                .language_services()
                .map(|s| s.capability_sources(None)),
            runtime.language_services().is_some(),
            state
                .get()
                .workspace
                .documents
                .values()
                .map(|d| (d.is_text(), d.meta.path.clone(), d.meta.lang.name()))
                .collect::<Vec<_>>()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    // The scrollbar's ruler marks the line too.
    assert!(
        session.tree().nodes().iter().any(|node| {
            node.style == lgui::prelude::VisualStyle::filled(theme::c().error)
                && node.layout_rect.right == rect.right - 1.0
        }),
        "no ruler mark for the error"
    );
    // The pointer over the underlined text opens its message.
    let has_message = |session: &UiSession| {
        session
            .tree()
            .nodes()
            .iter()
            .any(|node| node.text.as_deref() == Some("fake error"))
    };
    assert!(!has_message(&session));
    let over = Point::new(
        (underline.left + underline.right) / 2.0,
        underline.top - 4.0,
    );
    let send = |session: &mut UiSession, input| {
        let events = session.handle_input(input);
        dispatch_runtime_output(
            events,
            &context,
            &lgui::window::WindowId::new("diagnostic-test"),
            |action| session.handle_default_action(action),
            |_| {},
        );
        session.render_view(&view, rect, UiScale::ONE);
    };
    send(
        &mut session,
        InputEvent::PointerMove(PointerData::mouse(over)),
    );
    assert!(has_message(&session), "no hover card over the underline");
    assert!(
        session
            .tree()
            .nodes()
            .iter()
            .any(|node| node.text.as_deref() == Some("fake")),
        "the card names the reporting tool"
    );
    send(
        &mut session,
        InputEvent::PointerMove(PointerData::mouse(Point::new(700.0, 300.0))),
    );
    assert!(
        !has_message(&session),
        "the card stays after the pointer left"
    );
}

use std::sync::Arc;

/// What a language-server feature test needs: the real editor view over a real
/// fake-server process, with a way to pump the UI and move the pointer.
struct LanguageHarness {
    session: lgui::session::UiSession,
    view: lgui::application::AppView,
    context: lgui::application::ApplicationContext,
    state: AppContext,
    tasks: crate::app::test_support::UiTasks,
    runtime: Arc<crate::app::runtime::AppRuntime>,
    rect: UiRect,
}

impl LanguageHarness {
    fn new(source: &str, server_args: &[&str]) -> Self {
        Self::configured(source, server_args, |_| {})
    }

    /// Like `new`, with the person's settings changed by `configure` first.
    fn configured(
        source: &str,
        server_args: &[&str],
        configure: impl FnOnce(&mut AppState),
    ) -> Self {
        use lgui::{
            application::{AppView, ApplicationContext},
            core::UiScale,
            session::UiSession,
        };
        let context = ApplicationContext::empty(Default::default());
        let fonts_handle = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        std::mem::forget(fonts_handle);
        fonts::set(&fonts::FontSettings::EDITOR, &fonts::FontSettings::TERMINAL);
        let mut app = AppState::new();
        let name = format!(
            "loog-language-ui-{}-{}.rs",
            std::process::id(),
            LANGUAGE_CASES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        app.workspace
            .open_path(std::env::temp_dir().join(name), source.into());
        app.editor.focused = true;
        let mut args: Vec<String> = ["--sync", "object"].map(String::from).into();
        args.extend(server_args.iter().map(|a| a.to_string()));
        configure(&mut app);
        let rect = UiRect::new(0.0, 0.0, 800.0, 400.0);
        let initial = Arc::new(Mutex::new(Some(app)));
        let exposed = Arc::new(Mutex::new(None));
        let output = exposed.clone();
        let runtime = Arc::new(crate::app::runtime::AppRuntime::default());
        let view_runtime = runtime.clone();
        let tasks = crate::app::test_support::UiTasks::default();
        let application = tasks.application();
        runtime.set_application(application.clone());
        let view: AppView = Arc::new(move |cx| {
            let state =
                AppContext::detached(cx.state_with(|| initial.lock().unwrap().take().unwrap()))
                    .with_runtime(&view_runtime);
            if view_runtime.markdown().is_none() {
                view_runtime.install_markdown(crate::app::markdown_service::MarkdownService::new(
                    state.clone(),
                    application.clone(),
                    Arc::new(TestClipboard::default()),
                ));
            }
            if view_runtime.highlighter().is_none() {
                view_runtime.install_highlighter(crate::app::syntax::Highlighter::new(
                    state.clone(),
                    application.clone(),
                ));
                view_runtime.install_language_services(
                    crate::app::language_services::LanguageServices::new(),
                );
            }
            let git = cx.state_with(GitStoreSnapshot::default);
            *output.lock().unwrap() = Some(state.clone());
            let id = UiId::new("language-ui-test");
            let focus = cx.focus_handle(id.clone());
            render(PaneId::new(1), rect, state, git, id, focus)
        });
        let mut session = UiSession::new();
        session.render_view(&view, rect, UiScale::ONE);
        let state: AppContext = exposed.lock().unwrap().clone().unwrap();
        crate::app::test_support::language::install(&state, &runtime, &args);
        state.update(|app| app.editor.syntax_revision = app.editor.syntax_revision.wrapping_add(1));
        Self {
            session,
            view,
            context,
            state,
            tasks,
            runtime,
            rect,
        }
    }

    fn render(&mut self) {
        self.tasks.run_all();
        self.session
            .render_view(&self.view, self.rect, lgui::core::UiScale::ONE);
    }

    fn send(&mut self, input: lgui::core::InputEvent) {
        let events = self.session.handle_input(input);
        lgui::core::dispatch_runtime_output(
            events,
            &self.context,
            &lgui::window::WindowId::new("language-ui-test"),
            |action| self.session.handle_default_action(action),
            |_| {},
        );
        self.render();
    }

    fn move_to(&mut self, x: f32, y: f32) {
        use lgui::core::{InputEvent, Point, PointerData};
        self.send(InputEvent::PointerMove(PointerData::mouse(Point::new(
            x, y,
        ))));
    }

    fn has_text(&self, wanted: &str) -> bool {
        self.session.tree().nodes().iter().any(|node| {
            node.text.as_deref().or_else(|| {
                node.semantics
                    .as_ref()
                    .and_then(|semantics| semantics.value.as_deref())
            }) == Some(wanted)
        })
    }

    /// Whether a list row shows `label`: its matched and unmatched parts are
    /// drawn as separate runs, so the label is read from them joined.
    fn has_label(&self, label: &str) -> bool {
        self.session
            .tree()
            .nodes()
            .iter()
            .filter_map(|node| node.text.as_deref())
            .collect::<String>()
            .contains(label)
    }

    /// Keeps rendering until `check` passes, or fails with `what`.
    fn until(&mut self, what: &str, mut check: impl FnMut(&mut Self) -> bool) {
        self.until_for(what, std::time::Duration::from_secs(15), &mut check);
    }

    fn until_for(
        &mut self,
        what: &str,
        timeout: std::time::Duration,
        mut check: impl FnMut(&mut Self) -> bool,
    ) {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            self.render();
            if check(self) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}; infos {:?}; client {:?}; completion {:?}",
                self.runtime
                    .language_services()
                    .map(|s| s.capability_sources(None)),
                self.runtime.extensions().map(|host| host
                    .service()
                    .package("example.language-fixture")
                    .map(|package| (package.last_error.clone(), package.logs.clone()))),
                self.completion_labels()
            );
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
    }

    /// Waits until a server is ready and holds the document.
    fn until_ready(&mut self) {
        self.until("the server to be ready", |h| {
            h.state
                .get()
                .extension_widgets
                .roots()
                .iter()
                .any(|root| root.properties.label.as_deref() == Some("Fixture: Ready"))
        });
    }

    /// The screen position of the character at `column` of `line`.
    fn point_at(&self, line: usize, column: usize) -> (f32, f32) {
        let lines = self
            .state
            .get()
            .workspace
            .active_editor()
            .map_or(1, |buffer| buffer.line_count());
        let code_left =
            self.rect.left + fonts::editor().gutter_w_for_lines(lines) + theme::CODE_PAD;
        (
            code_left + (column as f32 + 0.5) * fonts::editor().char_w,
            self.rect.top + (line as f32 + 0.5) * fonts::editor().line_h,
        )
    }
}

static LANGUAGE_CASES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[path = "signature_help_tests.rs"]
mod signature_help;

#[path = "snippet_choice_tests.rs"]
mod snippet_choice;

#[path = "code_action_lightbulb_tests.rs"]
mod code_action_lightbulb;

#[test]
fn an_explicit_hover_retries_the_same_symbol_after_content_modified() {
    use crate::features::keymap::action::Action;
    let log = std::env::temp_dir().join(format!("loog-hover-retry-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&log);
    let log_arg = log.display().to_string();
    let mut h = LanguageHarness::new(
        "fn alpha() {}\n",
        &[
            "--features",
            "hover",
            "--feature-error",
            "--do-not-retrigger",
            "--log",
            &log_arg,
        ],
    );
    h.until_ready();
    h.set_cursor(5);
    h.act(Action::ShowHover);
    h.until("the first hover", |h| {
        server_log(h, &log)
            .iter()
            .filter(|m| *m == "textDocument/hover")
            .count()
            == 1
    });
    h.act(Action::ShowHover);
    h.until("the explicit retry", |h| {
        server_log(h, &log)
            .iter()
            .filter(|m| *m == "textDocument/hover")
            .count()
            == 2
    });
    assert!(h.state.get().editor.language.hover.card.is_none());
    let _ = std::fs::remove_file(log);
}

#[test]
fn resting_the_pointer_on_a_word_shows_what_the_server_says_about_it() {
    let mut h = LanguageHarness::new(
        "fn alpha() {}



last line
",
        &["--features", "hover", "--diagnostics"],
    );
    h.until_ready();
    let (x, y) = h.point_at(0, 1);
    h.move_to(x, y);
    assert!(!h.has_text("0:1"), "nothing before the pointer has rested");
    // The middle of "fn" is column 1 of line 0.
    h.until("the hover card", |h| h.has_text("0:1"));
    assert!(h.has_text("fake"), "the server's text is on the card");
    // Moving to another word replaces the card; leaving the text removes it.
    let (x, y) = h.point_at(4, 2);
    h.move_to(x, y);
    assert!(!h.has_text("0:1"), "the old card went with the pointer");
    h.until("the new hover card", |h| h.has_text("4:2"));
    h.move_to(790.0, 390.0);
    assert!(!h.has_text("4:2"), "the card stays after the pointer left");
    // Typing hides the card too.
    let (x, y) = h.point_at(0, 1);
    h.move_to(x, y);
    h.until("the card again", |h| h.has_text("0:1"));
    h.state.update(|app| {
        crate::app::editor::insert_text(app, "x", UiRect::new(0.0, 0.0, 800.0, 400.0));
    });
    h.render();
    assert!(!h.has_text("0:1"), "typing hides the card");
}

#[test]
fn dragging_across_the_hover_card_selects_its_text_and_copy_takes_it() {
    use lgui::core::{InputEvent, Point, PointerButton, PointerData};
    let mut h = LanguageHarness::new("fn alpha() {}\n", &["--features", "hover", "--diagnostics"]);
    h.until_ready();
    let (x, y) = h.point_at(0, 1);
    h.move_to(x, y);
    h.until("the hover card", |h| {
        h.has_text("0:1") && h.has_text("fake")
    });
    let rect_of = |h: &LanguageHarness, wanted: &str| {
        h.session
            .tree()
            .nodes()
            .iter()
            .find(|node| {
                node.text.as_deref().or_else(|| {
                    node.semantics
                        .as_ref()
                        .and_then(|semantics| semantics.value.as_deref())
                }) == Some(wanted)
            })
            .map(|node| node.layout_rect)
            .expect("the card text is drawn")
    };
    let (first, last) = (rect_of(&h, "0:1"), rect_of(&h, "fake"));
    let middle = |r: UiRect| (r.top + r.bottom) / 2.0;
    // Onto the card first, then press at the start of the first word and
    // drag to the end of the last.
    h.move_to(first.left + 1.0, middle(first));
    let at = |x, y| PointerData::mouse(Point::new(x, y));
    h.send(InputEvent::PointerDown {
        pointer: at(first.left + 1.0, middle(first)),
        button: PointerButton::Left,
    });
    h.send(InputEvent::PointerMove(at(last.right - 1.0, middle(last))));
    // Dragging out of the card keeps it, so the selection can be finished.
    h.send(InputEvent::PointerMove(at(790.0, 390.0)));
    assert!(
        h.has_text("0:1"),
        "the card stays while a drag from it is held"
    );
    h.send(InputEvent::PointerMove(at(last.right - 1.0, middle(last))));
    h.send(InputEvent::PointerUp {
        pointer: at(last.right - 1.0, middle(last)),
        button: PointerButton::Left,
    });
    let selected = h
        .state
        .get()
        .editor
        .language
        .hover
        .selection
        .clone()
        .expect("the card text is selected");
    assert!(
        selected.starts_with("0:1") && selected.contains("fake"),
        "selected {selected:?}"
    );
    assert!(h.has_text("0:1"), "selecting keeps the card");

    // Copy takes the selection, not the editor's.
    let clipboard = std::sync::Arc::new(TestClipboard::default());
    h.state.update(|app| {
        app.editor.language.hover.selection = Some(selected.clone());
    });
    crate::app::editor::execute_command(
        &h.state,
        loog_editor_core::commands::Command::Copy,
        h.rect,
        clipboard.clone(),
    );
    assert_eq!(
        clipboard.text.lock().unwrap().as_deref(),
        Some(selected.as_str())
    );
}
#[test]
fn a_server_without_hover_leaves_the_pointer_alone() {
    let mut h = LanguageHarness::new("fn alpha() {}\n", &["--features", "definition"]);
    h.until_ready();
    let (x, y) = h.point_at(0, 1);
    h.move_to(x, y);
    std::thread::sleep(std::time::Duration::from_millis(500));
    h.render();
    assert!(!h.has_text("0:1"));
    assert!(h.state.get().editor.language.hover.card.is_none());
}

impl LanguageHarness {
    /// Performs a keymap action the way the key handler does.
    fn act(&mut self, action: crate::features::keymap::action::Action) {
        crate::app::language_ui::keys::perform_with(&self.state, self.rect, action);
        self.render();
    }

    fn set_cursor(&mut self, offset: usize) {
        self.state.update(|app| {
            app.workspace
                .active_editor_mut()
                .unwrap()
                .set_cursor(offset)
        });
        self.render();
    }

    fn selection(&self) -> Option<std::ops::Range<usize>> {
        self.state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .selection()
    }

    fn cursor(&self) -> usize {
        self.state.get().workspace.active_editor().unwrap().cursor()
    }

    fn popup_labels(&self) -> Vec<String> {
        self.state
            .get()
            .editor
            .language
            .popup
            .as_ref()
            .map(|p| {
                p.shown
                    .iter()
                    .map(|(at, _)| p.items[*at].label.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn until_popup(&mut self, what: &str) {
        self.until(what, |h| h.state.get().editor.language.popup.is_some());
    }
}

const NAV_SOURCE: &str = "fn alpha() {}\nsecond line\nthird\n";

#[test]
fn go_to_definition_jumps_selects_and_goes_back() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new(NAV_SOURCE, &["--features", "definition"]);
    h.until_ready();
    h.set_cursor(15);
    h.act(Action::GoToDefinition);
    h.until("the jump", |h| h.selection() == Some(0..1));
    h.act(Action::GoBack);
    h.until("coming back", |h| h.cursor() == 15);
    h.act(Action::GoForward);
    h.until("going forward again", |h| h.cursor() == 1);
}

#[test]
fn a_server_without_definitions_says_so() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new(NAV_SOURCE, &["--features", "hover"]);
    h.until_ready();
    h.act(Action::GoToDefinition);
    h.until("the message", |h| {
        h.state
            .get()
            .chrome
            .toast
            .as_ref()
            .is_some_and(|toast| toast.message.contains("No language server"))
    });
    assert!(h.state.get().editor.language.popup.is_none());
}

#[test]
fn definition_peek_keeps_the_document_and_history_until_opened_and_rejects_old_events() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new(NAV_SOURCE, &["--features", "definition"]);
    h.until_ready();
    h.set_cursor(15);
    h.act(Action::PeekDefinition);
    h.until("definition preview", |h| {
        crate::app::language_ui::peek::view(&h.state.get(), PaneId::new(1)).is_some()
    });
    assert_eq!(h.cursor(), 15);
    assert!(!h.state.get().editor.language.history.can_go_back());
    let old = crate::app::language_ui::peek::view(&h.state.get(), PaneId::new(1))
        .unwrap()
        .revision;
    h.act(Action::ClosePeek);
    crate::app::language_ui::peek::jump(&h.state, old, h.rect);
    assert_eq!(h.cursor(), 15);
    h.act(Action::PeekDefinition);
    h.until("new preview", |h| {
        h.state.get().editor.language.peek.is_some()
    });
    h.act(Action::OpenPeekTarget);
    h.until("preview target", |h| h.selection() == Some(0..1));
    h.act(Action::GoBack);
    h.until("preview history", |h| h.cursor() == 15);
}

#[test]
fn definition_peek_expires_on_edits_and_late_definition_responses_do_not_navigate() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new(
        NAV_SOURCE,
        &["--features", "definition", "--slow-ms", "180"],
    );
    h.until_ready();
    h.set_cursor(15);
    h.act(Action::GoToDefinition);
    h.set_cursor(10);
    std::thread::sleep(std::time::Duration::from_millis(250));
    h.render();
    assert_eq!(h.cursor(), 10);
    h.act(Action::PeekDefinition);
    h.until("preview", |h| h.state.get().editor.language.peek.is_some());
    h.type_text("x");
    assert!(h.state.get().editor.language.peek.is_none());
}

#[test]
fn navigation_history_uses_line_and_column_after_a_prior_line_changes_length() {
    use crate::features::keymap::action::Action;
    let source = "// comment\nfn alpha() {}\n";
    let mut h = LanguageHarness::new(source, &["--features", "definition"]);
    h.until_ready();
    let before = source.find("alpha").unwrap() + 2;
    h.set_cursor(before);
    h.act(Action::GoToDefinition);
    h.until("definition jump", |h| h.selection() == Some(0..1));
    h.state.update(|app| {
        let mut editor = app.workspace.active_editor_mut().unwrap();
        editor.set_cursor(0);
        loog_editor_core::normal::insert(&mut editor, "😀");
    });
    h.act(Action::GoBack);
    h.until("line column history", |h| h.cursor() == before + "😀".len());
}

#[test]
fn peeking_an_unopened_target_does_not_create_a_tab_and_dismissal_invalidates_queued_reads() {
    use crate::app::language_ui::{navigate::Place, peek};
    let mut h = LanguageHarness::new(NAV_SOURCE, &["--features", "definition"]);
    h.until_ready();
    let path = std::env::temp_dir().join(format!(
        "loog-peek-{}-{}.rs",
        std::process::id(),
        LANGUAGE_CASES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&path, "fn target() {}\n").unwrap();
    let tabs = h.state.get().workspace.documents.len();
    let cursor = h.cursor();
    peek::open(&h.state, Place::bytes(path.clone(), 3..9));
    h.state.update(peek::close);
    std::thread::sleep(std::time::Duration::from_millis(40));
    h.render();
    assert!(h.state.get().editor.language.peek.is_none());
    peek::open(&h.state, Place::bytes(path.clone(), 3..9));
    h.until("unopened preview", |h| {
        h.state.get().editor.language.peek.is_some()
    });
    assert_eq!(h.state.get().workspace.documents.len(), tabs);
    assert_eq!(h.cursor(), cursor);
    h.act(crate::features::keymap::action::Action::OpenPeekTarget);
    h.until("opened preview target", |h| {
        h.state.get().workspace.active_path() == Some(path.as_path())
    });
    assert_eq!(h.state.get().workspace.documents.len(), tabs + 1);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn references_open_a_list_whose_rows_go_to_their_places() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new(NAV_SOURCE, &["--features", "references"]);
    h.until_ready();
    h.act(Action::FindAllReferences);
    h.until_popup("the references");
    h.render();
    assert_eq!(
        h.popup_labels().len(),
        3,
        "one file group and two references"
    );
    assert!(h.has_text("2 references"));
    h.act(Action::MenuSelectNext);
    h.act(Action::MenuConfirm);
    // The second reference is line 1, columns 2 to 4.
    h.until("the jump", |h| h.selection() == Some(16..18));
    assert!(
        h.state
            .get()
            .editor
            .language
            .popup
            .as_ref()
            .is_some_and(
                |p| p.kind == crate::app::language_ui::popup::Kind::References && !p.panel_focused
            )
    );
}

#[test]
fn escape_closes_the_list_without_going_anywhere() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new(NAV_SOURCE, &["--features", "references"]);
    h.until_ready();
    h.set_cursor(15);
    h.act(Action::FindAllReferences);
    h.until_popup("the references");
    h.act(Action::MenuCancel);
    assert!(h.state.get().editor.language.popup.is_none());
    assert_eq!(h.cursor(), 15);
}

#[test]
fn the_outline_filters_as_you_type_and_goes_to_the_symbol() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new(NAV_SOURCE, &["--features", "symbols"]);
    h.until_ready();
    h.act(Action::ToggleOutline);
    h.until_popup("the outline");
    assert_eq!(h.popup_labels(), ["main"]);
    assert!(crate::app::language_ui::keys::type_text(
        &h.state, "zz", h.rect
    ));
    assert!(h.popup_labels().is_empty(), "nothing matches zz");
    h.act(Action::Backspace);
    h.act(Action::Backspace);
    assert!(crate::app::language_ui::keys::type_text(
        &h.state, "ma", h.rect
    ));
    assert_eq!(h.popup_labels(), ["main"]);
    h.act(Action::MenuConfirm);
    h.until("the jump", |h| h.selection() == Some(3..7));
}

#[test]
fn the_workspace_symbol_search_asks_again_as_the_text_changes() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new(NAV_SOURCE, &["--features", "workspaceSymbols"]);
    h.until_ready();
    h.act(Action::ToggleProjectSymbols);
    h.until("the first answer", |h| h.popup_labels() == ["sym-"]);
    assert!(crate::app::language_ui::keys::type_text(
        &h.state, "ab", h.rect
    ));
    h.until("the answer for ab", |h| h.popup_labels() == ["sym-ab"]);
}

#[test]
fn problems_are_visited_in_order_and_wrap() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new(NAV_SOURCE, &["--diagnostics"]);
    h.until_ready();
    h.until("diagnostics", |h| {
        let app = h.state.get();
        let doc = app
            .workspace
            .documents
            .values()
            .find(|d| d.is_text())
            .unwrap();
        let id = *app.workspace.documents.keys().next().unwrap();
        h.runtime
            .language_services()
            .and_then(|s| s.results(&doc.meta.path, &doc.stamp(id)).diagnostics)
            .is_some_and(|found| !found.is_empty())
    });
    h.set_cursor(10);
    h.act(Action::GoToDiagnostic);
    assert_eq!(h.cursor(), 0, "after the last problem comes the first");
    h.set_cursor(10);
    h.act(Action::GoToPreviousDiagnostic);
    assert_eq!(h.cursor(), 0);
}

impl LanguageHarness {
    fn type_text(&mut self, text: &str) {
        let rect = self.rect;
        for ch in text.chars() {
            self.state.update(|app| {
                crate::app::editor::insert_text(app, &ch.to_string(), rect);
            });
            self.render();
        }
    }

    fn document_text(&self) -> String {
        self.state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .text()
            .to_owned()
    }

    fn completion_labels(&self) -> Vec<String> {
        let app = self.state.get();
        app.editor
            .language
            .completion
            .as_ref()
            .map(|s| {
                s.shown
                    .iter()
                    .map(|shown| s.data.entries[shown.entry].item.label.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[test]
fn typing_opens_a_completion_list_that_narrows_with_more_typing() {
    let mut h = LanguageHarness::new("\n", &["--features", "completion"]);
    h.until_ready();
    h.type_text("a");
    h.until("the list", |h| h.completion_labels().len() == 2);
    h.render();
    assert!(h.has_label("alpha-fake"));
    // More typing narrows it without another round trip.
    h.type_text("l");
    assert_eq!(h.completion_labels(), ["alpha-fake"]);
    // Typing something that matches nothing ends it.
    h.type_text("zz");
    assert!(h.completion_labels().is_empty());
    assert!(h.state.get().editor.language.completion.is_none());
}

#[test]
fn completion_refresh_can_shrink_reorder_or_empty_the_candidates() {
    use crate::features::keymap::action::Action;
    let file = std::env::temp_dir().join(format!(
        "loog-completion-refresh-{}-{}.json",
        std::process::id(),
        LANGUAGE_CASES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let write = |labels: &[&str]| {
        std::fs::write(&file, serde_json::to_vec(&serde_json::json!({
            "isIncomplete": true,
            "items": labels.iter().map(|label| serde_json::json!({"label": label, "kind": 6})).collect::<Vec<_>>()
        })).unwrap()).unwrap();
    };
    write(&[
        "alpha0", "alpha1", "alpha2", "alpha3", "alpha4", "alpha5", "alpha6",
    ]);
    let file_arg = file.display().to_string();
    let mut h = LanguageHarness::new(
        "\n",
        &[
            "--features",
            "completion",
            "--completion-items-file",
            &file_arg,
        ],
    );
    h.until_ready();
    h.type_text("a");
    h.until("seven candidates", |h| h.completion_labels().len() == 7);
    h.act(Action::MenuSelectPrevious);
    assert_eq!(
        h.state
            .get()
            .editor
            .language
            .completion
            .as_ref()
            .unwrap()
            .selected,
        6
    );
    // This used to index entry 6 in the new three-item list while refiltering.
    write(&["alpha9", "alpha6", "alpha8"]);
    h.type_text("l");
    h.until("three refreshed candidates", |h| {
        h.completion_labels() == ["alpha6", "alpha8", "alpha9"]
    });
    assert_eq!(
        h.state
            .get()
            .editor
            .language
            .completion
            .as_ref()
            .unwrap()
            .selected,
        0
    );
    assert!(h.has_label("alpha6"));
    write(&[]);
    h.act(Action::ShowCompletions);
    h.until("the empty refresh", |h| h.completion_labels().is_empty());
    assert_eq!(h.document_text(), "al\n");
    write(&["alpha6", "alpha8", "alpha9"]);
    h.act(Action::ShowCompletions);
    h.until("fresh completion after an empty reply", |h| {
        h.completion_labels().len() == 3
    });
    h.act(Action::MenuConfirm);
    assert_eq!(h.document_text(), "alpha6\n");
    let _ = std::fs::remove_file(file);
}

#[test]
fn enter_accepts_the_highlighted_item_in_place_of_the_typed_word() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new("x = \n", &["--features", "completion"]);
    h.until_ready();
    h.set_cursor(4);
    h.type_text("al");
    h.until("the list", |h| h.completion_labels() == ["alpha-fake"]);
    h.act(Action::MenuConfirm);
    assert_eq!(h.document_text(), "x = alpha-fake\n");
    assert!(h.state.get().editor.language.completion.is_none());
    assert_eq!(h.cursor(), 14);
}

#[test]
fn a_snippet_item_leaves_its_stops_to_fill_in() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new("\n", &["--features", "completion"]);
    h.until_ready();
    h.type_text("be");
    h.until("the list", |h| h.completion_labels() == ["beta"]);
    h.act(Action::MenuConfirm);
    assert_eq!(h.document_text(), "beta()\n");
    assert_eq!(
        h.cursor(),
        5,
        "the caret is on the first stop, between the brackets"
    );
    assert!(h.state.get().editor.language.snippet.is_some());
    h.type_text("1, 2");
    assert_eq!(h.document_text(), "beta(1, 2)\n");
    h.act(Action::NextSnippetTabstop);
    assert_eq!(
        h.cursor(),
        10,
        "the final stop is after the closing bracket"
    );
    assert!(h.state.get().editor.language.snippet.is_none());
}

#[test]
fn escape_closes_the_list_and_leaves_the_text_alone() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new("\n", &["--features", "completion"]);
    h.until_ready();
    h.type_text("a");
    h.until("the list", |h| !h.completion_labels().is_empty());
    h.act(Action::MenuCancel);
    assert!(h.completion_labels().is_empty());
    assert_eq!(h.document_text(), "a\n");
}

#[test]
fn the_completion_key_asks_even_with_nothing_typed() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new("\n", &["--features", "completion"]);
    h.until_ready();
    h.act(Action::ShowCompletions);
    h.until("the list", |h| h.completion_labels().len() == 2);
}

#[test]
fn a_trigger_character_opens_the_list_without_a_word() {
    let mut h = LanguageHarness::new("x\n", &["--features", "completion"]);
    h.until_ready();
    h.set_cursor(1);
    h.type_text(".");
    h.until("the list", |h| h.completion_labels().len() == 2);
    // Typing a letter narrows it, as for any list.
    h.type_text("b");
    assert_eq!(h.completion_labels(), ["beta"]);
}

#[test]
fn the_highlighted_item_shows_its_documentation_once_the_server_resolves_it() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new("\n", &["--features", "completion"]);
    h.until_ready();
    h.type_text("a");
    h.until("the list", |h| h.completion_labels().len() == 2);
    // Down to "beta", whose documentation the server only gives on request.
    h.act(Action::MenuSelectNext);
    h.until("the documentation", |h| h.has_text("resolved"));
}

#[test]
fn a_server_without_completion_never_opens_a_list() {
    let mut h = LanguageHarness::new("\n", &["--features", "hover"]);
    h.until_ready();
    h.type_text("abc");
    std::thread::sleep(std::time::Duration::from_millis(200));
    h.render();
    assert!(h.state.get().editor.language.completion.is_none());
}

fn server_log(h: &LanguageHarness, path: &std::path::Path) -> Vec<String> {
    let _ = h;
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|message| message["method"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn renaming_asks_for_a_name_and_applies_the_servers_edit() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new("fn alpha() {}\n", &["--features", "rename"]);
    h.until_ready();
    h.set_cursor(4);
    h.act(Action::Rename);
    h.until_popup("the rename box");
    let query = |h: &LanguageHarness| {
        h.state
            .get()
            .editor
            .language
            .popup
            .as_ref()
            .and_then(|p| p.query.clone())
    };
    // The server's placeholder is what the box starts with.
    assert_eq!(query(&h).as_deref(), Some("name"));
    for _ in 0..4 {
        h.act(Action::Backspace);
    }
    assert!(crate::app::language_ui::keys::type_text(
        &h.state, "renamed", h.rect
    ));
    h.act(Action::MenuConfirm);
    h.until("the rename", |h| h.document_text().starts_with("renamed"));
    assert!(h.state.get().editor.language.popup.is_none());
}

#[test]
fn a_position_that_cannot_be_renamed_says_so() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new("fn alpha() {}\n", &["--features", "hover"]);
    h.until_ready();
    h.act(Action::Rename);
    h.until("the message", |h| h.state.get().chrome.toast.is_some());
    assert!(h.state.get().editor.language.popup.is_none());
}

#[test]
fn code_actions_are_listed_and_the_chosen_ones_edit_or_resolve_first() {
    use crate::features::keymap::action::Action;
    let log = std::env::temp_dir().join(format!("loog-actions-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&log);
    let log_arg = log.display().to_string();
    let mut h = LanguageHarness::new(
        "fn alpha() {}\n",
        &["--features", "codeAction,execute", "--log", &log_arg],
    );
    h.until_ready();
    h.act(Action::ToggleCodeActions);
    h.until_popup("the actions");
    let labels = h.popup_labels();
    assert_eq!(labels.len(), 2);
    assert!(
        labels[0].contains("Fix it"),
        "the preferred fix first: {labels:?}"
    );
    // The second needs resolving, which brings a command to run.
    h.act(Action::MenuSelectNext);
    h.act(Action::MenuConfirm);
    h.until("the command", |h| {
        server_log(h, &log).contains(&"workspace/executeCommand".to_owned())
    });
    assert!(server_log(&h, &log).contains(&"codeAction/resolve".to_owned()));
    // The first has its edit with it.
    h.act(Action::ToggleCodeActions);
    h.until_popup("the actions again");
    h.act(Action::MenuConfirm);
    h.until("the fix", |h| h.document_text().starts_with("// fixed\n"));
}

#[test]
fn formatting_applies_the_servers_edits_and_undo_takes_them_back() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new("fn alpha() {}\n", &["--features", "formatting"]);
    h.until_ready();
    h.act(Action::Format);
    h.until("the formatting", |h| {
        h.document_text().starts_with("// formatted\n")
    });
    h.state.update(|app| {
        let rect = UiRect::new(0.0, 0.0, 800.0, 400.0);
        let _ = rect;
        app.workspace.active_editor_mut().unwrap().undo();
    });
    assert_eq!(h.document_text(), "fn alpha() {}\n");
}

#[test]
fn typing_an_opening_bracket_shows_the_signature_and_closing_it_hides_it() {
    let mut h = LanguageHarness::new("f\n", &["--features", "signature"]);
    h.until_ready();
    h.set_cursor(1);
    h.type_text("(");
    h.until("the signature", |h| {
        h.state
            .get()
            .editor
            .language
            .signature
            .as_ref()
            .is_some_and(|s| s.help.is_some())
    });
    h.render();
    // The active parameter (the second) is drawn on its own.
    assert!(h.has_text("b: i32"));
    h.type_text("1, 2)");
    assert!(h.state.get().editor.language.signature.is_none());
}

#[test]
fn escape_closes_the_signature_help() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new("f\n", &["--features", "signature"]);
    h.until_ready();
    h.set_cursor(1);
    h.act(Action::ShowSignatureHelp);
    h.until("the signature", |h| {
        h.state
            .get()
            .editor
            .language
            .signature
            .as_ref()
            .is_some_and(|s| s.help.is_some())
    });
    let rect = h.rect;
    h.state.update(|app| {
        assert!(crate::app::language_ui::dismiss(app));
        let _ = rect;
    });
    assert!(h.state.get().editor.language.signature.is_none());
}

#[test]
fn the_other_occurrences_of_a_symbol_are_tinted_once_the_caret_rests() {
    let mut h = LanguageHarness::new("ab\nab\n", &["--features", "highlight"]);
    h.until_ready();
    h.set_cursor(1);
    h.until("the tint", |h| {
        h.state.get().editor.language.highlights.shown.is_some()
    });
    let ranges = h
        .state
        .get()
        .editor
        .language
        .highlights
        .shown
        .as_ref()
        .unwrap()
        .ranges
        .to_vec();
    assert_eq!(ranges, [0..2, 3..5]);
    // Typing makes the old tint stale.
    h.type_text("x");
    h.render();
    let pane = h.state.get().workspace.active_pane();
    let app = h.state.get();
    let id = app.workspace.active().unwrap();
    let document = app.workspace.view_document(id).unwrap();
    let stamp = app.workspace.documents[&document].stamp(document);
    assert!(
        app.editor
            .language
            .highlights
            .for_view(pane, &stamp)
            .is_none()
    );
}

#[test]
fn growing_the_selection_walks_out_through_the_servers_ranges_and_shrinking_walks_back() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new("abcdefghij\n", &["--features", "selection"]);
    h.until_ready();
    h.set_cursor(1);
    h.act(Action::SelectLargerSyntaxNode);
    h.until("the first growth", |h| h.selection() == Some(1..2));
    h.act(Action::SelectLargerSyntaxNode);
    assert_eq!(h.selection(), Some(0..9));
    h.act(Action::SelectLargerSyntaxNode);
    assert_eq!(h.selection(), Some(0..9), "nothing larger");
    h.act(Action::SelectSmallerSyntaxNode);
    assert_eq!(h.selection(), Some(1..2));
    h.act(Action::SelectSmallerSyntaxNode);
    assert_eq!(h.selection(), None, "back to the caret");
}

fn server_responses(path: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| message.get("method").is_none() && message.get("result").is_some())
        .collect()
}

#[test]
fn a_server_can_edit_the_document_and_hear_that_it_was_done() {
    use crate::features::keymap::action::Action;
    let log = std::env::temp_dir().join(format!("loog-apply-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&log);
    let log_arg = log.display().to_string();
    let mut h = LanguageHarness::new(
        "fn alpha() {}\n",
        &[
            "--features",
            "codeAction,execute",
            "--exec-edit",
            "--log",
            &log_arg,
        ],
    );
    h.until_ready();
    h.act(Action::ToggleCodeActions);
    h.until_popup("the actions");
    // The second action resolves to a command; running it makes the server edit.
    h.act(Action::MenuSelectNext);
    h.act(Action::MenuConfirm);
    h.until("the server's edit", |h| {
        h.document_text().starts_with("// by server\n")
    });
    h.until("the server to hear back", |_| {
        server_responses(&log)
            .iter()
            .any(|r| r["id"] == 9300 && r["result"]["applied"] == true)
    });
}

#[test]
fn a_server_can_ask_for_a_document_to_be_shown() {
    use crate::features::keymap::action::Action;
    let log = std::env::temp_dir().join(format!("loog-show-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&log);
    let log_arg = log.display().to_string();
    let mut h = LanguageHarness::new(
        "fn alpha() {}\nsecond\n",
        &[
            "--features",
            "codeAction,execute",
            "--exec-show",
            "--log",
            &log_arg,
        ],
    );
    h.until_ready();
    h.act(Action::ToggleCodeActions);
    h.until_popup("the actions");
    h.act(Action::MenuSelectNext);
    h.act(Action::MenuConfirm);
    // The server selects the first three characters of the second line.
    h.until("the selection", |h| h.selection() == Some(14..17));
    h.until("the answer", |_| {
        server_responses(&log)
            .iter()
            .any(|r| r["id"] == 9301 && r["result"]["success"] == true)
    });
}

fn on_save(kinds: &[&str], format: bool) -> impl FnOnce(&mut AppState) + use<> {
    let kinds: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
    move |app: &mut AppState| {
        app.preferences.values_mut().languages.insert(
            "rust".to_owned(),
            crate::features::settings::language::LanguageSetting {
                format_on_save: format,
                providers: [("formatting".into(), vec!["example.language-fixture".into()])].into(),
                code_actions_on_save: kinds,
            },
        );
    }
}

#[test]
fn saving_formats_and_applies_the_chosen_actions_first() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::configured(
        "fn alpha() {}\n",
        &["--features", "formatting,codeAction"],
        on_save(&["quickfix"], true),
    );
    h.until_ready();
    h.act(Action::Save);
    let path = h.state.get().workspace.active_path().unwrap().to_path_buf();
    // The actions come first, then the formatting goes in front of them.
    h.until("the file on disk", |_| {
        std::fs::read_to_string(&path)
            .is_ok_and(|text| text == "// formatted\n// fixed\nfn alpha() {}\n")
    });
    assert_eq!(h.document_text(), "// formatted\n// fixed\nfn alpha() {}\n");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_server_that_does_not_answer_never_keeps_the_file_from_being_saved() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::configured(
        "fn alpha() {}\n",
        &["--features", "formatting", "--ignore-requests"],
        on_save(&[], true),
    );
    h.until_ready();
    h.act(Action::Save);
    let path = h.state.get().workspace.active_path().unwrap().to_path_buf();
    h.until("the file on disk", |_| {
        std::fs::read_to_string(&path).is_ok_and(|text| text == "fn alpha() {}\n")
    });
    assert_eq!(
        h.document_text(),
        "fn alpha() {}\n",
        "unformatted but saved"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_language_without_save_settings_saves_without_waiting_for_the_server() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new("fn alpha() {}\n", &["--features", "formatting"]);
    h.until_ready();
    h.act(Action::Save);
    let path = h.state.get().workspace.active_path().unwrap().to_path_buf();
    h.until("the background save", |h| {
        h.state.get().editor.saving.pending == 0
            && std::fs::read_to_string(&path).is_ok_and(|t| t == "fn alpha() {}\n")
    });
    let _ = std::fs::remove_file(&path);
}

#[test]
fn automatic_completion_can_be_turned_off_and_the_key_still_works() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::configured("\n", &["--features", "completion"], |app| {
        app.preferences
            .values_mut()
            .language_features
            .completion
            .auto = false
    });
    h.until_ready();
    h.type_text("a");
    std::thread::sleep(std::time::Duration::from_millis(200));
    h.render();
    assert!(h.state.get().editor.language.completion.is_none());
    h.act(Action::ShowCompletions);
    h.until("the list", |h| !h.completion_labels().is_empty());
}

#[test]
fn without_a_server_the_words_of_the_file_complete() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new("alphabet beta alphanumeric\n", &["--features", "hover"]);
    h.until_ready();
    h.set_cursor(27);
    h.type_text("alp");
    h.until("the words", |h| h.completion_labels().len() == 2);
    assert_eq!(h.completion_labels(), ["alphabet", "alphanumeric"]);
    h.act(Action::MenuConfirm);
    assert!(h.document_text().ends_with("alphabet\n") || h.document_text().contains("alphabet"));
}

#[test]
fn enter_alone_can_be_the_key_that_accepts() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::configured("\n", &["--features", "completion"], |app| {
        app.preferences
            .values_mut()
            .language_features
            .completion
            .accept_on = "enter".to_owned()
    });
    h.until_ready();
    h.type_text("al");
    h.until("the list", |h| h.completion_labels() == ["alpha-fake"]);
    // Tab is not an accepting key now: the list stays and nothing is inserted by it.
    let handled =
        crate::app::language_ui::keys::perform_with(&h.state, h.rect, Action::MenuConfirmTab);
    assert_eq!(handled, Some(false));
    assert_eq!(h.document_text(), "al\n");
    h.act(Action::MenuConfirm);
    assert_eq!(h.document_text(), "alpha-fake\n");
}

#[test]
fn callers_are_listed_and_choosing_one_goes_to_the_call() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new("fn f() {\n    f();\n}\n", &["--features", "callHierarchy"]);
    h.until_ready();
    h.set_cursor(3);
    h.act(Action::ShowCallHierarchy);
    h.until_popup("the callers");
    assert_eq!(h.popup_labels(), ["f"]);
    h.act(Action::MenuConfirm);
    // The server says the call is at line 1, column 0 to 1.
    h.until("the jump", |h| h.selection() == Some(9..10));
}

#[test]
fn supertypes_and_outgoing_calls_use_their_own_requests() {
    use crate::features::keymap::action::Action;
    let log = std::env::temp_dir().join(format!("loog-hier-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&log);
    let log_arg = log.display().to_string();
    let mut h = LanguageHarness::new(
        "fn f() {}\n",
        &[
            "--features",
            "callHierarchy,typeHierarchy",
            "--log",
            &log_arg,
        ],
    );
    h.until_ready();
    h.act(Action::ShowSupertypes);
    h.until_popup("the supertypes");
    h.act(Action::MenuCancel);
    h.act(Action::ShowOutgoingCalls);
    h.until_popup("the callees");
    let methods = server_log(&h, &log);
    assert!(methods.contains(&"typeHierarchy/supertypes".to_owned()));
    assert!(methods.contains(&"callHierarchy/outgoingCalls".to_owned()));
}

#[test]
fn code_lenses_with_commands_are_listed_and_run() {
    use crate::features::keymap::action::Action;
    let log = std::env::temp_dir().join(format!("loog-lens-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&log);
    let log_arg = log.display().to_string();
    let mut h = LanguageHarness::new(
        "fn f() {}\n",
        &["--features", "codeLens,execute", "--log", &log_arg],
    );
    h.until_ready();
    h.act(Action::ShowCodeLenses);
    h.until_popup("the lenses");
    assert_eq!(h.popup_labels(), ["Run test"]);
    h.act(Action::MenuConfirm);
    h.until("the command", |h| {
        server_log(h, &log).contains(&"workspace/executeCommand".to_owned())
    });
}

#[test]
fn the_problems_of_the_open_files_are_listed_with_the_worst_first() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::new("fn f() {}\nsecond\n", &["--diagnostics"]);
    h.until_ready();
    h.until("diagnostics", |h| {
        let app = h.state.get();
        let id = *app.workspace.documents.keys().next().unwrap();
        let doc = &app.workspace.documents[&id];
        h.runtime
            .language_services()
            .and_then(|s| s.results(&doc.meta.path, &doc.stamp(id)).diagnostics)
            .is_some_and(|items| !items.is_empty())
    });
    h.act(Action::ToggleProblems);
    h.until_popup("the problems");
    assert_eq!(h.popup_labels().last().unwrap(), "fake error");
    assert!(
        h.state
            .get()
            .editor
            .language
            .popup
            .as_ref()
            .unwrap()
            .view()
            .panel
    );
    h.act(Action::MenuConfirm);
    h.until("the jump", |h| h.selection() == Some(0..2));
    assert!(
        h.state
            .get()
            .editor
            .language
            .popup
            .as_ref()
            .is_some_and(|p| !p.panel_focused)
    );
}

#[test]
fn lazy_code_lenses_resolve_before_their_command_runs() {
    use crate::features::keymap::action::Action;
    let log = std::env::temp_dir().join(format!(
        "loog-lazy-lens-{}.log",
        LANGUAGE_CASES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let log_arg = log.display().to_string();
    let mut h = LanguageHarness::new(
        "fn f() {}\n",
        &[
            "--features",
            "codeLens,execute",
            "--lazy-lens",
            "--log",
            &log_arg,
        ],
    );
    h.until_ready();
    h.act(Action::ShowCodeLenses);
    h.until_popup("lazy lens");
    assert_eq!(h.popup_labels(), ["Resolve code lens…"]);
    h.act(Action::MenuConfirm);
    h.until("resolved command", |h| {
        server_log(h, &log).contains(&"workspace/executeCommand".into())
    });
    let calls = server_log(&h, &log);
    assert!(
        calls.iter().position(|m| m == "codeLens/resolve").unwrap()
            < calls
                .iter()
                .position(|m| m == "workspace/executeCommand")
                .unwrap()
    );
    assert_eq!(h.document_text(), "fn f() {}\n");
    let _ = std::fs::remove_file(log);
}

#[test]
fn async_navigation_does_not_open_a_target_after_the_source_cursor_moves() {
    use crate::app::language_ui::navigate::{self, Place};
    let mut h = LanguageHarness::new(NAV_SOURCE, &["--features", "definition"]);
    h.until_ready();
    let path = std::env::temp_dir().join(format!(
        "loog-stale-navigation-{}.rs",
        LANGUAGE_CASES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&path, "fn target() {}\n").unwrap();
    let tabs = h.state.get().workspace.documents.len();
    navigate::go(&h.state, Place::bytes(path.clone(), 3..9), h.rect, true);
    h.set_cursor(2);
    std::thread::sleep(std::time::Duration::from_millis(40));
    h.render();
    assert_eq!(h.state.get().workspace.documents.len(), tabs);
    assert_eq!(h.cursor(), 2);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn a_hint_moves_what_follows_it_and_clicks_still_find_the_source_character() {
    let context = lgui::application::ApplicationContext::empty(Default::default());
    let _fonts = lgui::backend::install_text_environment(
        &context,
        lgui::render_skia::skia_text_system_handle(),
    );
    fonts::set(&fonts::FontSettings::EDITOR, &fonts::FontSettings::TERMINAL);
    let plain = layout_line_at_column("abcd", 0.0, 0.0, 800.0, 0).unwrap();
    let hinted =
        crate::ui::editor::view::layout_line_hinted("abcd", &[(2, "XXX")], 0.0, 0.0, 800.0, 0)
            .unwrap();
    let width = fonts::editor().char_w;
    let x = |layout: &crate::ui::editor::view::LineLayout, i| caret_x(Some(layout), i).unwrap();
    // Before the hint nothing moves; from the character it belongs to on, it
    // moves by the hint's width.
    for i in 0..2 {
        assert!((x(&hinted, i) - x(&plain, i)).abs() < 0.5, "{i}");
    }
    for i in 2..=4 {
        assert!(
            (x(&hinted, i) - x(&plain, i) - 3.0 * width).abs() < 1.0,
            "{i}"
        );
    }
    // The hint's own place is between 'b' and 'c'.
    let hint_x = hinted.hint_x(0).unwrap();
    assert!((hint_x - x(&plain, 2)).abs() < 1.0);
    // A click on the hint lands next to it, on a character boundary of the source.
    let on_hint = crate::ui::editor::view::hit_column(&hinted, hint_x + width, 5.0);
    assert!(on_hint == 1 || on_hint == 2, "{on_hint}");
    // A click on the characters after it finds them where they now are.
    assert_eq!(
        crate::ui::editor::view::hit_column(&hinted, x(&hinted, 3) + 0.01, 5.0),
        3
    );
}

#[test]
fn inlay_hints_are_requested_after_the_text_settles_and_shown_inside_the_line() {
    let mut h = LanguageHarness::configured("let x = 1;\n", &["--features", "inlay"], |app| {
        app.preferences.values_mut().language_features.inlay_hints = true
    });
    h.until_ready();
    h.until("the hints", |h| {
        h.state
            .get()
            .editor
            .language
            .inlay
            .shown
            .as_ref()
            .is_some_and(|shown| !shown.hints.is_empty())
    });
    h.render();
    assert!(h.has_text(": i32"));
    // Typing makes the old hints stale at once.
    h.type_text("y");
    h.render();
    assert!(!h.has_text(": i32"));
}

#[test]
fn hints_are_off_unless_asked_for() {
    let mut h = LanguageHarness::new("let x = 1;\n", &["--features", "inlay"]);
    h.until_ready();
    std::thread::sleep(std::time::Duration::from_millis(500));
    h.render();
    assert!(h.state.get().editor.language.inlay.shown.is_none());
    assert!(!h.has_text(": i32"));
}

#[test]
fn typing_a_trigger_character_lets_the_server_format() {
    let mut h = LanguageHarness::new("let x = 1\n", &["--features", "onType"]);
    h.until_ready();
    h.set_cursor(9);
    h.type_text(";");
    h.until("the formatting", |h| {
        h.document_text().starts_with("// after ;\n")
    });
    assert_eq!(h.document_text(), "// after ;\nlet x = 1;\n");
}

#[test]
fn the_language_extension_owns_its_ready_status_color_and_text() {
    let mut h = LanguageHarness::new("x\n", &["--features", "hover,definition,rename"]);
    h.until_ready();
    let roots = h.state.get().extension_widgets.roots();
    let status = roots
        .iter()
        .find(|node| node.origin.owner.as_ref() == "example.language-fixture")
        .unwrap();
    assert_eq!(status.properties.background, Some(0x10b981));
    assert_eq!(status.properties.label.as_deref(), Some("Fixture: Ready"));
    assert_eq!(
        status
            .children
            .iter()
            .find(|node| node.properties.id.as_ref() == "language-status-text")
            .unwrap()
            .properties
            .text
            .as_deref(),
        Some("Fixture")
    );
    assert!(
        !h.runtime
            .language_services()
            .unwrap()
            .capability_sources(None)
            .is_empty()
    );
}

#[test]
fn granting_workspace_trust_again_restarts_the_relevant_language_owner() {
    let mut h = LanguageHarness::new("fn alpha() {}\n", &["--features", "hover"]);
    h.until_ready();
    let previous = h
        .runtime
        .language_services()
        .unwrap()
        .capability_sources(None)[0]
        .run;
    let root = std::env::temp_dir();
    crate::app::trust_prompt::decide(&h.state, &root, false);
    h.until("trust revocation stops the client", |h| {
        !h.state
            .get()
            .extension_widgets
            .roots()
            .iter()
            .any(|root| root.properties.label.as_deref() == Some("Fixture: Ready"))
    });
    crate::app::trust_prompt::decide(&h.state, &root, true);
    h.until_ready();
    assert!(
        h.runtime
            .language_services()
            .unwrap()
            .capability_sources(None)
            .iter()
            .any(|source| source.run > previous && source.providers.hover)
    );
}

#[test]
fn the_language_extension_updates_progress_and_reclaims_its_status_on_stop() {
    let mut h = LanguageHarness::new("fn alpha() {}\n", &["--progress", "--progress-cancellable"]);
    h.until("extension progress status", |h| {
        h.state.get().extension_widgets.roots().iter().any(|root| {
            root.properties.background == Some(0x818cf8)
                && root.children.iter().any(|node| {
                    node.properties.text.as_deref() == Some("Fixture: Loading project 3/10 30%")
                })
        })
    });
    let app = h.state.get();
    let file = app.workspace.active_document().unwrap();
    let document = &app.workspace.documents[&file];
    let path = document.meta.path.clone();
    let text = document.buffer.text_snapshot();
    drop(app);
    crate::app::test_support::language::saved(&h.state, path, text);
    h.until_ready();
    h.runtime.extensions().unwrap().stop_all(&h.state);
    h.until("status reclaimed with its owner", |h| {
        h.state.get().extension_widgets.roots().is_empty()
    });
}

#[test]
fn typed_delimiters_and_ime_commits_take_distinct_paths_through_editor_events() {
    use lgui::application::{AppView, ApplicationContext};
    use lgui::core::{ImeEvent, InputEvent, Point, PointerData, UiScale, dispatch_runtime_output};
    use lgui::session::UiSession;
    use std::sync::Arc;
    let exposed = Arc::new(Mutex::new(None::<AppContext>));
    let output = exposed.clone();
    let rect = UiRect::new(0.0, 0.0, 600.0, 300.0);
    let application = ApplicationContext::empty(Default::default());
    let view: AppView = Arc::new(move |cx| {
        let mut initial = document("");
        initial.editor.focused = true;
        let state = AppContext::detached(cx.state(initial));
        let git = cx.state(GitStoreSnapshot::default());
        *output.lock().unwrap() = Some(state.clone());
        let id = UiId::new("pair-editor-test");
        let focus = cx.focus_handle(id.clone());
        render(PaneId::new(1), rect, state, git, id, focus)
    });
    let mut session = UiSession::new();
    session.render_view(&view, rect, UiScale::ONE);
    let state = exposed.lock().unwrap().clone().unwrap();
    let mut send = |input| {
        dispatch_runtime_output(
            session.handle_input(input),
            &application,
            &lgui::window::WindowId::new("pair-test"),
            |action| session.handle_default_action(action),
            |_| {},
        );
        session.render_view(&view, rect, UiScale::ONE);
    };
    let pointer = PointerData::mouse(Point::new(
        fonts::editor().gutter_w() + theme::CODE_PAD + 1.0,
        5.0,
    ));
    send(InputEvent::PointerDown {
        pointer,
        button: PointerButton::Left,
    });
    send(InputEvent::PointerUp {
        pointer,
        button: PointerButton::Left,
    });
    send(InputEvent::Ime(ImeEvent::Commit("{".into())));
    assert_eq!(state.get().workspace.active_editor().unwrap().text(), "{}");
    send(InputEvent::Ime(ImeEvent::Preedit {
        text: "中文".into(),
        cursor: None,
    }));
    send(InputEvent::Ime(ImeEvent::Commit("中文()".into())));
    assert_eq!(
        state.get().workspace.active_editor().unwrap().text(),
        "{中文()}"
    );
    send(InputEvent::Ime(ImeEvent::Commit("}".into())));
    assert_eq!(
        state.get().workspace.active_editor().unwrap().text(),
        "{中文()}"
    );
    assert_eq!(
        state.get().workspace.active_editor().unwrap().cursor(),
        "{中文()}".len()
    );
}

#[test]
fn document_tab_width_changes_paint_geometry_hits_and_ime_columns_together() {
    use loog_editor_core::text::EditingOptions;
    let context = lgui::application::ApplicationContext::empty(Default::default());
    let _fonts = lgui::backend::install_text_environment(
        &context,
        lgui::render_skia::skia_text_system_handle(),
    );
    let rect = UiRect::new(0.0, 0.0, 800.0, 300.0);
    let left = fonts::editor().gutter_w() + theme::CODE_PAD;
    let mut positions = Vec::new();
    for width in [2, 4, 8] {
        let mut buffer = TextBuffer::new("\t中文\te\u{301}😀");
        buffer.configure(EditingOptions {
            tab_width: width,
            ..Default::default()
        });
        let metrics = scroll_metrics(rect, left, &buffer);
        let x = document_caret(
            &buffer,
            0,
            1,
            left,
            0.0,
            rect.right,
            (0.0, metrics.viewport_w),
        );
        positions.push(x);
        assert_eq!(
            cursor_offset_from_point(&buffer, rect, left, metrics, 0.0, 0.0, x + 0.01, 5.0),
            1
        );
        for column in [1, 2, 3, 4, 6] {
            let at = buffer.offset_at_char_column(0, column);
            let x = document_caret(
                &buffer,
                0,
                column,
                left,
                0.0,
                rect.right,
                (0.0, metrics.viewport_w),
            );
            assert_eq!(
                cursor_offset_from_point(&buffer, rect, left, metrics, 0.0, 0.0, x + 0.01, 5.0),
                buffer.grapheme_floor(at)
            );
        }
    }
    assert!(positions[0] < positions[1] && positions[1] < positions[2]);
}

#[test]
fn line_shortcuts_dispatch_through_real_source_editor_and_undo_in_order() {
    use lgui::application::{AppView, ApplicationContext};
    use lgui::core::{InputEvent, NamedKey, Point, PointerData, UiScale, dispatch_runtime_output};
    use lgui::session::UiSession;
    use std::sync::Arc;
    let exposed = Arc::new(Mutex::new(None::<AppContext>));
    let output = exposed.clone();
    let rect = UiRect::new(0.0, 0.0, 600.0, 300.0);
    let application = ApplicationContext::empty(Default::default());
    let environment = application.clone();
    let view: AppView = Arc::new(move |cx| {
        let mut initial = document("one\ntwo\n");
        initial.editor.focused = true;
        let id = initial.workspace.active_document().unwrap();
        initial.workspace.documents.get_mut(&id).unwrap().meta.lang =
            crate::domain::language::builtin("rust");
        let state = AppContext::detached(cx.state(initial));
        let git = cx.state(GitStoreSnapshot::default());
        *output.lock().unwrap() = Some(state.clone());
        let id = UiId::new("line-shortcut-test");
        let focus = cx.focus_handle(id.clone());
        let env = crate::app::key_actions::KeyEnv {
            state: state.clone(),
            git_store: git.clone(),
            application: environment.clone(),
            editor_focus: focus.clone(),
            editor_rect: rect,
            terminal: None,
        };
        crate::app::key_actions::attach(
            group(rect).child(render(PaneId::new(1), rect, state, git, id, focus)),
            env,
        )
    });
    let mut session = UiSession::new();
    session.render_view(&view, rect, UiScale::ONE);
    let state = exposed.lock().unwrap().clone().unwrap();
    let mut send = |input| {
        dispatch_runtime_output(
            session.handle_input(input),
            &application,
            &lgui::window::WindowId::new("line-shortcuts"),
            |a| session.handle_default_action(a),
            |_| {},
        );
        session.render_view(&view, rect, UiScale::ONE);
    };
    let pointer = PointerData::mouse(Point::new(
        fonts::editor().gutter_w() + theme::CODE_PAD + 1.0,
        fonts::editor().line_h + 5.0,
    ));
    send(InputEvent::PointerDown {
        pointer,
        button: PointerButton::Left,
    });
    send(InputEvent::PointerUp {
        pointer,
        button: PointerButton::Left,
    });
    let key = |key, modifiers| {
        InputEvent::Keyboard(KeyboardEvent {
            state: KeyState::Down,
            key,
            modifiers,
            ..Default::default()
        })
    };
    send(key(LogicalKey::Named(NamedKey::ArrowUp), KeyModifiers::ALT));
    assert_eq!(
        state.get().workspace.active_editor().unwrap().text(),
        "two\none\n"
    );
    send(key(
        LogicalKey::Named(NamedKey::ArrowDown),
        KeyModifiers::ALT | KeyModifiers::SHIFT,
    ));
    assert_eq!(
        state.get().workspace.active_editor().unwrap().text(),
        "two\ntwo\none\n"
    );
    let secondary = if cfg!(target_os = "macos") {
        KeyModifiers::META
    } else {
        KeyModifiers::CONTROL
    };
    send(key(LogicalKey::Character("/".into()), secondary));
    assert_eq!(
        state.get().workspace.active_editor().unwrap().text(),
        "two\n// two\none\n"
    );
    send(key(
        LogicalKey::Character("k".into()),
        secondary | KeyModifiers::SHIFT,
    ));
    assert_eq!(
        state.get().workspace.active_editor().unwrap().text(),
        "two\none\n"
    );
    for expected in [
        "two\n// two\none\n",
        "two\ntwo\none\n",
        "two\none\n",
        "one\ntwo\n",
    ] {
        send(key(LogicalKey::Character("z".into()), secondary));
        assert_eq!(
            state.get().workspace.active_editor().unwrap().text(),
            expected
        );
    }
}

#[test]
fn reading_aids_render_only_when_enabled_and_do_not_modify_source() {
    use lgui::{application::AppView, core::UiScale, session::UiSession};
    use std::sync::Arc;
    let exposed = Arc::new(Mutex::new(None::<AppContext>));
    let output = exposed.clone();
    let rect = UiRect::new(0.0, 0.0, 800.0, 300.0);
    let view: AppView = Arc::new(move |cx| {
        let mut initial = document("\t  call([中]);\n");
        initial.preferences.values_mut().editor.show_whitespace = true;
        let id = initial.workspace.active_document().unwrap();
        initial.workspace.documents.get_mut(&id).unwrap().meta.lang =
            crate::domain::language::builtin("rust");
        crate::app::editing::configure_all(&mut initial);
        initial.workspace.active_editor_mut().unwrap().set_cursor(8);
        let state = AppContext::detached(cx.state(initial));
        let git = cx.state(GitStoreSnapshot::default());
        *output.lock().unwrap() = Some(state.clone());
        let id = UiId::new("reading-aids-test");
        let focus = cx.focus_handle(id.clone());
        render(PaneId::new(1), rect, state, git, id, focus)
    });
    let mut session = UiSession::new();
    session.render_view(&view, rect, UiScale::ONE);
    let count = |session: &UiSession, name: &str| {
        session
            .tree()
            .nodes()
            .iter()
            .filter(|n| {
                n.semantics
                    .as_ref()
                    .is_some_and(|s| s.name.as_deref() == Some(name))
            })
            .count()
    };
    assert_eq!(count(&session, "Matching bracket"), 2);
    assert!(count(&session, "Indent guide") > 0);
    for mark in ["→", "·", "¬"] {
        assert!(
            session
                .tree()
                .nodes()
                .iter()
                .any(|n| n.text.as_deref() == Some(mark)),
            "{mark}"
        );
    }
    let state = exposed.lock().unwrap().clone().unwrap();
    state.update(|app| {
        let settings = &mut app.preferences.values_mut().editor;
        settings.show_whitespace = false;
        settings.match_brackets = false;
        settings.indent_guides = false;
        crate::app::editing::configure_all(app);
    });
    session.render_view(&view, rect, UiScale::ONE);
    assert_eq!(count(&session, "Matching bracket"), 0);
    assert_eq!(count(&session, "Indent guide"), 0);
    assert!(
        !session
            .tree()
            .nodes()
            .iter()
            .any(|n| n.text.as_deref() == Some("¬"))
    );
    assert_eq!(
        state.get().workspace.active_editor().unwrap().text(),
        "\t  call([中]);\n"
    );
    assert!(!state.get().workspace.active_editor().unwrap().can_undo());
}

#[test]
fn soft_wrap_keeps_paint_hits_navigation_selection_ime_and_resize_in_agreement() {
    use lgui::{
        application::{AppView, ApplicationContext},
        core::{
            ImeEvent, InputEvent, NamedKey, Point, PointerData, UiScale, dispatch_runtime_output,
        },
        session::UiSession,
    };
    use loog_editor_core::text::{Assoc, Selection};
    use std::sync::Arc;
    let exposed = Arc::new(Mutex::new(None::<AppContext>));
    let output = exposed.clone();
    let bounds = Arc::new(Mutex::new(UiRect::new(0.0, 0.0, 220.0, 300.0)));
    let layout = bounds.clone();
    let application = ApplicationContext::empty(Default::default());
    let environment = application.clone();
    let _fonts = lgui::backend::install_text_environment(
        &application,
        lgui::render_skia::skia_text_system_handle(),
    );
    let source = format!("{}\r\nsecond", "a中😀e\u{301}\t".repeat(30));
    let initial_text = source.clone();
    let view: AppView = Arc::new(move |cx| {
        let rect = *layout.lock().unwrap();
        let mut initial = document(&initial_text);
        initial.editor.focused = true;
        initial.preferences.values_mut().editor.soft_wrap = true;
        initial.preferences.values_mut().smooth_caret = false;
        crate::app::editing::configure_all(&mut initial);
        let state = AppContext::detached(cx.state(initial));
        let git = cx.state(GitStoreSnapshot::default());
        *output.lock().unwrap() = Some(state.clone());
        let id = UiId::new("soft-wrap-editor");
        let focus = cx.focus_handle(id.clone());
        let env = crate::app::key_actions::KeyEnv {
            state: state.clone(),
            git_store: git.clone(),
            application: environment.clone(),
            editor_focus: focus.clone(),
            editor_rect: rect,
            terminal: None,
        };
        crate::app::key_actions::attach(
            group(rect).child(render(PaneId::new(1), rect, state, git, id, focus)),
            env,
        )
    });
    let mut session = UiSession::new();
    let rect = *bounds.lock().unwrap();
    session.render_view(&view, rect, UiScale::ONE);
    let state = exposed.lock().unwrap().clone().unwrap();
    let send = |session: &mut UiSession, input| {
        dispatch_runtime_output(
            session.handle_input(input),
            &application,
            &lgui::window::WindowId::new("soft-wrap"),
            |a| session.handle_default_action(a),
            |_| {},
        );
        let viewport = *bounds.lock().unwrap();
        session.render_view(&view, viewport, UiScale::ONE);
    };
    let get_map = || crate::app::editing::active_projection(&state.get(), *bounds.lock().unwrap());
    let map = get_map();
    assert!(map.wrapped());
    assert!(map.len() > 10);
    let left = fonts::editor().gutter_w() + theme::CODE_PAD;
    for row in 0..5 {
        let app = state.get();
        let buffer = app.workspace.active_editor().unwrap();
        let source_row = map.row(&buffer, row);
        let metrics = crate::ui::editor::projection::metrics(rect, left, &buffer, &map);
        assert_eq!(metrics.max_x, 0.0);
        let (x, y) = crate::ui::editor::projection::caret(
            &buffer,
            &map,
            Selection::caret(source_row.range.start),
            left,
            rect.top,
            0.0,
            metrics.viewport_w,
        );
        assert!(x < rect.right);
        drop(app);
        let pointer = PointerData::mouse(Point::new(x + 0.1, y + 4.0));
        send(
            &mut session,
            InputEvent::PointerDown {
                pointer,
                button: PointerButton::Left,
            },
        );
        send(
            &mut session,
            InputEvent::PointerUp {
                pointer,
                button: PointerButton::Left,
            },
        );
        assert_eq!(
            state.get().workspace.active_editor().unwrap().cursor(),
            source_row.range.start,
            "row {row}"
        );
    }
    let key = |key, modifiers| {
        InputEvent::Keyboard(KeyboardEvent {
            state: KeyState::Down,
            key: LogicalKey::Named(key),
            modifiers,
            ..Default::default()
        })
    };
    send(
        &mut session,
        key(NamedKey::ArrowDown, KeyModifiers::empty()),
    );
    let app = state.get();
    let ed = app.workspace.active_editor().unwrap();
    let row = map.caret_row(&ed, ed.cursor(), ed.selection_state().wrap_affinity);
    assert_eq!(row, 5);
    drop(app);
    send(&mut session, key(NamedKey::End, KeyModifiers::empty()));
    let before = state.get().workspace.active_editor().unwrap().cursor();
    assert_eq!(
        state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .selection_state()
            .wrap_affinity,
        Assoc::Before
    );
    send(
        &mut session,
        key(NamedKey::ArrowRight, KeyModifiers::empty()),
    );
    assert_eq!(
        state.get().workspace.active_editor().unwrap().cursor(),
        before
    );
    assert_eq!(
        state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .selection_state()
            .wrap_affinity,
        Assoc::After
    );
    send(&mut session, key(NamedKey::ArrowDown, KeyModifiers::SHIFT));
    assert!(
        state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .selection()
            .is_some()
    );
    send(
        &mut session,
        InputEvent::Ime(ImeEvent::Preedit {
            text: "zhong".into(),
            cursor: Some(0..5),
        }),
    );
    let ime = ime_cursor_rect(&state.get(), rect);
    assert!(ime.top >= rect.top && ime.bottom <= rect.bottom);
    send(
        &mut session,
        InputEvent::Ime(ImeEvent::Commit("中文".into())),
    );
    assert!(
        state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .text()
            .contains("中文")
    );
    assert!(
        state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .text()
            .ends_with("\r\nsecond")
    );
    for width in [180.0, 420.0, 220.0] {
        *bounds.lock().unwrap() = UiRect::new(0.0, 0.0, width, 300.0);
        let rect = *bounds.lock().unwrap();
        session.render_view(&view, rect, UiScale::ONE);
        let app = state.get();
        let buffer = app.workspace.active_editor().unwrap();
        let map = get_map();
        let metrics = crate::ui::editor::projection::metrics(rect, left, &buffer, &map);
        let at = map.row(&buffer, 2).range.start;
        let (x, y) = crate::ui::editor::projection::caret(
            &buffer,
            &map,
            Selection::caret(at),
            left,
            0.0,
            0.0,
            metrics.viewport_w,
        );
        assert_eq!(
            crate::ui::editor::projection::hit(
                &buffer,
                &map,
                rect,
                left,
                metrics,
                0.0,
                0.0,
                x + 0.1,
                y + 4.0
            )
            .0,
            at
        );
    }
    let previous = state
        .get()
        .workspace
        .active_editor()
        .unwrap()
        .text()
        .to_owned();
    send(
        &mut session,
        InputEvent::Keyboard(KeyboardEvent {
            state: KeyState::Down,
            key: LogicalKey::Character("z".into()),
            modifiers: KeyModifiers::ALT,
            ..Default::default()
        }),
    );
    assert!(!get_map().wrapped());
    assert_eq!(
        state.get().workspace.active_editor().unwrap().text(),
        previous
    );
    state.update(|app| {
        app.workspace.active_editor_mut().unwrap().undo();
    });
    assert_eq!(
        state.get().workspace.active_editor().unwrap().text(),
        source
    );
}

#[test]
fn folding_controls_hide_source_and_keep_wrapped_hits_caret_and_expansion_correct() {
    use lgui::{
        application::{AppView, ApplicationContext},
        core::{InputEvent, Point, PointerData, UiScale, dispatch_runtime_output},
        session::UiSession,
    };
    use std::sync::Arc;
    let exposed = Arc::new(Mutex::new(None::<AppContext>));
    let output = exposed.clone();
    let rect = UiRect::new(0.0, 0.0, 240.0, 350.0);
    let application = ApplicationContext::empty(Default::default());
    let source = "fn long_function_name_with_parameters() {\n  hidden();\n}\nnext\n";
    let view: AppView = Arc::new(move |cx| {
        let mut initial = document(source);
        initial.editor.focused = true;
        initial.preferences.values_mut().editor.soft_wrap = true;
        initial.preferences.values_mut().smooth_caret = false;
        crate::app::editing::configure_all(&mut initial);
        let id = initial.workspace.active_document().unwrap();
        let doc = &initial.workspace.documents[&id];
        let stamp = doc.stamp(id);
        let range = 0..doc.buffer.line_start(3);
        crate::app::folding::publish_syntax(
            &mut initial,
            stamp,
            Some(Arc::from(std::slice::from_ref(&range))),
        );
        let state = AppContext::detached(cx.state(initial));
        let git = cx.state(GitStoreSnapshot::default());
        *output.lock().unwrap() = Some(state.clone());
        let id = UiId::new("fold-controls-test");
        let focus = cx.focus_handle(id.clone());
        render(PaneId::new(1), rect, state, git, id, focus)
    });
    let mut session = UiSession::new();
    session.render_view(&view, rect, UiScale::ONE);
    let state = exposed.lock().unwrap().clone().unwrap();
    let send = |session: &mut UiSession, input| {
        dispatch_runtime_output(
            session.handle_input(input),
            &application,
            &lgui::window::WindowId::new("fold-controls"),
            |a| session.handle_default_action(a),
            |_| {},
        );
        session.render_view(&view, rect, UiScale::ONE);
    };
    let click = |session: &mut UiSession, name: &str| {
        let r = session
            .tree()
            .nodes()
            .iter()
            .find(|n| n.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some(name))
            .unwrap()
            .layout_rect;
        let pointer = PointerData::mouse(Point::new(
            (r.left + r.right) / 2.0,
            (r.top + r.bottom) / 2.0,
        ));
        send(
            session,
            InputEvent::PointerDown {
                pointer,
                button: PointerButton::Left,
            },
        );
        send(
            session,
            InputEvent::PointerUp {
                pointer,
                button: PointerButton::Left,
            },
        );
    };
    click(&mut session, "Fold code at line 1");
    let app = state.get();
    let id = app.workspace.active().unwrap();
    let map = crate::app::editing::projection(&app, id, rect);
    assert!(map.folded());
    assert!(map.wrapped());
    assert!(map.hidden(
        &app.workspace.active_editor().unwrap(),
        source.find("hidden").unwrap()
    ));
    assert!(
        !session
            .tree()
            .nodes()
            .iter()
            .any(|n| n.text.as_deref().is_some_and(|t| t.contains("hidden")))
    );
    drop(app);
    let app = state.get();
    let buffer = app.workspace.active_editor().unwrap();
    let target = source.find("next").unwrap();
    let left = fonts::editor().gutter_w() + theme::CODE_PAD;
    let metrics = crate::ui::editor::projection::metrics(rect, left, &buffer, &map);
    let (x, y) = crate::ui::editor::projection::caret(
        &buffer,
        &map,
        loog_editor_core::text::Selection::caret(target),
        left,
        0.0,
        0.0,
        metrics.viewport_w,
    );
    drop(app);
    let pointer = PointerData::mouse(Point::new(x + 1.0, y + 4.0));
    send(
        &mut session,
        InputEvent::PointerDown {
            pointer,
            button: PointerButton::Left,
        },
    );
    send(
        &mut session,
        InputEvent::PointerUp {
            pointer,
            button: PointerButton::Left,
        },
    );
    assert_eq!(
        state.get().workspace.active_editor().unwrap().line_col().0,
        3
    );
    click(&mut session, "Expand folded lines");
    assert!(!crate::app::editing::active_projection(&state.get(), rect).folded());
    assert_eq!(
        state.get().workspace.active_editor().unwrap().text(),
        source
    );
    assert!(!state.get().workspace.active_editor().unwrap().can_undo());
}

#[test]
fn lsp_folding_fallback_is_versioned_and_late_results_do_not_fold_a_switched_tab() {
    let log = std::env::temp_dir().join(format!(
        "loog-fold-late-{}-{}.jsonl",
        std::process::id(),
        LANGUAGE_CASES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let path = log.to_string_lossy().to_string();
    let mut h = LanguageHarness::configured(
        "fn f() {\n body();\n}\nnext\n",
        &[
            "--features",
            "folding",
            "--defer-feature-replies",
            "--log",
            &path,
        ],
        |app| {
            let _ = app;
        },
    );
    h.until_ready();
    let first = h.state.get().workspace.active().unwrap();
    let count_requests = || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .filter(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .ok()
                    .is_some_and(|v| {
                        v.get("method").and_then(|v| v.as_str())
                            == Some("textDocument/foldingRange")
                    })
            })
            .count()
    };
    let before = count_requests();
    crate::app::folding::command(&h.state, crate::app::folding::FoldAction::Fold, h.rect);
    h.until("the folding request to reach the server", |_| {
        count_requests() > before
    });
    let target = h.state.get().workspace.active_path().unwrap().to_path_buf();
    h.state.update(|app| {
        app.workspace
            .test_open_view("fold-other.txt".into(), "other".into());
    });
    crate::app::test_support::language::saved(
        &h.state,
        target,
        Arc::new("fn f() {\n body();\n}\nnext\n".into()),
    );
    h.until("the folding reply after the save barrier", |h| {
        h.state.try_update(crate::app::folding::tick);
        let app = h.state.get();
        !crate::app::folding::markers(&app, first, &(0..100)).is_empty()
    });
    assert!(!crate::app::editing::projection(&h.state.get(), first, h.rect).folded());
    assert_eq!(
        h.state.get().workspace.active_editor().unwrap().text(),
        "other"
    );
    h.state.update(|app| {
        app.workspace.set_active(first);
    });
    crate::app::folding::command(&h.state, crate::app::folding::FoldAction::Fold, h.rect);
    h.render();
    assert!(crate::app::editing::active_projection(&h.state.get(), h.rect).folded());
    let _ = std::fs::remove_file(log);
}

#[test]
fn multicursor_ui_shortcuts_rectangle_unicode_ime_and_undo_use_one_transaction() {
    use lgui::{
        application::{AppView, ApplicationContext},
        core::{
            ImeEvent, InputEvent, NamedKey, Point, PointerData, UiScale, dispatch_runtime_output,
        },
        session::UiSession,
    };
    use loog_editor_core::text::Selection;
    use std::sync::Arc;
    let exposed = Arc::new(Mutex::new(None::<AppContext>));
    let output = exposed.clone();
    let rect = UiRect::new(0.0, 0.0, 600.0, 320.0);
    let application = ApplicationContext::empty(Default::default());
    let environment = application.clone();
    let _fonts = lgui::backend::install_text_environment(
        &application,
        lgui::render_skia::skia_text_system_handle(),
    );
    let source = "foo foo\r\nfoo bar\r\n中😀 e\u{301}\r\n中😀 tail";
    let view: AppView = Arc::new(move |cx| {
        let mut initial = document(source);
        initial.editor.focused = true;
        initial.preferences.values_mut().smooth_caret = false;
        let state = AppContext::detached(cx.state(initial));
        let git = cx.state(GitStoreSnapshot::default());
        *output.lock().unwrap() = Some(state.clone());
        let id = UiId::new("multicursor");
        let focus = cx.focus_handle(id.clone());
        let env = crate::app::key_actions::KeyEnv {
            state: state.clone(),
            git_store: git.clone(),
            application: environment.clone(),
            editor_focus: focus.clone(),
            editor_rect: rect,
            terminal: None,
        };
        crate::app::key_actions::attach(
            group(rect).child(render(PaneId::new(1), rect, state, git, id, focus)),
            env,
        )
    });
    let mut session = UiSession::new();
    session.render_view(&view, rect, UiScale::ONE);
    let state = exposed.lock().unwrap().clone().unwrap();
    let send = |session: &mut UiSession, input| {
        dispatch_runtime_output(
            session.handle_input(input),
            &application,
            &lgui::window::WindowId::new("multicursor"),
            |a| session.handle_default_action(a),
            |_| {},
        );
        session.render_view(&view, rect, UiScale::ONE);
    };
    let key = |key, modifiers| {
        InputEvent::Keyboard(KeyboardEvent {
            state: KeyState::Down,
            key,
            modifiers,
            ..Default::default()
        })
    };
    let secondary = if cfg!(target_os = "macos") {
        KeyModifiers::META
    } else {
        KeyModifiers::CONTROL
    };
    let left = fonts::editor().gutter_w() + theme::CODE_PAD;
    let click = |session: &mut UiSession, x, y| {
        let pointer = PointerData::mouse(Point::new(x, y));
        send(
            session,
            InputEvent::PointerDown {
                pointer,
                button: PointerButton::Left,
            },
        );
        send(
            session,
            InputEvent::PointerUp {
                pointer,
                button: PointerButton::Left,
            },
        );
    };
    click(&mut session, left + 1.0, 5.0);
    for _ in 0..3 {
        send(
            &mut session,
            key(LogicalKey::Character("d".into()), secondary),
        );
    }
    assert_eq!(
        state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .selections()
            .len(),
        3
    );
    let selected = state.get().workspace.active_editor().unwrap().selections();
    send(
        &mut session,
        InputEvent::Ime(ImeEvent::Preedit {
            text: "zhong".into(),
            cursor: Some(0..5),
        }),
    );
    assert_eq!(
        state.get().workspace.active_editor().unwrap().text(),
        source
    );
    send(
        &mut session,
        InputEvent::Ime(ImeEvent::Commit("中文()".into())),
    );
    assert_eq!(
        state.get().workspace.active_editor().unwrap().text(),
        "中文() 中文()\r\n中文() bar\r\n中😀 e\u{301}\r\n中😀 tail"
    );
    send(
        &mut session,
        key(LogicalKey::Character("z".into()), secondary),
    );
    assert_eq!(
        state.get().workspace.active_editor().unwrap().text(),
        source
    );
    assert_eq!(
        state.get().workspace.active_editor().unwrap().selections(),
        selected
    );
    send(
        &mut session,
        key(LogicalKey::Named(NamedKey::Escape), KeyModifiers::empty()),
    );
    assert!(
        state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .secondary()
            .is_empty()
    );
    state.update(|app| app.workspace.active_editor_mut().unwrap().set_cursor(0));
    send(
        &mut session,
        key(
            LogicalKey::Named(NamedKey::ArrowDown),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ),
    );
    assert_eq!(
        state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .selections()
            .len(),
        2
    );
    send(
        &mut session,
        key(
            LogicalKey::Named(NamedKey::ArrowRight),
            KeyModifiers::empty(),
        ),
    );
    assert_eq!(
        state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .selections()
            .iter()
            .map(|s| s.cursor)
            .collect::<Vec<_>>(),
        [10, 1]
    );
    // Alt-click adds then removes a caret; plain click restores a single caret.
    state.update(|app| app.editor.modifiers = KeyModifiers::ALT);
    click(&mut session, left + 1.0, fonts::editor().line_h * 3.0 + 5.0);
    assert_eq!(
        state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .selections()
            .len(),
        3
    );
    click(&mut session, left + 1.0, fonts::editor().line_h * 3.0 + 5.0);
    assert_eq!(
        state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .selections()
            .len(),
        2
    );
    state.update(|app| app.editor.modifiers = KeyModifiers::empty());
    click(&mut session, left + 1.0, 5.0);
    assert!(
        state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .secondary()
            .is_empty()
    );
    state.update(|app| app.editor.modifiers = KeyModifiers::ALT | KeyModifiers::SHIFT);
    let start = PointerData::mouse(Point::new(left + 1.0, fonts::editor().line_h * 2.0 + 5.0));
    send(
        &mut session,
        InputEvent::PointerDown {
            pointer: start,
            button: PointerButton::Left,
        },
    );
    let app = state.get();
    let buffer = app.workspace.active_editor().unwrap();
    let map = crate::app::editing::active_projection(&app, rect);
    let at = buffer.line_start(3) + "中😀".len();
    let metrics = crate::ui::editor::projection::metrics(rect, left, &buffer, &map);
    let (x, y) = crate::ui::editor::projection::caret(
        &buffer,
        &map,
        Selection::caret(at),
        left,
        0.0,
        0.0,
        metrics.viewport_w,
    );
    drop(app);
    let end = PointerData::mouse(Point::new(x + 0.1, y + 5.0));
    send(&mut session, InputEvent::PointerMove(end));
    send(
        &mut session,
        InputEvent::PointerUp {
            pointer: end,
            button: PointerButton::Left,
        },
    );
    let app = state.get();
    let editor = app.workspace.active_editor().unwrap();
    assert_eq!(editor.selections().len(), 2);
    assert_eq!(editor.selected_texts().as_deref(), Some("中😀\r\n中😀"));
    drop(app);
    send(&mut session, InputEvent::Ime(ImeEvent::Commit("替".into())));
    assert_eq!(
        state.get().workspace.active_editor().unwrap().text(),
        "foo foo\r\nfoo bar\r\n替 e\u{301}\r\n替 tail"
    );
    send(
        &mut session,
        key(LogicalKey::Character("z".into()), secondary),
    );
    assert_eq!(
        state.get().workspace.active_editor().unwrap().text(),
        source
    );
    state.update(crate::app::language_ui::completion::words);
    assert!(
        state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .secondary()
            .is_empty()
    );
}

#[test]
fn multicursor_display_rows_and_split_view_state_stay_independent() {
    use loog_editor_core::text::Selection;
    let mut app = document("one two three four five six seven eight nine\nend");
    app.preferences.values_mut().editor.soft_wrap = true;
    crate::app::editing::configure_all(&mut app);
    let rect = UiRect::new(0.0, 0.0, 180.0, 320.0);
    let map = crate::app::editing::active_projection(&app, rect);
    assert!(map.len() > 3);
    crate::app::multicursor::command(&mut app, false, true, rect);
    let buffer = app.workspace.active_editor().unwrap();
    let states = buffer.selections();
    assert_eq!(states.len(), 2);
    assert_eq!(
        map.caret_row(&buffer, states[0].cursor, states[0].wrap_affinity),
        1
    );
    let first = app.workspace.active().unwrap();
    let (primary, others) =
        crate::app::multicursor::rectangle(&buffer, &map, 0, map.row(&buffer, 2).range.start + 1);
    assert_eq!(others.len(), 2);
    assert!(
        others
            .iter()
            .chain(std::iter::once(&primary))
            .all(|s| s.range().is_none_or(|r| !buffer.text()[r].contains('\n')))
    );
    let other = app
        .workspace
        .test_open_view("another.txt".into(), "other".into());
    assert!(
        app.workspace
            .active_editor()
            .unwrap()
            .secondary()
            .is_empty()
    );
    app.workspace.set_active(first);
    assert_eq!(app.workspace.active_editor().unwrap().selections(), states);
    app.workspace.set_active(other);
    app.workspace
        .active_editor_mut()
        .unwrap()
        .set_selections(Selection::caret(0), vec![Selection::caret(5)]);
    crate::app::multicursor::primary_only(&mut app);
    assert_eq!(
        app.workspace.active_editor().unwrap().selections(),
        vec![Selection::caret(0)]
    );
}

#[test]
fn multicursor_cancels_pending_completion_and_snippets_before_late_reply() {
    use crate::app::language_ui::completion;
    let log = std::env::temp_dir().join(format!(
        "loog-multi-completion-{}-{}.jsonl",
        std::process::id(),
        LANGUAGE_CASES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let log_arg = log.display().to_string();
    let mut h = LanguageHarness::configured(
        "b\nb",
        &[
            "--features",
            "completion",
            "--defer-feature-replies",
            "--log",
            &log_arg,
        ],
        |app| {
            let _ = app;
        },
    );
    h.until_ready();
    h.set_cursor(1);
    let count = |h: &LanguageHarness| {
        server_log(h, &log)
            .iter()
            .filter(|m| m.as_str() == "textDocument/completion")
            .count()
    };
    let before = count(&h);
    h.state
        .update(|app| completion::request(&h.state, app, true, None));
    h.until("first completion request to reach the server", |h| {
        count(h) > before
    });
    h.state
        .update(|app| crate::app::multicursor::command(app, false, true, h.rect));
    assert!(h.state.get().editor.language.completion.is_none());
    let path = h.state.get().workspace.active_path().unwrap().to_path_buf();
    crate::app::test_support::language::saved(&h.state, path.clone(), Arc::new("b\nb".into()));
    h.state
        .update(|app| completion::observe(&h.state, app, Some('o')));
    assert!(h.state.get().editor.language.completion.is_none());
    assert_eq!(
        h.state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .selections()
            .len(),
        2
    );
    let before = count(&h);
    h.state
        .update(|app| completion::request(&h.state, app, true, None));
    assert!(
        h.state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .secondary()
            .is_empty()
    );
    h.until(
        "new manual completion request after releasing the cancelled response",
        |h| count(h) > before,
    );
    crate::app::test_support::language::saved(&h.state, path, Arc::new("b\nb".into()));
    h.until("manual completion at the primary caret", |h| {
        h.state
            .get()
            .editor
            .language
            .completion
            .as_ref()
            .is_some_and(|s| s.visible())
    });
    h.state.update(|app| {
        completion::accept(app, None);
    });
    assert!(h.state.get().editor.language.snippet.is_some());
    h.state
        .update(|app| crate::app::multicursor::command(app, false, false, h.rect));
    assert!(h.state.get().editor.language.snippet.is_none());
    let _ = std::fs::remove_file(log);
}

#[test]
fn multicursor_save_formats_crlf_bytes_and_maps_all_selections_with_undo() {
    use crate::features::keymap::action::Action;
    use lgui::ServicesContextExt;
    use loog_editor_core::text::Selection;
    let source = "fn alpha() {}\r\nlast";
    let mut h =
        LanguageHarness::configured(source, &["--features", "formatting"], on_save(&[], true));
    h.until_ready();
    h.state.update(|app| {
        app.workspace
            .active_editor_mut()
            .unwrap()
            .set_selections(Selection::caret(5), vec![Selection::caret(source.len())])
    });
    let before = h
        .state
        .get()
        .workspace
        .active_editor()
        .unwrap()
        .selections();
    h.act(Action::Save);
    let path = h.state.get().workspace.active_path().unwrap().to_path_buf();
    let expected = format!("// formatted\r\n{source}");
    h.until("formatted multi-cursor CRLF file bytes", |_| {
        std::fs::read(&path).is_ok_and(|bytes| bytes == expected.as_bytes())
    });
    let prefix = "// formatted\r\n".len();
    assert_eq!(
        h.state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .selections()
            .iter()
            .map(|s| s.cursor)
            .collect::<Vec<_>>(),
        [5 + prefix, source.len() + prefix]
    );
    execute_command(
        &h.state,
        loog_editor_core::commands::Command::Undo,
        h.rect,
        h.context.clipboard(),
    );
    h.render();
    assert_eq!(h.document_text(), source);
    assert_eq!(
        h.state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .selections(),
        before
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn vim_editor_completion_snippet_pairing_and_escape_share_input_ownership() {
    use crate::features::keymap::action::Action;
    let mut h = LanguageHarness::configured("\n", &["--features", "completion"], |app| {
        app.editor.vim.options.enabled = true
    });
    h.until_ready();
    // Explicit completion from Normal mode enters Insert rather than opening an inert list.
    h.act(Action::ShowCompletions);
    h.until("Vim manual completion", |h| {
        !h.completion_labels().is_empty()
    });
    assert_eq!(
        h.state.get().editor.vim.mode(),
        loog_editor_core::vim::Mode::Insert
    );
    h.act(Action::MenuSelectNext);
    h.act(Action::MenuConfirm);
    assert!(h.state.get().editor.language.snippet.is_some());
    h.type_text("(");
    assert_eq!(h.document_text(), "beta(())\n");
    crate::app::editor_vim::feed_keys(&h.state, "<Esc>", h.rect, &TestClipboard::default());
    h.render();
    assert_eq!(
        h.state.get().editor.vim.mode(),
        loog_editor_core::vim::Mode::Normal
    );
    assert!(h.state.get().editor.language.snippet.is_none());
    assert!(h.state.get().editor.language.completion.is_none());
}

#[test]
fn vim_editor_pending_completion_cannot_reopen_after_escape() {
    use crate::features::keymap::action::Action;
    let log = std::env::temp_dir().join(format!(
        "loog-vim-completion-{}-{}.jsonl",
        std::process::id(),
        LANGUAGE_CASES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let log_arg = log.display().to_string();
    let mut h = LanguageHarness::configured(
        "\n",
        &[
            "--features",
            "completion",
            "--defer-feature-replies",
            "--log",
            &log_arg,
        ],
        |app| {
            app.editor.vim.options.enabled = true;
        },
    );
    h.until_ready();
    h.act(Action::ShowCompletions);
    h.until("pending Vim completion at the server", |h| {
        server_log(h, &log)
            .iter()
            .any(|m| m == "textDocument/completion")
    });
    crate::app::editor_vim::feed_keys(&h.state, "<Esc>", h.rect, &TestClipboard::default());
    h.render();
    let path = h.state.get().workspace.active_path().unwrap().to_path_buf();
    crate::app::test_support::language::saved(&h.state, path.clone(), Arc::new("\n".into()));
    h.until("save barrier after cancelling Vim completion", |h| {
        server_log(h, &log)
            .iter()
            .any(|m| m == "textDocument/didSave")
    });
    assert!(h.state.get().editor.language.completion.is_none());
    assert_eq!(
        h.state.get().editor.vim.mode(),
        loog_editor_core::vim::Mode::Normal
    );
    let before = server_log(&h, &log)
        .iter()
        .filter(|m| m.as_str() == "textDocument/completion")
        .count();
    h.act(Action::ShowCompletions);
    h.until("new Vim completion request", |h| {
        server_log(h, &log)
            .iter()
            .filter(|m| m.as_str() == "textDocument/completion")
            .count()
            > before
    });
    crate::app::test_support::language::saved(&h.state, path, Arc::new("\n".into()));
    h.until("fresh completion after Vim re-enters Insert", |h| {
        !h.completion_labels().is_empty()
    });
    assert_eq!(
        h.state.get().editor.vim.mode(),
        loog_editor_core::vim::Mode::Insert
    );
    let _ = std::fs::remove_file(log);
}

#[test]
fn vim_editor_save_formatting_preserves_multi_selections_and_insert_history() {
    use crate::features::keymap::action::Action;
    use loog_editor_core::text::Selection;
    let source = "fn alpha() {}\r\nlast";
    let mut h = LanguageHarness::configured(source, &["--features", "formatting"], |app| {
        app.editor.vim.options.enabled = true;
        on_save(&[], true)(app);
    });
    h.until_ready();
    crate::app::editor_vim::feed_keys(&h.state, "i", h.rect, &TestClipboard::default());
    h.state.update(|app| {
        app.workspace
            .active_editor_mut()
            .unwrap()
            .set_selections(Selection::caret(5), vec![Selection::caret(source.len())])
    });
    let before = h
        .state
        .get()
        .workspace
        .active_editor()
        .unwrap()
        .selections();
    h.act(Action::Save);
    let path = h.state.get().workspace.active_path().unwrap().to_path_buf();
    let expected = format!("// formatted\r\n{source}");
    h.until("Vim save formatting bytes", |_| {
        std::fs::read(&path).is_ok_and(|bytes| bytes == expected.as_bytes())
    });
    assert_eq!(
        h.state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .selections()
            .len(),
        2
    );
    execute_command(
        &h.state,
        loog_editor_core::commands::Command::Undo,
        h.rect,
        Arc::new(TestClipboard::default()),
    );
    h.render();
    assert_eq!(h.document_text(), source);
    assert_eq!(
        h.state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .selections(),
        before
    );
    assert_eq!(
        h.state.get().editor.vim.mode(),
        loog_editor_core::vim::Mode::Insert
    );
    h.type_text("字");
    crate::app::editor_vim::feed_keys(&h.state, "<Esc>", h.rect, &TestClipboard::default());
    crate::app::editor_vim::feed_keys(&h.state, "u", h.rect, &TestClipboard::default());
    h.render();
    assert_eq!(h.document_text(), source);
    let _ = std::fs::remove_file(path);
}

#[test]
fn vim_editor_inlay_hints_recover_when_a_provider_appears_without_an_edit() {
    let script = std::env::temp_dir().join(format!(
        "loog-vim-inlay-registration-{}-{}.json",
        std::process::id(),
        LANGUAGE_CASES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&script,serde_json::json!([
        {"step":1,"id":8208,"method":"client/registerCapability","params":{"registrations":[{"id":"late-hints","method":"textDocument/inlayHint","registerOptions":{}}]}}
    ]).to_string()).unwrap();
    let script_arg = script.display().to_string();
    let mut h = LanguageHarness::configured(
        "let x = 1;\n",
        &["--features", "hover", "--registration-script", &script_arg],
        |app| {
            app.editor.vim.options.enabled = true;
            app.preferences.values_mut().language_features.inlay_hints = true;
        },
    );
    h.until_ready();
    // Consume the first settled request with no eligible provider. The next
    // capability transition is released explicitly, without an edit or a sleep.
    h.until("settled hint request before provider registration", |h| {
        h.state.try_update(crate::app::language_ui::inlay::tick);
        h.state
            .get()
            .editor
            .language
            .inlay
            .shown
            .as_ref()
            .is_some_and(|s| s.hints.is_empty())
    });
    let path = h.state.get().workspace.active_path().unwrap().to_path_buf();
    let before = h.document_text();
    let cursor = h.cursor();
    let save = |h: &LanguageHarness| {
        crate::app::test_support::language::saved(&h.state, path.clone(), Arc::new(before.clone()))
    };
    save(&h);
    h.until("inlay hints after late capability registration", |h| {
        h.state
            .get()
            .editor
            .language
            .inlay
            .shown
            .as_ref()
            .is_some_and(|s| !s.hints.is_empty())
    });
    assert_eq!(h.document_text(), before);
    assert_eq!(h.cursor(), cursor);
    let run = h
        .runtime
        .language_services()
        .unwrap()
        .capability_sources(None)[0]
        .run;
    h.runtime.extensions().unwrap().reload_all(&h.state);
    h.until("inlay provider restart", |h| {
        h.runtime
            .language_services()
            .unwrap()
            .capability_sources(None)
            .iter()
            .any(|i| i.run > run && i.providers.inlay_hint)
    });
    h.until("old hints cleared after provider restart", |h| {
        h.state.try_update(crate::app::language_ui::inlay::tick);
        h.state
            .get()
            .editor
            .language
            .inlay
            .shown
            .as_ref()
            .is_some_and(|s| s.hints.is_empty())
    });
    save(&h);
    h.until("hints after re-registering the restarted provider", |h| {
        h.state
            .get()
            .editor
            .language
            .inlay
            .shown
            .as_ref()
            .is_some_and(|s| !s.hints.is_empty())
    });
    assert_eq!(h.cursor(), cursor);
    let _ = std::fs::remove_file(script);
}

#[test]
fn folding_gutter_hover_reveals_right_column_and_preserves_numbers_actions_and_git() {
    use crate::ui::editor_binding::Binding;
    use lgui::{
        application::{AppView, ApplicationContext},
        core::{InputEvent, Point, PointerData, UiScale, dispatch_runtime_output},
        session::UiSession,
    };
    use std::sync::Arc;
    let application = ApplicationContext::empty(Default::default());
    let _text = lgui::backend::install_text_environment(
        &application,
        lgui::render_skia::skia_text_system_handle(),
    );
    let rect = UiRect::new(0.0, 0.0, 640.0, 180.0);
    let source = "first {\n  hidden\n}\nnext {\n  tail\n}\n";
    for (size, expected_left) in [(12, 60.0), (18, 83.0), (24, 106.0)] {
        let font = fonts::FontSettings {
            size,
            ..fonts::FontSettings::EDITOR
        };
        fonts::set(&font, &fonts::FontSettings::TERMINAL);
        for factor in [1.0, 1.25, 2.0] {
            let font = font.clone();
            let scale = UiScale::new(factor);
            let exposed = Arc::new(Mutex::new(None::<AppContext>));
            let output = exposed.clone();
            let view: AppView = Arc::new(move |cx| {
                let mut initial = document(source);
                initial.editor.focused = true;
                initial.preferences.values_mut().editor_font = font.clone();
                initial.preferences.values_mut().smooth_caret = false;
                let document = initial.workspace.active_document().unwrap();
                let doc = &initial.workspace.documents[&document];
                let stamp = doc.stamp(document);
                let ranges = [
                    0..doc.buffer.line_start(3),
                    doc.buffer.line_start(3)..doc.buffer.line_start(6),
                ];
                crate::app::folding::publish_syntax(&mut initial, stamp, Some(Arc::from(ranges)));
                let state = AppContext::detached(cx.state(initial));
                *output.lock().unwrap() = Some(state.clone());
                let git = cx.state(GitStoreSnapshot::default());
                let base =
                    crate::app::editor_view::binding(state.clone(), git, PaneId::new(1), rect);
                let binding = Binding::new(
                    move || {
                        let mut s = crate::app::editor_view::snapshot(
                            state.get(),
                            GitStoreSnapshot::default(),
                            PaneId::new(1),
                            rect,
                        );
                        s.lightbulb = Some(0);
                        s.has_repository = true;
                        s.line_changes.insert(1, LineChange::Modified);
                        s.editor_gutter_hover = Some((PaneId::new(1), 1));
                        s
                    },
                    move |event| base.send(event),
                    |_| None,
                );
                let id = UiId::new("fold-margin-test");
                let focus = cx.focus_handle(id.clone());
                crate::ui::editor::view::render(PaneId::new(1), rect, binding, id, focus)
            });
            let mut session = UiSession::new();
            session.render_view(&view, rect, scale);
            let bounds = |session: &UiSession, name| {
                session
                    .tree()
                    .nodes()
                    .iter()
                    .find(|n| n.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some(name))
                    .unwrap()
                    .layout_rect
            };
            let arrow_visible = |session: &UiSession, name| {
                let control = session
                    .tree()
                    .nodes()
                    .iter()
                    .find(|n| n.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some(name))
                    .unwrap();
                session.tree().nodes().iter().any(|node| {
                    node.parent.as_ref() == Some(&control.id)
                        && node.icon_key.is_some()
                        && node.icon_style.alpha > 0
                })
            };
            assert!(!arrow_visible(&session, "Fold code at line 1"));
            assert!(!arrow_visible(&session, "Fold code at line 4"));
            let fold = bounds(&session, "Fold code at line 1");
            let bulb = bounds(&session, "Code actions (Ctrl+.)");
            let git = bounds(&session, "Show change");
            assert!(bulb.right <= git.left && git.right <= fold.left);
            assert_eq!(fold.right - fold.left, 10.0);
            let check_source = |session: &UiSession| {
                let painted = session
                    .tree()
                    .nodes()
                    .iter()
                    .find(|n| n.text.as_deref() == Some("first {"))
                    .unwrap();
                assert_eq!(
                    painted.layout_rect.left, expected_left,
                    "only the narrow column is added at size {size}, scale {factor}"
                );
                let number = session
                    .tree()
                    .nodes()
                    .iter()
                    .find(|n| n.text.as_deref() == Some("1"))
                    .unwrap();
                assert_eq!(
                    number.layout_rect.right,
                    expected_left - 24.0,
                    "original line-number position is unchanged"
                );
                assert_eq!(fold.left - number.layout_rect.right, 7.0);
                assert_eq!(painted.layout_rect.left - fold.right, 7.0);
            };
            check_source(&session);
            let send = |session: &mut UiSession, input| {
                dispatch_runtime_output(
                    session.handle_input(input),
                    &application,
                    &lgui::window::WindowId::new("fold-margin"),
                    |a| session.handle_default_action(a),
                    |_| {},
                );
                session.render_view(&view, rect, scale);
            };
            let click = |session: &mut UiSession, x, y| {
                let pointer = PointerData::mouse(Point::new(x, y));
                send(session, InputEvent::PointerMove(pointer));
                send(
                    session,
                    InputEvent::PointerDown {
                        pointer,
                        button: PointerButton::Left,
                    },
                );
                send(
                    session,
                    InputEvent::PointerUp {
                        pointer,
                        button: PointerButton::Left,
                    },
                );
            };
            let blank = Point::new((fold.left + fold.right) / 2.0, fonts::editor().line_h + 2.0);
            for (point, cursor) in [
                (Point::new(1.0, blank.y), CursorIcon::Default),
                (
                    Point::new(expected_left - 25.0, blank.y),
                    CursorIcon::Default,
                ),
                (blank, CursorIcon::Default),
                (
                    Point::new(
                        (bulb.left + bulb.right) / 2.0,
                        (bulb.top + bulb.bottom) / 2.0,
                    ),
                    CursorIcon::Pointer,
                ),
                (
                    Point::new((git.left + git.right) / 2.0, (git.top + git.bottom) / 2.0),
                    CursorIcon::Pointer,
                ),
            ] {
                send(
                    &mut session,
                    InputEvent::PointerMove(PointerData::mouse(point)),
                );
                assert!(
                    arrow_visible(&session, "Fold code at line 1"),
                    "gutter point {point:?}"
                );
                assert!(
                    arrow_visible(&session, "Fold code at line 4"),
                    "gutter point {point:?}"
                );
                assert_eq!(
                    session
                        .tree()
                        .cursor_at(point)
                        .unwrap_or(CursorIcon::Default),
                    cursor,
                    "gutter point {point:?}, font={size}, scale={factor}"
                );
            }
            send(
                &mut session,
                InputEvent::PointerLeave(PointerData::mouse(blank)),
            );
            assert!(!arrow_visible(&session, "Fold code at line 1"));
            assert!(!arrow_visible(&session, "Fold code at line 4"));
            send(
                &mut session,
                InputEvent::PointerMove(PointerData::mouse(blank)),
            );
            assert!(arrow_visible(&session, "Fold code at line 1"));
            assert!(arrow_visible(&session, "Fold code at line 4"));
            assert_eq!(
                session
                    .tree()
                    .cursor_at(blank)
                    .unwrap_or(CursorIcon::Default),
                CursorIcon::Default
            );
            click(&mut session, blank.x, blank.y);
            assert_eq!(
                exposed
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .get()
                    .workspace
                    .active_editor()
                    .unwrap()
                    .cursor(),
                0,
                "blank column click must not move the caret"
            );
            click(
                &mut session,
                (fold.left + fold.right) / 2.0,
                (fold.top + fold.bottom) / 2.0,
            );
            assert_eq!(
                session.tree().cursor_at(Point::new(
                    (fold.left + fold.right) / 2.0,
                    (fold.top + fold.bottom) / 2.0
                )),
                Some(CursorIcon::Pointer)
            );
            assert!(arrow_visible(&session, "Fold code at line 4"));
            assert!(arrow_visible(&session, "Unfold code at line 1"));
            let state = exposed.lock().unwrap().clone().unwrap();
            assert!(
                crate::app::editing::active_projection(&state.get(), rect).folded(),
                "Git hover and lightbulb must not intercept folding"
            );
            check_source(&session);
            click(&mut session, expected_left + 0.5, 4.0);
            assert!(
                !arrow_visible(&session, "Unfold code at line 1"),
                "moving from the gutter into source hides every arrow"
            );
            assert!(!arrow_visible(&session, "Fold code at line 4"));
            assert_eq!(
                state.get().workspace.active_editor().unwrap().cursor(),
                0,
                "first character hit matches its painted position"
            );
            let fold = bounds(&session, "Unfold code at line 1");
            click(
                &mut session,
                (fold.left + fold.right) / 2.0,
                (fold.top + fold.bottom) / 2.0,
            );
            assert!(!crate::app::editing::active_projection(&state.get(), rect).folded());
            assert!(arrow_visible(&session, "Fold code at line 1"));
            check_source(&session);
            if size == 12 && factor == 1.0 {
                if let Some(path) = std::env::var_os("LOOG_FOLD_PREVIEW") {
                    application
                        .resources()
                        .provide(lgui::icons::IconRegistration(Arc::new(
                            crate::app::svg_icons(),
                        )));
                    let mut surface = lgui::render_skia::SkiaSoftwareSurface::new(4 * 1024 * 1024);
                    let frame = lgui::renderer::FrameInfo::new(
                        lgui::core::PhysicalRect::new(0, 0, 640, 180),
                        &[],
                        scale,
                        lgui::renderer::FrameReason::SceneChange,
                        true,
                    );
                    lgui::assets::backend::with_render_resources(
                        &application,
                        lgui::RenderResources::new(),
                        || surface.draw(&session.tree().scene().project_to_physical(scale), &frame),
                    )
                    .unwrap();
                    let mut pixels = surface.pixels().to_vec();
                    for pixel in pixels.as_chunks_mut::<4>().0 {
                        pixel.swap(0, 2);
                    }
                    image::save_buffer(path, &pixels, 640, 180, image::ColorType::Rgba8).unwrap();
                }
            }
        }
    }
    fonts::set(&fonts::FontSettings::EDITOR, &fonts::FontSettings::TERMINAL);
}
