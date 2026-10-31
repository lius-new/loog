//! Extensions page composition.
//!
//! The page is a presentation stage: its entries are placeholders and no
//! command acts on an extension yet. Navigation (source, filter, selection,
//! tab, search) is real, so the whole interface can be exercised.
use crate::app::context::AppContext;
use crate::ui::components::state_access::StateAccess;
use crate::ui::extensions_binding::{
    Action, Binding, Command, DetailTab, Entry, Frame, Source, StateFilter,
};
use lgui::core::{UiFocusHandle, UiId};
use lgui::prelude::{Element, UiRect};
use std::sync::LazyLock;

mod sample;

static FRAME: LazyLock<Frame> = LazyLock::new(sample::frame);

/// Whether an enabled extension failed to start, which the title bar flags.
pub fn needs_attention() -> bool {
    FRAME.installed.iter().any(Entry::failed)
}

pub fn binding(state: &AppContext) -> Binding {
    let writer = state.clone();
    Binding::new(
        StateAccess::project(
            state.model(),
            |app| &app.extensions_ui,
            |app| &mut app.extensions_ui,
        ),
        || FRAME.clone(),
        move |command| run(&writer, command),
    )
}

pub fn run(state: &AppContext, command: Command) {
    state.update(|app| {
        let ui = &mut app.extensions_ui;
        match command {
            Command::SetSource(source) => {
                ui.source = source;
                ui.search.clear();
                ui.filter = StateFilter::All;
                ui.category = None;
                ui.tab = DetailTab::Details;
            }
            Command::SetFilter(filter) => ui.filter = filter,
            Command::SetCategory(category) => ui.category = category,
            Command::Select(id) => {
                ui.selected = Some(id);
                ui.tab = DetailTab::Details;
            }
            Command::SetTab(tab) => ui.tab = tab,
            Command::ClearSearch => ui.search.clear(),
            Command::SearchMarketplace => {
                ui.source = Source::Marketplace;
                ui.filter = StateFilter::All;
                ui.category = None;
                ui.tab = DetailTab::Details;
            }
            // Going from the marketplace to the same extension in the installed list.
            Command::Act(Action::Manage, id) => {
                ui.source = Source::Installed;
                ui.search.clear();
                ui.filter = StateFilter::All;
                ui.selected = Some(id);
                ui.tab = DetailTab::Details;
            }
            // Acting on extensions arrives with the extension manager.
            Command::Act(..) | Command::Footer(_) => {}
        }
    });
}

pub fn render(rect: UiRect, state: AppContext, id: UiId, focus: UiFocusHandle) -> Element {
    crate::ui::extensions_page::render(rect, binding(&state), id, focus)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::extensions_binding::ExtensionsUiState;
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

    /// Every list, every tab and every sample extension draws at several sizes.
    #[test]
    fn every_source_tab_and_extension_renders() {
        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let frame = FRAME.clone();
        let sources = [
            (Source::Installed, &frame.installed),
            (Source::Marketplace, &frame.marketplace),
            (Source::Updates, &frame.installed),
        ];
        let tabs = [
            DetailTab::Details,
            DetailTab::Contributions,
            DetailTab::Changelog,
            DetailTab::Runtime,
        ];
        for (width, height) in [(1280.0, 800.0), (900.0, 600.0), (640.0, 420.0)] {
            let rect = UiRect::new(0.0, 0.0, width, height);
            for (source, entries) in sources {
                for entry in entries {
                    let has_update = entry.installed.as_ref().is_some_and(|i| i.update.is_some());
                    if source == Source::Updates && !has_update {
                        continue;
                    }
                    for tab in tabs {
                        let id = entry.id.clone();
                        let view: AppView = Arc::new(move |cx| {
                            let ui = cx.state_with({
                                let id = id.clone();
                                move || ExtensionsUiState {
                                    source,
                                    tab,
                                    selected: Some(id.clone()),
                                    ..Default::default()
                                }
                            });
                            let search = cx.use_stable_id();
                            let focus = cx.focus_handle(search.clone());
                            let binding =
                                Binding::new(StateAccess::from(ui), || FRAME.clone(), |_| {});
                            crate::ui::extensions_page::render(rect, binding, search, focus)
                        });
                        let mut session = UiSession::new();
                        session.render_view(&view, rect, UiScale::ONE);
                        // A narrow pane may shorten the title with an ellipsis.
                        let shown = texts(&session);
                        assert!(
                            width < 900.0 || shown.iter().any(|text| text.contains(&entry.name)),
                            "{} missing at {width}x{height} in {source:?}/{tab:?}",
                            entry.name
                        );
                    }
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
        let shots = [
            (
                "installed-details",
                Source::Installed,
                "acme.weave",
                DetailTab::Details,
            ),
            (
                "installed-contrib",
                Source::Installed,
                "acme.weave",
                DetailTab::Contributions,
            ),
            ("failed", Source::Installed, "acme.lint", DetailTab::Details),
            (
                "themes",
                Source::Installed,
                "acme.night",
                DetailTab::Contributions,
            ),
            (
                "runtime",
                Source::Installed,
                "example.tools",
                DetailTab::Runtime,
            ),
            (
                "market",
                Source::Marketplace,
                "ridge.bracket",
                DetailTab::Details,
            ),
            ("updates", Source::Updates, "acme.weave", DetailTab::Details),
        ];
        for (name, source, id, tab) in shots {
            let view: AppView = Arc::new(move |cx| {
                let ui = cx.state_with(move || ExtensionsUiState {
                    source,
                    tab,
                    selected: Some(id.to_owned()),
                    ..Default::default()
                });
                let search = cx.use_stable_id();
                let focus = cx.focus_handle(search.clone());
                let binding = Binding::new(StateAccess::from(ui), || FRAME.clone(), |_| {});
                crate::ui::extensions_page::render(rect, binding, search, focus)
            });
            let mut session = UiSession::new();
            session.render_view(&view, rect, UiScale::ONE);
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
        }
    }

    #[test]
    fn an_empty_install_invites_browsing() {
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
}
