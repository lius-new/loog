//! Quiet code-action discovery. Only a deliberate click opens the action list.
use super::{AppContext, AppState, Target, refactor, services, target};
use crate::app::ui_timer;
use crate::domain::{document::DocumentStamp, identity::PaneId};
use crate::features::language_service::{reply, reply::CodeAction, request::Feature};
use crate::features::providers::{CodeActionContext, RequestHandle, RequestSpec};
use std::{ops::Range, sync::Arc, time::Duration};

#[derive(Clone, PartialEq, Eq)]
struct Seen {
    pane: PaneId,
    view: crate::domain::identity::ViewId,
    input_generation: u64,
    stamp: DocumentStamp,
    cursor: usize,
    selection: Option<Range<usize>>,
    context: CodeActionContext,
    workspace: Vec<(loog_workspace_api::ResourceRef, DocumentStamp)>,
}

#[derive(Clone)]
struct Pending {
    target: Target,
    handle: Arc<RequestHandle>,
}

#[derive(Clone, Default)]
pub struct Lightbulb {
    revision: u64,
    seen: Option<Seen>,
    timer: Option<Arc<ui_timer::Timer>>,
    pending: Option<Pending>,
    shown: Option<(Target, Vec<(u64, CodeAction)>)>,
    menu_open: bool,
}

fn current(ctx: &AppContext, app: &AppState) -> Option<(Target, Seen)> {
    let t = target(app, app.workspace.active_pane())?;
    let context = services(ctx)?.code_action_context(&t.resource, t.lsp_stamp());
    let mut workspace = super::edit::stamps(app);
    workspace.sort_by(|a, b| a.0.cmp(&b.0));
    let seen = Seen {
        pane: t.pane,
        view: t.view,
        input_generation: t.input_generation,
        stamp: t.stamp,
        cursor: t.cursor,
        selection: t.selection.clone(),
        context,
        workspace,
    };
    Some((t, seen))
}

pub fn observe(ctx: &AppContext, app: &mut AppState) {
    let now = current(ctx, app).map(|(_, seen)| seen);
    let bulb = &mut app.editor.language.lightbulb;
    if bulb.seen == now {
        return;
    }
    bulb.revision = bulb.revision.wrapping_add(1);
    bulb.seen = now;
    bulb.timer = None;
    bulb.pending = None;
    bulb.shown = None;
    if bulb.menu_open {
        bulb.menu_open = false;
        if app
            .editor
            .language
            .popup
            .as_ref()
            .is_some_and(|p| p.kind == super::popup::Kind::CodeActions)
        {
            app.editor.language.popup = None;
        }
    }
    if app.editor.language.save.is_some()
        || !bulb.seen.as_ref().is_some_and(|s| s.context.has_sources())
        || app
            .editor
            .language
            .refactor
            .as_ref()
            .is_some_and(|p| matches!(p.wanted, refactor::Wanted::CodeActions))
    {
        return;
    }
    let Some(application) = ctx.services().and_then(|r| r.application()) else {
        return;
    };
    let revision = app.editor.language.lightbulb.revision;
    let timed = ctx.clone();
    app.editor.language.lightbulb.timer = Some(Arc::new(ui_timer::after(
        application,
        Duration::from_millis(350),
        move || {
            timed.update(|app| request(&timed, app, revision));
        },
    )));
}

fn request(ctx: &AppContext, app: &mut AppState, revision: u64) {
    if app.editor.language.save.is_some() {
        return;
    }
    if app
        .editor
        .language
        .refactor
        .as_ref()
        .is_some_and(|p| matches!(p.wanted, refactor::Wanted::CodeActions))
    {
        return;
    }
    let Some((t, seen)) = current(ctx, app) else {
        return;
    };
    let bulb = &mut app.editor.language.lightbulb;
    if bulb.revision != revision || bulb.seen.as_ref() != Some(&seen) {
        return;
    }
    bulb.timer = None;
    let Some(services) = services(ctx) else {
        return;
    };
    services.sync(app);
    let range = t.selection.clone().unwrap_or(t.cursor..t.cursor);
    let handle = Arc::new(
        services.request(
            RequestSpec::new(
                Feature::CodeAction,
                t.resource.clone(),
                t.lsp_stamp(),
                range.start,
            )
            .range(range.end)
            .extra(serde_json::json!({"context": {"triggerKind": 2}})),
        ),
    );
    app.editor.language.lightbulb.pending = Some(Pending { target: t, handle });
}

/// Explicit requests take priority: the manager replaces older action requests.
pub fn pause(app: &mut AppState) {
    app.editor.language.lightbulb.menu_open = false;
    app.editor.language.lightbulb.timer = None;
    app.editor.language.lightbulb.pending = None;
    // Invalidate callbacks that have already reached the UI queue.
    app.editor.language.lightbulb.revision = app.editor.language.lightbulb.revision.wrapping_add(1);
}

pub fn tick(ctx: &AppContext, app: &mut AppState) {
    let Some(pending) = app.editor.language.lightbulb.pending.clone() else {
        return;
    };
    let progress = pending.handle.poll();
    if !progress.finished {
        return;
    }
    let valid = current(ctx, app)
        .is_some_and(|(_, now)| app.editor.language.lightbulb.seen.as_ref() == Some(&now));
    let bulb = &mut app.editor.language.lightbulb;
    bulb.pending = None;
    if !valid {
        return;
    }
    let actions: Vec<_> = progress
        .usable(Feature::CodeAction)
        .iter()
        .flat_map(|part| {
            reply::code_actions(&part.value, part.encoding)
                .into_iter()
                .map(move |action| (part.instance, action))
        })
        .filter(|(_, action)| action.disabled.is_none())
        .collect();
    // Disabled rows remain available in a manually opened menu, but do not light
    // the bulb when nothing can be chosen.
    bulb.shown = (!actions.is_empty()).then_some((pending.target, actions));
}

pub fn for_view(ctx: &AppContext, app: &AppState, pane: PaneId) -> Option<u64> {
    let bulb = &app.editor.language.lightbulb;
    let (t, _) = bulb.shown.as_ref()?;
    if t.pane != pane {
        return None;
    }
    let (_, now) = current(ctx, app)?;
    (bulb.seen.as_ref() == Some(&now)).then_some(bulb.revision)
}

pub fn open(ctx: &AppContext, app: &mut AppState, pane: PaneId, revision: u64) -> bool {
    if for_view(ctx, app, pane) != Some(revision) {
        return false;
    }
    let (t, actions) = app
        .editor
        .language
        .lightbulb
        .shown
        .as_ref()
        .unwrap()
        .clone();
    app.editor.language.close_popups();
    super::snippet_session::dismiss_choices(app);
    app.editor.language.popup = refactor::action_popup(&t, actions);
    app.editor.language.lightbulb.menu_open = true;
    true
}
