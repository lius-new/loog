//! Image presentation commands; SVG shares the normal editable document.
use crate::app::context::AppContext;
use crate::app::state::AppState;
use crate::domain::document::DocumentStamp;
use crate::domain::identity::{FileId, PaneId, ViewId};
use crate::domain::media::Image;
use crate::domain::view::Presentation;
use crate::ui::image_preview::{self as ui, Command, Frame, MenuKind};
use lgui::core::{KeyboardEvent, LogicalKey, NamedKey, UiFocusHandle, UiId};
use lgui::prelude::{Element, UiRect};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct SvgPreview {
    pub stamp: DocumentStamp,
    pub density: u32,
    pub image: Option<Arc<Image>>,
    pub error: Option<String>,
}
#[derive(Clone)]
pub struct GifPreview {
    pub incarnation: u64,
    pub identity: u64,
    pub frame: crate::domain::media::Frame,
    pub error: Option<String>,
}
#[derive(Clone, Copy)]
pub struct Menu {
    pub view: ViewId,
    pub kind: MenuKind,
    pub selected: usize,
    pub hovered: Option<usize>,
}
#[derive(Clone, Copy)]
pub struct Drag {
    pub view: ViewId,
    pub origin: (f32, f32),
    pub offset: (f32, f32),
}
#[derive(Clone, Default)]
pub struct State {
    /// Delayed feedback belongs to a visible view and its current document lifetime.
    pub loading: HashMap<ViewId, DocumentStamp>,
    pub svg: HashMap<FileId, SvgPreview>,
    pub gif: HashMap<FileId, GifPreview>,
    pub bounds: HashMap<ViewId, UiRect>,
    pub pointer: HashMap<ViewId, (f32, f32)>,
    pub menu: Option<Menu>,
    pub drag: Option<Drag>,
}
pub fn cleanup(app: &mut AppState) {
    app.editor.images.loading = app
        .editor
        .images
        .loading
        .iter()
        .filter(|(view, stamp)| loading_stamp(app, **view) == Some(**stamp))
        .map(|(view, stamp)| (*view, *stamp))
        .collect();
    app.editor.images.gif.retain(|id, preview| {
        app.workspace.documents.get(id).is_some_and(|doc| {
            doc.incarnation == preview.incarnation
                && doc
                    .image
                    .as_ref()
                    .is_some_and(|image| image.identity == preview.identity)
        })
    });
    app.editor
        .images
        .svg
        .retain(|id, _| app.workspace.documents.contains_key(id));
    app.editor
        .images
        .bounds
        .retain(|id, _| app.workspace.views.contains_key(id));
    app.editor
        .images
        .pointer
        .retain(|id, _| app.workspace.views.contains_key(id));
    if app.editor.images.menu.is_some_and(|menu| {
        app.workspace.active() != Some(menu.view)
            || app
                .workspace
                .view_document(menu.view)
                .is_some_and(|id| app.workspace.open_error(id).is_some())
            || app
                .workspace
                .view(menu.view)
                .is_none_or(|v| v.presentation != Presentation::Image)
    }) {
        app.editor.images.menu = None;
    }
    if app.editor.images.drag.is_some_and(|drag| {
        app.workspace
            .view(drag.view)
            .is_none_or(|v| v.presentation != Presentation::Image)
    }) {
        app.editor.images.drag = None;
    }
}
pub fn eligible(app: &AppState, view: ViewId) -> bool {
    app.workspace
        .view_document(view)
        .and_then(|id| app.workspace.documents.get(&id))
        .is_some_and(|doc| doc.is_text() && crate::domain::media::is_svg(doc.meta.language_hint()))
}
pub fn image(app: &AppState, view: ViewId) -> Option<Arc<Image>> {
    let id = app.workspace.view_document(view)?;
    let doc = app.workspace.documents.get(&id)?;
    doc.image.clone().or_else(|| {
        app.editor
            .images
            .svg
            .get(&id)
            .filter(|p| p.stamp.incarnation == doc.incarnation)
            .and_then(|p| p.image.clone())
    })
}
pub fn frame(app: &AppState, view: ViewId) -> Option<Frame> {
    let value = app.workspace.view(view)?;
    if value.presentation != Presentation::Image {
        return None;
    }
    let id = value.document()?;
    let doc = app.workspace.documents.get(&id)?;
    if doc.open_error.is_some() {
        return None;
    }
    let gif = app.editor.images.gif.get(&id).filter(|p| {
        p.incarnation == doc.incarnation
            && doc
                .image
                .as_ref()
                .is_some_and(|image| image.identity == p.identity)
    });
    let error = gif.and_then(|p| p.error.clone()).or_else(|| {
        app.editor
            .images
            .svg
            .get(&id)
            .filter(|p| p.stamp == doc.stamp(id))
            .and_then(|p| p.error.clone())
    });
    Some(Frame {
        image: image(app, view),
        animation_frame: gif.map(|p| p.frame.clone()),
        format: doc.image.as_ref().map_or(
            if crate::domain::media::is_svg(doc.meta.language_hint()) {
                "SVG"
            } else {
                "Image"
            },
            |image| image.format,
        ),
        byte_len: doc
            .image
            .as_ref()
            .map_or(doc.disk_state.size, |image| image.byte_len),
        position: value.image,
        bounds: app
            .editor
            .images
            .bounds
            .get(&view)
            .copied()
            .unwrap_or(UiRect::new(0.0, 0.0, 512.0, 512.0)),
        error,
        show_loading: app.editor.images.loading.get(&view) == Some(&doc.stamp(id)),
    })
}
/// A visible image canvas waiting for its first usable pixels, regardless of format.
pub(super) fn loading_stamp(app: &AppState, view: ViewId) -> Option<DocumentStamp> {
    let pane = app.workspace.pane_of(view)?;
    if app.workspace.pane(pane)?.active() != Some(view) {
        return None;
    }
    let frame = frame(app, view)?;
    if frame.image.is_some() || frame.error.is_some() {
        return None;
    }
    let id = app.workspace.view_document(view)?;
    Some(app.workspace.documents.get(&id)?.stamp(id))
}
pub fn send(state: &AppContext, view: ViewId) -> ui::Send {
    let state = state.clone();
    Arc::new(move |command| {
        state.try_update(|app| run(app, view, command));
    })
}
pub fn render(
    pane: PaneId,
    rect: UiRect,
    state: AppContext,
    id: UiId,
    focus: UiFocusHandle,
) -> Element {
    let app = state.get();
    let Some(view) = app.workspace.pane(pane).and_then(|p| p.active()) else {
        return lgui::prelude::group(rect);
    };
    // Geometry bookkeeping deliberately does not invalidate the application.
    state.model().try_update(|app| {
        app.editor.images.bounds.insert(view, rect);
        false
    });
    if let Some(service) = state.services().and_then(|runtime| runtime.images()) {
        service.request_svg(&app, view);
    }
    let mut frame = frame(&app, view).unwrap();
    frame.bounds = rect;
    ui::render(frame, id, focus, send(&state, view))
}
fn reset_input(app: &mut AppState) {
    app.editor.ime = None;
    app.editor.drag = None;
    app.editor.images.drag = None;
    app.editor.menu = None;
    app.editor.images.menu = None;
    app.editor.language.close_popups();
}
/// The tree menu acts on the requested SVG, including one not loaded yet.
pub fn toggle_path(state: &AppContext, path: std::path::PathBuf) -> bool {
    if !crate::domain::media::is_svg(&path) {
        return false;
    }
    let loader = state.services().and_then(|runtime| runtime.file_loader());
    let mut opened = false;
    state.update(|app| {
        if let Some(id) = app.workspace.file_id_for_path(&path) {
            let tabs = app.workspace.tabs_of(id);
            let target = tabs
                .iter()
                .find(|(pane, view)| {
                    *pane == app.workspace.active_pane() && app.workspace.active() == Some(*view)
                })
                .or_else(|| {
                    tabs.iter()
                        .find(|(pane, _)| *pane == app.workspace.active_pane())
                })
                .or_else(|| tabs.first())
                .map(|(_, view)| *view);
            if let Some(view) = target.filter(|view| eligible(app, *view)) {
                opened = run(app, view, Command::Toggle);
                return;
            }
        }
        opened = crate::app::explorer::open_tree_file(app, path.clone(), 2, loader.as_ref());
        if !opened {
            return;
        }
        if let Some(pending) = app
            .editor
            .pending_file
            .as_mut()
            .filter(|pending| pending.path == path)
        {
            pending.toggle_svg = true;
        } else if let Some(view) = app.workspace.active().filter(|view| eligible(app, *view)) {
            run(app, view, Command::Toggle);
        }
    });
    opened
}
fn constrain(app: &mut AppState, id: ViewId) {
    let Some(frame) = frame(app, id) else {
        return;
    };
    let Some(image) = frame.image else {
        return;
    };
    let scale = frame
        .position
        .scale_for(frame.bounds.width(), frame.bounds.height(), &image);
    let max_x = ((image.width as f32 * scale - frame.bounds.width()) / 2.0 + 24.0).max(0.0);
    let max_y = ((image.height as f32 * scale - frame.bounds.height()) / 2.0 + 24.0).max(0.0);
    if let Some(view) = app.workspace.views.get_mut(&id) {
        view.image.x = view.image.x.clamp(-max_x, max_x);
        view.image.y = view.image.y.clamp(-max_y, max_y);
    }
}
pub fn drag(app: &mut AppState, x: f32, y: f32) -> bool {
    let Some(drag) = app.editor.images.drag else {
        return false;
    };
    let Some(view) = app
        .workspace
        .views
        .get_mut(&drag.view)
        .filter(|v| v.presentation == Presentation::Image)
    else {
        app.editor.images.drag = None;
        return false;
    };
    let next = (
        drag.offset.0 + x - drag.origin.0,
        drag.offset.1 + y - drag.origin.1,
    );
    let changed = view.image.x != next.0 || view.image.y != next.1;
    view.image.x = next.0;
    view.image.y = next.1;
    constrain(app, drag.view);
    changed
}
pub fn run(app: &mut AppState, view: ViewId, command: Command) -> bool {
    if matches!(command, Command::CloseMenu) {
        return app.editor.images.menu.take().is_some();
    }
    if !app.workspace.views.contains_key(&view) {
        return false;
    }
    match command {
        Command::Toggle if eligible(app, view) => {
            if let Some(pane) = app.workspace.pane_of(view) {
                app.workspace.activate(pane, view);
            }
            let presentation =
                if app.workspace.view(view).unwrap().presentation == Presentation::Image {
                    Presentation::Source
                } else {
                    Presentation::Image
                };
            reset_input(app);
            return app.workspace.set_presentation(view, presentation);
        }
        _ => {}
    }
    if app
        .workspace
        .view(view)
        .is_none_or(|v| v.presentation != Presentation::Image)
    {
        return false;
    }
    match command {
        Command::Begin(x, y) => {
            reset_input(app);
            if let Some(pane) = app.workspace.pane_of(view) {
                app.workspace.activate(pane, view);
            }
            let position = app.workspace.view(view).unwrap().image;
            app.editor.images.pointer.insert(view, (x, y));
            app.editor.images.drag = Some(Drag {
                view,
                origin: (x, y),
                offset: (position.x, position.y),
            });
            app.editor.focused = true;
            app.terminal.ui.terminal_focused = false;
            true
        }
        Command::Pointer(x, y) => {
            app.editor.images.pointer.insert(view, (x, y));
            false
        }
        Command::End => app.editor.images.drag.take().is_some(),
        Command::Focus => {
            let changed = !app.editor.focused || app.terminal.ui.terminal_focused;
            app.editor.focused = true;
            app.terminal.ui.terminal_focused = false;
            changed
        }
        Command::Blur => {
            let changed = app.editor.focused;
            app.editor.focused = false;
            changed
        }
        Command::Zoom(scale) => {
            app.workspace
                .views
                .get_mut(&view)
                .unwrap()
                .image
                .zoom(scale);
            app.editor.images.menu = None;
            true
        }
        Command::Background(background) => {
            app.workspace.views.get_mut(&view).unwrap().image.background = background;
            app.editor.images.menu = None;
            true
        }
        Command::Wheel(dx, dy) => {
            if app.editor.modifiers.ctrl() || app.editor.modifiers.meta() {
                let Some(frame) = frame(app, view) else {
                    return false;
                };
                let Some(image) = frame.image else {
                    return false;
                };
                let previous =
                    frame
                        .position
                        .scale_for(frame.bounds.width(), frame.bounds.height(), &image);
                let next = (previous * (dy / 300.0).exp()).clamp(0.05, 16.0);
                let (x, y) = app.editor.images.pointer.get(&view).copied().unwrap_or((
                    (frame.bounds.left + frame.bounds.right) / 2.0,
                    (frame.bounds.top + frame.bounds.bottom) / 2.0,
                ));
                let (x, y) = (
                    x - (frame.bounds.left + frame.bounds.right) / 2.0,
                    y - (frame.bounds.top + frame.bounds.bottom) / 2.0,
                );
                let position = &mut app.workspace.views.get_mut(&view).unwrap().image;
                position.x = x - (x - position.x) * next / previous;
                position.y = y - (y - position.y) * next / previous;
                position.scale = Some(next);
            } else {
                let shift = app.editor.modifiers.shift();
                let position = &mut app.workspace.views.get_mut(&view).unwrap().image;
                position.x += dx + if shift { dy } else { 0.0 };
                if !shift {
                    position.y += dy;
                }
            }
            constrain(app, view);
            true
        }
        Command::Menu(kind) => {
            let position = app.workspace.view(view).unwrap().image;
            let selected = ui::choices(kind)
                .iter()
                .position(|(_, command)| match command {
                    Command::Zoom(scale) => *scale == position.scale,
                    Command::Background(background) => *background == position.background,
                    _ => false,
                })
                .unwrap_or(0);
            let same = app
                .editor
                .images
                .menu
                .is_some_and(|m| m.view == view && m.kind == kind);
            app.editor.images.menu = (!same).then_some(Menu {
                view,
                kind,
                selected,
                hovered: None,
            });
            app.chrome.encoding = None;
            true
        }
        Command::Hover(hovered) => {
            let Some(menu) = app.editor.images.menu.as_mut().filter(|m| m.view == view) else {
                return false;
            };
            if menu.hovered == hovered {
                return false;
            }
            menu.hovered = hovered;
            if let Some(index) = hovered {
                menu.selected = index;
            }
            true
        }
        Command::Choose(index) => {
            let Some(menu) = app.editor.images.menu.filter(|m| m.view == view) else {
                return false;
            };
            let Some((_, command)) = ui::choices(menu.kind).get(index).copied() else {
                return false;
            };
            run(app, view, command)
        }
        _ => false,
    }
}
pub fn key(state: &AppContext, event: &KeyboardEvent) -> bool {
    let app = state.get();
    let facts = crate::app::keymap_context::facts(&app);
    if facts.close || facts.clone || facts.stash || facts.confirm || facts.branch {
        return false;
    }
    if let Some(menu) = app.editor.images.menu {
        let LogicalKey::Named(key) = &event.key else {
            return true;
        };
        let command = match key {
            NamedKey::Escape => Command::CloseMenu,
            NamedKey::Enter => Command::Choose(menu.selected),
            NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::Home | NamedKey::End => {
                let len = ui::choices(menu.kind).len();
                let next = match key {
                    NamedKey::Home => 0,
                    NamedKey::End => len - 1,
                    NamedKey::ArrowUp => menu.selected.saturating_sub(1),
                    _ => (menu.selected + 1).min(len - 1),
                };
                Command::Hover(Some(next))
            }
            _ => return true,
        };
        state.try_update(|app| run(app, menu.view, command));
        return true;
    }
    if app.main_surface() != crate::app::state::MainSurface::Image
        || !app.editor.focused
        || app.terminal.ui.terminal_focused
        || event.is_composing
        || event.modifiers.ctrl()
        || event.modifiers.meta()
        || event.modifiers.alt()
    {
        return false;
    }
    let Some(view) = app.workspace.active().filter(|id| {
        app.workspace
            .view(*id)
            .is_some_and(|v| v.presentation == Presentation::Image)
    }) else {
        return false;
    };
    let command = match &event.key {
        LogicalKey::Character(ch) if ch.eq_ignore_ascii_case("f") => Command::Zoom(None),
        LogicalKey::Character(ch) if ch == "1" => Command::Zoom(Some(1.0)),
        LogicalKey::Character(ch) if matches!(ch.as_str(), "+" | "=" | "-") => {
            let Some(frame) = frame(&app, view) else {
                return false;
            };
            let Some(image) = frame.image else {
                return false;
            };
            Command::Zoom(Some(
                frame
                    .position
                    .scale_for(frame.bounds.width(), frame.bounds.height(), &image)
                    * if ch == "-" { 0.8 } else { 1.25 },
            ))
        }
        _ => return false,
    };
    state.try_update(|app| run(app, view, command));
    true
}
