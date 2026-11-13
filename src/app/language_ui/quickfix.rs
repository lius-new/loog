//! Diagnostic-scoped quick fixes offered directly in the hover card.
use super::{AppContext, AppState, Target, refactor, services, target};
use crate::{
    app::ui_timer,
    domain::{document::DocumentStamp, identity::PaneId},
    features::language_service::{reply, reply::CodeAction, request::Feature},
    features::providers::{CodeActionContext, RequestHandle, RequestSpec},
};
use std::{ops::Range, sync::Arc, time::Duration};

#[derive(Clone, PartialEq, Eq)]
struct Key {
    pane: PaneId,
    view: crate::domain::identity::ViewId,
    input_generation: u64,
    stamp: DocumentStamp,
    range: Range<usize>,
    cursor: usize,
    scroll: (u32, u32),
    context: CodeActionContext,
    workspace: Vec<(loog_workspace_api::ResourceRef, DocumentStamp)>,
}
#[derive(Clone, Default)]
pub struct QuickFix {
    revision: u64,
    key: Option<Key>,
    timer: Option<Arc<ui_timer::Timer>>,
    pending: Option<Arc<RequestHandle>>,
    actions: Vec<(u64, CodeAction)>,
}
fn current(ctx: &AppContext, app: &AppState) -> Option<(Target, Key)> {
    let (pane, range) = app.editor.editor_diagnostic_hover.as_ref()?;
    if *pane != app.workspace.active_pane() {
        return None;
    }
    let t = target(app, *pane)?;
    let s = services(ctx)?;
    let found = s.results(&t.resource, &t.stamp).diagnostics?;
    if !found.iter().any(|d| &d.range == range) {
        return None;
    }
    let view = &app.workspace.view(t.view)?.source;
    let mut workspace = super::edit::stamps(app);
    workspace.sort_by(|a, b| a.0.cmp(&b.0));
    let key = Key {
        pane: *pane,
        view: t.view,
        input_generation: t.input_generation,
        stamp: t.stamp,
        range: range.clone(),
        cursor: t.cursor,
        scroll: (view.scroll_x.to_bits(), view.scroll_y.to_bits()),
        context: s.code_action_context(&t.resource, t.lsp_stamp()),
        workspace,
    };
    Some((t, key))
}
pub fn pause(app: &mut AppState) {
    let q = &mut app.editor.language.quickfix;
    q.revision = q.revision.wrapping_add(1);
    q.timer = None;
    q.pending = None;
    q.actions.clear();
}
pub fn observe(ctx: &AppContext, app: &mut AppState) {
    let now = current(ctx, app).map(|(_, key)| key);
    if now == app.editor.language.quickfix.key {
        return;
    }
    pause(app);
    app.editor.language.quickfix.key = now;
    if app.editor.language.save.is_some()
        || app.editor.language.quickfix.key.is_none()
        || app
            .editor
            .language
            .refactor
            .as_ref()
            .is_some_and(|p| matches!(p.wanted, refactor::Wanted::CodeActions))
        || app
            .editor
            .language
            .popup
            .as_ref()
            .is_some_and(|p| !p.kind.is_panel() || p.panel_focused)
    {
        return;
    }
    let Some(application) = ctx.services().and_then(|r| r.application()) else {
        return;
    };
    let revision = app.editor.language.quickfix.revision;
    let timed = ctx.clone();
    app.editor.language.quickfix.timer = Some(Arc::new(ui_timer::after(
        application,
        Duration::from_millis(200),
        move || {
            timed.update(|app| {
                let Some((t, key)) = current(&timed, app) else {
                    return;
                };
                if app.editor.language.quickfix.revision != revision
                    || app.editor.language.quickfix.key.as_ref() != Some(&key)
                    || app.editor.language.refactor.is_some()
                    || app.editor.language.save.is_some()
                    || app
                        .editor
                        .language
                        .popup
                        .as_ref()
                        .is_some_and(|p| !p.kind.is_panel() || p.panel_focused)
                {
                    return;
                }
                super::lightbulb::pause(app);
                let Some(s) = services(&timed) else { return };
                s.sync(app);
                app.editor.language.quickfix.timer = None;
                app.editor.language.quickfix.pending = Some(Arc::new(
                    s.request(
                        RequestSpec::new(
                            Feature::CodeAction,
                            t.resource.clone(),
                            t.lsp_stamp(),
                            key.range.start,
                        )
                        .range(key.range.end)
                        .extra(
                            serde_json::json!({"context":{"triggerKind":2,"only":["quickfix"]}}),
                        ),
                    ),
                ));
            });
        },
    )));
}
pub fn tick(ctx: &AppContext, app: &mut AppState) {
    let Some(pending) = app.editor.language.quickfix.pending.clone() else {
        return;
    };
    let progress = pending.poll();
    if !progress.finished {
        return;
    }
    let valid = current(ctx, app)
        .is_some_and(|(_, key)| app.editor.language.quickfix.key.as_ref() == Some(&key));
    let q = &mut app.editor.language.quickfix;
    q.pending = None;
    if !valid {
        return;
    }
    q.actions = progress
        .usable(Feature::CodeAction)
        .iter()
        .flat_map(|p| {
            reply::code_actions(&p.value, p.encoding)
                .into_iter()
                .map(move |a| (p.instance, a))
        })
        .filter(|(_, a)| {
            a.disabled.is_none()
                && a.kind
                    .as_ref()
                    .is_none_or(|k| k == "quickfix" || k.starts_with("quickfix."))
        })
        .collect();
    q.actions.sort_by_key(|(_, a)| !a.preferred);
}
pub fn view(
    ctx: &AppContext,
    app: &AppState,
    pane: PaneId,
) -> Option<crate::ui::editor_binding::QuickFixView> {
    let (_, key) = current(ctx, app)?;
    let q = &app.editor.language.quickfix;
    if key.pane != pane || q.key.as_ref() != Some(&key) || q.actions.is_empty() {
        return None;
    }
    let mut labels: Vec<_> = q
        .actions
        .iter()
        .take(5)
        .map(|(_, a)| a.title.clone())
        .collect();
    if q.actions.len() > 5 {
        labels.push("More quick fixes…".into());
    }
    Some(crate::ui::editor_binding::QuickFixView {
        revision: q.revision,
        range: key.range,
        labels,
    })
}
pub fn choose(
    ctx: &AppContext,
    pane: PaneId,
    revision: u64,
    index: usize,
    rect: lgui::prelude::UiRect,
) {
    let mut picked = None;
    ctx.update(|app| {
        if view(ctx, app, pane).is_some_and(|v| v.revision == revision) {
            if index == 5 && app.editor.language.quickfix.actions.len() > 5 {
                if let Some(mut t) = target(app, pane) {
                    t.cursor = app
                        .editor
                        .language
                        .quickfix
                        .key
                        .as_ref()
                        .unwrap()
                        .range
                        .start;
                    app.editor.language.popup =
                        refactor::action_popup(&t, app.editor.language.quickfix.actions.clone())
                            .map(|p| p.titled("Quick fixes"));
                }
            } else if index < 5 {
                picked = app.editor.language.quickfix.actions.get(index).cloned();
            }
            super::hover::clear(app);
            app.editor.editor_diagnostic_hover = None;
            pause(app);
        }
    });
    if let Some((instance, action)) = picked {
        refactor::run_action(ctx, instance, Arc::new(action), rect);
    }
}
