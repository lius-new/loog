//! Extensions page composition.
//!
//! Installed entries come from the extension host; imports use the same package
//! installer as the command palette.
use crate::app::context::AppContext;
use crate::ui::components::state_access::StateAccess;
use crate::ui::extensions_binding::{Action, Binding, Command, DetailTab};
use lgui::core::{UiFocusHandle, UiId};
use lgui::prelude::{Element, UiRect};

mod installed;
#[cfg(test)]
mod markdown_tests;
#[cfg(test)]
mod sample;

pub(crate) use installed::frame as installed_frame;

#[cfg(test)]
static FRAME: std::sync::LazyLock<crate::ui::extensions_binding::Frame> =
    std::sync::LazyLock::new(sample::frame);

/// Whether an enabled extension failed to start, which the title bar flags.
pub fn needs_attention(state: &AppContext) -> bool {
    state
        .services()
        .and_then(|runtime| runtime.extensions())
        .is_some_and(|host| host.needs_attention())
}

pub fn binding(state: &AppContext) -> Binding {
    binding_in(state, None)
}

fn binding_in(state: &AppContext, pane: Option<crate::domain::identity::PaneId>) -> Binding {
    let writer = state.clone();
    let reader = state.clone();
    let documents = state.clone();
    Binding::new(
        StateAccess::project(
            state.model(),
            |app| &app.extensions_ui,
            |app| &mut app.extensions_ui,
        ),
        move || {
            reader
                .services()
                .and_then(|runtime| runtime.extensions())
                .map(|host| host.frame())
                .unwrap_or_default()
        },
        move |command| run(&writer, command),
    )
    .with_documentation(move |entry, tab| {
        let document = entry.documentation(tab)?;
        let app = documents.get();
        let view = pane
            .and_then(|pane| app.workspace.pane(pane).and_then(|pane| pane.active()))
            .or_else(|| app.workspace.active())
            .and_then(|id| app.workspace.view(id));
        Some(
            match (
                documents.services().and_then(|runtime| runtime.markdown()),
                view,
            ) {
                (Some(service), Some(view)) => service.bind_extension_card(
                    view.id,
                    view.input_generation,
                    &entry.id,
                    tab,
                    document,
                ),
                _ => document.content.clone().into(),
            },
        )
    })
}

pub fn run(state: &AppContext, command: Command) {
    if command == Command::Import {
        import_package(state);
        return;
    }
    if let Command::Act(action @ (Action::Enable | Action::Disable | Action::Uninstall), id) =
        &command
    {
        let Some(host) = state.services().and_then(|runtime| runtime.extensions()) else {
            state.update(|app| app.show_error("Extensions are not available"));
            return;
        };
        match action {
            Action::Enable => host.set_enabled(state, id, true),
            Action::Disable => host.set_enabled(state, id, false),
            _ => host.uninstall(state, id),
        }
        return;
    }
    state.update(|app| {
        let ui = &mut app.extensions_ui;
        match command {
            Command::SetFilter(filter) => ui.filter = filter,
            Command::Select(id) => {
                ui.selected = Some(id);
                ui.tab = DetailTab::Details;
            }
            Command::SetTab(tab) => ui.tab = tab,
            Command::ClearSearch => ui.search.clear(),
            // Reloading arrives with the extension manager; the rest was handled above.
            Command::Act(..) | Command::Import => {}
        }
    });
}

pub(crate) fn import_package(state: &AppContext) {
    let Some(host) = state.services().and_then(|runtime| runtime.extensions()) else {
        state.update(|app| app.show_error("Extensions are not available"));
        return;
    };
    if let Some(package) = crate::infrastructure::file_dialogs::pick_extension_package() {
        host.import(state, &package);
    }
}

