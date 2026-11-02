//! Where else the symbol under the caret appears: a faint tint on each
//! occurrence, as VS Code and IntelliJ draw them.
use super::{AppContext, AppState, services, target};
use crate::app::ui_timer;
use crate::domain::document::DocumentStamp;
use crate::domain::identity::PaneId;
use crate::features::language_service::reply;
use crate::features::language_service::request::Feature;
use crate::features::providers::{RequestHandle, RequestSpec};
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

/// How long the caret rests before the servers are asked.
const REST: Duration = Duration::from_millis(250);

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
pub struct Highlights {
    seen: Option<(DocumentStamp, usize)>,
    timer: Option<Arc<ui_timer::Timer>>,
    pending: Option<Pending>,
    pub shown: Option<Shown>,
}

#[derive(Clone)]
pub struct Shown {
    pub pane: PaneId,
    pub stamp: DocumentStamp,
    pub ranges: Arc<[Range<usize>]>,
}

impl Highlights {
    /// The ranges to tint in `pane` showing the document in this state.
    pub fn for_view(&self, pane: PaneId, stamp: &DocumentStamp) -> Option<Arc<[Range<usize>]>> {
        self.shown
            .as_ref()
            .filter(|shown| shown.pane == pane && shown.stamp == *stamp)
            .map(|shown| shown.ranges.clone())
    }
}

/// Runs after every update: a moved caret clears the tint and, once it rests,
/// asks for the new one.
pub fn observe(ctx: &AppContext, app: &mut AppState) {
    let pane = app.workspace.active_pane();
    let Some(t) = target(app, pane) else {
        app.editor.language.highlights = Highlights::default();
        return;
    };
    let now = (t.stamp, t.cursor);
    if app.editor.language.highlights.seen == Some(now) {
        return;
    }
    let highlights = &mut app.editor.language.highlights;
    highlights.seen = Some(now);
    highlights.pending = None;
    highlights.timer = None;
    // Staying on the same symbol keeps its tint.
    let still_on_it = highlights.shown.as_ref().is_some_and(|shown| {
        shown.stamp == t.stamp
            && shown
                .ranges
                .iter()
                .any(|r| r.start <= t.cursor && t.cursor <= r.end)
    });
    if !still_on_it {
        highlights.shown = None;
    }
    if t.selection.is_some() || still_on_it {
        return;
    }
    let Some(application) = ctx.services().and_then(|r| r.application()) else {
        return;
    };
    let timed = ctx.clone();
    highlights.timer = Some(Arc::new(ui_timer::after(application, REST, move || {
        timed.update(|app| request(&timed, app));
    })));
}

fn request(ctx: &AppContext, app: &mut AppState) {
    let pane = app.workspace.active_pane();
    let Some(t) = target(app, pane) else { return };
    if app.editor.language.highlights.seen != Some((t.stamp, t.cursor)) {
        return;
    }
    let Some(services) = services(ctx) else {
        return;
    };
    let handle = Arc::new(services.request(RequestSpec::new(
        Feature::DocumentHighlight,
        t.path.clone(),
        t.lsp_stamp(),
        t.cursor,
    )));
    app.editor.language.highlights.pending = Some(Pending {
        handle,
        pane,
        view: t.view,
        input_generation: t.input_generation,
        stamp: t.stamp,
        text: t.text.clone(),
    });
}

pub fn tick(app: &mut AppState) -> bool {
    let Some(pending) = app.editor.language.highlights.pending.clone() else {
        return false;
    };
    let progress = pending.handle.poll();
    if !progress.finished {
        return false;
    }
    app.editor.language.highlights.pending = None;
    if target(app, pending.pane).is_none_or(|t| {
        t.view != pending.view
            || t.input_generation != pending.input_generation
            || t.stamp != pending.stamp
    }) {
        return false;
    }
    let ranges: Vec<Range<usize>> = progress
        .usable(Feature::DocumentHighlight)
        .iter()
        .flat_map(|part| reply::highlights(&part.value, part.encoding))
        .map(|h| h.span.bytes(&pending.text))
        .filter(|range| !range.is_empty())
        .collect();
    // Only one occurrence is not worth showing.
    app.editor.language.highlights.shown = (ranges.len() > 1).then(|| Shown {
        pane: pending.pane,
        stamp: pending.stamp,
        ranges: ranges.into(),
    });
    true
}
