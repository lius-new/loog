use super::*;

fn entry(id: &str, enabled: bool, update: bool) -> Entry {
    use crate::ui::extensions_binding::{Installed, Update};
    Entry {
        id: id.to_owned(),
        name: id.to_owned(),
        publisher: "Acme".to_owned(),
        version: "1.0.0".to_owned(),
        tagline: String::new(),
        updated: String::new(),
        icon: "extensions",
        icon_image: None,
        tint: 0,
        categories: vec!["Themes".to_owned()],
        readme_document: None,
        changelog_document: None,
        readme: Vec::new(),
        contributions: Vec::new(),
        changelog: Vec::new(),
        installed: Some(Installed {
            enabled,
            bundled: false,
            pinned: false,
            previous: None,
            location: String::new(),
            update: update.then(|| Update {
                version: "2.0.0".to_owned(),
                notes: String::new(),
            }),
            error: None,
            warning: None,
        }),
    }
}

fn frame() -> Frame {
    Frame {
        installed: vec![
            entry("a.on", true, false),
            entry("b.off", false, false),
            entry("c.update", true, true),
        ],
        available: vec![{
            let mut available = entry("d.new", true, false);
            available.installed = None;
            available
        }],
        ..Default::default()
    }
}

fn ids<'a>(entries: &[&'a Entry]) -> Vec<&'a str> {
    entries.iter().map(|entry| entry.id.as_str()).collect()
}

#[test]
fn the_list_shows_what_is_installed_and_a_search_adds_what_the_registry_offers() {
    let frame = frame();
    assert_eq!(ids(&visible(&frame, "")), ["a.on", "b.off", "c.update"]);
    assert_eq!(ids(&visible(&frame, "UPDATE")), ["c.update"]);
    // Installed matches come first, then the registry's extensions.
    assert_eq!(
        ids(&visible(&frame, ".")),
        ["a.on", "b.off", "c.update", "d.new"]
    );
    assert_eq!(ids(&visible(&frame, "d.new")), ["d.new"]);
    assert!(visible(&frame, "nothing like this").is_empty());
}

#[test]
fn the_detail_falls_back_to_the_first_result() {
    let frame = frame();
    let entries = visible(&frame, "");
    assert_eq!(current(&entries, Some("b.off")).unwrap().id, "b.off");
    assert_eq!(current(&entries, Some("gone")).unwrap().id, "a.on");
    assert_eq!(current(&entries, None).unwrap().id, "a.on");
    assert!(current(&[], Some("a.on")).is_none());
}

#[test]
fn installed_extension_list_materializes_only_the_visible_rows() {
    use lgui::{
        application::{AppView, ApplicationContext},
        core::{InputEvent, Point, UiScale, WheelDelta, dispatch_runtime_output},
        session::UiSession,
    };
    let data = Frame {
        installed: (0..5000)
            .map(|i| entry(&format!("entry.{i}"), true, false))
            .collect(),
        ..Default::default()
    };
    let view: AppView = Arc::new(move |cx| {
        let ui = cx.state_with(ExtensionsUiState::default);
        let commands = ui.clone();
        let data = data.clone();
        let binding = Binding::new(
            ui.into(),
            move || data.clone(),
            move |command| {
                commands.update(|ui| match command {
                    Command::SetTab(tab) => ui.tab = tab,
                    Command::Select(id) => ui.selected = Some(id),
                    _ => {}
                })
            },
        );
        let id = cx.use_stable_id();
        let focus = cx.focus_handle(id.clone());
        render(cx.viewport(), binding, id, focus)
    });
    let viewport = UiRect::new(0.0, 0.0, 1000.0, 700.0);
    let mut session = UiSession::new();
    session.render_view(&view, viewport, UiScale::ONE);
    assert!(session.tree().nodes().len() < 600);
    assert!(
        !session
            .tree()
            .nodes()
            .iter()
            .any(|n| n.text.as_deref() == Some("entry.4999"))
    );
    let application = ApplicationContext::empty(Default::default());
    let output = session.handle_input(InputEvent::Wheel {
        point: Point::new(40.0, 150.0),
        delta: WheelDelta::lines(0.0, -100000.0),
    });
    dispatch_runtime_output(
        output,
        &application,
        &lgui::window::WindowId::new("extension-list"),
        |action| session.handle_default_action(action),
        |_| {},
    );
    session.render_view(&view, viewport, UiScale::ONE);
    assert!(session.tree().nodes().len() < 600);
    assert!(
        session
            .tree()
            .nodes()
            .iter()
            .any(|n| n.text.as_deref() == Some("entry.4999"))
    );
}

