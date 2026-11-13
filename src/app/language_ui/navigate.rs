//! Going to a place in a file, and back: the position history behind
//! "go to definition" and the back and forward keys.
use super::{AppContext, AppState};
use crate::features::language_service::reply::Span;
use lgui::prelude::UiRect;
use loog_workspace_api::ResourceRef;
use std::ops::Range;
use std::path::{Path, PathBuf};

/// Positions kept for going back.
const MAX_HISTORY: usize = 100;
/// Files larger than this are not opened by a jump.
const MAX_FILE: u64 = 16 * 1024 * 1024;

/// Where to go: a file, and what to select there.
#[derive(Clone, Debug)]
pub struct Place {
    pub resource: ResourceRef,
    pub target: Target,
}

#[derive(Clone, Debug)]
pub enum Target {
    /// A span in the server's terms, read in the text of the target file.
    Span(Span),
    /// Bytes of the file as it is open now.
    Bytes(Range<usize>),
}

impl Place {
    pub fn bytes(path: PathBuf, range: Range<usize>) -> Self {
        Self::resource_bytes(path.into(), range)
    }
    pub fn resource_bytes(resource: ResourceRef, range: Range<usize>) -> Self {
        Self {
            resource,
            target: Target::Bytes(range),
        }
    }

    pub fn span(path: PathBuf, span: Span) -> Self {
        Self::resource_span(path.into(), span)
    }
    pub fn resource_span(resource: ResourceRef, span: Span) -> Self {
        Self {
            resource,
            target: Target::Span(span),
        }
    }

    pub(super) fn range(&self, text: &str) -> Range<usize> {
        match &self.target {
            Target::Span(span) => span.bytes(text),
            Target::Bytes(range) => {
                let end = range.end.min(text.len());
                range.start.min(end)..end
            }
        }
    }
}

/// A place visited, remembered by line and column so an edit elsewhere in the
/// file does not leave it pointing into the middle of something.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    pub resource: ResourceRef,
    pub offset: usize,
    pub position: Option<(u32, u32)>,
}

#[derive(Clone, Default)]
pub struct History {
    back: Vec<Mark>,
    forward: Vec<Mark>,
}

impl History {
    /// Remembers `mark` as a place to come back to. Going somewhere new forgets
    /// what was ahead.
    pub fn visit(&mut self, mark: Mark) {
        self.forward.clear();
        if self.back.last() != Some(&mark) {
            self.back.push(mark);
            if self.back.len() > MAX_HISTORY {
                self.back.remove(0);
            }
        }
    }

    /// The place to go back to, given where the person is now.
    pub fn go_back(&mut self, here: Mark) -> Option<Mark> {
        let mark = loop {
            let mark = self.back.pop()?;
            if mark != here {
                break mark;
            }
        };
        if self.forward.last() != Some(&here) {
            self.forward.push(here);
        }
        Some(mark)
    }

    pub fn go_forward(&mut self, here: Mark) -> Option<Mark> {
        let mark = loop {
            let mark = self.forward.pop()?;
            if mark != here {
                break mark;
            }
        };
        if self.back.last() != Some(&here) {
            self.back.push(here);
        }
        Some(mark)
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }
}

fn here(app: &AppState) -> Option<Mark> {
    let resource = app
        .workspace
        .active_document()
        .and_then(|id| app.workspace.documents.get(&id))?
        .meta
        .resource
        .clone();
    let editor = app.workspace.active_editor()?;
    Some(Mark {
        resource,
        offset: editor.cursor(),
        position: {
            let p = crate::features::language_service::position::LineIndex::new(editor.text())
                .position(
                    editor.cursor(),
                    crate::features::language_service::capabilities::PositionEncoding::Utf8,
                );
            Some((p.line, p.character))
        },
    })
}

/// Selects `range` of the active document and brings it into view.
fn select(app: &mut AppState, range: Range<usize>, rect: UiRect) {
    if let Some(mut editor) = app.workspace.active_editor_mut() {
        if range.is_empty() {
            editor.set_cursor(range.start);
        } else {
            editor.select_range(range);
        }
    }
    crate::app::editor::reveal_cursor(app, rect);
}

