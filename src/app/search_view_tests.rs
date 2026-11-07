use super::*;
use crate::app::context::AppContext;
use crate::app::search::render;
use crate::app::state::AppState;
use lgui::ApplicationHandle;
use lgui::application::{AppView, ApplicationContext};
use lgui::core::{
    InputEvent, PhysicalRect, Point, PointerButton, PointerData, UiScale, dispatch_runtime_output,
};
use lgui::render_skia::SkiaSoftwareSurface;
use lgui::renderer::{FrameInfo, FrameReason};
use lgui::session::UiSession;
use std::sync::Mutex;

#[cfg(windows)]
#[test]
fn search_file_labels_normalize_verbatim_paths_before_relativizing() {
    let file = |path: &str, root: &str| FileMatches {
        path: path.into(),
        root: root.into(),
        matches: Vec::new(),
        content_hash: 0,
    };
    for (path, root) in [
        (
            r"\\?\C:\Users\yys\msites\cmd\matomo-db\main.go",
            r"C:\Users\yys\msites",
        ),
        (
            r"C:\Users\yys\msites\cmd\matomo-db\main.go",
            r"\\?\C:\Users\yys\msites",
        ),
        (
            r"\\?\UNC\server\share\msites\cmd\matomo-db\main.go",
            r"\\server\share\msites",
        ),
    ] {
        let result = file(path, root);
        assert_eq!(
            file_labels(&result, false),
            ("cmd/matomo-db/main.go".into(), "cmd/matomo-db".into())
        );
        assert_eq!(
            file_labels(&result, true),
            (
                "cmd/matomo-db/main.go".into(),
                "msites/cmd/matomo-db".into()
            )
        );
    }
    let top_level = file(r"\\?\C:\msites\main.go", r"C:\msites");
    assert_eq!(
        file_labels(&top_level, false),
        ("main.go".into(), String::new())
    );
    assert_eq!(
        file_labels(&top_level, true),
        ("main.go".into(), "msites".into())
    );
    let outside = file(r"\\?\C:\other\main.go", r"C:\msites");
    assert_eq!(
        file_labels(&outside, true),
        ("C:/other/main.go".into(), "C:/other".into())
    );
}

fn click(session: &mut UiSession, context: &ApplicationContext, x: f32, y: f32) {
    for input in [
        InputEvent::PointerDown {
            pointer: PointerData::mouse(Point::new(x, y)),
            button: PointerButton::Left,
        },
        InputEvent::PointerUp {
            pointer: PointerData::mouse(Point::new(x, y)),
            button: PointerButton::Left,
        },
    ] {
        dispatch_runtime_output(
            session.handle_input(input),
            context,
            &lgui::window::WindowId::new("search-test"),
            |action| session.handle_default_action(action),
            |_| {},
        );
    }
}

fn click_named(session: &mut UiSession, context: &ApplicationContext, name: &str) {
    let rect = session
        .tree()
        .nodes()
        .iter()
        .find(|node| {
            node.semantics
                .as_ref()
                .and_then(|semantics| semantics.name.as_deref())
                == Some(name)
        })
        .unwrap_or_else(|| panic!("Missing control: {name}"))
        .hit_rect;
    click(
        session,
        context,
        (rect.left + rect.right) / 2.0,
        (rect.top + rect.bottom) / 2.0,
    );
}

fn render_pixels(
    session: &UiSession,
    context: &ApplicationContext,
    width: f32,
    height: f32,
) -> Vec<u8> {
    let physical = PhysicalRect::new(0, 0, width as i32, height as i32);
    let dirty = [physical];
    let frame = FrameInfo::new(
        physical,
        &dirty,
        UiScale::ONE,
        FrameReason::SceneChange,
        true,
    );
    let mut surface = SkiaSoftwareSurface::new(16 * 1024 * 1024);
    lgui::assets::backend::with_render_resources(
        context,
        lgui::assets::backend::render_resources(),
        || {
            surface.draw(
                &session.tree().scene().project_to_physical(UiScale::ONE),
                &frame,
            )
        },
    )
    .unwrap();
    let mut rgba = surface.pixels().to_vec();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    rgba
}