#[test]
fn compact_search_keeps_the_complete_text_line_inside_its_clip() {
    use lgui::{
        application::{AppView, ApplicationContext},
        core::UiScale,
        session::UiSession,
    };
    let context = ApplicationContext::empty(Default::default());
    let _fonts = lgui::backend::install_text_environment(
        &context,
        lgui::render_skia::skia_text_system_handle(),
    );
    for query in ["", "rust gyp", "\u{4e2d}\u{6587}\u{6269}\u{5c55}"] {
        let view: AppView = Arc::new(move |cx| {
            let ui = cx.state_with(move || {
                let mut ui = ExtensionsUiState::default();
                ui.search.set_text(query);
                ui.search.focused = true;
                ui
            });
            let id = cx.use_stable_id();
            let focus = cx.focus_handle(id.clone());
            let binding = Binding::new(ui.into(), Frame::default, |_| {});
            render(cx.viewport(), binding, id, focus)
        });
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let mut session = UiSession::new();
            session.render_view(
                &view,
                UiRect::new(0.0, 0.0, 1000.0, 500.0),
                UiScale::new(scale),
            );
            let expected = if query.is_empty() { "Search" } else { query };
            let text = session
                .tree()
                .nodes()
                .iter()
                .find(|node| node.text.as_deref() == Some(expected))
                .unwrap();
            let layout = lgui::text::layout(&lgui::text::TextLayoutRequest::for_style(
                expected,
                text.layout_rect,
                &text.text_style.unwrap(),
            ))
            .unwrap();
            if query == "rust gyp" && scale == 1.0 {
                if let Ok(directory) = std::env::var("LOOG_PREVIEW_DIR") {
                    use lgui::{
                        core::PhysicalRect,
                        renderer::{FrameInfo, FrameReason},
                    };
                    let mut surface = lgui::render_skia::SkiaSoftwareSurface::new_opaque(64 << 20);
                    let viewport = PhysicalRect::new(0, 0, 1000, 500);
                    let damage = [viewport];
                    let frame = FrameInfo::new(
                        viewport,
                        &damage,
                        UiScale::ONE,
                        FrameReason::SceneChange,
                        true,
                    );
                    surface.draw(&session.tree().scene(), &frame).unwrap();
                    let mut rgba = surface.pixels().to_vec();
                    for pixel in rgba.as_chunks_mut::<4>().0 {
                        pixel.swap(0, 2);
                        pixel[3] = 255;
                    }
                    std::fs::create_dir_all(&directory).unwrap();
                    image::save_buffer(
                        std::path::Path::new(&directory).join("extensions-search.png"),
                        &rgba,
                        1000,
                        500,
                        image::ColorType::Rgba8,
                    )
                    .unwrap();
                }
            }
            assert!(!layout.lines().is_empty());
            for line in layout.lines() {
                assert!(
                    line.bounds.top >= text.paint_bounds.top - 0.01
                        && line.bounds.bottom <= text.paint_bounds.bottom + 0.01,
                    "query={query:?}, scale={scale}, line={:?}, clip={:?}",
                    line.bounds,
                    text.paint_bounds
                );
            }
        }
    }
}