pub fn render(
    pane: crate::domain::identity::PaneId,
    rect: UiRect,
    state: AppContext,
    id: UiId,
    focus: UiFocusHandle,
) -> Element {
    crate::ui::extensions_page::render(rect, binding_in(&state, Some(pane)), id, focus)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::extensions_binding::{ExtensionsUiState, Frame};
    use lgui::application::{AppView, ApplicationContext};
    use lgui::core::UiScale;
    use lgui::session::UiSession;
    use std::sync::Arc;

    fn texts(session: &UiSession) -> Vec<String> {
        session
            .tree()
            .nodes()
            .iter()
            .filter_map(|node| node.text.as_deref().map(str::to_owned))
            .collect()
    }

    /// Every tab and every sample extension draws at several sizes.
    #[test]
    fn every_tab_and_extension_renders() {
        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let frame = FRAME.clone();
        let tabs = [
            DetailTab::Details,
            DetailTab::Contributions,
            DetailTab::Changelog,
        ];
        for (width, height) in [(1280.0, 800.0), (900.0, 600.0), (640.0, 420.0)] {
            let rect = UiRect::new(0.0, 0.0, width, height);
            for entry in &frame.installed {
                for tab in tabs {
                    let id = entry.id.clone();
                    let view: AppView = Arc::new(move |cx| {
                        let ui = cx.state_with({
                            let id = id.clone();
                            move || ExtensionsUiState {
                                tab,
                                selected: Some(id.clone()),
                                ..Default::default()
                            }
                        });
                        let search = cx.use_stable_id();
                        let focus = cx.focus_handle(search.clone());
                        let binding = Binding::new(StateAccess::from(ui), || FRAME.clone(), |_| {});
                        crate::ui::extensions_page::render(rect, binding, search, focus)
                    });
                    let mut session = UiSession::new();
                    session.render_view(&view, rect, UiScale::ONE);
                    // A narrow pane may shorten the title with an ellipsis.
                    let shown = texts(&session);
                    assert!(
                        width < 900.0 || shown.iter().any(|text| text.contains(&entry.name)),
                        "{} missing at {width}x{height} in {tab:?}",
                        entry.name
                    );
                }
            }
        }
    }

    /// Draws the page to PNG files in `LOOM_PREVIEW_DIR`, to look at the layout.
    /// Run on purpose: `cargo test preview_png -- --ignored`.
    #[test]
    #[ignore]
    fn preview_png() {
        use lgui::assets::RenderResources;
        use lgui::assets::icons::IconRegistration;
        use lgui::core::PhysicalRect;
        use lgui::render_skia::SkiaSoftwareSurface;
        use lgui::renderer::{FrameInfo, FrameReason};

        let Ok(dir) = std::env::var("LOOM_PREVIEW_DIR") else {
            return;
        };
        let context = ApplicationContext::empty(Default::default());
        context.resources().provide(IconRegistration(Arc::new(
            crate::app::bootstrap::svg_icons(),
        )));
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let (width, height) = (1280.0_f32, 800.0_f32);
        let rect = UiRect::new(0.0, 0.0, width, height);
        let mut packages = crate::app::extensions::ExtensionService::new(
            std::env::temp_dir().join("loom-icon-preview"),
        );
        let samples =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/extensions/packages");
        packages
            .load_package(&samples.join("loom-midnight"))
            .unwrap();
        packages
            .load_package(&samples.join("loom-properties"))
            .unwrap();
        let packages = installed_frame(&packages);
        let shots = [
            ("installed-details", "acme.weave", DetailTab::Details),
            ("installed-contrib", "acme.weave", DetailTab::Contributions),
            ("failed", "acme.lint", DetailTab::Details),
            ("themes", "acme.night", DetailTab::Contributions),
            ("package-theme", "loom.midnight", DetailTab::Details),
            ("package-language", "loom.properties", DetailTab::Details),
            ("package-changelog", "loom.midnight", DetailTab::Changelog),
        ];
        for (name, id, tab) in shots {
            let entries = if id.starts_with("loom.") {
                packages.clone()
            } else {
                FRAME.clone()
            };
            let mut app = crate::app::state::AppState::new();
            app.workspace.open_extensions();
            app.extensions_ui.selected = Some(id.to_owned());
            app.extensions_ui.tab = tab;
            let preview = crate::app::test_support::SavingUi::new(app);
            preview
                .runtime
                .install_markdown(crate::app::markdown_service::MarkdownService::new(
                    preview.state.clone(),
                    preview.tasks.application(),
                    Arc::new(super::markdown_tests::Clipboard::default()),
                ));
            let documents = preview.runtime.markdown().unwrap();
            let document_view = preview.state.get().workspace.active().unwrap();
            let preview_state = preview.state.clone();
            let view: AppView = Arc::new(move |cx| {
                let ui = StateAccess::project(
                    preview_state.model(),
                    |app| &app.extensions_ui,
                    |app| &mut app.extensions_ui,
                );
                let search = cx.use_stable_id();
                let focus = cx.focus_handle(search.clone());
                let entries = entries.clone();
                let documents = documents.clone();
                let binding = Binding::new(ui, move || entries.clone(), |_| {}).with_documentation(
                    move |entry, tab| {
                        entry.documentation(tab).map(|document| {
                            documents.bind_extension_card(
                                document_view,
                                0,
                                &entry.id,
                                tab,
                                document,
                            )
                        })
                    },
                );
                crate::ui::extensions_page::render(rect, binding, search, focus)
            });
            let mut session = UiSession::new();
            session.render_view(&view, rect, UiScale::ONE);
            if id.starts_with("loom.") {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                loop {
                    preview.tasks.run_all();
                    // SavingUi owns a separate session, so deliver its repaint here.
                    session.invalidate_all();
                    session.render_view(&view, rect, UiScale::ONE);
                    if session.tree().scene().image_requests().iter().any(|request| matches!(request.source(), lgui::core::UiImageSource::Bytes { key, .. } if key.starts_with("markdown-png-"))) { break; }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "Markdown preview {name} did not render: {:?}",
                        texts(&session)
                    );
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            }
            let scene = session.tree().scene();
            let mut surface = SkiaSoftwareSurface::new_opaque(64 << 20);
            let viewport = PhysicalRect::new(0, 0, width as i32, height as i32);
            let damage = [viewport];
            let frame = FrameInfo::new(
                viewport,
                &damage,
                UiScale::ONE,
                FrameReason::SceneChange,
                true,
            );
            lgui::assets::backend::with_render_resources(&context, RenderResources::new(), || {
                surface.draw(&scene, &frame).expect("draw the page")
            });
            let mut rgba = surface.pixels().to_vec();
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.swap(0, 2);
                pixel[3] = 255;
            }
            std::fs::create_dir_all(&dir).unwrap();
            image::save_buffer(
                std::path::Path::new(&dir).join(format!("{name}.png")),
                &rgba,
                width as u32,
                height as u32,
                image::ColorType::Rgba8,
            )
            .unwrap();
            preview.runtime.clear_markdown();
        }
    }

    #[test]
    fn package_artwork_is_drawn_in_both_the_list_and_detail() {
        let mut service = crate::app::extensions::ExtensionService::new(
            std::env::temp_dir().join("loom-icon-scene"),
        );
        let samples =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/extensions/packages");
        service
            .load_package(&samples.join("loom-midnight"))
            .unwrap();
        service
            .load_package(&samples.join("loom-properties"))
            .unwrap();
        let entries = installed_frame(&service);
        let image = entries
            .installed
            .iter()
            .find(|entry| entry.id == "loom.midnight")
            .unwrap()
            .icon_image
            .as_ref()
            .unwrap();
        let expected = lgui::core::UiImageSource::bytes(
            format!("extension-icon-{:016x}", image.identity),
            0,
            image.bytes.clone(),
        );
        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let rect = UiRect::new(0.0, 0.0, 1280.0, 800.0);
        let view: AppView = Arc::new(move |cx| {
            let ui = cx.state_with(|| ExtensionsUiState {
                selected: Some("loom.midnight".to_owned()),
                ..Default::default()
            });
            let search = cx.use_stable_id();
            let focus = cx.focus_handle(search.clone());
            let entries = entries.clone();
            crate::ui::extensions_page::render(
                rect,
                Binding::new(StateAccess::from(ui), move || entries.clone(), |_| {}),
                search,
                focus,
            )
        });
        let mut session = UiSession::new();
        session.render_view(&view, rect, UiScale::ONE);
        let requests = session.tree().scene().image_requests();
        assert_eq!(requests.len(), 3, "two list icons and one detail icon");
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.source() == &expected)
                .count(),
            2
        );
    }

    #[test]
    fn an_extension_with_an_update_says_so_on_its_row() {
        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let rect = UiRect::new(0.0, 0.0, 1280.0, 800.0);
        let view: AppView = Arc::new(move |cx| {
            let ui = cx.state_with(ExtensionsUiState::default);
            let search = cx.use_stable_id();
            let focus = cx.focus_handle(search.clone());
            let binding = Binding::new(StateAccess::from(ui), || FRAME.clone(), |_| {});
            crate::ui::extensions_page::render(rect, binding, search, focus)
        });
        let mut session = UiSession::new();
        session.render_view(&view, rect, UiScale::ONE);
        let shown = texts(&session);
        assert!(shown.iter().any(|text| text.contains("Update to 0.5.0")));
        assert!(!shown.iter().any(|text| text == "Marketplace"));
    }

    #[test]
    fn an_empty_install_invites_importing() {
        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let rect = UiRect::new(0.0, 0.0, 900.0, 600.0);
        let view: AppView = Arc::new(move |cx| {
            let ui = cx.state_with(ExtensionsUiState::default);
            let search = cx.use_stable_id();
            let focus = cx.focus_handle(search.clone());
            let binding = Binding::new(StateAccess::from(ui), Frame::default, |_| {});
            crate::ui::extensions_page::render(rect, binding, search, focus)
        });
        let mut session = UiSession::new();
        session.render_view(&view, rect, lgui::core::UiScale::ONE);
        assert!(
            texts(&session)
                .iter()
                .any(|text| text == "No extensions installed")
        );
    }

    #[test]
    fn the_footer_and_empty_page_offer_import_and_dispatch_it() {
        use lgui::core::{InputEvent, Point, PointerButton, PointerData, dispatch_runtime_output};
        use std::sync::Mutex;

        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let rect = UiRect::new(0.0, 0.0, 900.0, 600.0);
        for empty in [false, true] {
            let commands = Arc::new(Mutex::new(Vec::new()));
            let output = commands.clone();
            let view: AppView = Arc::new(move |cx| {
                let ui = cx.state_with(ExtensionsUiState::default);
                let search = cx.use_stable_id();
                let focus = cx.focus_handle(search.clone());
                let output = output.clone();
                let binding = Binding::new(
                    StateAccess::from(ui),
                    move || {
                        if empty {
                            Frame::default()
                        } else {
                            FRAME.clone()
                        }
                    },
                    move |command| output.lock().unwrap().push(command),
                );
                crate::ui::extensions_page::render(rect, binding, search, focus)
            });
            let mut session = UiSession::new();
            session.render_view(&view, rect, UiScale::ONE);
            assert!(
                !texts(&session)
                    .iter()
                    .any(|text| text == "Open extensions folder" || text == "Reload")
            );
            let node = session
                .tree()
                .nodes()
                .iter()
                .find(|node| {
                    node.semantics.as_ref().and_then(|s| s.name.as_deref())
                        == Some("Import Extension")
                })
                .expect("an import button even with no installed extensions");
            let button = session.tree().hit_for_id(&node.id).unwrap().rect;
            if !empty {
                assert_eq!(button.bottom, rect.bottom - 4.0);
            }
            let point = Point::new(
                (button.left + button.right) / 2.0,
                (button.top + button.bottom) / 2.0,
            );
            for input in [
                InputEvent::PointerDown {
                    pointer: PointerData::mouse(point),
                    button: PointerButton::Left,
                },
                InputEvent::PointerUp {
                    pointer: PointerData::mouse(point),
                    button: PointerButton::Left,
                },
            ] {
                dispatch_runtime_output(
                    session.handle_input(input),
                    &context,
                    &lgui::window::WindowId::new("extensions-import-test"),
                    |action| session.handle_default_action(action),
                    |_| {},
                );
                session.render_view(&view, rect, UiScale::ONE);
            }
            assert_eq!(*commands.lock().unwrap(), [Command::Import]);
        }
    }
}
