//! Saving with the language servers' help: organise imports and format first,
//! when the language's settings ask for it, then write the file. A server that
//! is slow or silent never keeps the file from being saved.
use super::edit::{self, Request};
use super::refactor::single_document_edit;
use super::{AppContext, AppState, services, target};
use crate::app::ui_timer;
use crate::domain::document::DocumentStamp;
use crate::domain::identity::{FileId, PaneId};
use crate::features::language_service::reply;
use crate::features::language_service::request::Feature;
use crate::features::language_service::uri;
use crate::features::providers::{RequestHandle, RequestSpec};
use lgui::prelude::UiRect;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long each server step may take before the file is saved anyway.
const STEP: Duration = Duration::from_secs(1);

#[cfg(test)]
thread_local! {
    static TEST_STEP: std::cell::Cell<Duration> = const { std::cell::Cell::new(STEP) };
}

fn step_budget() -> Duration {
    #[cfg(test)]
    {
        TEST_STEP.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        STEP
    }
}

/// UI correctness tests can allow scheduling slack without changing production
/// deadlines or the dedicated timeout tests. The guard stays on the UI thread.
#[cfg(test)]
pub(crate) fn test_step_budget(step: Duration) -> impl Drop {
    struct Reset(Duration, std::marker::PhantomData<std::rc::Rc<()>>);
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_STEP.with(|value| value.set(self.0));
        }
    }
    Reset(
        TEST_STEP.with(|value| value.replace(step)),
        std::marker::PhantomData,
    )
}

/// What to do once the steps are over (normally: write the file).
pub type Then = Arc<dyn Fn(&AppContext) + Send + Sync>;
type Loaded = HashMap<std::path::PathBuf, String>;
type Loading = Arc<std::sync::Mutex<Option<Result<Loaded, String>>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Actions,
    Resolve,
    Loading,
    Command,
    Format,
}

#[derive(Clone)]
pub struct Run {
    id: u64,
    stage: Stage,
    pane: PaneId,
    document: FileId,
    view: crate::domain::identity::ViewId,
    input_generation: u64,
    stamp: DocumentStamp,
    handle: Option<Arc<RequestHandle>>,
    timer: Option<Arc<ui_timer::Timer>>,
    then: Then,
    rect: UiRect,
    kinds: Vec<String>,
    format: Option<String>,
    do_format: bool,
    kind: usize,
    action: Option<(u64, reply::CodeAction)>,
    deadline: Instant,
    stamps: Vec<(std::path::PathBuf, DocumentStamp)>,
    loading: Option<Loading>,
}

/// What the language's settings ask for before saving.
struct Wanted {
    kinds: Vec<String>,
    format: bool,
    formatter: Option<String>,
}

fn wanted(app: &AppState) -> Option<Wanted> {
    let pane = app.workspace.active_pane();
    let t = target(app, pane)?;
    let name = app
        .workspace
        .documents
        .get(&t.document)?
        .meta
        .lang
        .name()
        .to_owned();
    let setting = app.preferences.values().languages.get(&name)?;
    (setting.format_on_save || !setting.code_actions_on_save.is_empty()).then(|| Wanted {
        kinds: setting.code_actions_on_save.clone(),
        format: setting.format_on_save,
        formatter: None,
    })
}

/// Saves the active document after the steps its language asks for.
pub fn save(state: &AppContext, rect: UiRect, then: Then) {
    let app = state.get();
    let Some(wanted) = wanted(&app) else {
        then(state);
        return;
    };
    let pane = app.workspace.active_pane();
    let Some(t) = target(&app, pane) else {
        then(state);
        return;
    };
    let Some(services) = services(state) else {
        then(state);
        return;
    };
    drop(app);
    let run = Run {
        id: 0,
        stage: if wanted.kinds.is_empty() {
            Stage::Format
        } else {
            Stage::Actions
        },
        pane,
        document: t.document,
        view: t.view,
        input_generation: t.input_generation,
        stamp: t.stamp,
        handle: None,
        timer: None,
        then: then.clone(),
        rect,
        kinds: wanted.kinds,
        format: wanted.formatter,
        do_format: wanted.format,
        kind: 0,
        action: None,
        deadline: Instant::now() + step_budget() * 2,
        stamps: Vec::new(),
        loading: None,
    };
    let _ = services;
    let mut started = false;
    state.update(|app| {
        super::lightbulb::pause(app);
        super::quickfix::pause(app);
        app.editor.language.save_counter += 1;
        let mut run = run;
        run.id = app.editor.language.save_counter;
        started = begin_stage(state, app, run);
    });
    if !started {
        then(state);
    }
}

