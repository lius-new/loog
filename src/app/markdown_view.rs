//! File reading host. Documents, commands and I/O stay outside the reusable UI.
use crate::app::{
    context::AppContext,
    markdown_render::{self, Rendered, Status},
    state::AppState,
};
use crate::domain::{
    identity::{PaneId, ViewId},
    pane_layout::Direction,
    view::{Presentation, ReaderPosition},
};
use crate::features::markdown::model::*;
use crate::infrastructure::markdown_worker::ParseStatus;
use crate::ui::{
    markdown::{
        native::Ink,
        reader::{self, Binding, ReaderEvent, ReaderFrame},
    },
    theme,
};
use lgui::core::{
    CursorIcon, EventPolicy, ImageFit, PointerButton, UiElement, UiFocusHandle, UiId, UiImageSource,
};
use lgui::prelude::{Element, UiRect, VisualStyle, group, panel, text};
use std::sync::Arc;

pub fn eligible(app: &AppState, view: ViewId) -> bool {
    app.workspace
        .view_meta(view)
        .is_some_and(|meta| crate::features::markdown::is_markdown_path(&meta.path))
        && app.workspace.view_document(view).is_some()
}
fn reset_input(app: &mut AppState) {
    for state in app.editor.markdown_views.values_mut() {
        crate::features::markdown::reader::cancel_gesture(state);
    }
    app.editor.ime = None;
    app.editor.drag = None;
    app.editor.menu = None;
    app.editor.language.close_popups();
}
pub fn toggle(app: &mut AppState) -> bool {
    let Some(id) = app.workspace.active() else {
        return false;
    };
    if !eligible(app, id) {
        return false;
    }
    let value = if app
        .workspace
        .view(id)
        .is_some_and(|view| view.presentation == Presentation::Markdown)
    {
        Presentation::Source
    } else {
        Presentation::Markdown
    };
    reset_input(app);
    app.workspace.set_presentation(id, value)
}
pub fn side(app: &mut AppState) -> bool {
    let Some(current) = app.workspace.active() else {
        return false;
    };
    if !eligible(app, current) {
        return false;
    }
    if app
        .workspace
        .view(current)
        .is_some_and(|view| view.presentation == Presentation::Markdown)
    {
        if app
            .workspace
            .view(current)
            .and_then(|view| view.associated_source)
            .and_then(|id| app.workspace.view(id))
            .is_some_and(|view| view.accepts_text())
        {
            return true;
        }
        source(app, None);
    }
    let Some(source) = app.workspace.active() else {
        return false;
    };
    if let Some(existing) = app
        .workspace
        .views
        .values()
        .find(|view| {
            view.associated_source == Some(source) && view.presentation == Presentation::Markdown
        })
        .map(|view| view.id)
    {
        if let Some(pane) = app.workspace.pane_of(existing) {
            app.workspace.activate(pane, existing);
            reset_input(app);
            return true;
        }
    }
    let pane = app.workspace.active_pane();
    let Some(new) = app.workspace.split_empty(pane, Direction::Right) else {
        return false;
    };
    if !app.workspace.copy_item(pane, source, new, 0) {
        return false;
    }
    let id = app.workspace.active().unwrap();
    app.workspace.set_presentation(id, Presentation::Markdown);
    app.workspace.views.get_mut(&id).unwrap().associated_source = Some(source);
    reset_input(app);
    true
}
pub fn source(app: &mut AppState, at: Option<usize>) -> bool {
    let Some(current) = app.workspace.active() else {
        return false;
    };
    let Some(document) = app.workspace.view_document(current) else {
        return false;
    };
    let related = app
        .workspace
        .view(current)
        .and_then(|view| view.associated_source)
        .filter(|id| {
            app.workspace
                .view(*id)
                .is_some_and(|view| view.document() == Some(document) && view.accepts_text())
        });
    let id = related
        .or_else(|| {
            app.workspace
                .views
                .values()
                .find(|view| view.document() == Some(document) && view.accepts_text())
                .map(|view| view.id)
        })
        .unwrap_or(current);
    app.workspace.set_presentation(id, Presentation::Source);
    if let Some(pane) = app.workspace.pane_of(id) {
        app.workspace.activate(pane, id);
        if let Some(at) = at {
            if let Some(mut editor) = app.workspace.editor_mut(pane) {
                editor.set_cursor(at);
            }
        }
    }
    reset_input(app);
    true
}
fn button(rect: UiRect, label: &'static str, action: impl Fn() + Send + Sync + 'static) -> Element {
    panel(rect, VisualStyle::default())
        .event_policy(EventPolicy::INTERACTIVE)
        .cursor(CursorIcon::Pointer)
        .child(text(rect, label, theme::sans(theme::c().accent, 12.0)))
        .on_click(move |cx: &mut lgui::core::UiEventContext| {
            action();
            cx.stop_propagation();
        })
}
pub fn render(
    pane: PaneId,
    rect: UiRect,
    state: AppContext,
    id: UiId,
    focus: UiFocusHandle,
    clipboard: lgui::services::ClipboardHandle,
) -> Element {
    let app = state.get();
    let Some(view) = app
        .workspace
        .pane(pane)
        .and_then(|pane| pane.active())
        .and_then(|id| app.workspace.view(id))
        .cloned()
    else {
        return group(rect);
    };
    let mut body = rect;
    let body_area = body;
    let app_hovered = app.editor.editor_hovered == Some(pane);
    let margin =
        ((body.width() - app.preferences.values().markdown.max_width as f32) * 0.5).max(0.0);
    body.left += margin;
    body.right -= margin;
    // The centered reading column is narrower than the pane; fill the margins.
    let mut root = group(rect).child(panel(rect, VisualStyle::filled(theme::c().bg)));
    let Some(service) = state.services().and_then(|runtime| runtime.markdown()) else {
        return root;
    };
    let status = match service.query(&app, view.id) {
        Some(ParseStatus::Ready(publication)) => {
            if let Err(failure) = &publication.outcome {
                Status::Failed(format!(
                    "Markdown preview unavailable: {:?}. The complete source remains available.",
                    failure.reason
                ))
            } else {
                let ink = Ink::current();
                service.render.request(markdown_render::Request {
                    view: crate::infrastructure::markdown_worker::Consumer::Document(view.id),
                    key: markdown_render::RequestKey {
                        parse: publication.revision,
                        width: body.width().max(1.0),
                        height: body.height().max(1.0),
                        position: view.reader,
                        ink,
                        density: crate::ui::device_scale::observed(),
                        font_size: app.preferences.values().markdown.font_size as f32,
                        line_height: (app.preferences.values().markdown.line_height as f32)
                            .max(app.preferences.values().markdown.font_size as f32 * 1.2),
                        fragment: app
                            .editor
                            .markdown_views
                            .get(&view.id)
                            .and_then(|value| value.fragment.clone()),
                        horizontal: {
                            let mut entries = app
                                .editor
                                .markdown_views
                                .get(&view.id)
                                .filter(|value| value.revision == publication.revision)
                                .map(|value| {
                                    value
                                        .horizontal
                                        .iter()
                                        .map(|(id, value)| (*id, *value))
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default();
                            entries.sort_by_key(|(id, _)| id.0);
                            entries
                        },
                        base: app
                            .workspace
                            .view_meta(view.id)
                            .map(|meta| meta.path.clone()),
                        remote: true,
                        page: true,
                    },
                    publication,
                })
            }
        }
        Some(ParseStatus::Declined(failure)) => Status::Failed(format!(
            "Markdown preview limit: {:?}. Toggle the preview to read the complete source.",
            failure.reason
        )),
        Some(ParseStatus::Stopped) => Status::Failed("Markdown service stopped".into()),
        _ => Status::Pending,
    };
    match status {
        Status::Ready(result) => {
            let read_state = state.clone();
            let read_result = result.clone();
            let emit_state = state.clone();
            let emit_result = result.clone();
            let paint_result = result.clone();
            let image_result = result.clone();
            let binding = Binding::new(
                move || frame_for(&read_state.get(), view.id, &read_result),
                move |action| {
                    event(
                        &emit_state,
                        pane,
                        view.id,
                        view.input_generation,
                        &emit_result,
                        action,
                        clipboard.clone(),
                    )
                },
                move |_, paragraph, bounds, _| paint(&paint_result, paragraph, bounds),
            )
            .with_images(move |frame, image, bounds| {
                paint_image(&image_result, frame, image, bounds)
            });
            root = root.child(reader::render_in(
                id,
                UiRect::new(body_area.left, body.top, body_area.right, body.bottom),
                body,
                app_hovered,
                VisualStyle::filled(theme::c().bg),
                binding,
                focus,
            ));
        }
        Status::Pending => {
            root = root.child(status_surface(
                id,
                body,
                "Preparing Markdown preview…".into(),
                focus,
                state.clone(),
                pane,
            ));
        }
        Status::Failed(message) => {
            root = root.child(status_surface(
                id,
                body,
                message,
                focus,
                state.clone(),
                pane,
            ));
        }
    }
    root
}
pub(super) fn paint(result: &Rendered, paragraph: usize, bounds: UiRect) -> Element {
    let mut root = group(bounds);
    for tile in result
        .tiles
        .iter()
        .filter(|tile| tile.paragraph == paragraph)
    {
        let rect = UiRect::new(
            bounds.left + tile.left,
            bounds.top + tile.top,
            bounds.left + tile.left + tile.width,
            bounds.top + tile.top + tile.height,
        );
        let source = UiImageSource::bytes(
            format!("markdown-png-{:016x}-{}", tile.hash, tile.png.len()),
            0,
            tile.png.clone(),
        );
        let request = crate::ui::image_rendering::pixel_exact(source)
            .cache_policy(lgui::memory::ImageCachePolicy::WhileVisible)
            .namespace("markdown-reader");
        let id = UiId::owned(format!(
            "markdown-tile-{}-{paragraph}-{}",
            result.frame.layout,
            tile.top.to_bits()
        ));
        let density = tile.density;
        let pixel_width = (tile.width * density).ceil() as i32;
        let pixel_height = (tile.height * density).ceil() as i32;
        root = root.child(Element::new(move |cx| {
            // Skia samples the tile nearest: land every texel on one physical pixel.
            let scale = cx.context.scale();
            crate::ui::device_scale::observe(scale.factor());
            let rect = if (scale.factor() - density).abs() < 0.01 {
                let left = scale.physical_value(rect.left);
                let top = scale.physical_value(rect.top);
                scale.logical_rect(lgui::core::PhysicalRect::new(
                    left,
                    top,
                    left + pixel_width,
                    top + pixel_height,
                ))
            } else {
                rect
            };
            UiElement::requested_image(id, rect, request, ImageFit::Fill)
        }));
    }
    root
}
pub(super) fn event(
    state: &AppContext,
    pane: PaneId,
    view: ViewId,
    generation: u64,
    result: &Arc<Rendered>,
    action: ReaderEvent,
    clipboard: lgui::services::ClipboardHandle,
) {
    let Some(service) = state.services().and_then(|runtime| runtime.markdown()) else {
        return;
    };
    let mut effects = crate::features::markdown::reader::Effects::default();
    let activate = matches!(
        action,
        ReaderEvent::PointerDown { .. } | ReaderEvent::ScrollbarDown { .. }
    );
    let copied_node = match action {
        ReaderEvent::CopyNode { node, .. } => Some(node),
        _ => None,
    };
    state.update(|app| {
        if !app.workspace.view(view).is_some_and(|value| {
            value.presentation == Presentation::Markdown && value.input_generation == generation
        }) || app.workspace.pane(pane).and_then(|pane| pane.active()) != Some(view)
        {
            return;
        }
        if let ReaderEvent::Focus(focused) = action {
            app.editor.focused = focused;
            app.editor.ime = None;
            if focused {
                app.workspace.activate_pane(pane);
            } else {
                crate::features::markdown::reader::cancel_gesture(
                    app.editor.markdown_views.entry(view).or_default(),
                );
            }
            return;
        }
        if activate {
            app.editor.focused = true;
            app.editor.ime = None;
            app.workspace.activate(pane, view);
        }
        if service.query(app, view).and_then(|value| match value {
            ParseStatus::Ready(publication) => Some(publication.revision),
            _ => None,
        }) != Some(result.frame.parse)
        {
            if action.ends_gesture() {
                crate::features::markdown::reader::cancel_gesture(
                    app.editor.markdown_views.entry(view).or_default(),
                );
                if matches!(action, ReaderEvent::Focus(false)) {
                    app.editor.focused = false;
                }
            }
            return;
        }
        let mut frame = frame_for(app, view, result);
        let interaction = app.editor.markdown_views.entry(view).or_default();
        if interaction.fragment.take().is_some() {
            frame.viewport = result.viewport;
        }
        let (viewport, output) =
            crate::features::markdown::reader::reduce(interaction, &frame, action);
        effects = output;
        if let Some(focused) = effects.focus {
            app.editor.focused = focused;
            app.editor.ime = None;
            if focused {
                app.workspace.activate_pane(pane);
            }
        }
        if activate {
            app.workspace.activate(pane, view);
        }
        let anchor = viewport.anchor(&result.layout);
        app.workspace.set_reader_position(
            view,
            ReaderPosition {
                source_byte: anchor.source.0,
                block_fraction: anchor.fraction,
            },
        );
    });
    if let Some(copy) = effects.copy {
        if let Err(error) = clipboard.write_text(&copy) {
            state.update(|app| app.show_error(format!("Clipboard: {error}")));
        } else if let Some(node) = copied_node {
            reader::mark_copied(node);
            // Repaint once the "Copied" label has run out.
            let refresh = state.clone();
            std::thread::spawn(move || {
                std::thread::sleep(reader::COPIED_FOR + std::time::Duration::from_millis(50));
                refresh.try_update(|_| true);
            });
        }
    }
    if let Some(node) = effects.link {
        open_link(state, pane, view, result, node);
    }
    service.schedule_drag(crate::infrastructure::markdown_worker::Consumer::Document(
        view,
    ));
}
pub(super) fn frame_for(app: &AppState, view: ViewId, result: &Rendered) -> ReaderFrame {
    let interaction = app
        .editor
        .markdown_views
        .get(&view)
        .cloned()
        .unwrap_or_default();
    let mut viewport = result.viewport;
    if interaction.fragment.is_none() {
        if let Some(view) = app.workspace.view(view) {
            viewport.restore(
                &result.layout,
                crate::features::markdown::reader::ReadingAnchor {
                    source: SourceByte(view.reader.source_byte),
                    fraction: view.reader.block_fraction,
                },
            );
        }
    }
    ReaderFrame {
        scrollbar_dragging: interaction.scrollbar.is_some(),
        key: result.frame,
        document: result.layout.document.clone(),
        layout: result.layout.clone(),
        viewport,
        selection: interaction.selection,
        geometry: markdown_render::geometry_for(result, viewport, &interaction.horizontal),
        geometry_key: result.frame,
        horizontal: if interaction.revision == result.frame.parse {
            interaction.horizontal
        } else {
            Default::default()
        },
        focused: app.editor.focused,
        extend_selection: app.editor.modifiers.shift(),
    }
}
fn open_link(state: &AppContext, pane: PaneId, view: ViewId, result: &Rendered, node: NodeId) {
    let Some(NodeKind::Link { destination, .. }) =
        result.layout.document.node(node).map(|node| &node.kind)
    else {
        return;
    };
    let base = state
        .get()
        .workspace
        .view_meta(view)
        .map(|meta| meta.path.clone());
    use crate::features::markdown::resources::{Address, resolve};
    match resolve(base.as_deref(), destination) {
        Some(Address::Web(url)) => crate::app::language_ui::open_link(state, url),
        Some(Address::Anchor(fragment)) => {
            if let Some((_, node)) =
                crate::features::markdown::resources::headings(&result.layout.document)
                    .iter()
                    .find(|(slug, _)| *slug == fragment)
            {
                let at = result.layout.document.nodes[node.0].source.start.0;
                state.update(|app| {
                    app.editor.markdown_views.entry(view).or_default().fragment = None;
                    app.workspace.set_reader_position(
                        view,
                        ReaderPosition {
                            source_byte: at,
                            block_fraction: 0.0,
                        },
                    );
                });
            } else {
                state.update(|app| app.show_error(format!("Heading not found: {fragment}")));
            }
        }
        // As with Ctrl-click in the editor, a folder is shown in the Explorer.
        Some(Address::Local { path, .. }) if path.is_dir() => {
            crate::app::explorer::reveal_folder(state, &path);
        }
        Some(Address::Local { path, fragment }) => open_local(state, pane, path, fragment),
        None => state.update(|app| app.show_error("Unsupported Markdown link")),
    }
}
/// Opens `path` in `pane` as a Markdown preview, reading it first when needed.
pub(crate) fn open_local(
    state: &AppContext,
    pane: PaneId,
    path: std::path::PathBuf,
    fragment: Option<String>,
) {
    let loader = state.services().and_then(|runtime| runtime.file_loader());
    state.update(|app| {
        app.workspace.activate_pane(pane);
        if let Some(document) = app
            .workspace
            .file_id_for_path(&path)
            .filter(|id| app.workspace.open_error(*id).is_none())
        {
            app.workspace.activate_document(document);
            if let Some(view) = app.workspace.active() {
                finish_navigation(app, view, fragment);
            }
        } else if crate::app::file_loading::request(
            app,
            path,
            crate::app::workspace::OpenMode::Permanent,
            None,
            loader.as_ref(),
        ) && let Some(pending) = app.editor.pending_file.as_mut()
        {
            pending.markdown = true;
            pending.markdown_fragment = fragment;
        }
    });
}
pub fn finish_navigation(app: &mut AppState, view: ViewId, fragment: Option<String>) {
    if eligible(app, view) {
        app.workspace.set_presentation(view, Presentation::Markdown);
        app.editor.markdown_views.entry(view).or_default().fragment = fragment;
    }
}
/// Keeps every associated source/preview pair at the same place. Whichever
/// side moved since the last pass drives the other, so neither needs focus and
/// the two cannot feed back into each other.
pub fn sync_sources(app: &mut AppState) {
    // A preview pairs with the visible editor of the same document in another
    // pane: its recorded source first, otherwise any such editor.
    let mut panes = app
        .workspace
        .panes
        .iter()
        .filter_map(|(id, pane)| pane.active.map(|view| (id.get(), view)))
        .collect::<Vec<_>>();
    panes.sort_by_key(|(id, _)| *id);
    let active = panes.into_iter().map(|(_, view)| view).collect::<Vec<_>>();
    let mut pairs = Vec::new();
    for &id in &active {
        let Some(view) = app.workspace.view(id).cloned() else {
            continue;
        };
        if view.presentation != Presentation::Markdown {
            continue;
        }
        let Some(document) = view.document() else {
            continue;
        };
        let editors = active.iter().copied().filter(|other| {
            app.workspace
                .view(*other)
                .is_some_and(|other| other.accepts_text() && other.document() == Some(document))
        });
        let editors = editors.collect::<Vec<_>>();
        let source = view
            .associated_source
            .filter(|source| editors.contains(source))
            .or_else(|| editors.first().copied());
        if let Some(source) = source {
            pairs.push((id, source));
        }
    }
    let height = crate::ui::fonts::editor().line_h.max(1.0);
    for (preview, source) in pairs {
        let Some(source_view) = app
            .workspace
            .view(source)
            .cloned()
            .filter(|view| view.accepts_text())
        else {
            continue;
        };
        let Some(document) = source_view
            .document()
            .and_then(|id| app.workspace.documents.get(&id))
        else {
            continue;
        };
        let Some(reader) = app.workspace.view(preview).map(|view| view.reader) else {
            continue;
        };
        let scroll = source_view.source.scroll_y;
        let last = app
            .editor
            .markdown_views
            .get(&preview)
            .and_then(|state| state.synced);
        let reader_moved = last.is_some_and(|(_, byte)| byte != reader.source_byte);
        let source_moved = last.is_none_or(|(y, _)| y != scroll);
        if reader_moved {
            let line = document.buffer.line_of(reader.source_byte);
            let target = line as f32 * height;
            if let Some(view) = app.workspace.views.get_mut(&source) {
                view.source.scroll_y = target;
            }
        } else if source_moved {
            let line = (scroll / height).max(0.0) as usize;
            let byte = document
                .buffer
                .line_start(line.min(document.buffer.line_count().saturating_sub(1)));
            app.workspace.set_reader_position(
                preview,
                ReaderPosition {
                    source_byte: byte,
                    block_fraction: 0.0,
                },
            );
        }
        let scroll = app
            .workspace
            .view(source)
            .map_or(scroll, |view| view.source.scroll_y);
        let byte = app
            .workspace
            .view(preview)
            .map_or(reader.source_byte, |view| view.reader.source_byte);
        app.editor.markdown_views.entry(preview).or_default().synced = Some((scroll, byte));
    }
}
pub fn key_action(
    state: &AppContext,
    action: crate::features::keymap::action::Action,
    clipboard: lgui::services::ClipboardHandle,
) -> bool {
    use crate::features::keymap::action::Action;
    let app = state.get();
    if app.main_surface() != crate::app::state::MainSurface::Markdown || !app.editor.focused {
        return false;
    }
    let Some(view) = app
        .workspace
        .active()
        .and_then(|id| app.workspace.view(id))
        .cloned()
    else {
        return false;
    };
    let Some(result) = state
        .services()
        .and_then(|runtime| runtime.markdown())
        .and_then(|service| {
            service
                .render
                .latest(crate::infrastructure::markdown_worker::Consumer::Document(
                    view.id,
                ))
        })
    else {
        return false;
    };
    let command = match action {
        Action::Copy => ReaderEvent::Copy { key: result.frame },
        Action::SelectAll => ReaderEvent::SelectAll { key: result.frame },
        Action::Cancel => ReaderEvent::ClearSelection { key: result.frame },
        _ => return action.editor_command(1).is_some(),
    };
    let pane = app.workspace.active_pane();
    drop(app);
    event(
        state,
        pane,
        view.id,
        view.input_generation,
        &result,
        command,
        clipboard,
    );
    true
}

pub(super) fn paint_image(
    result: &Rendered,
    frame: &ReaderFrame,
    image: &crate::features::markdown::layout::ImageBox,
    bounds: UiRect,
) -> Element {
    let alt = frame
        .document
        .text(image.display.clone())
        .unwrap_or("Image");
    let accessible_alt = alt.to_owned();
    let resource_id = UiId::owned(format!(
        "markdown-resource-{}-{}",
        result.frame.layout, image.node.0
    ));
    match result.images.get(&image.node).map(|asset| &asset.outcome) {
        Some(Ok(image)) => {
            let source = UiImageSource::bytes(
                format!("md-image-{}", Arc::as_ptr(&image.encoded) as usize),
                0,
                image.encoded.clone(),
            );
            let request = crate::ui::image_rendering::image(source)
                .cache_policy(lgui::memory::ImageCachePolicy::WhileVisible)
                .namespace("markdown-resources");
            Element::new(move |_| {
                UiElement::requested_image(resource_id, bounds, request, ImageFit::Contain)
                    .semantics(
                        lgui::core::Semantics::new(lgui::core::SemanticRole::Image)
                            .name(accessible_alt),
                    )
            })
        }
        // Still loading: it has no room yet, so there is nothing to draw.
        None => panel(bounds, VisualStyle::default()),
        Some(Err(message)) => panel(bounds, VisualStyle::filled(theme::c().surface)).child(text(
            bounds,
            format!("{alt}\n{message}"),
            theme::sans(theme::c().text_soft, 12.0),
        )),
    }
}

fn status_surface(
    id: UiId,
    rect: UiRect,
    message: String,
    focus: UiFocusHandle,
    state: AppContext,
    pane: PaneId,
) -> Element {
    let description = message.clone();
    let mut root = Element::new(move |cx| {
        UiElement::panel(id, rect, VisualStyle::filled(theme::c().bg))
            .semantics(
                lgui::core::Semantics::new(lgui::core::SemanticRole::Group)
                    .name("Markdown preview")
                    .description(description),
            )
            .children(cx.children)
    })
    .event_policy(EventPolicy::INTERACTIVE)
    .child({
        // Centered both ways, whether it is a short wait note or a long reason.
        let mut style = theme::sans(theme::c().text_soft, 14.0);
        style.align = lgui::prelude::TextAlign::Center;
        style.vertical_align = lgui::text::TextVerticalAlign::Center;
        text(
            UiRect::new(
                rect.left + 24.0,
                rect.top,
                (rect.right - 24.0).max(rect.left + 24.0),
                rect.bottom,
            ),
            message,
            style,
        )
    });
    for kind in [
        lgui::core::UiEventKind::Input,
        lgui::core::UiEventKind::CompositionStart,
        lgui::core::UiEventKind::CompositionUpdate,
        lgui::core::UiEventKind::CompositionEnd,
    ] {
        root = root.on_event_capture(kind, |cx, _| {
            cx.prevent_default();
            cx.stop_propagation();
        });
    }
    root.on_pointer_down_with_button(move |cx, _, button| {
        if button == PointerButton::Left {
            focus.focus();
            state.update(|app| {
                app.workspace.activate_pane(pane);
                app.editor.focused = true;
                app.editor.ime = None;
            });
            cx.stop_propagation();
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn linked_preview_reuses_document_and_repeated_command_reuses_view() {
        let mut app = AppState::new();
        let doc = app
            .workspace
            .open_path("README.md".into(), "# unsaved".into());
        let source_view = app.workspace.active().unwrap();
        let source_pane = app.workspace.active_pane();
        assert!(side(&mut app));
        let preview = app.workspace.active().unwrap();
        assert_ne!(preview, source_view);
        assert_eq!(app.workspace.view_document(preview), Some(doc));
        assert_eq!(app.workspace.documents.len(), 1);
        assert_eq!(
            app.pane_surface(app.workspace.active_pane()),
            crate::app::state::MainSurface::Markdown
        );
        app.workspace.activate(source_pane, source_view);
        assert!(side(&mut app));
        assert_eq!(app.workspace.active(), Some(preview));
        assert_eq!(app.workspace.views.len(), 2);
        assert!(source(&mut app, Some(2)));
        assert_eq!(app.workspace.active(), Some(source_view));
        assert_eq!(app.workspace.active_editor().unwrap().cursor(), 2);
    }
    #[test]
    fn toggling_is_read_only_and_keeps_view_identity() {
        let mut app = AppState::new();
        let doc = app.workspace.open_path("文档.MD".into(), "正文".into());
        let view = app.workspace.active().unwrap();
        assert!(toggle(&mut app));
        assert!(app.workspace.active_editor_mut().is_none());
        assert_eq!(app.workspace.active_document(), Some(doc));
        assert_eq!(app.workspace.active(), Some(view));
        assert!(toggle(&mut app));
        assert!(app.workspace.active_editor_mut().is_some());
        assert_eq!(app.workspace.active(), Some(view));
    }
}
