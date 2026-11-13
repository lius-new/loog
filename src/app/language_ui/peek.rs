//! Read-only definition preview without changing the active document or history.
use super::{
    AppContext, AppState, Target,
    navigate::{self, Place},
};
use crate::domain::identity::PaneId;
use std::sync::Arc;

#[derive(Clone)]
pub struct Peek {
    revision: u64,
    open_generation: u64,
    origin: Target,
    place: Place,
    target_stamp: Option<crate::domain::document::DocumentStamp>,
    line: usize,
    lines: Vec<(usize, String, bool)>,
}
pub fn open(ctx: &AppContext, place: Place) {
    let app = ctx.get();
    let Some(origin) = super::target(&app, app.workspace.active_pane()) else {
        return;
    };
    let open_generation = app.explorer.resources.open_generation;
    let cached = app
        .workspace
        .file_id_for_resource(&place.resource)
        .and_then(|id| app.workspace.documents.get(&id))
        .filter(|d| d.is_text())
        .map(|d| d.buffer.text_snapshot());
    let mut revision = 0;
    ctx.update(|app| {
        app.editor.language.peek_revision = app.editor.language.peek_revision.wrapping_add(1);
        revision = app.editor.language.peek_revision;
        app.editor.language.peek = None;
    });
    if let Some(text) = cached {
        ctx.update(|app| install(app, revision, open_generation, origin, place, text));
        return;
    }
    if place.resource.environment().is_some() {
        let guard_origin = origin.clone();
        crate::app::resources::read_text(
            ctx,
            place.resource.clone(),
            16 * 1024 * 1024,
            crate::app::resources::TextConsumer {
                valid: Box::new(move |app| {
                    app.editor.language.peek_revision == revision
                        && valid(app, open_generation, &guard_origin)
                }),
                ready: Box::new(move |app, result| match result {
                    Ok(prepared) => install(
                        app,
                        revision,
                        open_generation,
                        origin,
                        place,
                        prepared.text_snapshot(),
                    ),
                    Err(error) => app.show_error(format!(
                        "Could not preview {}: {error}",
                        place.resource.display()
                    )),
                }),
            },
        );
        return;
    }
    let path = place.resource.local_path().unwrap().to_path_buf();
    let Some(application) = ctx.services().and_then(|r| r.application()) else {
        return;
    };
    let state = ctx.clone();
    let result = std::thread::Builder::new()
        .name("loog-peek-read".into())
        .spawn(move || {
            let text = std::fs::metadata(&path).and_then(|m| {
                if m.len() > 16 * 1024 * 1024 {
                    Err(std::io::Error::other("file is too large"))
                } else {
                    std::fs::read_to_string(&path)
                }
            });
            application.post(move || {
                state.update(|app| match text {
                    Ok(text) => install(
                        app,
                        revision,
                        open_generation,
                        origin,
                        place,
                        Arc::new(text),
                    ),
                    Err(e)
                        if app.editor.language.peek_revision == revision
                            && valid(app, open_generation, &origin) =>
                    {
                        app.show_error(format!(
                            "Could not preview {}: {e}",
                            place.resource.display()
                        ))
                    }
                    _ => {}
                })
            });
        });
    if result.is_err() {
        ctx.update(|app| app.show_error("Could not start reading the preview"));
    }
}
fn valid(app: &AppState, open_generation: u64, origin: &Target) -> bool {
    app.explorer.resources.open_generation == open_generation
        && app.editor.pending_file.is_none()
        && app.workspace.active_pane() == origin.pane
        && super::target(app, origin.pane).is_some_and(|t| {
            t.same_view(origin)
                && t.stamp == origin.stamp
                && t.cursor == origin.cursor
                && t.selection == origin.selection
        })
}
fn install(
    app: &mut AppState,
    revision: u64,
    open_generation: u64,
    origin: Target,
    place: Place,
    text: Arc<String>,
) {
    if app.editor.language.peek_revision != revision || !valid(app, open_generation, &origin) {
        return;
    }
    let range = place.range(&text);
    let target_stamp = app
        .workspace
        .file_id_for_resource(&place.resource)
        .and_then(|id| app.workspace.documents.get(&id).map(|d| (id, d)))
        .map(|(id, d)| d.stamp(id));
    if let Some(stamp) = target_stamp
        && app.workspace.documents[&stamp.document].buffer.text() != text.as_str()
    {
        return;
    }
    let buffer = crate::model::text::TextBuffer::new(text.as_ref().clone());
    let line = buffer.line_of(range.start);
    let start = line.saturating_sub(4);
    let end = (line + 9).min(buffer.line_count());
    let lines = (start..end)
        .map(|n| {
            (
                n + 1,
                buffer
                    .line(n)
                    .trim_end_matches(['\r', '\n'])
                    .chars()
                    .take(1200)
                    .collect(),
                n == line,
            )
        })
        .collect();
    app.editor.language.close_popups();
    app.editor.language.peek = Some(Peek {
        revision,
        open_generation,
        origin,
        place,
        target_stamp,
        line,
        lines,
    });
}
pub fn close(app: &mut AppState) {
    app.editor.language.peek_revision = app.editor.language.peek_revision.wrapping_add(1);
    app.editor.language.peek = None;
}
pub fn observe(app: &mut AppState) {
    if app
        .editor
        .language
        .peek
        .as_ref()
        .is_some_and(|p| !valid_peek(app, p))
    {
        close(app);
    }
}
pub fn view(app: &AppState, pane: PaneId) -> Option<crate::ui::editor::peek::View> {
    let p = app
        .editor
        .language
        .peek
        .as_ref()
        .filter(|p| p.origin.pane == pane && valid_peek(app, p))?;
    Some(crate::ui::editor::peek::View {
        revision: p.revision,
        title: format!("{}:{}", p.place.resource.display(), p.line + 1),
        lines: p.lines.clone(),
    })
}
pub fn jump(ctx: &AppContext, revision: u64, rect: lgui::prelude::UiRect) {
    let mut place = None;
    ctx.update(|app| {
        if let Some(p) = app
            .editor
            .language
            .peek
            .as_ref()
            .filter(|p| p.revision == revision && valid_peek(app, p))
        {
            place = Some(p.place.clone());
        }
        if place.is_some() {
            close(app);
        }
    });
    if let Some(place) = place {
        navigate::go(ctx, place, rect, true);
    }
}

fn valid_peek(app: &AppState, peek: &Peek) -> bool {
    valid(app, peek.open_generation, &peek.origin)
        && peek.target_stamp.is_none_or(|stamp| {
            app.workspace
                .file_id_for_resource(&peek.place.resource)
                .and_then(|id| app.workspace.documents.get(&id).map(|d| d.stamp(id)))
                .is_some_and(|now| now == stamp)
        })
}
