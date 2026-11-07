//! A snippet being filled in: Tab moves between its tab stops, typing in one
//! updates its mirrors, and leaving it ends the session.
use super::AppState;
use crate::domain::identity::{FileId, PaneId, ViewId};
use crate::features::language_service::snippet::Snippet;
use crate::model::text::{Assoc, Edit, EditKind, Selection};
use std::ops::Range;

#[derive(Clone)]
pub struct Stop {
    pub index: u32,
    /// The first is the stop itself; the others mirror it.
    pub ranges: Vec<Range<usize>>,
    pub choices: Vec<String>,
    transforms: Vec<Option<crate::features::language_service::snippet::Transform>>,
    orders: Vec<usize>,
    applied: bool,
    raw: Option<String>,
}

#[derive(Clone)]
pub struct Session {
    pub pane: PaneId,
    pub document: FileId,
    pub view: ViewId,
    pub input_generation: u64,
    stops: Vec<Stop>,
    /// Which stop is active.
    current: usize,
    /// The text state the ranges describe.
    version: u64,
    identity: usize,
    incarnation: u64,
    language: u64,
    revision: u64,
    choice: Option<Choice>,
}

#[derive(Clone, Default)]
struct Choice {
    selected: usize,
    scroll: usize,
}

fn revision(app: &mut AppState) -> u64 {
    app.editor.language.snippet_revision += 1;
    app.editor.language.snippet_revision
}

fn current(app: &AppState, session: &Session) -> bool {
    app.workspace.active_pane() == session.pane
        && app.workspace.active() == Some(session.view)
        && app
            .workspace
            .view(session.view)
            .is_some_and(|v| v.accepts_text() && v.input_generation == session.input_generation)
        && app
            .workspace
            .documents
            .get(&session.document)
            .is_some_and(|document| {
                let stamp = document.stamp(session.document);
                stamp.incarnation == session.incarnation
                    && stamp.language_generation == session.language
                    && document.buffer.version() == session.version
                    && document.buffer.snapshot_identity() == session.identity
            })
}

pub fn has_choices(app: &AppState) -> bool {
    app.editor.language.snippet.as_ref().is_some_and(|session| {
        current(app, session)
            && session.stops.get(session.current).is_some_and(|s| {
                !s.choices.is_empty()
                    && app.workspace.editor(session.pane).is_some_and(|editor| {
                        s.ranges.first().is_some_and(|range| {
                            range.start <= editor.cursor()
                                && editor.cursor() <= range.end
                                && editor.selection().is_none_or(|selection| {
                                    range.start <= selection.start && selection.end <= range.end
                                })
                        })
                    })
            })
    })
}

pub fn choice_visible(app: &AppState) -> bool {
    has_choices(app)
        && app
            .editor
            .language
            .snippet
            .as_ref()
            .is_some_and(|s| s.choice.is_some())
}

/// Reopens the choices for the current stop without changing its text.
pub fn show_choices(app: &mut AppState) -> bool {
    if !has_choices(app) {
        return false;
    }
    super::completion::close(app);
    app.editor.language.popup = None;
    let choice = choice_for(app, app.editor.language.snippet.as_ref().unwrap());
    let next = revision(app);
    let session = app.editor.language.snippet.as_mut().unwrap();
    session.revision = next;
    session.choice = Some(choice);
    true
}

pub fn dismiss_choices(app: &mut AppState) {
    if let Some(session) = app.editor.language.snippet.as_mut() {
        session.choice = None;
    }
}

pub fn choice_view(app: &AppState, pane: PaneId) -> Option<(u64, crate::ui::editor::popup::View)> {
    if !choice_visible(app) {
        return None;
    }
    let session = app
        .editor
        .language
        .snippet
        .as_ref()
        .filter(|s| s.pane == pane)?;
    let stop = session.stops.get(session.current)?;
    let choice = session.choice.as_ref()?;
    Some((
        session.revision,
        crate::ui::editor::popup::View {
            panel: false,
            rows: stop
                .choices
                .iter()
                .map(|value| crate::ui::editor::popup::Row {
                    label: if value.is_empty() {
                        "(empty)".into()
                    } else {
                        value.clone()
                    },
                    detail: String::new(),
                    badge: "",
                    matches: Vec::new(),
                    faint: false,
                })
                .collect::<Vec<_>>()
                .into(),
            selected: choice.selected,
            scroll: choice.scroll,
            visible: super::popup::VISIBLE,
            query: None,
            title: Some("Choose a snippet value".into()),
            empty: String::new(),
            anchor: stop.ranges.first().map(|r| r.start),
            docs: None,
        },
    ))
}

