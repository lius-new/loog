//! Production-path coverage for image opening, reload, SVG editing and publications.
use super::{
    image_preview,
    state::{AppState, MainSurface},
    workspace::OpenMode,
};
use crate::ui::image_preview::{Command, MenuKind};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "loog-image-tests-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn png(path: &std::path::Path, width: u32) {
    image::RgbaImage::from_pixel(width, 12, image::Rgba([30, 100, 200, 128]))
        .save(path)
        .unwrap();
}
fn svg() -> &'static str {
    r##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="20"><rect width="32" height="20" fill="#818cf8"/></svg>"##
}
fn replace(app: &mut AppState, document: crate::domain::identity::FileId, text: &str) {
    let mut editor = app.workspace.document_editor_mut(document).unwrap();
    editor.select_all();
    loog_editor_core::normal::insert(&mut editor, text);
}
#[test]
fn image_open_save_rejection_and_reloads_preserve_view_positions() {
    let temp = Temp::new();
    let path = temp.0.join("中文.png");
    png(&path, 16);
    let mut app = AppState::new();
    let id = app
        .workspace
        .open_prepared_text(super::encoding::prepare(&path).unwrap(), OpenMode::Preview);
    let view = app.workspace.active().unwrap();
    assert_eq!(app.main_surface(), MainSurface::Image);
    assert!(app.workspace.active_editor().is_none());
    assert!(app.workspace.text_snapshots().is_empty());
    assert!(app.workspace.save_snapshot(id).is_none());
    assert!(app.workspace.set_encoding(id, Default::default()).is_err());
    image_preview::run(&mut app, view, Command::Zoom(Some(2.0)));
    image_preview::run(
        &mut app,
        view,
        Command::Background(crate::domain::media::Background::Dark),
    );
    let bytes = std::fs::read(&path).unwrap();
    let state = super::test_support::live(app);
    assert!(!super::workspace_commands::save_active_document(&state));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    state.update(|app| {
        app.workspace
            .documents
            .get_mut(&id)
            .unwrap()
            .disk_state
            .modified_time = None;
    });
    png(&path, 32);
    let updates = state
        .get()
        .workspace
        .prepare_disk_updates(std::slice::from_ref(&path), false);
    state.update(|app| {
        app.workspace.apply_disk_updates(updates);
    });
    let app = state.get();
    assert_eq!(
        app.workspace.documents[&id].image.as_ref().unwrap().width,
        32
    );
    assert_eq!(app.workspace.view(view).unwrap().image.scale, Some(2.0));
    assert_eq!(
        app.workspace.view(view).unwrap().image.background,
        crate::domain::media::Background::Dark
    );
    assert!(!app.workspace.is_dirty(id));
    let before = app.workspace.view(view).unwrap().image.scale.unwrap();
    let mut zoomed = app.clone();
    zoomed.editor.modifiers = lgui::core::KeyModifiers::CONTROL;
    image_preview::run(&mut zoomed, view, Command::Wheel(0.0, 36.0));
    assert!(zoomed.workspace.view(view).unwrap().image.scale.unwrap() > before);
    std::fs::write(&path, b"broken image").unwrap();
    let updates = app
        .workspace
        .prepare_disk_updates(std::slice::from_ref(&path), false);
    state.update(|app| {
        app.workspace.apply_disk_updates(updates);
    });
    assert_eq!(state.get().main_surface(), MainSurface::Empty);
    assert!(
        state
            .get()
            .workspace
            .pane_open_error(state.get().workspace.active_pane())
            .is_some()
    );
    assert!(state.get().chrome.toast.is_none());
    png(&path, 48);
    let updates = state
        .get()
        .workspace
        .prepare_disk_updates(std::slice::from_ref(&path), false);
    state.update(|app| {
        app.workspace.apply_disk_updates(updates);
    });
    assert_eq!(state.get().main_surface(), MainSurface::Image);
    assert_eq!(state.get().workspace.active(), Some(view));
}
#[test]
fn svg_defaults_to_preview_and_toggles_in_place_without_writing_until_save() {
    let temp = Temp::new();
    let path = temp.0.join("logo.svg");
    std::fs::write(&path, svg()).unwrap();
    let mut app = AppState::new();
    let document = app.workspace.open_prepared_text(
        super::encoding::prepare(&path).unwrap(),
        OpenMode::Permanent,
    );
    let preview = app.workspace.active().unwrap();
    assert_eq!(app.main_surface(), MainSurface::Image);
    assert!(image_preview::run(&mut app, preview, Command::Toggle));
    assert_eq!(app.main_surface(), MainSurface::Editor);
    assert_eq!(app.workspace.view_document(preview), Some(document));
    assert_eq!(app.workspace.pane_count(), 1);
    let unsaved = format!("{}\n<!-- unsaved -->", svg());
    replace(&mut app, document, &unsaved);
    assert!(app.workspace.is_view_dirty(preview));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), svg());
    assert!(image_preview::run(&mut app, preview, Command::Toggle));
    assert_eq!(app.workspace.active(), Some(preview));
    assert_eq!(app.workspace.active_items().len(), 1);
    image_preview::run(&mut app, preview, Command::Zoom(Some(2.0)));
    let saving = super::test_support::SavingUi::new(app);
    let state = saving.state.clone();
    assert!(super::workspace_commands::save_active_document(&state));
    saving.wait();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), unsaved);
    assert!(!state.get().workspace.is_view_dirty(preview));
    let layout = super::workspace_session::saved_app_editor_layout(&state.get()).unwrap();
    let json = serde_json::to_string(&layout).unwrap();
    let layout = serde_json::from_str(&json).unwrap();
    let mut restored = AppState::new();
    super::workspace_session::hydrate_editor_layout(&mut restored, Some(layout));
    for (id, path) in restored.workspace.loading_files() {
        let prepared = super::encoding::prepare(&path).unwrap();
        assert!(restored.workspace.install_restored(id, &path, prepared));
    }
    let restored_preview = restored.workspace.active().unwrap();
    assert_eq!(restored.main_surface(), MainSurface::Image);
    assert_eq!(
        restored
            .workspace
            .view(restored_preview)
            .unwrap()
            .image
            .scale,
        Some(2.0)
    );
    image_preview::run(
        &mut restored,
        restored_preview,
        Command::Menu(MenuKind::Zoom),
    );
    image_preview::run(&mut restored, restored_preview, Command::Toggle);
    image_preview::cleanup(&mut restored);
    assert!(restored.editor.images.menu.is_none());
    assert_eq!(restored.workspace.pane_count(), 1);
    assert_eq!(restored.main_surface(), MainSurface::Editor);
}
#[test]
fn tree_menu_opens_unopened_svg_as_source_and_toggles_only_that_file() {
    let temp = Temp::new();
    let path = temp.0.join("menu.svg");
    std::fs::write(&path, svg()).unwrap();
    let mut app = AppState::new();
    app.workspace
        .open_path("keep.txt".into(), "keep this text".into());
    let previous = app.workspace.active().unwrap();
    let state = super::test_support::live(app);
    assert!(image_preview::toggle_path(&state, path.clone()));
    let view = state.get().workspace.active().unwrap();
    assert_ne!(view, previous);
    assert_eq!(state.get().main_surface(), MainSurface::Editor);
    assert_eq!(state.get().workspace.active_editor().unwrap().text(), svg());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), svg());
    assert!(image_preview::toggle_path(&state, path));
    assert_eq!(state.get().main_surface(), MainSurface::Image);
    assert_eq!(state.get().workspace.active(), Some(view));
    assert_eq!(state.get().workspace.active_items().len(), 2);
    assert!(!image_preview::toggle_path(
        &state,
        temp.0.join("not-svg.png")
    ));
    assert_eq!(state.get().workspace.active(), Some(view));
}
fn drain(
    tasks: &super::test_support::UiTasks,
    state: &super::context::AppContext,
    document: crate::domain::identity::FileId,
    phase: &str,
    condition: impl Fn() -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        tasks.run_all();
        if condition() {
            return;
        }
        let app = state.get();
        let actual = app
            .editor
            .images
            .svg
            .get(&document)
            .map(|p| (p.stamp, p.density, p.image.is_some(), p.error.clone()));
        assert!(
            Instant::now() < deadline,
            "SVG worker timed out during {phase}; publication (stamp, density, image, error): {actual:?}; queued UI tasks: {:?}",
            tasks.pending_ids()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn svg_worker_keeps_last_valid_pixels_and_publishes_current_unsaved_revision() {
    for (scale, density) in [(0.75, 300), (1.0, 400), (1.5, 600), (2.0, 800)] {
        crate::ui::device_scale::observe(scale);
        svg_worker_revision_scenario(density);
    }
}

fn svg_worker_revision_scenario(expected_density: u32) {
    // System-font discovery is cold-start setup, not a worker publication budget.
    crate::infrastructure::image_filesystem::svg(svg(), 1.0).expect("SVG renderer setup");
    let mut app = AppState::new();
    let document = app.workspace.open_path("logo.svg".into(), svg().into());
    let preview = app.workspace.active().unwrap();
    let pane = app.workspace.active_pane();
    let source_pane = app
        .workspace
        .split(pane, crate::domain::pane_layout::Direction::Right)
        .unwrap();
    let source = app.workspace.active().unwrap();
    image_preview::run(&mut app, source, Command::Toggle);
    assert_eq!(
        app.workspace.pane(source_pane).unwrap().active(),
        Some(source)
    );
    app.workspace.activate(pane, preview);
    let state = super::test_support::live(app);
    let tasks = super::test_support::UiTasks::default();
    let service = super::image_service::ImageService::new(state.clone(), tasks.application());
    service.request_svg(&state.get(), preview);
    drain(&tasks, &state, document, "initial pixels", || {
        state
            .get()
            .editor
            .images
            .svg
            .get(&document)
            .is_some_and(|p| p.image.is_some())
    });
    let previous = image_preview::image(&state.get(), preview).unwrap();
    state.update(|app| {
        image_preview::run(app, preview, Command::Zoom(Some(4.0)));
    });
    service.request_svg(&state.get(), preview);
    drain(&tasks, &state, document, "zoom density", || {
        state
            .get()
            .editor
            .images
            .svg
            .get(&document)
            .is_some_and(|p| p.density == expected_density)
    });
    let previous = image_preview::image(&state.get(), preview).unwrap_or(previous);
    state.update(|app| replace(app, document, "<svg>"));
    service.request_svg(&state.get(), preview);
    drain(&tasks, &state, document, "invalid-source fallback", || {
        state
            .get()
            .editor
            .images
            .svg
            .get(&document)
            .is_some_and(|p| p.error.is_some())
    });
    assert!(Arc::ptr_eq(
        &previous,
        &image_preview::image(&state.get(), preview).unwrap()
    ));
    state.update(|app| replace(app, document, &svg().replace("#818cf8", "#ff0000")));
    service.request_svg(&state.get(), preview);
    state.update(|app| replace(app, document, &svg().replace("#818cf8", "#00ff00")));
    let stamp = state.get().workspace.documents[&document].stamp(document);
    service.request_svg(&state.get(), preview);
    drain(&tasks, &state, document, "latest unsaved revision", || {
        state
            .get()
            .editor
            .images
            .svg
            .get(&document)
            .is_some_and(|p| p.stamp == stamp && p.error.is_none())
    });
    assert_ne!(
        previous.identity,
        image_preview::image(&state.get(), preview)
            .unwrap()
            .identity
    );
    state.update(|app| {
        app.workspace.close(document);
    });
    assert!(state.get().editor.images.svg.is_empty());
    service.shutdown();
}