#[test]
fn search_keeps_focus_and_edits_selection_ime_and_long_queries() {
    use lgui::{
        application::{AppView, ApplicationContext},
        core::{
            ImeEvent, InputEvent, KeyModifiers, KeyState, KeyboardEvent, LogicalKey, NamedKey,
            Point, PointerButton, PointerData, UiScale, dispatch_runtime_output,
        },
        prelude::State,
        session::UiSession,
    };
    use std::sync::Mutex;

    let exposed = Arc::new(Mutex::new(None::<State<ExtensionsUiState>>));
    let observed = exposed.clone();
    let view: AppView = Arc::new(move |cx| {
        let ui = cx.state_with(ExtensionsUiState::default);
        *observed.lock().unwrap() = Some(ui.clone());
        let commands = ui.clone();
        let binding = Binding::new(ui.into(), frame, move |command| {
            if matches!(command, Command::ClearSearch) {
                commands.update(|ui| ui.search.clear());
            }
        });
        let id = cx.use_stable_id();
        let focus = cx.focus_handle(id.clone());
        render(cx.viewport(), binding, id, focus)
    });
    let application = ApplicationContext::empty(Default::default());
    let _fonts = lgui::backend::install_text_environment(
        &application,
        lgui::render_skia::skia_text_system_handle(),
    );
    let viewport = UiRect::new(0.0, 0.0, 1000.0, 500.0);
    let mut session = UiSession::new();
    session.render_view(&view, viewport, UiScale::ONE);
    let state = exposed.lock().unwrap().clone().unwrap();
    let send = |session: &mut UiSession, event| {
        let output = session.handle_input(event);
        dispatch_runtime_output(
            output,
            &application,
            &lgui::window::WindowId::new("extensions-search"),
            |action| session.handle_default_action(action),
            |_| {},
        );
        session.render_view(&view, viewport, UiScale::ONE);
    };
    let key = |key, modifiers| {
        InputEvent::Keyboard(KeyboardEvent {
            state: KeyState::Down,
            key,
            modifiers,
            ..Default::default()
        })
    };
    let click = |session: &mut UiSession, point| {
        send(
            session,
            InputEvent::PointerDown {
                pointer: PointerData::mouse(point),
                button: PointerButton::Left,
            },
        );
        send(
            session,
            InputEvent::PointerUp {
                pointer: PointerData::mouse(point),
                button: PointerButton::Left,
            },
        );
    };
    click(&mut session, Point::new(30.0, 24.0));
    assert!(state.get().search.focused);
    assert_search_caret_is_visible(&session);
    send(&mut session, InputEvent::TextInput("update".into()));
    assert_eq!(state.get().search.text(), "update");
    assert!(
        state.get().search.focused,
        "Filtering must retain keyboard focus"
    );
    assert_eq!(
        ids(&visible(&frame(), state.get().search.text())),
        ["c.update"]
    );

    send(
        &mut session,
        key(LogicalKey::Named(NamedKey::Home), KeyModifiers::default()),
    );
    send(&mut session, InputEvent::TextInput("c.".into()));
    assert_eq!(state.get().search.text(), "c.update");
    send(
        &mut session,
        key(LogicalKey::Character("a".into()), KeyModifiers::CONTROL),
    );
    send(&mut session, InputEvent::TextInput("rust".into()));
    assert_eq!(
        state.get().search.text(),
        "rust",
        "Typing replaces the selected query"
    );

    send(
        &mut session,
        InputEvent::Ime(ImeEvent::Preedit {
            text: "zhongwen".into(),
            cursor: Some(8..8),
        }),
    );
    assert_eq!(state.get().search.text(), "rust");
    assert_eq!(state.get().search.preedit, "zhongwen");
    send(
        &mut session,
        InputEvent::Ime(ImeEvent::Commit("\u{4e2d}\u{6587}".into())),
    );
    assert_eq!(state.get().search.text(), "rust\u{4e2d}\u{6587}");
    assert!(state.get().search.preedit.is_empty());
    send(
        &mut session,
        key(
            LogicalKey::Named(NamedKey::Backspace),
            KeyModifiers::default(),
        ),
    );
    assert_eq!(state.get().search.text(), "rust\u{4e2d}");

    send(
        &mut session,
        key(LogicalKey::Character("a".into()), KeyModifiers::CONTROL),
    );
    let long = "rust-analyzer ".repeat(40);
    send(&mut session, InputEvent::TextInput(long.clone()));
    assert_eq!(state.get().search.text(), long);
    assert!(state.get().search.scroll_x > 0.0);
    send(
        &mut session,
        key(LogicalKey::Named(NamedKey::Home), KeyModifiers::default()),
    );
    assert_eq!(state.get().search.scroll_x, 0.0);
    send(
        &mut session,
        key(LogicalKey::Named(NamedKey::End), KeyModifiers::default()),
    );
    assert!(state.get().search.scroll_x > 0.0);
    send(
        &mut session,
        key(LogicalKey::Named(NamedKey::Enter), KeyModifiers::default()),
    );
    assert_eq!(
        state.get().search.text(),
        long,
        "Enter must not add a newline"
    );
    send(
        &mut session,
        key(LogicalKey::Named(NamedKey::Escape), KeyModifiers::default()),
    );
    assert_eq!(state.get().search.text(), "");
    assert_eq!(state.get().search.scroll_x, 0.0);
    assert!(state.get().search.focused);
    assert_search_caret_is_visible(&session);

    // Clicking at the start must move the insertion point using the drawn text.
    send(&mut session, InputEvent::TextInput("rust".into()));
    click(&mut session, Point::new(20.0, 24.0));
    send(&mut session, InputEvent::TextInput("a".into()));
    assert_eq!(state.get().search.text(), "arust");
}