pub fn move_choice(app: &mut AppState, by: isize) {
    if !choice_visible(app) {
        return;
    }
    let session = app.editor.language.snippet.as_mut().unwrap();
    let total = session.stops[session.current].choices.len();
    let choice = session.choice.as_mut().unwrap();
    choice.selected = choice.selected.saturating_add_signed(by).min(total - 1);
    reveal_choice(choice);
}

fn reveal_choice(choice: &mut Choice) {
    if choice.selected < choice.scroll {
        choice.scroll = choice.selected;
    }
    if choice.selected >= choice.scroll + super::popup::VISIBLE {
        choice.scroll = choice.selected + 1 - super::popup::VISIBLE;
    }
}

fn choice_for(app: &AppState, session: &Session) -> Choice {
    let stop = &session.stops[session.current];
    let value = stop.ranges.first().and_then(|range| {
        app.workspace
            .documents
            .get(&session.document)?
            .buffer
            .text()
            .get(range.clone())
    });
    let selected = stop
        .choices
        .iter()
        .position(|choice| Some(choice.as_str()) == value)
        .unwrap_or(0);
    let mut choice = Choice {
        selected,
        scroll: 0,
    };
    reveal_choice(&mut choice);
    choice
}

/// A mouse callback names the exact menu generation that produced it.
pub fn choice_event(
    app: &mut AppState,
    expected: u64,
    event: crate::ui::editor::popup::Event,
) -> bool {
    if !choice_visible(app) || app.editor.language.snippet.as_ref().unwrap().revision != expected {
        return false;
    }
    use crate::ui::editor::popup::Event;
    match event {
        Event::Focus | Event::Select(_) | Event::OpenLink(_) => {}
        Event::Dismiss => dismiss_choices(app),
        Event::Hover(index) | Event::Pick(index) => {
            let session = app.editor.language.snippet.as_mut().unwrap();
            if index >= session.stops[session.current].choices.len() {
                return false;
            }
            let choice = session.choice.as_mut().unwrap();
            choice.selected = index;
            reveal_choice(choice);
            if matches!(event, Event::Pick(_)) {
                return accept_choice(app);
            }
        }
        Event::Scroll(by) => {
            let session = app.editor.language.snippet.as_mut().unwrap();
            let total = session.stops[session.current].choices.len();
            let choice = session.choice.as_mut().unwrap();
            choice.scroll = choice
                .scroll
                .saturating_add_signed(by)
                .min(total.saturating_sub(super::popup::VISIBLE));
        }
    }
    true
}

/// Replace the primary stop, propagate mirrors, and keep the stop selected.
pub fn accept_choice(app: &mut AppState) -> bool {
    if !choice_visible(app) {
        return false;
    }
    let mut session = app.editor.language.snippet.take().unwrap();
    let stop = &session.stops[session.current];
    let value = stop.choices[session.choice.as_ref().unwrap().selected].clone();
    let range = stop.ranges[0].clone();
    if app.workspace.documents[&session.document]
        .buffer
        .text()
        .get(range.clone())
        == Some(value.as_str())
    {
        session.choice = None;
        select(app, &session);
        app.editor.language.snippet = Some(session);
        return true;
    }
    session.stops[session.current].applied = false;
    session.stops[session.current].raw = None;
    let Some(mut editor) = app.workspace.view_editor_mut(session.pane, session.view) else {
        return false;
    };
    editor.begin_group();
    let applied = editor.edit(
        EditKind::Other,
        vec![Edit::replace(range.clone(), value)],
        |changes, _| Selection::caret(changes.map(range.start, Assoc::After)),
    );
    drop(editor);
    let mut valid = applied.is_ok() && follow(app, &mut session);
    // Inner stops may change an enclosing placeholder which has mirrors itself.
    if valid {
        for _ in 0..=session.stops.len() {
            match mirror(app, &mut session) {
                Ok(false) => break,
                Ok(true) => {}
                Err(()) => {
                    valid = false;
                    break;
                }
            }
        }
    }
    if let Some(mut editor) = app.workspace.view_editor_mut(session.pane, session.view) {
        editor.end_group();
    }
    if valid {
        session.choice = None;
        select(app, &session);
        app.editor.language.snippet = Some(session);
    }
    valid
}

impl Session {
    pub fn current_index(&self) -> usize {
        self.current
    }

    pub fn stop_count(&self) -> usize {
        self.stops.len()
    }

    fn contains(&self, offset: usize) -> bool {
        self.stops
            .iter()
            .flat_map(|stop| &stop.ranges)
            .any(|range| range.start <= offset && offset <= range.end)
    }
}

