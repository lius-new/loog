//! Folding belongs to views; ranges and asynchronous results belong to documents.
use crate::app::{context::AppContext, state::AppState};
use crate::domain::{
    document::DocumentStamp,
    identity::{FileId, ViewId},
};
use crate::features::language_service::{reply, request::Feature};
use crate::features::providers::{RequestHandle, RequestSpec};
use loom_editor_core::text::TextBuffer;
use std::{
    collections::{BTreeMap, HashMap},
    ops::Range,
    sync::Arc,
    time::Duration,
};

#[derive(Clone)]
struct Available {
    stamp: DocumentStamp,
    ranges: Arc<[Range<usize>]>,
    syntax: bool,
}
#[derive(Clone)]
struct Collapsed {
    stamp: DocumentStamp,
    ranges: BTreeMap<usize, usize>,
}
#[derive(Clone, Copy)]
pub enum FoldAction {
    Fold,
    Unfold,
    FoldAll,
    UnfoldAll,
}
#[derive(Clone)]
struct Intent {
    view: ViewId,
    generation: u64,
    cursor: usize,
    action: FoldAction,
}
#[derive(Clone)]
struct Pending {
    stamp: DocumentStamp,
    handle: Option<Arc<RequestHandle>>,
    timer: Option<Arc<super::ui_timer::Timer>>,
    intent: Option<Intent>,
}
type Providers = Vec<(u64, u64, u64)>;
#[derive(Clone, Default)]
pub struct State {
    available: HashMap<FileId, Available>,
    views: HashMap<ViewId, Collapsed>,
    pending: HashMap<FileId, Pending>,
    seen: HashMap<FileId, (DocumentStamp, Providers)>,
}
fn supported(buffer: &TextBuffer) -> bool {
    buffer.len() <= 1024 * 1024 && buffer.line_count() <= 100_000
}
fn normalize(buffer: &TextBuffer, ranges: &[Range<usize>]) -> Arc<[Range<usize>]> {
    if !supported(buffer) {
        return Arc::from([]);
    }
    let mut headers = BTreeMap::new();
    for range in ranges.iter().take(8192) {
        if range.start >= range.end || range.end > buffer.len() {
            continue;
        }
        let first = buffer.line_of(range.start);
        let last = buffer.line_of(range.end - 1);
        if last <= first {
            continue;
        }
        let start = buffer.line_start(first);
        let end = if last + 1 < buffer.line_count() {
            buffer.line_start(last + 1)
        } else {
            buffer.len()
        };
        headers
            .entry(start)
            .and_modify(|old: &mut usize| *old = (*old).max(end))
            .or_insert(end);
    }
    let mut accepted = Vec::new();
    let mut parents = Vec::new();
    for (start, end) in headers {
        while parents.last().is_some_and(|&p| p <= start) {
            parents.pop();
        }
        if parents.last().is_some_and(|&p| end > p) {
            continue;
        }
        parents.push(end);
        accepted.push(start..end);
    }
    accepted.into()
}
pub fn sync(app: &mut AppState) {
    let documents = &app.workspace.documents;
    let views = &app.workspace.views;
    let state = &mut app.editor.editing.folding;
    state.available.retain(|id, a| {
        documents
            .get(id)
            .is_some_and(|d| d.is_text() && d.stamp(*id) == a.stamp)
    });
    state.pending.retain(|id, p| {
        documents
            .get(id)
            .is_some_and(|d| d.is_text() && d.stamp(*id) == p.stamp)
    });
    state.seen.retain(|id, (stamp, _)| {
        documents
            .get(id)
            .is_some_and(|d| d.is_text() && d.stamp(*id) == *stamp)
    });
    state.views.retain(|view, folded| {
        let Some(doc) = views
            .get(view)
            .and_then(|v| v.document())
            .and_then(|id| documents.get(&id))
        else {
            return false;
        };
        let id = views[view].document().unwrap();
        let stamp = doc.stamp(id);
        if stamp == folded.stamp {
            return true;
        }
        if stamp.incarnation != folded.stamp.incarnation
            || stamp.language_generation != folded.stamp.language_generation
            || !supported(&doc.buffer)
        {
            return false;
        }
        if stamp.version != folded.stamp.version {
            let Some(revisions) = doc
                .buffer
                .revisions_since(folded.stamp.version, folded.stamp.snapshot_identity)
            else {
                return false;
            };
            let mut mapped = BTreeMap::new();
            for (&start, &end) in &folded.ranges {
                let mut range = start..end;
                let mut valid = true;
                for revision in &revisions {
                    if revision.edits.iter().any(|e| {
                        if e.old.is_empty() {
                            range.contains(&e.old.start)
                        } else {
                            e.old.start < range.end && e.old.end > range.start
                        }
                    }) {
                        valid = false;
                        break;
                    }
                    range = revision.map(range.start, true)..revision.map(range.end, false);
                }
                if valid && range.start < range.end && range.end <= doc.buffer.len() {
                    mapped.insert(range.start, range.end);
                }
            }
            folded.ranges = mapped;
        }
        folded.stamp = stamp;
        true
    });
}
pub fn collapsed(app: &AppState, view: ViewId) -> Vec<Range<usize>> {
    let Some(doc) = app
        .workspace
        .view_document(view)
        .and_then(|id| app.workspace.documents.get(&id))
    else {
        return Vec::new();
    };
    app.editor
        .editing
        .folding
        .views
        .get(&view)
        .filter(|f| doc.stamp(f.stamp.document) == f.stamp)
        .map(|f| f.ranges.iter().map(|(&a, &b)| a..b).collect())
        .unwrap_or_default()
}
pub fn markers(
    app: &AppState,
    view: ViewId,
    visible: &Range<usize>,
) -> Arc<[crate::ui::editor_binding::FoldMarker]> {
    let Some(id) = app.workspace.view_document(view) else {
        return Arc::from([]);
    };
    let doc = &app.workspace.documents[&id];
    let stamp = doc.stamp(id);
    if !supported(&doc.buffer) {
        return Arc::from([]);
    }
    let mut headers = BTreeMap::new();
    if let Some(a) = app
        .editor
        .editing
        .folding
        .available
        .get(&id)
        .filter(|a| a.stamp == stamp)
    {
        for range in a.ranges.iter() {
            headers.insert(range.start, (range.end, false));
        }
    }
    if let Some(f) = app
        .editor
        .editing
        .folding
        .views
        .get(&view)
        .filter(|f| f.stamp == stamp)
    {
        for (&start, &end) in &f.ranges {
            headers.insert(start, (end, true));
        }
    }
    headers
        .into_iter()
        .filter_map(|(start, (end, collapsed))| {
            let line = doc.buffer.line_of(start);
            visible
                .contains(&line)
                .then_some(crate::ui::editor_binding::FoldMarker {
                    line,
                    start,
                    end,
                    collapsed,
                })
        })
        .collect::<Vec<_>>()
        .into()
}
pub fn publish_syntax(
    app: &mut AppState,
    stamp: DocumentStamp,
    ranges: Option<Arc<[Range<usize>]>>,
) {
    let Some(doc) = app
        .workspace
        .documents
        .get(&stamp.document)
        .filter(|d| d.is_text() && d.stamp(stamp.document) == stamp)
    else {
        return;
    };
    let ranges = normalize(&doc.buffer, ranges.as_deref().unwrap_or(&[]));
    let state = &mut app.editor.editing.folding;
    if ranges.is_empty()
        && state
            .available
            .get(&stamp.document)
            .is_some_and(|a| a.stamp == stamp && !a.syntax && !a.ranges.is_empty())
    {
        return;
    }
    let syntax = !ranges.is_empty();
    state.available.insert(
        stamp.document,
        Available {
            stamp,
            ranges,
            syntax,
        },
    );
    if syntax {
        let intent = state.pending.remove(&stamp.document).and_then(|p| p.intent);
        if let Some(intent) = intent {
            fulfill(app, stamp, intent);
        }
    }
}
fn fulfill(app: &mut AppState, stamp: DocumentStamp, intent: Intent) {
    if app.workspace.active() != Some(intent.view)
        || app
            .workspace
            .view(intent.view)
            .is_none_or(|v| v.input_generation != intent.generation)
        || app
            .workspace
            .active_editor()
            .is_none_or(|e| e.cursor() != intent.cursor)
    {
        return;
    }
    apply(app, intent.view, stamp, intent.action);
    if let Some(rect) = app.editor.language.rect {
        super::editor::reveal_cursor(app, rect);
    }
}
fn normalize_selection(app: &mut AppState, view: ViewId) {
    let ranges = collapsed(app, view);
    let Some(pane) = app.workspace.pane_of(view) else {
        return;
    };
    if let Some(mut editor) = app.workspace.view_editor_mut(pane, view) {
        editor.map_selections(|buffer, mut selection| {
            for range in &ranges {
                let end = buffer.line_end(buffer.line_of(range.start));
                if selection.cursor > end && selection.cursor < range.end {
                    selection.cursor = end;
                }
                if let Some(anchor) = selection.anchor
                    && anchor > end
                    && anchor < range.end
                {
                    selection.anchor = Some(end);
                }
            }
            selection
        });
    }
}
fn apply(app: &mut AppState, view: ViewId, stamp: DocumentStamp, action: FoldAction) {
    let Some(doc) = app
        .workspace
        .documents
        .get(&stamp.document)
        .filter(|d| d.stamp(stamp.document) == stamp)
    else {
        return;
    };
    if !supported(&doc.buffer) {
        return;
    }
    let ranges = app
        .editor
        .editing
        .folding
        .available
        .get(&stamp.document)
        .filter(|a| a.stamp == stamp)
        .map(|a| a.ranges.clone())
        .unwrap_or_else(|| Arc::from([]));
    let cursor = app
        .workspace
        .pane_of(view)
        .and_then(|p| app.workspace.editor(p))
        .map_or(0, |e| e.cursor());
    let line = doc.buffer.line_of(cursor);
    let folding = &mut app.editor.editing.folding.views;
    let folded = folding.entry(view).or_insert_with(|| Collapsed {
        stamp,
        ranges: BTreeMap::new(),
    });
    if folded.stamp != stamp {
        folded.stamp = stamp;
        folded.ranges.clear();
    }
    match action {
        FoldAction::UnfoldAll => folded.ranges.clear(),
        FoldAction::FoldAll => {
            for r in ranges.iter() {
                folded.ranges.insert(r.start, r.end);
            }
        }
        FoldAction::Unfold => {
            let found = folded
                .ranges
                .iter()
                .filter(|&(&a, &b)| a <= cursor && (cursor < b || doc.buffer.line_of(a) == line))
                .min_by_key(|&(&a, &b)| b - a)
                .map(|(&a, _)| a);
            if let Some(start) = found {
                folded.ranges.remove(&start);
            }
        }
        FoldAction::Fold => {
            let found = ranges
                .iter()
                .filter(|r| {
                    (r.start <= cursor && cursor < r.end) || doc.buffer.line_of(r.start) == line
                })
                .filter(|r| !folded.ranges.contains_key(&r.start))
                .min_by_key(|r| r.end - r.start);
            if let Some(r) = found {
                folded.ranges.insert(r.start, r.end);
            }
        }
    }
    app.editor.drag = None;
    normalize_selection(app, view);
}
pub fn toggle(
    app: &mut AppState,
    view: ViewId,
    stamp: DocumentStamp,
    start: usize,
    rect: lgui::prelude::UiRect,
) -> bool {
    sync(app);
    if app.workspace.view_document(view) != Some(stamp.document)
        || app
            .workspace
            .documents
            .get(&stamp.document)
            .is_none_or(|d| d.stamp(stamp.document) != stamp)
    {
        return false;
    }
    if app
        .workspace
        .pane_of(view)
        .and_then(|p| app.workspace.pane(p))
        .is_none_or(|p| p.active() != Some(view))
    {
        return false;
    }
    let existing = app
        .editor
        .editing
        .folding
        .views
        .get_mut(&view)
        .and_then(|f| f.ranges.remove(&start));
    if existing.is_none() {
        let Some(range) = app
            .editor
            .editing
            .folding
            .available
            .get(&stamp.document)
            .filter(|a| a.stamp == stamp)
            .and_then(|a| a.ranges.iter().find(|r| r.start == start))
            .cloned()
        else {
            return false;
        };
        let folded = app
            .editor
            .editing
            .folding
            .views
            .entry(view)
            .or_insert_with(|| Collapsed {
                stamp,
                ranges: BTreeMap::new(),
            });
        folded.stamp = stamp;
        folded.ranges.insert(range.start, range.end);
        normalize_selection(app, view);
    }
    if let Some(pane) = app.workspace.pane_of(view) {
        app.workspace.activate_pane(pane);
    }
    super::editor::reveal_cursor(app, rect);
    true
}
pub fn reveal(app: &mut AppState) {
    sync(app);
    let Some(view) = app.workspace.active() else {
        return;
    };
    let Some(editor) = app.workspace.active_editor() else {
        return;
    };
    let selections = editor.selections();
    let remove = collapsed(app, view)
        .into_iter()
        .filter(|r| {
            selections.iter().any(|s| {
                [Some(s.cursor), s.anchor]
                    .into_iter()
                    .flatten()
                    .any(|cursor| {
                        editor.line_of(r.start) < editor.line_of(cursor) && cursor < r.end
                    })
            })
        })
        .map(|r| r.start)
        .collect::<Vec<_>>();
    if let Some(f) = app.editor.editing.folding.views.get_mut(&view) {
        for start in remove {
            f.ranges.remove(&start);
        }
    }
}
fn providers(ctx: &AppContext, path: &std::path::Path) -> Providers {
    let mut keys = super::language_ui::services(ctx)
        .map(|s| {
            s.capability_sources(Some(path))
                .into_iter()
                .filter(|i| i.providers.folding_range)
                .map(|i| (i.id, i.run, i.revision))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    keys.sort_unstable();
    keys
}
fn request(ctx: &AppContext, app: &mut AppState, id: FileId) {
    let Some(pending) = app.editor.editing.folding.pending.get(&id).cloned() else {
        return;
    };
    let Some(doc) = app
        .workspace
        .documents
        .get(&id)
        .filter(|d| d.stamp(id) == pending.stamp)
    else {
        return;
    };
    if app
        .editor
        .editing
        .folding
        .available
        .get(&id)
        .is_some_and(|a| a.stamp == pending.stamp && a.syntax)
    {
        return;
    }
    let Some(services) = super::language_ui::services(ctx) else {
        return;
    };
    services.sync(app);
    let handle = Arc::new(services.request(RequestSpec::new(
        Feature::FoldingRange,
        doc.meta.path.clone(),
        super::language_ui::lsp_stamp(&pending.stamp),
        0,
    )));
    if let Some(p) = app.editor.editing.folding.pending.get_mut(&id) {
        p.timer = None;
        p.handle = Some(handle);
    }
}
pub fn observe(ctx: &AppContext, app: &mut AppState) {
    reveal(app);
    let Some(t) = super::language_ui::target(app, app.workspace.active_pane()) else {
        return;
    };
    if !supported(&app.workspace.documents[&t.document].buffer) {
        return;
    }
    if app
        .editor
        .editing
        .folding
        .available
        .get(&t.document)
        .is_some_and(|a| a.stamp == t.stamp && a.syntax)
    {
        return;
    }
    let keys = providers(ctx, &t.path);
    if keys.is_empty() {
        return;
    }
    let state = &mut app.editor.editing.folding;
    if state.seen.get(&t.document) == Some(&(t.stamp, keys.clone())) {
        return;
    }
    state.seen.insert(t.document, (t.stamp, keys));
    let intent = state.pending.remove(&t.document).and_then(|p| p.intent);
    let Some(application) = ctx.services().and_then(|r| r.application()) else {
        return;
    };
    let timed = ctx.clone();
    let id = t.document;
    let timer = Arc::new(super::ui_timer::after(
        application,
        Duration::from_millis(250),
        move || {
            timed.update(|app| request(&timed, app, id));
        },
    ));
    state.pending.insert(
        id,
        Pending {
            stamp: t.stamp,
            handle: None,
            timer: Some(timer),
            intent,
        },
    );
}
pub fn command(ctx: &AppContext, action: FoldAction, rect: lgui::prelude::UiRect) {
    ctx.update(|app| {
        app.editor.language.rect = Some(rect);
        sync(app);
        let Some(t) = super::language_ui::target(app, app.workspace.active_pane()) else {
            return;
        };
        if !supported(&app.workspace.documents[&t.document].buffer) {
            app.show_toast("Folding is unavailable in this large-file view");
            return;
        }
        if matches!(action, FoldAction::Unfold | FoldAction::UnfoldAll)
            || app
                .editor
                .editing
                .folding
                .available
                .get(&t.document)
                .is_some_and(|a| a.stamp == t.stamp && !a.ranges.is_empty())
        {
            apply(app, t.view, t.stamp, action);
            super::editor::reveal_cursor(app, rect);
            return;
        }
        let keys = providers(ctx, &t.path);
        app.editor
            .editing
            .folding
            .seen
            .insert(t.document, (t.stamp, keys));
        let intent = Intent {
            view: t.view,
            generation: t.input_generation,
            cursor: t.cursor,
            action,
        };
        app.editor.editing.folding.pending.insert(
            t.document,
            Pending {
                stamp: t.stamp,
                handle: None,
                timer: None,
                intent: Some(intent),
            },
        );
        request(ctx, app, t.document);
    });
}
pub fn tick(app: &mut AppState) -> bool {
    sync(app);
    let mut completed = Vec::new();
    for (&id, p) in &app.editor.editing.folding.pending {
        if let Some(handle) = &p.handle {
            let result = handle.poll();
            if result.finished {
                completed.push((id, p.clone(), result));
            }
        }
    }
    let mut changed = false;
    for (id, pending, progress) in completed {
        app.editor.editing.folding.pending.remove(&id);
        let Some(doc) = app
            .workspace
            .documents
            .get(&id)
            .filter(|d| d.stamp(id) == pending.stamp)
        else {
            continue;
        };
        if app
            .editor
            .editing
            .folding
            .available
            .get(&id)
            .is_some_and(|a| a.stamp == pending.stamp && a.syntax)
        {
            continue;
        }
        let ranges = progress
            .usable(Feature::FoldingRange)
            .iter()
            .flat_map(|p| reply::folding_ranges(&p.value))
            .filter_map(|r| {
                let first = r.start_line as usize;
                let last = r.end_line as usize;
                (first < last && last < doc.buffer.line_count()).then(|| {
                    doc.buffer.line_start(first)..if last + 1 < doc.buffer.line_count() {
                        doc.buffer.line_start(last + 1)
                    } else {
                        doc.buffer.len()
                    }
                })
            })
            .collect::<Vec<_>>();
        let ranges = normalize(&doc.buffer, &ranges);
        app.editor.editing.folding.available.insert(
            id,
            Available {
                stamp: pending.stamp,
                ranges,
                syntax: false,
            },
        );
        changed = true;
        if let Some(intent) = pending.intent {
            fulfill(app, pending.stamp, intent);
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::pane_layout::Direction;
    const TEXT: &str = "head\nfn f() {\n  body();\n}\ntail\n";
    fn fixture() -> (AppState, ViewId, DocumentStamp, usize, usize) {
        let mut app = AppState::new();
        let view = app.workspace.test_open_view("fold.rs".into(), TEXT.into());
        let id = app.workspace.view_document(view).unwrap();
        let doc = &app.workspace.documents[&id];
        let stamp = doc.stamp(id);
        let start = doc.buffer.line_start(1);
        let end = doc.buffer.line_start(4);
        let range = start..end;
        publish_syntax(
            &mut app,
            stamp,
            Some(Arc::from(std::slice::from_ref(&range))),
        );
        (app, view, stamp, start, end)
    }
    #[test]
    fn folds_belong_to_views_and_map_only_across_edits_outside_the_region() {
        let (mut app, view, stamp, start, end) = fixture();
        let rect = lgui::prelude::UiRect::new(0.0, 0.0, 500.0, 300.0);
        assert!(toggle(&mut app, view, stamp, start, rect));
        assert_eq!(super::super::editing::projection(&app, view, rect).len(), 4);
        let left = app.workspace.active_pane();
        let right = app.workspace.split(left, Direction::Right).unwrap();
        let peer = app.workspace.pane(right).unwrap().active().unwrap();
        assert!(collapsed(&app, peer).is_empty());
        assert_eq!(collapsed(&app, view).len(), 1);
        app.workspace.activate_pane(left);
        let mut ed = app.workspace.active_editor_mut().unwrap();
        ed.set_cursor(0);
        loom_editor_core::normal::insert(&mut ed, "新\n");
        sync(&mut app);
        assert_eq!(collapsed(&app, view), vec![start + 4..end + 4]);
        let id = app.workspace.view_document(view).unwrap();
        let header = app.workspace.documents[&id].buffer.line_end(2);
        app.workspace
            .active_editor_mut()
            .unwrap()
            .set_cursor(header);
        app.workspace
            .active_editor_mut()
            .unwrap()
            .set_line_ending(loom_editor_core::text::LineEnding::Lf)
            .unwrap();
        sync(&mut app);
        assert_eq!(
            collapsed(&app, view).len(),
            1,
            "metadata-only changes preserve structure"
        );
        app.workspace.activate_pane(right);
        let body = app.workspace.active_editor().unwrap().line_start(3);
        let mut ed = app.workspace.active_editor_mut().unwrap();
        ed.set_cursor(body);
        loom_editor_core::normal::insert(&mut ed, "change");
        sync(&mut app);
        assert!(collapsed(&app, view).is_empty());
    }
    #[test]
    fn navigation_skips_hidden_lines_and_search_reveal_expands_the_target() {
        let (mut app, view, stamp, start, _) = fixture();
        let rect = lgui::prelude::UiRect::new(0.0, 0.0, 500.0, 300.0);
        app.workspace.active_editor_mut().unwrap().set_cursor(start);
        toggle(&mut app, view, stamp, start, rect);
        assert!(super::super::editing::move_displayed(
            &mut app,
            loom_editor_core::commands::Command::Move(
                loom_editor_core::normal::Movement::Down,
                false
            )
        ));
        assert_eq!(app.workspace.active_editor().unwrap().line_col().0, 4);
        let body = app.workspace.active_editor().unwrap().line_start(2);
        app.workspace
            .active_editor_mut()
            .unwrap()
            .select_range(body + 2..body + 6);
        super::super::editor::reveal_cursor(&mut app, rect);
        assert!(collapsed(&app, view).is_empty());
        assert_eq!(
            app.workspace.active_editor().unwrap().selected_text(),
            Some("body")
        );
    }
    #[test]
    fn stale_ranges_and_deferred_intents_cannot_change_another_view_or_revision() {
        let (mut app, view, stamp, start, _) = fixture();
        let intent = Intent {
            view,
            generation: app.workspace.view(view).unwrap().input_generation,
            cursor: 0,
            action: FoldAction::FoldAll,
        };
        app.editor.editing.folding.pending.insert(
            stamp.document,
            Pending {
                stamp,
                handle: None,
                timer: None,
                intent: Some(intent),
            },
        );
        app.workspace
            .test_open_view("other.txt".into(), "other".into());
        let range = start..TEXT.find("tail").unwrap();
        publish_syntax(
            &mut app,
            stamp,
            Some(Arc::from(std::slice::from_ref(&range))),
        );
        assert!(collapsed(&app, view).is_empty());
        assert!(!toggle(
            &mut app,
            view,
            stamp,
            start,
            lgui::prelude::UiRect::new(0.0, 0.0, 500.0, 300.0)
        ));
        app.workspace.set_active(view);
        loom_editor_core::normal::insert(&mut app.workspace.active_editor_mut().unwrap(), "new\n");
        publish_syntax(
            &mut app,
            stamp,
            Some(Arc::from(std::slice::from_ref(&range))),
        );
        sync(&mut app);
        assert!(markers(&app, view, &(0..100)).is_empty());
        assert!(!toggle(
            &mut app,
            view,
            stamp,
            start,
            lgui::prelude::UiRect::new(0.0, 0.0, 500.0, 300.0)
        ));
    }
    #[test]
    fn nested_and_crossing_ranges_and_hidden_selection_are_normalized() {
        let buffer = TextBuffer::new("0\n1\n2\n3\n4\n5\n6\n7");
        let ranges = [0..8, 2..6, 4..12];
        let found = normalize(&buffer, &ranges);
        assert_eq!(found.len(), 2, "crossing ranges are rejected");
        let (mut app, view, stamp, start, _) = fixture();
        let a = app.workspace.active_editor().unwrap().line_start(2);
        app.workspace
            .active_editor_mut()
            .unwrap()
            .select_range(a + 2..a + 6);
        toggle(
            &mut app,
            view,
            stamp,
            start,
            lgui::prelude::UiRect::new(0.0, 0.0, 500.0, 300.0),
        );
        assert!(app.workspace.active_editor().unwrap().selection().is_none());
        assert_eq!(app.workspace.active_editor().unwrap().line_col().0, 1);
    }
}
