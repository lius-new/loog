//! Growing and shrinking the selection by syntax: the ranges a server says
//! enclose the caret, innermost first.
use super::{AppContext, AppState, services, target};
use crate::domain::document::DocumentStamp;
use crate::domain::identity::PaneId;
use crate::features::language_service::reply;
use crate::features::language_service::request::Feature;
use crate::features::providers::{RequestHandle, RequestSpec};
use std::ops::Range;
use std::sync::Arc;

#[derive(Clone)]
struct Pending {
    handle: Arc<RequestHandle>,
    pane: PaneId,
    view: crate::domain::identity::ViewId,
    input_generation: u64,
    stamp: DocumentStamp,
    text: Arc<String>,
}

#[derive(Clone, Default)]
pub struct Expansion {
    pending: Option<Pending>,
    /// The enclosing ranges around the caret, innermost first.
    chain: Vec<Range<usize>>,
    chain_for: Option<DocumentStamp>,
    /// The selections before each growth, to shrink back through.
    stack: Vec<Option<Range<usize>>>,
    /// What the last step selected, to know whether the person moved since.
    last: Option<(DocumentStamp, Option<Range<usize>>)>,
}

/// The first range of `chain` that strictly contains `current` (or the caret).
pub fn next_larger(chain: &[Range<usize>], current: &Range<usize>) -> Option<Range<usize>> {
    chain
        .iter()
        .find(|r| {
            r.start <= current.start
                && current.end <= r.end
                && (r.start < current.start || r.end > current.end)
        })
        .cloned()
}

/// Selects the next larger enclosing range; asks the servers first if needed.
pub fn grow(ctx: &AppContext, app: &mut AppState) {
    let pane = app.workspace.active_pane();
    let Some(t) = target(app, pane) else { return };
    let state = &mut app.editor.language.expansion;
    let current = t.selection.clone().unwrap_or(t.cursor..t.cursor);
    let continuing = state.last.as_ref() == Some(&(t.stamp, Some(current.clone())))
        && state.chain_for == Some(t.stamp);
    if !continuing {
        state.stack.clear();
    }
    if state.chain_for == Some(t.stamp) && !state.chain.is_empty() {
        step(app, pane, t.stamp, current);
        return;
    }
    let Some(services) = services(ctx) else {
        return;
    };
    services.sync(app);
    let handle = Arc::new(services.request(RequestSpec::new(
        Feature::SelectionRange,
        t.path.clone(),
        t.lsp_stamp(),
        t.cursor,
    )));
    app.editor.language.expansion.pending = Some(Pending {
        handle,
        pane,
        view: t.view,
        input_generation: t.input_generation,
        stamp: t.stamp,
        text: t.text.clone(),
    });
}

fn step(app: &mut AppState, pane: PaneId, stamp: DocumentStamp, current: Range<usize>) {
    let state = &mut app.editor.language.expansion;
    let Some(next) = next_larger(&state.chain, &current) else {
        return;
    };
    state.stack.push(if current.is_empty() {
        None
    } else {
        Some(current)
    });
    state.last = Some((stamp, Some(next.clone())));
    if let Some(mut editor) = app.workspace.editor_mut(pane) {
        editor.select_range(next);
    }
}

/// Goes back to the selection before the last growth.
pub fn shrink(app: &mut AppState) {
    let pane = app.workspace.active_pane();
    let state = &mut app.editor.language.expansion;
    let Some(previous) = state.stack.pop() else {
        return;
    };
    let Some(mut editor) = app.workspace.editor_mut(pane) else {
        return;
    };
    match previous {
        Some(range) => editor.select_range(range.clone()),
        None => {
            let at = editor.selection().map_or(editor.cursor(), |r| r.start);
            editor.set_cursor(at);
        }
    }
    drop(editor);
    let stamp = app
        .workspace
        .pane(pane)
        .and_then(|p| p.active())
        .and_then(|view| app.workspace.view_document(view))
        .and_then(|id| app.workspace.documents.get(&id).map(|d| d.stamp(id)));
    let selection = app.workspace.editor(pane).and_then(|e| e.selection());
    if let Some(stamp) = stamp {
        app.editor.language.expansion.last = Some((stamp, selection));
    }
}

pub fn tick(app: &mut AppState) -> bool {
    let Some(pending) = app.editor.language.expansion.pending.clone() else {
        return false;
    };
    let progress = pending.handle.poll();
    if !progress.finished {
        return false;
    }
    app.editor.language.expansion.pending = None;
    let chain: Vec<Range<usize>> = progress
        .usable(Feature::SelectionRange)
        .first()
        .map(|part| {
            reply::selection_chain(&part.value, part.encoding)
                .into_iter()
                .map(|span| span.bytes(&pending.text))
                .collect()
        })
        .unwrap_or_default();
    let Some(t) = target(app, pending.pane).filter(|t| {
        t.view == pending.view
            && t.input_generation == pending.input_generation
            && t.stamp == pending.stamp
    }) else {
        return false;
    };
    if chain.is_empty() {
        app.show_toast("No language server here can expand the selection");
        return true;
    }
    app.editor.language.expansion.chain = chain;
    app.editor.language.expansion.chain_for = Some(pending.stamp);
    let current = t.selection.clone().unwrap_or(t.cursor..t.cursor);
    step(app, pending.pane, pending.stamp, current);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_next_larger_range_strictly_contains_the_current_one() {
        let chain = vec![2..3, 0..9, 0..20];
        assert_eq!(next_larger(&chain, &(2..2)), Some(2..3));
        assert_eq!(next_larger(&chain, &(2..3)), Some(0..9));
        assert_eq!(next_larger(&chain, &(0..9)), Some(0..20));
        assert_eq!(next_larger(&chain, &(0..20)), None);
        assert_eq!(next_larger(&chain, &(5..6)), Some(0..9));
    }
}
