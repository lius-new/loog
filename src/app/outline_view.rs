//! Binds the Outline drawer page to application state.
use super::context::AppContext;
use super::language_ui::navigate::{self, Place};
use super::language_ui::outline::Status;
use crate::ui::outline::view::{self, Body, Command, Commands, Frame};
use lgui::prelude::{Element, UiRect};

pub(crate) fn render(rect: UiRect, state: &AppContext, code_rect: UiRect) -> Element {
    let app = state.get();
    let ui = &app.editor.language.outline;
    let header = super::explorer_view::drawer_header(
        UiRect::new(
            rect.left,
            rect.top,
            rect.right,
            rect.top + crate::ui::theme::TABS_H,
        ),
        state,
    );
    let rows = ui.outline.rows(&ui.collapsed, ui.sorted);
    let markdown = ui
        .resource
        .as_ref()
        .and_then(|resource| {
            app.workspace
                .documents
                .values()
                .find(|doc| &doc.meta.resource == resource)
        })
        .is_some_and(|doc| doc.meta.lang.definition().name.as_ref() == "markdown");
    let message = match &ui.status {
        Status::NoDocument => Some("Open a text file to see its structure."),
        Status::Loading if ui.outline.is_empty() => Some("Reading structure…"),
        Status::Unavailable(reason) if ui.outline.is_empty() => Some(reason.as_str()),
        _ if rows.is_empty() && markdown => Some("No headings in this file."),
        _ if rows.is_empty() => Some("No symbols found in this file."),
        _ => None,
    };
    let frame = Frame {
        body: match message {
            Some(message) => Body::Message(message),
            None => Body::Tree {
                outline: &ui.outline,
                rows: &rows,
                current: ui.current.as_deref(),
                reveal: ui.reveal.as_deref(),
            },
        },
        sorted: ui.sorted,
        scroll: ui.scroll,
        hovered: ui.hovered,
        pointer_inside: ui.pointer_inside,
        scrollbar_grab: ui.scrollbar_dragging.then_some(ui.scrollbar_drag_offset),
        resizable: app.layout.application_layout.is_none(),
        resizing: app.drawer.resizing,
    };
    let run_state = state.clone();
    view::render(
        rect,
        &frame,
        Commands::new(move |command| run(&run_state, command, code_rect)),
        header,
    )
}

fn run(state: &AppContext, command: Command, code_rect: UiRect) {
    match command {
        Command::Go(node) => {
            let app = state.get();
            let ui = &app.editor.language.outline;
            // COMPAT(native-outline-navigation): Keep the old native navigator
            // local until it accepts ResourceRef; no removal version is scheduled.
            if let (Some(path), Some(node)) = (
                ui.resource.as_ref().and_then(|r| r.local_path()),
                ui.outline.node(node),
            ) {
                let at = node.select.start;
                navigate::go(
                    state,
                    Place::bytes(path.to_path_buf(), at..at),
                    code_rect,
                    true,
                );
            }
        }
        Command::Toggle(key) => state.update(|app| app.editor.language.outline.toggle(&key)),
        Command::ExpandAll => state.update(|app| app.editor.language.outline.expand_all()),
        Command::CollapseAll => state.update(|app| app.editor.language.outline.collapse_all()),
        Command::ToggleSort => state.update(|app| app.editor.language.outline.toggle_sort()),
        Command::Scroll(value) => {
            state.try_update(|app| {
                let ui = &mut app.editor.language.outline;
                let changed = ui.scroll != value || ui.reveal.is_some();
                ui.scroll = value;
                ui.reveal = None;
                changed
            });
        }
        Command::Hover(row) => {
            state.try_update(|app| {
                let ui = &mut app.editor.language.outline;
                let changed = ui.hovered != row;
                ui.hovered = row;
                changed
            });
        }
        Command::ScrollbarGrab(offset) => state.update(|app| {
            let ui = &mut app.editor.language.outline;
            ui.scrollbar_dragging = true;
            ui.scrollbar_drag_offset = offset;
        }),
        Command::ScrollbarRelease => {
            state.update(|app| app.editor.language.outline.scrollbar_dragging = false)
        }
        Command::ResizeStart => state.update(|app| app.drawer.resizing = true),
        Command::Resize(width) => state.update(move |app| {
            if app.drawer.resizing {
                app.drawer.width = width;
            }
        }),
        Command::ResizeEnd => state.update(|app| app.drawer.resizing = false),
    }
}