/// Opens `place` (reading the file if it is not open), selects its target, and
/// remembers where the person was. `remember` is false when going back or forward.
pub fn go(state: &AppContext, place: Place, rect: UiRect, remember: bool) {
    let app = state.get();
    let origin = here(&app);
    let source = super::target(&app, app.workspace.active_pane());
    let open_generation = app.explorer.resources.open_generation;
    let mut revision = 0;
    state.update(|app| {
        app.editor.language.navigation_revision =
            app.editor.language.navigation_revision.wrapping_add(1);
        revision = app.editor.language.navigation_revision;
    });
    if app
        .workspace
        .file_id_for_resource(&place.resource)
        .is_some_and(|id| app.workspace.open_error(id).is_none())
    {
        state.update(|app| arrive(app, &place, rect, remember.then_some(origin).flatten()));
        return;
    }
    if place.resource.environment().is_some() {
        let guard_source = source.clone();
        crate::app::resources::read_text(
            state,
            place.resource.clone(),
            MAX_FILE as usize,
            crate::app::resources::TextConsumer {
                valid: Box::new(move |app| {
                    current(app, revision, open_generation, guard_source.as_ref())
                }),
                ready: Box::new(move |app, result| match result {
                    Ok(prepared) => {
                        app.workspace.open_prepared_text(
                            prepared,
                            crate::app::workspace::OpenMode::Permanent,
                        );
                        arrive(app, &place, rect, remember.then_some(origin).flatten());
                    }
                    Err(error) => app.show_error(format!(
                        "Could not open {}: {error}",
                        place.resource.display()
                    )),
                }),
            },
        );
        return;
    }
    let path = place.resource.local_path().unwrap().to_path_buf();
    let size_ok = std::fs::metadata(&path)
        .map(|meta| meta.len() <= MAX_FILE)
        .unwrap_or(false);
    if !size_ok {
        let name = path.display().to_string();
        state.update(move |app| {
            app.workspace.open_failed(
                path,
                format!("Could not open {name}: file is unavailable or larger than 16 MiB."),
                crate::app::workspace::OpenMode::Permanent,
            );
            app.chrome.toast = None;
        });
        return;
    }
    let Some(application) = state.services().and_then(|runtime| runtime.application()) else {
        return;
    };
    let worker_state = state.clone();
    let failed_path = path.clone();
    let spawned = std::thread::Builder::new()
        .name("loog-goto-open".to_owned())
        .spawn(move || {
            let read =
                crate::infrastructure::document_filesystem::read_encoded(&path, None, MAX_FILE)
                    .map_err(|error| error.to_string())
                    .map(|(decoded, disk)| {
                        crate::app::workspace::PreparedTextFile::with_encoding(path, decoded, disk)
                    });
            application.post(move || {
                worker_state.update(|app| {
                    if !current(app, revision, open_generation, source.as_ref()) {
                        return;
                    }
                    match read {
                        Ok(prepared) => {
                            app.workspace.open_prepared_text(
                                prepared,
                                crate::app::workspace::OpenMode::Permanent,
                            );
                            arrive(app, &place, rect, remember.then_some(origin).flatten());
                            app.chrome.toast = None;
                        }
                        Err(error) => {
                            app.workspace.open_failed(
                                place.resource.local_path().unwrap().to_path_buf(),
                                format!("Could not open {}: {error}", place.resource.display()),
                                crate::app::workspace::OpenMode::Permanent,
                            );
                            app.chrome.toast = None;
                        }
                    }
                })
            });
        });
    if spawned.is_err() {
        state.update(|app| {
            app.workspace.open_failed(
                failed_path,
                "Could not start reading the file".into(),
                crate::app::workspace::OpenMode::Permanent,
            );
            app.chrome.toast = None;
        });
    }
}

fn current(
    app: &AppState,
    revision: u64,
    open_generation: u64,
    source: Option<&super::Target>,
) -> bool {
    app.editor.language.navigation_revision == revision
        && app.explorer.resources.open_generation == open_generation
        && app.editor.pending_file.is_none()
        && (source.is_some() || app.workspace.active_document().is_none())
        && source.is_none_or(|source| {
            app.workspace.active_pane() == source.pane
                && super::target(app, source.pane).is_some_and(|now| {
                    now.same_view(source)
                        && now.stamp == source.stamp
                        && now.cursor == source.cursor
                        && now.selection == source.selection
                })
        })
}
fn arrive(app: &mut AppState, place: &Place, rect: UiRect, origin: Option<Mark>) {
    if let Some(origin) = origin {
        app.editor.vim.record_jump(origin.offset);
        app.editor.language.history.visit(origin);
    }
    let Some(id) = app.workspace.file_id_for_resource(&place.resource) else {
        return;
    };
    app.workspace.activate_document(id);
    let Some(range) = app
        .workspace
        .active_editor()
        .map(|editor| place.range(editor.text()))
    else {
        return;
    };
    let panel = app
        .editor
        .language
        .popup
        .take()
        .filter(|p| p.kind.is_panel());
    app.editor.language.close_popups();
    app.editor.language.popup = panel.map(|mut p| {
        p.panel_focused = false;
        p
    });
    select(app, range, rect);
}