/// Sends the request of the run's stage. False when there is nothing to ask.
fn begin_stage(state: &AppContext, app: &mut AppState, mut run: Run) -> bool {
    let remaining = run
        .deadline
        .saturating_duration_since(Instant::now())
        .min(step_budget());
    if remaining.is_zero() {
        return false;
    }
    let Some(t) = target(app, run.pane).filter(|t| {
        t.view == run.view && t.input_generation == run.input_generation && t.stamp == run.stamp
    }) else {
        return false;
    };
    let Some(services) = services(state) else {
        return false;
    };
    services.sync(app);
    let spec = match run.stage {
        Stage::Loading => return false,
        Stage::Actions => RequestSpec::new(Feature::CodeAction, t.path.clone(), t.lsp_stamp(), 0)
            .range(t.text.len())
            .extra(json!({ "context": { "triggerKind":1, "only": [run.kinds[run.kind]] } })),
        Stage::Resolve => {
            let Some((instance, action)) = &run.action else {
                return false;
            };
            RequestSpec::new(Feature::CodeActionResolve, t.path.clone(), t.lsp_stamp(), 0)
                .target(*instance)
                .extra(action.raw.clone())
        }
        Stage::Command => {
            let Some((instance, action)) = &run.action else {
                return false;
            };
            let Some(command) = &action.command else {
                return false;
            };
            RequestSpec::new(Feature::ExecuteCommand, t.path.clone(), t.lsp_stamp(), 0)
                .target(*instance)
                .extra(json!({"command":command.command,"arguments":command.arguments}))
        }
        Stage::Format => {
            let mut spec = RequestSpec::new(Feature::Formatting, t.path.clone(), t.lsp_stamp(), 0)
                .extra(json!({ "options": super::indentation(app, t.document) }));
            if let Some(name) = &run.format {
                spec = spec.server(name.clone());
            }
            spec
        }
    };
    run.stamps = edit::stamps(app);
    run.handle = Some(Arc::new(services.request(spec.timeout(remaining))));
    app.editor.language.save_counter = app.editor.language.save_counter.wrapping_add(1);
    run.id = app.editor.language.save_counter;
    let id = run.id;
    let timed = state.clone();
    if let Some(application) = state.services().and_then(|r| r.application()) {
        run.timer = Some(Arc::new(ui_timer::after(
            application,
            remaining + Duration::from_millis(50),
            move || expire(&timed, id),
        )));
    }
    app.editor.language.save = Some(run);
    true
}

/// A step took too long: save without it.
fn expire(state: &AppContext, id: u64) {
    let mut then = None;
    state.update(|app| {
        if app
            .editor
            .language
            .save
            .as_ref()
            .is_some_and(|r| r.id == id)
        {
            then = app.editor.language.save.take().map(|r| r.then);
        }
    });
    if let Some(then) = then {
        then(state);
    }
}

/// Applies a finished step and moves on. Follow-up work must run after the UI
/// has delivered queued server actions, including edits from a completed command.
pub fn tick(state: &AppContext, app: &mut AppState) -> Option<Then> {
    let mut run = app.editor.language.save.clone()?;
    if run.stage == Stage::Loading {
        let loaded = run.loading.as_ref()?.lock().unwrap().take()?;
        app.editor.language.save = None;
        if target(app, run.pane).is_none_or(|t| {
            t.view != run.view || t.input_generation != run.input_generation || t.stamp != run.stamp
        }) {
            return Some(run.then);
        }
        return match loaded {
            Ok(loaded) => apply_loaded(state, app, run, loaded),
            Err(error) => {
                app.show_toast(format!("Save: {error}"));
                next_kind(state, app, run)
            }
        };
    }
    let progress = run.handle.as_ref()?.poll();
    if !progress.finished {
        return None;
    }
    app.editor.language.save = None;
    let Some(t) = target(app, run.pane).filter(|t| {
        t.document == run.document
            && (t.view == run.view
                && t.input_generation == run.input_generation
                && t.stamp == run.stamp
                || run.stage == Stage::Command
                    && t.stamp.incarnation == run.stamp.incarnation
                    && t.stamp.language_generation == run.stamp.language_generation)
    }) else {
        return Some(run.then);
    };
    match run.stage {
        Stage::Loading => unreachable!(),
        Stage::Actions => {
            let kind = &run.kinds[run.kind];
            let mut actions: Vec<_> = progress
                .parts
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
                            .is_some_and(|k| k == kind || k.starts_with(&format!("{kind}.")))
                })
                .collect();
            actions.sort_by_key(|(_, a)| !a.preferred);
            run.action = actions.into_iter().next();
            if run.action.as_ref().is_some_and(|(_, a)| a.needs_resolve) {
                run.stage = Stage::Resolve;
                if begin_stage(state, app, run.clone()) {
                    return None;
                }
            } else if run.action.is_some() {
                return apply_action(state, app, run);
            }
        }
        Stage::Resolve => {
            if let Some(part) = progress.parts.first()
                && let Some(action) =
                    reply::code_actions(&json!([part.value.clone()]), part.encoding)
                        .pop()
                        .filter(|a| a.disabled.is_none())
            {
                let instance = run.action.as_ref()?.0;
                run.action = Some((instance, action));
                return apply_action(state, app, run);
            }
        }
        Stage::Command => {
            // The command reply and its applyEdit can share one UI notification.
            // tick() delivers server actions before invoking this continuation.
            return Some(Arc::new(move |state| {
                let mut then = None;
                state.update(|app| then = next_kind(state, app, run.clone()));
                if let Some(then) = then {
                    then(state);
                }
            }));
        }
        Stage::Format => {
            if let (Some(part), Some(link)) = (
                progress.usable(Feature::Formatting).first(),
                uri::file_uri(&t.path),
            ) {
                let edits = reply::text_edits(&part.value, part.encoding);
                if !edits.is_empty() {
                    let request = Request {
                        edit: single_document_edit(link, edits),
                        stamps: run.stamps.clone(),
                        rect: run.rect,
                        title: "Save".into(),
                        reply: None,
                    };
                    if let Err(error) = edit::apply_now(app, &request, HashMap::new()) {
                        app.show_toast(format!("Save: {error}"));
                    }
                }
            }
            return Some(run.then);
        }
    }
    next_kind(state, app, run)
}