/// Starts filling in `snippet`, inserted at `origin`, with the document at
/// `version`. The first stop is selected.
pub fn begin(app: &mut AppState, pane: PaneId, origin: usize, snippet: &Snippet, version: u64) {
    end(app);
    let Some(view) = app.workspace.pane(pane).and_then(|p| p.active()) else {
        return;
    };
    let Some(id) = app.workspace.view_document(view) else {
        return;
    };
    let input_generation = app.workspace.view(view).unwrap().input_generation;
    let Some(document) = app.workspace.documents.get(&id) else {
        return;
    };
    if origin > document.buffer.len()
        || snippet.stops.iter().any(|s| {
            s.ranges.is_empty()
                || s.transforms.len() != s.ranges.len()
                || s.orders.len() != s.ranges.len()
                || s.ranges
                    .iter()
                    .any(|r| r.start > r.end || r.end > document.buffer.len() - origin)
        })
    {
        return;
    }
    let stops: Vec<Stop> = snippet
        .stops
        .iter()
        .map(|stop| Stop {
            index: stop.index,
            ranges: stop
                .ranges
                .iter()
                .map(|r| origin + r.start..origin + r.end)
                .collect(),
            choices: stop.choices.clone(),
            transforms: stop.transforms.clone(),
            orders: stop.orders.clone(),
            applied: false,
            raw: None,
        })
        .collect();
    let Some(document) = app.workspace.documents.get(&id) else {
        return;
    };
    if stops.is_empty()
        || document.buffer.version() != version
        || stops.iter().flat_map(|s| &s.ranges).any(|range| {
            range.start > range.end || document.buffer.text().get(range.clone()).is_none()
        })
    {
        return;
    }
    let identity = document.buffer.snapshot_identity();
    let stamp = document.stamp(id);
    // Only the final stop: the caret goes there and there is nothing to visit.
    let single = stops.len() == 1 && stops[0].choices.is_empty();
    let mut session = Session {
        pane,
        document: id,
        view,
        input_generation,
        stops,
        current: 0,
        version,
        identity,
        incarnation: stamp.incarnation,
        language: stamp.language_generation,
        revision: revision(app),
        choice: (!snippet.stops[0].choices.is_empty()).then(Choice::default),
    };
    select(app, &session);
    if single {
        return;
    }
    session.version = app
        .workspace
        .documents
        .get(&id)
        .map_or(version, |d| d.buffer.version());
    app.editor.language.snippet = Some(session);
    if choice_visible(app) {
        super::completion::close(app);
        app.editor.language.popup = None;
    }
}

fn select(app: &mut AppState, session: &Session) {
    let Some(range) = session
        .stops
        .get(session.current)
        .and_then(|s| s.ranges.first().cloned())
    else {
        return;
    };
    if let Some(mut editor) = app.workspace.view_editor_mut(session.pane, session.view) {
        if range.is_empty() {
            editor.set_cursor(range.start);
        } else {
            editor.select_range(range);
        }
    }
}

/// Moves to the next (or previous) stop. After the last one the session ends and
/// the caret is where the snippet said. Returns false when there is no session.
pub fn step(app: &mut AppState, forward: bool) -> bool {
    let Some(mut session) = app.editor.language.snippet.take() else {
        return false;
    };
    if !current(app, &session) || !follow(app, &mut session) {
        return false;
    }
    // Transform only on leaving a stop. Re-entering a transformed stop does not
    // apply a non-idempotent substitution twice; typing starts a fresh value.
    let stop = &mut session.stops[session.current];
    if !stop.applied {
        if stop.transforms.first().is_some_and(Option::is_some) {
            stop.raw = app
                .workspace
                .documents
                .get(&session.document)
                .and_then(|d| d.buffer.text().get(stop.ranges[0].clone()))
                .map(str::to_owned);
        }
        stop.applied = true;
    }
    if let Some(mut editor) = app.workspace.view_editor_mut(session.pane, session.view) {
        editor.begin_group();
    }
    let mut valid = true;
    for _ in 0..=session.stops.len() {
        match mirror(app, &mut session) {
            Ok(false) => break,
            Ok(true) => {}
            Err(()) => {
                valid = false;
                break;
            }
        }
    }
    if let Some(mut editor) = app.workspace.view_editor_mut(session.pane, session.view) {
        editor.end_group();
    }
    if !valid {
        return false;
    }
    let last = session.stops.len() - 1;
    let was_last = session.current == last;
    let target = if forward {
        session.current + 1
    } else {
        session.current.saturating_sub(1)
    };
    session.current = target.min(last);
    session.choice =
        (!session.stops[session.current].choices.is_empty()).then(|| choice_for(app, &session));
    session.revision = revision(app);
    select(app, &session);
    // Landing on the final stop ends the session.
    if session.current < last || (!was_last && session.choice.is_some()) {
        app.editor.language.snippet = Some(session);
        if choice_visible(app) {
            super::completion::close(app);
            app.editor.language.popup = None;
        }
    }
    true
}