fn save_preview(
    session: &UiSession,
    context: &ApplicationContext,
    width: f32,
    height: f32,
    name: &str,
) {
    let Some(directory) = std::env::var_os("LOOG_SEARCH_PREVIEW_DIR") else {
        return;
    };
    let rgba = render_pixels(session, context, width, height);
    image::save_buffer(
        std::path::Path::new(&directory).join(format!("{name}-{}.png", width as u32)),
        &rgba,
        width as u32,
        height as u32,
        image::ColorType::Rgba8,
    )
    .unwrap();
}

#[test]
fn search_panel_renders_bounded_rows_and_switches_drawer_at_normal_and_small_sizes() {
    let context = ApplicationContext::empty(Default::default());
    context
        .resources()
        .provide(lgui::icons::IconRegistration(Arc::new(
            crate::app::svg_icons(),
        )));
    let _fonts = lgui::backend::install_text_environment(
        &context,
        lgui::render_skia::skia_text_system_handle(),
    );
    for (width, height) in [(220.0, 600.0), (340.0, 600.0), (80.0, 80.0)] {
        let viewport = UiRect::new(0.0, 0.0, width, height);
        let exposed = Arc::new(Mutex::new(None::<AppContext>));
        let output = exposed.clone();
        let application = ApplicationHandle::new(|task| task(), || {});
        let runtime = Arc::new(crate::app::runtime::AppRuntime::default());
        let view_runtime = runtime.clone();
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.apply(crate::features::keymap::action::Action::OpenSearch);
                app.explorer.model.restore(vec!["demo".into()], Vec::new());
                app.search.ui.query.set_text("needle");
                app.search.ui.filters_open = true;
                app.search.ui.include.set_text("*.rs, *.ts");
                let spec = app.search.spec();
                let matcher = crate::features::search::Matcher::new(&spec).unwrap();
                let value = "let 中文 = needle; // search preview\n".repeat(10_000);
                let matches =
                    matcher.matches(&value, &std::sync::atomic::AtomicBool::new(false), 10_000);
                let file = Arc::new(FileMatches {
                    path: "demo/src/main.rs".into(),
                    root: "demo".into(),
                    content_hash: crate::features::search::hash(&value),
                    matches,
                });
                app.search.model.fixture_status().results = Arc::new(Results {
                    files: vec![file],
                    matches: 10_000,
                    searched: 100,
                    complete: true,
                    ..Results::default()
                });
                app
            }));
            let state = state.with_runtime(&view_runtime);
            *output.lock().unwrap() = Some(state.clone());
            let ids = [
                UiId::new("search-test-query"),
                UiId::new("search-test-include"),
                UiId::new("search-test-exclude"),
            ];
            let focus = [
                cx.focus_handle(ids[0].clone()),
                cx.focus_handle(ids[1].clone()),
                cx.focus_handle(ids[2].clone()),
            ];
            let results_id = UiId::new("search-test-results");
            let editor_focus = cx.focus_handle(UiId::new("search-test-editor"));
            render(
                viewport,
                state,
                application.clone(),
                editor_focus,
                viewport,
                Inputs {
                    ids,
                    focus,
                    results_focus: cx.focus_handle(results_id.clone()),
                    results_id,
                },
            )
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        session.render_view(&view, viewport, UiScale::ONE);
        if width >= 220.0 {
            let before = render_pixels(&session, &context, width, height);
            for name in [
                "Filters",
                "Search hidden files",
                "Search ignored files",
                "Match case (Alt+C)",
            ] {
                let hit = session
                    .tree()
                    .nodes()
                    .iter()
                    .find(|node| {
                        node.semantics
                            .as_ref()
                            .and_then(|semantics| semantics.name.as_deref())
                            == Some(name)
                    })
                    .unwrap()
                    .hit_rect;
                let point = Point::new((hit.left + hit.right) / 2.0, (hit.top + hit.bottom) / 2.0);
                dispatch_runtime_output(
                    session.handle_input(InputEvent::PointerMove(PointerData::mouse(point))),
                    &context,
                    &lgui::window::WindowId::new("search-test"),
                    |action| session.handle_default_action(action),
                    |_| {},
                );
                session.render_view(&view, viewport, UiScale::ONE);
                assert!(
                    render_pixels(&session, &context, width, height) == before,
                    "Hovering {name} must not paint a background or tooltip"
                );
            }
            save_preview(&session, &context, width, height, "search-hover-verified");
            let context = ApplicationContext::empty(Default::default());
            for input in [
                InputEvent::PointerDown {
                    pointer: PointerData::mouse(Point::new(50.0, 48.0)),
                    button: PointerButton::Left,
                },
                InputEvent::PointerUp {
                    pointer: PointerData::mouse(Point::new(50.0, 48.0)),
                    button: PointerButton::Left,
                },
            ] {
                dispatch_runtime_output(
                    session.handle_input(input),
                    &context,
                    &lgui::window::WindowId::new("search-test"),
                    |action| session.handle_default_action(action),
                    |_| {},
                );
            }
            session.render_view(&view, viewport, UiScale::ONE);
        }
        assert!(
            session.tree().nodes().len() < 300,
            "10,000 matches must not create 10,000 UI nodes"
        );
        let physical = PhysicalRect::new(0, 0, width as i32, height as i32);
        let dirty = [physical];
        let frame = FrameInfo::new(
            physical,
            &dirty,
            UiScale::ONE,
            FrameReason::SceneChange,
            true,
        );
        let mut surface = SkiaSoftwareSurface::new(16 * 1024 * 1024);
        lgui::assets::backend::with_render_resources(
            &context,
            lgui::assets::backend::render_resources(),
            || {
                surface.draw(
                    &session.tree().scene().project_to_physical(UiScale::ONE),
                    &frame,
                )
            },
        )
        .unwrap();
        if let Some(path) = std::env::var_os("LOOG_SEARCH_PREVIEW_DIR") {
            let mut rgba = surface.pixels().to_vec();
            for pixel in rgba.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
            image::save_buffer(
                std::path::Path::new(&path).join(format!("search-panel-{}.png", width as u32)),
                &rgba,
                width as u32,
                height as u32,
                image::ColorType::Rgba8,
            )
            .unwrap();
        }
        if width >= 220.0 {
            let context = ApplicationContext::empty(Default::default());
            context
                .resources()
                .provide(lgui::icons::IconRegistration(Arc::new(
                    crate::app::svg_icons(),
                )));
            assert!(
                exposed
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .get()
                    .search
                    .query
                    .focused
            );
            // Option buttons preserve query focus so typing can continue.
            for input in [
                InputEvent::PointerDown {
                    pointer: PointerData::mouse(Point::new(width - 72.0, 48.0)),
                    button: PointerButton::Left,
                },
                InputEvent::PointerUp {
                    pointer: PointerData::mouse(Point::new(width - 72.0, 48.0)),
                    button: PointerButton::Left,
                },
            ] {
                dispatch_runtime_output(
                    session.handle_input(input),
                    &context,
                    &lgui::window::WindowId::new("search-test"),
                    |action| session.handle_default_action(action),
                    |_| {},
                );
            }
            let app = exposed.lock().unwrap().as_ref().unwrap().get();
            assert!(app.search.ui.case_sensitive);
            assert!(app.search.ui.query.focused);

            let state = exposed.lock().unwrap().as_ref().unwrap().clone();
            // Filter controls have real hit targets and preserve query focus.
            click_named(&mut session, &context, "Search hidden files");
            assert!(state.get().search.ui.include_hidden);
            assert!(state.get().search.ui.query.focused);
            session.render_view(&view, viewport, UiScale::ONE);
            click_named(&mut session, &context, "Reset filters");
            assert_eq!(filter_count(&state.get().search), 0);
            assert!(state.get().search.ui.filters_open);
            session.render_view(&view, viewport, UiScale::ONE);
            click_named(&mut session, &context, "Collapse or expand all results");
            assert!(
                state
                    .get()
                    .search
                    .collapsed
                    .contains(std::path::Path::new("demo/src/main.rs"))
            );
            session.render_view(&view, viewport, UiScale::ONE);
            click_named(&mut session, &context, "Collapse or expand all results");
            assert!(state.get().search.ui.collapsed.is_empty());
            session.render_view(&view, viewport, UiScale::ONE);

            if std::env::var_os("LOOG_SEARCH_PREVIEW_DIR").is_some() {
                state.update(|app| {
                        app.search.ui.query.set_text("search");
                        app.search.ui.case_sensitive = false;
                        app.search.ui.include.set_text("src/**, *.rs");
                        app.search.ui.exclude.set_text("target/**, *.lock");
                        app.search.ui.include_hidden = true;
                        let matcher = crate::features::search::Matcher::new(&app.search.spec()).unwrap();
                        let files = [
                            ("src/ui/search_panel.rs", "fn build_panel() {\n    let query = app.search.ui.query.text();\n    let results = app.search.model.status().results.clone();\n}\n"),
                            ("src/search_actions.rs", "pub fn start(app: &mut AppState) {\n    app.search.model.fixture_status().running = true;\n}\n"),
                            ("src/input/key_actions.rs", "    let state = app.search.clone();\n    state.search_next();\n"),
                        ].into_iter().map(|(path, content)| Arc::new(FileMatches {
                            path: std::path::Path::new("demo").join(path), root: "demo".into(),
                            content_hash: crate::features::search::hash(content),
                            matches: matcher.matches(content, &std::sync::atomic::AtomicBool::new(false), 100),
                        })).collect::<Vec<_>>();
                        app.search.ui.selected = Some((files[0].path.clone(), 0));
                        app.search.model.fixture_status().results = Arc::new(Results {
                            matches: files.iter().map(|file| file.matches.len()).sum(), files,
                            skipped: 290, skipped_binary: 246, skipped_large: 38, skipped_encoding: 6,
                            complete: true, ..Results::default()
                        });
                    });
                session.render_view(&view, viewport, UiScale::ONE);
                save_preview(&session, &context, width, height, "search-expanded");
                state.update(|app| {
                    app.search.ui.filters_open = false;
                    app.search.ui.details_open = true;
                });
                session.render_view(&view, viewport, UiScale::ONE);
                save_preview(&session, &context, width, height, "search-compact");
                state.update(|app| {
                    app.search.model.fixture_status().running = true;
                    app.search.ui.details_open = false;
                });
                session.render_view(&view, viewport, UiScale::ONE);
                save_preview(&session, &context, width, height, "search-running");
            }

            // Clearing invalidates queued worker results immediately.
            let token = Arc::new(std::sync::atomic::AtomicBool::new(false));
            runtime.install_search_cancel(token.clone());
            state.update(|app| {
                app.search.model.fixture_status().running = true;
            });
            let generation = state.get().search.model.status().generation;
            session.render_view(&view, viewport, UiScale::ONE);
            click(&mut session, &context, width - 50.0, 12.0);
            assert!(token.load(std::sync::atomic::Ordering::Relaxed));
            assert!(state.get().search.model.status().generation > generation);
            assert!(state.get().search.ui.query.text().is_empty());
            assert!(state.get().search.ui.query.focused);
            assert_eq!(state.get().search.model.status().results.matches, 0);
            session.render_view(&view, viewport, UiScale::ONE);
            for input in [
                InputEvent::PointerDown {
                    pointer: PointerData::mouse(Point::new(22.0, 12.0)),
                    button: PointerButton::Left,
                },
                InputEvent::PointerUp {
                    pointer: PointerData::mouse(Point::new(22.0, 12.0)),
                    button: PointerButton::Left,
                },
            ] {
                dispatch_runtime_output(
                    session.handle_input(input),
                    &context,
                    &lgui::window::WindowId::new("search-test"),
                    |action| session.handle_default_action(action),
                    |_| {},
                );
            }
            assert_eq!(
                exposed.lock().unwrap().as_ref().unwrap().get().drawer.mode,
                crate::ui::drawer::DrawerMode::Files
            );
        }
    }
}