/// Check the submitted scene and pixels, rather than only a focused state flag.
fn assert_search_caret_is_visible(session: &lgui::session::UiSession) {
    use lgui::{
        core::{PhysicalRect, UiScale},
        renderer::{FrameInfo, FrameReason},
    };
    let input = session
        .tree()
        .nodes()
        .iter()
        .find(|node| {
            node.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some("Search extensions")
        })
        .unwrap();
    let caret = input
        .ime_cursor_rect
        .expect("Focused search must expose a caret");
    let accent = theme::c().accent;
    let scene = session.tree().scene();
    assert!(
        session
            .tree()
            .nodes()
            .iter()
            .any(|node| node.layout_rect == caret && node.style.fill == Some(accent)),
        "Focused search has no caret drawing node: {caret:?}; narrow panels: {:?}",
        session
            .tree()
            .nodes()
            .iter()
            .filter(|node| node.layout_rect.width() <= 2.0 && node.layout_rect.top < 60.0)
            .map(|node| (node.layout_rect, node.style))
            .collect::<Vec<_>>()
    );
    let mut surface = lgui::render_skia::SkiaSoftwareSurface::new_opaque(64 << 20);
    let viewport = PhysicalRect::new(0, 0, 1000, 500);
    let damage = [viewport];
    let frame = FrameInfo::new(
        viewport,
        &damage,
        UiScale::ONE,
        FrameReason::SceneChange,
        true,
    );
    surface.draw(&scene, &frame).unwrap();
    let (x, y) = (
        caret.left.round() as usize,
        ((caret.top + caret.bottom) / 2.0).floor() as usize,
    );
    let offset = (y * 1000 + x) * 4;
    let expected = [
        (accent.0 & 255) as u8,
        ((accent.0 >> 8) & 255) as u8,
        ((accent.0 >> 16) & 255) as u8,
    ];
    assert_eq!(
        &surface.pixels()[offset..offset + 3],
        &expected,
        "Caret pixels must be visible"
    );
    if let Ok(directory) = std::env::var("LOOG_PREVIEW_DIR") {
        let mut rgba = surface.pixels().to_vec();
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        std::fs::create_dir_all(&directory).unwrap();
        image::save_buffer(
            std::path::Path::new(&directory).join("extensions-search-focused-empty.png"),
            &rgba,
            1000,
            500,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
}