pub fn end(app: &mut AppState) {
    app.editor.language.snippet = None;
}

/// Brings the ranges up to date with edits made since they were recorded, and
/// copies what was typed into a stop to its mirrors. Ends the session when the
/// caret has left it or the edits cannot be followed.
pub fn observe(app: &mut AppState) {
    let Some(mut session) = app.editor.language.snippet.take() else {
        return;
    };
    if app.workspace.active_pane() != session.pane
        || app.workspace.active() != Some(session.view)
        || app
            .workspace
            .view(session.view)
            .is_none_or(|v| !v.accepts_text() || v.input_generation != session.input_generation)
    {
        return;
    }
    let Some(version) = app
        .workspace
        .documents
        .get(&session.document)
        .filter(|document| {
            let stamp = document.stamp(session.document);
            stamp.incarnation == session.incarnation
                && stamp.language_generation == session.language
        })
        .map(|document| document.buffer.version())
    else {
        return;
    };
    let cursor = app
        .workspace
        .view_editor_mut(session.pane, session.view)
        .map(|editor| editor.cursor());
    if version == session.version {
        if !current(app, &session) {
            return;
        }
        // Only the caret moved: leaving every stop ends the session.
        if cursor.is_some_and(|c| session.contains(c)) {
            app.editor.language.snippet = Some(session);
        }
        return;
    }
    if !follow(app, &mut session) {
        return;
    }
    session.stops[session.current].applied = false;
    session.stops[session.current].raw = None;
    session.choice = None;
    session.revision = revision(app);
    if mirror(app, &mut session).is_err() {
        return;
    }
    let cursor = app
        .workspace
        .view_editor_mut(session.pane, session.view)
        .map(|editor| editor.cursor());
    if cursor.is_some_and(|c| session.contains(c)) {
        app.editor.language.snippet = Some(session);
    }
}

/// Maps every range across the revisions since the session last looked.
fn follow(app: &AppState, session: &mut Session) -> bool {
    follow_owned(app, session, &[(session.current, 0)])
}

fn follow_owned(app: &AppState, session: &mut Session, owners: &[(usize, usize)]) -> bool {
    let Some(document) = app.workspace.documents.get(&session.document) else {
        return false;
    };
    if document.buffer.version() == session.version {
        return true;
    }
    let Some(revisions) = document
        .buffer
        .revisions_since(session.version, session.identity)
    else {
        return false;
    };
    for revision in &revisions {
        let owned: Vec<_> = owners
            .iter()
            .filter_map(|(s, r)| {
                Some((
                    session.stops.get(*s)?.ranges.get(*r)?.clone(),
                    session.stops[*s].orders[*r],
                ))
            })
            .collect();
        for (s, stop) in session.stops.iter_mut().enumerate() {
            for (r, range) in stop.ranges.iter_mut().enumerate() {
                let owns_insert = owners.contains(&(s, r))
                    || owned
                        .iter()
                        .any(|(owner, _)| range.start < owner.start && owner.end < range.end);
                let precedes_insert = range.start == range.end
                    && owned.iter().any(|(owner, order)| {
                        owner.is_empty() && owner.start == range.start && stop.orders[r] < *order
                    });
                let start_before = revision.edits.iter().any(|edit| {
                    if edit.old.is_empty() {
                        (owns_insert || precedes_insert) && edit.old.start == range.start
                    } else {
                        edit.old.start <= range.start && range.start < edit.old.end
                    }
                });
                let end_after = revision.edits.iter().any(|edit| {
                    if edit.old.is_empty() {
                        owns_insert && edit.old.start == range.end
                    } else {
                        edit.old.start < range.end && range.end < edit.old.end
                    }
                });
                let start = revision.map(range.start, !start_before);
                let end = revision.map(range.end, end_after);
                *range = start..end.max(start);
            }
        }
    }
    session.version = document.buffer.version();
    session.identity = document.buffer.snapshot_identity();
    true
}

