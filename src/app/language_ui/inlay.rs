//! Inlay hints: the types and parameter names a server offers to show inside
//! lines. They are asked for around the caret once the text settles, and only
//! shown for exactly the text they were computed for.
use super::{AppContext, AppState, services, target};
use crate::app::ui_timer;
use crate::domain::document::DocumentStamp;
use crate::domain::identity::PaneId;
use crate::features::language_service::position::LineIndex;
use crate::features::language_service::reply;
use crate::features::language_service::request::Feature;
use crate::features::providers::{RequestHandle, RequestSpec};
use crate::ui::editor::hints::Hint;
use std::sync::Arc;
use std::time::Duration;

/// How long the text must rest before hints are asked for.
const SETTLE: Duration = Duration::from_millis(300);
/// Lines asked for on each side of the caret.
const REACH: usize = 120;
/// Hints beyond this many are not shown.
const MAX_HINTS: usize = 3000;

#[derive(Clone)]
struct Pending {
    handle: Arc<RequestHandle>,
    pane: PaneId,
    view: crate::domain::identity::ViewId,
    input_generation: u64,
    stamp: DocumentStamp,
    text: Arc<String>,
    lines: std::ops::Range<usize>,
}

#[derive(Clone)]
pub struct Shown {
    pub pane: PaneId,
    pub stamp: DocumentStamp,
    /// The lines the hints cover; the caret leaving them asks again.
    lines: std::ops::Range<usize>,
    pub hints: Arc<[Hint]>,
}

#[derive(Clone, PartialEq, Eq)]
struct Seen {
    pane: PaneId,
    view: crate::domain::identity::ViewId,
    generation: u64,
    stamp: DocumentStamp,
    line: usize,
    providers: Vec<(u64, u64, u64)>,
}
#[derive(Clone, Default)]
pub struct Inlay {
    timer: Option<Arc<ui_timer::Timer>>,
    pending: Option<Pending>,
    seen: Option<Seen>,
    pub shown: Option<Shown>,
}

impl Inlay {
    /// The hints to draw in \`pane\` for the document in this state.
    pub fn for_view(&self, pane: PaneId, stamp: &DocumentStamp) -> Option<Arc<[Hint]>> {
        self.shown
            .as_ref()
            .filter(|shown| shown.pane == pane && shown.stamp == *stamp)
            .map(|shown| shown.hints.clone())
    }
}

/// Runs after every update.
pub fn observe(ctx: &AppContext, app: &mut AppState) {
    if !app.preferences.values().language_features.inlay_hints {
        app.editor.language.inlay = Inlay::default();
        return;
    }
    let pane = app.workspace.active_pane();
    let Some(t) = target(app, pane) else {
        app.editor.language.inlay = Inlay::default();
        return;
    };
    let line = t.text[..t.cursor.min(t.text.len())].matches('\n').count();
    let mut providers = services(ctx)
        .map(|services| {
            services
                .capability_sources(Some(&t.resource))
                .into_iter()
                .filter(|i| i.providers.inlay_hint)
                .map(|i| (i.id, i.run, i.revision))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    providers.sort_unstable();
    let now = Seen {
        pane,
        view: t.view,
        generation: t.input_generation,
        stamp: t.stamp,
        line,
        providers,
    };
    let inlay = &mut app.editor.language.inlay;
    if inlay.seen.as_ref() == Some(&now) {
        return;
    }
    let same_provider = inlay.seen.as_ref().is_some_and(|previous| {
        previous.providers == now.providers
            && previous.pane == now.pane
            && previous.view == now.view
            && previous.generation == now.generation
    });
    inlay.seen = Some(now.clone());
    // Hints for the same text stay while the caret is among the lines they cover.
    let covered = same_provider
        && inlay.shown.as_ref().is_some_and(|shown| {
            shown.stamp == t.stamp && shown.lines.start + 20 <= line && line + 20 <= shown.lines.end
                || shown.stamp == t.stamp && shown.lines.start == 0 && line + 20 <= shown.lines.end
        });
    if covered {
        return;
    }
    inlay.pending = None;
    inlay.timer = None;
    inlay.shown = None;
    let Some(application) = ctx.services().and_then(|r| r.application()) else {
        return;
    };
    let timed = ctx.clone();
    inlay.timer = Some(Arc::new(ui_timer::after(application, SETTLE, move || {
        timed.update(|app| {
            if app.editor.language.inlay.seen.as_ref() == Some(&now) {
                request(&timed, app);
            }
        });
    })));
}

fn request(ctx: &AppContext, app: &mut AppState) {
    app.editor.language.inlay.timer = None;
    let pane = app.workspace.active_pane();
    let Some(t) = target(app, pane) else { return };
    let Some(services) = services(ctx) else {
        return;
    };
    services.sync(app);
    let index = LineIndex::new(&t.text);
    let line = t.text[..t.cursor.min(t.text.len())].matches('\n').count();
    let first = line.saturating_sub(REACH);
    let last = (line + REACH).min(index.line_count().saturating_sub(1));
    let start = index.offset(
        crate::features::language_service::position::Position {
            line: first as u32,
            character: 0,
        },
        crate::features::language_service::capabilities::PositionEncoding::Utf8,
    );
    let end = index
        .offset(
            crate::features::language_service::position::Position {
                line: last as u32,
                character: u32::MAX,
            },
            crate::features::language_service::capabilities::PositionEncoding::Utf8,
        )
        .min(t.text.len());
    let handle = Arc::new(services.request(
        RequestSpec::new(Feature::InlayHint, t.resource.clone(), t.lsp_stamp(), start).range(end),
    ));
    app.editor.language.inlay.pending = Some(Pending {
        handle,
        pane,
        view: t.view,
        input_generation: t.input_generation,
        stamp: t.stamp,
        text: t.text.clone(),
        lines: first..last + 1,
    });
}

/// Takes the servers' hints when they arrive.
pub fn tick(app: &mut AppState) -> bool {
    let Some(pending) = app.editor.language.inlay.pending.clone() else {
        return false;
    };
    let progress = pending.handle.poll();
    if !progress.finished {
        return false;
    }
    app.editor.language.inlay.pending = None;
    if target(app, pending.pane).is_none_or(|t| {
        t.view != pending.view
            || t.input_generation != pending.input_generation
            || t.stamp != pending.stamp
    }) {
        return false;
    }
    let index = LineIndex::new(&pending.text);
    let mut hints: Vec<Hint> = Vec::new();
    for part in &progress.parts {
        for hint in reply::inlay_hints(&part.value, part.encoding) {
            let offset = index.offset(hint.position, hint.encoding);
            if !pending.text.is_char_boundary(offset) || hint.label.is_empty() {
                continue;
            }
            hints.push(Hint {
                offset,
                text: format!(
                    "{}{}{}",
                    if hint.padding_left { " " } else { "" },
                    hint.label,
                    if hint.padding_right { " " } else { "" }
                ),
            });
        }
    }
    hints.sort_by_key(|hint| hint.offset);
    hints.truncate(MAX_HINTS);
    app.editor.language.inlay.shown = Some(Shown {
        pane: pending.pane,
        stamp: pending.stamp,
        lines: pending.lines,
        hints: hints.into(),
    });
    true
}