/// Back to the previous place visited by a jump.
pub fn back(state: &AppContext, rect: UiRect) {
    let app = state.get();
    let Some(here) = here(&app) else { return };
    let mut target = None;
    state.update(|app| target = app.editor.language.history.go_back(here));
    if let Some(mark) = target {
        go(state, mark_place(mark), rect, false);
    }
}

/// Forward again after going back.
pub fn forward(state: &AppContext, rect: UiRect) {
    let app = state.get();
    let Some(here) = here(&app) else { return };
    let mut target = None;
    state.update(|app| target = app.editor.language.history.go_forward(here));
    if let Some(mark) = target {
        go(state, mark_place(mark), rect, false);
    }
}

/// A path as the person reads it: relative to the first workspace folder.
pub fn display_path(app: &AppState, path: &Path) -> String {
    app.explorer
        .model
        .roots()
        .iter()
        .find_map(|root| path.strip_prefix(root).ok())
        .unwrap_or(path)
        .display()
        .to_string()
        .replace('\\', "/")
}
pub fn display_resource(app: &AppState, resource: &ResourceRef) -> String {
    match resource.local_path() {
        Some(path) => display_path(app, path),
        None => resource.display().to_string(),
    }
}

fn mark_place(mark: Mark) -> Place {
    match mark.position {
        Some((line, character)) => {
            let p = crate::features::language_service::position::Position { line, character };
            Place::resource_span(
                mark.resource,
                Span {
                    start: p,
                    end: p,
                    encoding:
                        crate::features::language_service::capabilities::PositionEncoding::Utf8,
                },
            )
        }
        None => Place::resource_bytes(mark.resource, mark.offset..mark.offset),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(path: &str, offset: usize) -> Mark {
        Mark {
            resource: ResourceRef::local(path),
            offset,
            position: None,
        }
    }

    #[test]
    fn going_back_and_forward_walks_the_places_visited() {
        let mut history = History::default();
        history.visit(mark("a", 1));
        history.visit(mark("b", 2));
        // The person is now at c.
        let back = history.go_back(mark("c", 3)).unwrap();
        assert_eq!(back, mark("b", 2));
        let back = history.go_back(mark("b", 2)).unwrap();
        assert_eq!(back, mark("a", 1));
        assert!(history.go_back(mark("a", 1)).is_none());
        let ahead = history.go_forward(mark("a", 1)).unwrap();
        assert_eq!(ahead, mark("b", 2));
        let ahead = history.go_forward(mark("b", 2)).unwrap();
        assert_eq!(ahead, mark("c", 3));
        assert!(!history.can_go_forward());
    }

    #[test]
    fn a_new_place_forgets_what_was_ahead() {
        let mut history = History::default();
        history.visit(mark("a", 1));
        history.go_back(mark("b", 2));
        assert!(history.can_go_forward());
        history.visit(mark("x", 9));
        assert!(!history.can_go_forward());
    }

    #[test]
    fn the_same_place_twice_is_one_entry_and_the_history_is_bounded() {
        let mut history = History::default();
        history.visit(mark("a", 1));
        history.visit(mark("a", 1));
        assert_eq!(history.back.len(), 1);
        for n in 0..300 {
            history.visit(mark("f", n));
        }
        assert_eq!(history.back.len(), MAX_HISTORY);
    }

    #[test]
    fn going_back_skips_the_place_the_person_is_already_at() {
        let mut history = History::default();
        history.visit(mark("a", 1));
        history.visit(mark("b", 2));
        let back = history.go_back(mark("b", 2)).unwrap();
        assert_eq!(back, mark("a", 1));
    }

    #[test]
    fn a_places_range_is_clamped_to_the_text() {
        let place = Place::bytes("f".into(), 5..99);
        assert_eq!(place.range("abcdefgh"), 5..8);
        let place = Place::bytes("f".into(), 40..99);
        assert_eq!(place.range("abc"), 3..3);
    }
    #[test]
    fn navigation_history_keeps_identical_paths_in_different_environments() {
        let mut history = History::default();
        let first = Mark {
            resource: ResourceRef::provided(
                loog_workspace_api::EnvironmentKey::new("provider", "one").unwrap(),
                "/work/a",
            )
            .unwrap(),
            offset: 3,
            position: Some((0, 3)),
        };
        let second = Mark {
            resource: ResourceRef::provided(
                loog_workspace_api::EnvironmentKey::new("provider", "two").unwrap(),
                "/work/a",
            )
            .unwrap(),
            ..first.clone()
        };
        history.visit(first.clone());
        history.visit(second.clone());
        assert_eq!(history.go_back(mark("/work/a", 3)), Some(second.clone()));
        assert_eq!(history.go_back(second), Some(first.clone()));
        assert_eq!(mark_place(first.clone()).resource, first.resource);
    }
}