/// Makes each mirror show what the stop shows.
fn mirror(app: &mut AppState, session: &mut Session) -> Result<bool, ()> {
    let Some(text) = app
        .workspace
        .documents
        .get(&session.document)
        .map(|d| d.buffer.text_snapshot())
    else {
        return Err(());
    };
    let mut edits = Vec::new();
    let mut owners = Vec::new();
    let mut empty = std::collections::BTreeMap::<usize, Vec<(usize, usize, String)>>::new();
    for (s, stop) in session.stops.iter().enumerate() {
        let Some(primary) = stop.ranges.first() else {
            continue;
        };
        let shown = stop
            .raw
            .as_deref()
            .filter(|_| stop.applied)
            .unwrap_or(text.get(primary.clone()).ok_or(())?);
        for (r, range) in stop.ranges.iter().enumerate() {
            let value = match stop
                .transforms
                .get(r)
                .and_then(Option::as_ref)
                .filter(|_| stop.applied)
            {
                Some(transform) => transform.apply(shown),
                None => shown.to_owned(),
            };
            if text.get(range.clone()).ok_or(())? != value {
                owners.push((s, r));
                if range.is_empty() {
                    empty.entry(range.start).or_default().push((s, r, value));
                } else {
                    edits.push(Edit::replace(range.clone(), value));
                }
            }
        }
    }
    for (at, values) in &mut empty {
        values.sort_by_key(|(s, r, _)| session.stops[*s].orders[*r]);
        edits.push(Edit::insert(
            *at,
            values
                .iter()
                .map(|(_, _, text)| text.as_str())
                .collect::<String>(),
        ));
    }
    if edits.is_empty() {
        return Ok(false);
    }
    let coincident: Vec<_> = session
        .stops
        .iter()
        .enumerate()
        .flat_map(|(s, stop)| {
            stop.ranges
                .iter()
                .enumerate()
                .filter(|(_, range)| range.is_empty() && empty.contains_key(&range.start))
                .map(move |(r, range)| (s, r, range.start))
        })
        .collect();
    let Some(mut editor) = app.workspace.view_editor_mut(session.pane, session.view) else {
        return Err(());
    };
    let cursor = editor.cursor();
    // The caret stays in the stop being typed into.
    let applied = editor.edit(EditKind::Typing, edits, |changes, _| {
        Selection::caret(changes.map(cursor, Assoc::Before))
    });
    drop(editor);
    let changes = applied.map_err(|_| ())?;
    if !follow_owned(app, session, &owners) {
        return Err(());
    }
    for (s, r, at) in coincident {
        let values = &empty[&at];
        let before: usize = values
            .iter()
            .filter(|(vs, vr, _)| session.stops[*vs].orders[*vr] < session.stops[s].orders[r])
            .map(|(_, _, text)| text.len())
            .sum();
        let len = values
            .iter()
            .find(|(vs, vr, _)| *vs == s && *vr == r)
            .map_or(0, |(_, _, text)| text.len());
        let start = changes.map(at, Assoc::Before) + before;
        session.stops[s].ranges[r] = start..start + len;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::language_service::snippet::parse;

    fn app_with(text: &str) -> (AppState, PaneId) {
        let mut app = AppState::new();
        app.workspace.open_path("s.rs".into(), text.into());
        let pane = app.workspace.active_pane();
        (app, pane)
    }

    /// Inserts `snippet` at the cursor, as accepting a completion does.
    fn insert(app: &mut AppState, pane: PaneId, source: &str) {
        let snippet = parse(source, &|_| None);
        let origin = app.workspace.active_editor().unwrap().cursor();
        let mut editor = app.workspace.editor_mut(pane).unwrap();
        editor
            .edit(
                EditKind::Other,
                vec![Edit::insert(origin, snippet.text.clone())],
                |_, _| Selection::caret(origin),
            )
            .unwrap();
        let version = editor.version();
        drop(editor);
        begin(app, pane, origin, &snippet, version);
    }

    fn type_text(app: &mut AppState, text: &str) {
        let mut editor = app.workspace.active_editor_mut().unwrap();
        loog_editor_core::normal::insert(&mut editor, text);
        drop(editor);
        observe(app);
    }

    fn text(app: &AppState) -> String {
        app.workspace.active_editor().unwrap().text().to_owned()
    }

    #[test]
    fn the_first_stop_is_selected_and_tab_visits_the_rest_in_order() {
        let (mut app, pane) = app_with("");
        insert(&mut app, pane, "fn ${1:name}(${2:args}) {\n\t$0\n}");
        assert_eq!(text(&app), "fn name(args) {\n\t\n}");
        let editor = app.workspace.active_editor().unwrap();
        assert_eq!(editor.selection(), Some(3..7));
        assert!(step(&mut app, true));
        assert_eq!(
            app.workspace.active_editor().unwrap().selection(),
            Some(8..12)
        );
        assert!(step(&mut app, true));
        assert_eq!(app.workspace.active_editor().unwrap().cursor(), 17);
        assert!(
            app.editor.language.snippet.is_none(),
            "the last stop ends it"
        );
    }

    #[test]
    fn shift_tab_goes_back_and_stops_at_the_first() {
        let (mut app, pane) = app_with("");
        insert(&mut app, pane, "${1:a} ${2:b} $0");
        step(&mut app, true);
        assert_eq!(
            app.workspace.active_editor().unwrap().selection(),
            Some(2..3)
        );
        step(&mut app, false);
        assert_eq!(
            app.workspace.active_editor().unwrap().selection(),
            Some(0..1)
        );
        step(&mut app, false);
        assert_eq!(
            app.workspace.active_editor().unwrap().selection(),
            Some(0..1)
        );
    }

    #[test]
    fn typing_into_a_stop_updates_its_mirrors() {
        let (mut app, pane) = app_with("");
        insert(&mut app, pane, "let ${1:x} = ${2:0}; // $1");
        assert_eq!(text(&app), "let x = 0; // x");
        type_text(&mut app, "count");
        assert_eq!(text(&app), "let count = 0; // count");
        type_text(&mut app, "er");
        assert_eq!(text(&app), "let counter = 0; // counter");
        assert!(app.editor.language.snippet.is_some());
    }

    #[test]
    fn placeholder_transforms_run_on_leave_survive_other_edits_and_reset_on_retyping() {
        let (mut app, pane) = app_with("");
        insert(
            &mut app,
            pane,
            "${1:thing} $1 ${1/(.*)/${1:/upcase}/} ${2:end}$0",
        );
        type_text(&mut app, "éname");
        assert_eq!(text(&app), "éname éname éname end");
        step(&mut app, true);
        assert_eq!(text(&app), "éname éname ÉNAME end");
        type_text(&mut app, "tail");
        assert_eq!(text(&app), "éname éname ÉNAME tail");
        step(&mut app, false);
        type_text(&mut app, "new");
        assert_eq!(text(&app), "new new new tail");
        step(&mut app, true);
        assert_eq!(text(&app), "new new NEW tail");
        // Editor Undo/Redo commands end the snippet before touching history.
        end(&mut app);
        app.workspace.active_editor_mut().unwrap().undo();
        assert_eq!(text(&app), "new new new tail");
        observe(&mut app);
        assert!(app.editor.language.snippet.is_none());
        app.workspace.active_editor_mut().unwrap().redo();
        assert_eq!(text(&app), "new new NEW tail");
    }

    #[test]
    fn transform_only_stops_do_not_transform_their_value_twice_on_backtracking() {
        let (mut app, pane) = app_with("");
        insert(
            &mut app,
            pane,
            "${1/(.*)/${1:/upcase}/}:${1/(.*)/${1:/capitalize}/} ${2:x}$0",
        );
        type_text(&mut app, "blue");
        assert_eq!(text(&app), "blue:blue x");
        step(&mut app, true);
        assert_eq!(text(&app), "BLUE:Blue x");
        step(&mut app, false);
        step(&mut app, true);
        assert_eq!(text(&app), "BLUE:Blue x");
    }

    #[test]
    fn transformed_empty_occurrences_keep_source_order_when_the_primary_is_between_them() {
        let (mut app, pane) = app_with("");
        insert(&mut app, pane, "${1/(.*)/${1:/upcase}/}$1${1/(.*)/[$1]/}$0");
        type_text(&mut app, "é");
        assert_eq!(text(&app), "ééé");
        assert_eq!(
            app.editor.language.snippet.as_ref().unwrap().stops[0].ranges,
            [2..4, 0..2, 4..6]
        );
        step(&mut app, true);
        assert_eq!(text(&app), "Éé[é]");
        assert_eq!(
            app.workspace.active_editor().unwrap().cursor(),
            text(&app).len()
        );
        let (mut app, pane) = app_with("");
        insert(
            &mut app,
            pane,
            "${1/^$/${1:-before}/}$1${1/^$/${1:-after}/}$0",
        );
        step(&mut app, true);
        assert_eq!(text(&app), "beforeafter");
        assert_eq!(app.workspace.active_editor().unwrap().cursor(), 11);
    }

    #[test]
    fn nested_choices_keep_an_enclosing_transform_updated_and_undo_together() {
        let (mut app, pane) = app_with("");
        insert(
            &mut app,
            pane,
            "${1:pre ${2|a,blue|} end} ${1/(.*)/${1:/upcase}/} $0",
        );
        step(&mut app, true);
        assert_eq!(text(&app), "pre a end PRE A END ");
        move_choice(&mut app, 1);
        accept_choice(&mut app);
        assert_eq!(text(&app), "pre blue end PRE BLUE END ");
        app.workspace.active_editor_mut().unwrap().undo();
        assert_eq!(text(&app), "pre a end PRE A END ");
    }

    #[test]
    fn unknown_variables_can_be_edited_and_visited_after_explicit_stops() {
        let (mut app, pane) = app_with("");
        insert(&mut app, pane, "${1:first}:$UNKNOWN/$UNKNOWN:$0");
        step(&mut app, true);
        type_text(&mut app, "名字");
        assert_eq!(text(&app), "first:名字/名字:");
        step(&mut app, true);
        assert!(app.editor.language.snippet.is_none());
        assert_eq!(
            app.workspace.active_editor().unwrap().cursor(),
            text(&app).len()
        );
    }

    #[test]
    fn accepting_an_unchanged_choice_keeps_already_committed_transforms() {
        let (mut app, pane) = app_with("");
        insert(&mut app, pane, "${1|a,b|} ${1/(.*)/${1:/upcase}/} ${2:x}$0");
        accept_choice(&mut app);
        step(&mut app, true);
        assert_eq!(text(&app), "a A x");
        step(&mut app, false);
        accept_choice(&mut app);
        step(&mut app, true);
        type_text(&mut app, "changed");
        assert_eq!(text(&app), "a A changed");
    }

    #[test]
    fn delayed_transforms_keep_multiline_indentation_and_final_cursor_position() {
        let (mut app, pane) = app_with("");
        let mut snippet = parse("${1:x}: ${1/(.*)/$1\ncreated/}$0", &|_| None);
        crate::features::language_service::snippet::indent_lines(&mut snippet, "  ");
        let mut editor = app.workspace.editor_mut(pane).unwrap();
        editor
            .edit(
                EditKind::Other,
                vec![Edit::insert(0, snippet.text.clone())],
                |_, _| Selection::caret(0),
            )
            .unwrap();
        let version = editor.version();
        drop(editor);
        begin(&mut app, pane, 0, &snippet, version);
        step(&mut app, true);
        assert_eq!(text(&app), "x: x\n  created");
        assert_eq!(
            app.workspace.active_editor().unwrap().cursor(),
            text(&app).len()
        );
    }

    #[test]
    fn leaving_the_snippet_ends_the_session() {
        let (mut app, pane) = app_with("");
        insert(&mut app, pane, "a ${1:b} ${2:c}");
        app.workspace.active_editor_mut().unwrap().set_cursor(0);
        observe(&mut app);
        assert!(app.editor.language.snippet.is_none());
    }

    #[test]
    fn a_snippet_with_only_the_final_stop_leaves_nothing_to_visit() {
        let (mut app, pane) = app_with("");
        insert(&mut app, pane, "println!($0)");
        assert!(app.editor.language.snippet.is_none());
        assert_eq!(app.workspace.active_editor().unwrap().cursor(), 9);
    }

    #[test]
    fn a_choice_updates_all_mirrors_in_one_undo_step_and_reopens_on_the_current_value() {
        let (mut app, pane) = app_with("");
        insert(&mut app, pane, "${1|red,蓝色,green|}:$1 $2$0");
        assert!(choice_visible(&app));
        move_choice(&mut app, 1);
        assert!(accept_choice(&mut app));
        assert_eq!(text(&app), "蓝色:蓝色 ");
        assert_eq!(
            app.workspace.active_editor().unwrap().selection(),
            Some(0..6)
        );
        assert!(!choice_visible(&app));
        assert!(show_choices(&mut app));
        let (_, shown) = choice_view(&app, pane).unwrap();
        assert_eq!(shown.selected, 1);
        app.workspace.active_editor_mut().unwrap().undo();
        assert_eq!(text(&app), "red:red ", "primary and mirrors undo together");
        app.workspace.active_editor_mut().unwrap().redo();
        assert_eq!(text(&app), "蓝色:蓝色 ");
    }

    #[test]
    fn empty_and_adjacent_choice_mirrors_keep_individual_ranges_and_the_caret() {
        for source in ["${1|,long|}$1$1$0", "${1|a,long|}$1$1$0"] {
            let (mut app, pane) = app_with("");
            insert(&mut app, pane, source);
            move_choice(&mut app, 1);
            assert!(accept_choice(&mut app));
            assert_eq!(text(&app), "longlonglong");
            assert_eq!(
                app.editor.language.snippet.as_ref().unwrap().stops[0].ranges,
                [0..4, 4..8, 8..12]
            );
            type_text(&mut app, "x");
            assert_eq!(text(&app), "xxx");
            assert_eq!(app.workspace.active_editor().unwrap().cursor(), 1);
            type_text(&mut app, "y");
            assert_eq!(text(&app), "xyxyxy");
            assert_eq!(app.workspace.active_editor().unwrap().cursor(), 2);
        }
    }

    #[test]
    fn a_nested_choice_updates_its_enclosing_placeholders_mirror_in_the_same_group() {
        let (mut app, pane) = app_with("");
        insert(&mut app, pane, "${1:pre ${2|a,blue|} end} // $1 $0");
        step(&mut app, true);
        assert!(choice_visible(&app));
        move_choice(&mut app, 1);
        assert!(accept_choice(&mut app));
        assert_eq!(text(&app), "pre blue end // pre blue end ");
        app.workspace.active_editor_mut().unwrap().undo();
        assert_eq!(text(&app), "pre a end // pre a end ");
    }

    #[test]
    fn choosing_the_default_adds_no_undo_step_and_a_final_choice_can_be_filled() {
        let (mut app, pane) = app_with("");
        insert(&mut app, pane, "${1|a,b|}${0|tail,end|}");
        let version = app.workspace.active_editor().unwrap().version();
        assert!(accept_choice(&mut app));
        assert_eq!(app.workspace.active_editor().unwrap().version(), version);
        assert!(step(&mut app, true));
        assert!(
            choice_visible(&app),
            "the final stop may itself be a choice"
        );
        move_choice(&mut app, 1);
        assert!(accept_choice(&mut app));
        assert_eq!(text(&app), "aend");
        assert!(step(&mut app, true));
        assert!(app.editor.language.snippet.is_none());
        let (mut single, pane) = app_with("");
        insert(&mut single, pane, "${0|a,b|}");
        assert!(choice_visible(&single));
        dismiss_choices(&mut single);
        assert!(step(&mut single, true));
        assert!(single.editor.language.snippet.is_none());
    }

    #[test]
    fn choice_callbacks_are_bound_to_the_menu_generation_document_and_language() {
        use crate::ui::editor::popup::Event;
        for invalidate in [0, 1, 2, 3, 4] {
            let (mut app, pane) = app_with("");
            insert(&mut app, pane, "${1|a,b|} $0");
            let old = choice_view(&app, pane).unwrap().0;
            match invalidate {
                0 => {
                    dismiss_choices(&mut app);
                    show_choices(&mut app);
                }
                1 => {
                    app.workspace.open_path("other.rs".into(), "other".into());
                }
                2 => {
                    let id = app.workspace.active_document().unwrap();
                    app.workspace
                        .documents
                        .get_mut(&id)
                        .unwrap()
                        .meta
                        .language_generation += 1;
                }
                3 => {
                    let id = app.workspace.active_document().unwrap();
                    app.workspace
                        .documents
                        .get_mut(&id)
                        .unwrap()
                        .buffer
                        .reload("new".into());
                }
                _ => {
                    let end = app.workspace.active_editor().unwrap().len();
                    app.workspace.active_editor_mut().unwrap().set_cursor(end);
                }
            }
            let before = text(&app);
            assert!(!choice_event(&mut app, old, Event::Pick(1)));
            assert_eq!(text(&app), before);
        }
    }

    #[test]
    fn choice_scroll_and_tab_navigation_preserve_the_selected_value_and_reject_invalid_rows() {
        use crate::ui::editor::popup::Event;
        let (mut app, pane) = app_with("");
        let options = (0..25)
            .map(|n| format!("value{n}"))
            .collect::<Vec<_>>()
            .join(",");
        insert(&mut app, pane, &format!("${{1|{options}|}} ${{2|x,y|}} $0"));
        move_choice(&mut app, 20);
        let (old, view) = choice_view(&app, pane).unwrap();
        assert_eq!((view.selected, view.scroll), (20, 11));
        assert!(!choice_event(&mut app, old, Event::Pick(100)));
        accept_choice(&mut app);
        step(&mut app, true);
        assert!(choice_visible(&app));
        assert!(!choice_event(&mut app, old, Event::Pick(0)));
        step(&mut app, false);
        assert_eq!(choice_view(&app, pane).unwrap().1.selected, 20);
    }
}