fn apply_action(state: &AppContext, app: &mut AppState, run: Run) -> Option<Then> {
    if let Some(edit) = run.action.as_ref().and_then(|(_, a)| a.edit.as_ref()) {
        match edit::unopened(app, edit) {
            Ok(paths) if !paths.is_empty() => return load_action(state, app, run, paths),
            Ok(_) => {}
            Err(error) => {
                app.show_toast(format!("Save: {error}"));
                return next_kind(state, app, run);
            }
        }
    }
    apply_loaded(state, app, run, HashMap::new())
}

fn load_action(
    state: &AppContext,
    app: &mut AppState,
    mut run: Run,
    paths: Vec<std::path::PathBuf>,
) -> Option<Then> {
    let remaining = run
        .deadline
        .saturating_duration_since(Instant::now())
        .min(step_budget());
    let Some(application) = state
        .services()
        .and_then(|r| r.application())
        .filter(|_| !remaining.is_zero())
    else {
        return Some(run.then);
    };
    app.editor.language.save_counter = app.editor.language.save_counter.wrapping_add(1);
    run.id = app.editor.language.save_counter;
    run.stage = Stage::Loading;
    run.handle = None;
    let loading: Loading = Default::default();
    run.loading = Some(loading.clone());
    let id = run.id;
    let timed = state.clone();
    run.timer = Some(Arc::new(ui_timer::after(
        application.clone(),
        remaining,
        move || expire(&timed, id),
    )));
    let next = state.clone();
    let then = run.then.clone();
    app.editor.language.save = Some(run);
    if std::thread::Builder::new()
        .name("loog-save-read".into())
        .spawn(move || {
            *loading.lock().unwrap() = Some(edit::load(paths));
            application.post(move || {
                if next
                    .get()
                    .editor
                    .language
                    .save
                    .as_ref()
                    .is_some_and(|r| r.id == id)
                {
                    super::tick(&next);
                }
            });
        })
        .is_err()
    {
        app.editor.language.save = None;
        app.show_toast("Save: could not start reading the files");
        return Some(then);
    }
    None
}

fn apply_loaded(
    state: &AppContext,
    app: &mut AppState,
    mut run: Run,
    loaded: Loaded,
) -> Option<Then> {
    let (_, action) = run.action.as_ref()?;
    if let Some(edit) = &action.edit {
        let request = Request {
            edit: edit.clone(),
            stamps: run.stamps.clone(),
            rect: run.rect,
            title: "Save".into(),
            reply: None,
        };
        if let Err(error) = edit::apply_now(app, &request, loaded) {
            app.show_toast(format!("Save: {error}"));
            return next_kind(state, app, run);
        }
    }
    if action.command.is_some() {
        run.stage = Stage::Command;
        run.stamp = target(app, run.pane).map_or(run.stamp, |t| t.stamp);
        if begin_stage(state, app, run.clone()) {
            return None;
        }
    }
    next_kind(state, app, run)
}
fn next_kind(state: &AppContext, app: &mut AppState, mut run: Run) -> Option<Then> {
    run.kind += 1;
    run.action = None;
    run.stamp = target(app, run.pane).map_or(run.stamp, |t| t.stamp);
    if run.kind < run.kinds.len() {
        run.stage = Stage::Actions;
    } else if run.do_format {
        run.stage = Stage::Format;
    } else {
        return Some(run.then);
    }
    if begin_stage(state, app, run.clone()) {
        None
    } else {
        Some(run.then)
    }
}
