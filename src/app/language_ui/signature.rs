//! Signature help: the parameters of the call the caret is in, shown above it.
use super::{AppContext, AppState, services, target};
use crate::app::ui_timer;
use crate::domain::identity::{FileId, PaneId, ViewId};
use crate::features::language_service::reply::{self, SignatureHelp};
use crate::features::language_service::request::Feature;
use crate::features::markdown::content::Markup;
use crate::features::providers::{RequestHandle, RequestSpec};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

const DEBOUNCE: Duration = Duration::from_millis(20);

#[derive(Clone)]
pub struct Session {
    pub revision: u64,
    pub pane: PaneId,
    pub document: FileId,
    pub view: ViewId,
    pub input_generation: u64,
    pub incarnation: u64,
    /// Where the call's opening bracket is, as far as is known.
    pub opened_at: usize,
    handle: Option<Arc<RequestHandle>>,
    pub help: Option<SignatureHelp>,
    last_cursor: usize,
    last_version: u64,
    requested: crate::domain::document::DocumentStamp,
    requested_cursor: usize,
    source: Option<u64>,
    raw_help: Option<Value>,
    selected: Option<(usize, String)>,
}

#[derive(Clone, Copy)]
enum Trigger {
    Invoked,
    Character(char),
    ContentChange,
}

fn revision(app: &mut AppState) -> u64 {
    app.editor.language.signature_revision += 1;
    app.editor.language.signature_revision
}

fn current(app: &AppState, session: &Session) -> bool {
    target(app, app.workspace.active_pane()).is_some_and(|t| {
        t.pane == session.pane
            && t.document == session.document
            && t.view == session.view
            && t.input_generation == session.input_generation
            && t.stamp.incarnation == session.incarnation
            && t.stamp.language_generation == session.requested.language_generation
            && ((t.cursor == session.last_cursor && t.stamp.version == session.last_version)
                || inside_call(&t.text, session.opened_at, t.cursor))
    })
}

pub fn can_step(app: &AppState) -> bool {
    app.editor
        .language
        .signature
        .as_ref()
        .is_some_and(|session| {
            current(app, session)
                && session
                    .help
                    .as_ref()
                    .is_some_and(|help| help.signatures.len() > 1)
        })
}

/// Changes only the local selection. A button from an obsolete card is ignored.
pub fn step(app: &mut AppState, forward: bool, expected: Option<u64>) -> bool {
    if !can_step(app) {
        return false;
    }
    let session = app.editor.language.signature.as_mut().unwrap();
    if expected.is_some_and(|value| value != session.revision) {
        return false;
    }
    let help = session.help.as_mut().unwrap();
    let total = help.signatures.len();
    help.active_signature = (help.active_signature + if forward { 1 } else { total - 1 }) % total;
    session.selected = Some((
        help.active_signature,
        help.signatures[help.active_signature].label.clone(),
    ));
    true
}

/// What to draw.
#[derive(Clone)]
pub struct View {
    pub anchor: usize,
    pub label: String,
    /// The active parameter, as character offsets into the label.
    pub active: Option<std::ops::Range<usize>>,
    pub docs: Option<Markup>,
    /// Which signature of how many, when there are several.
    pub position: Option<(usize, usize)>,
}

pub fn view(session: &Session) -> Option<View> {
    let help = session.help.as_ref()?;
    let signature = help.signatures.get(help.active_signature)?;
    let parameter = signature
        .active_parameter
        .or(help.active_parameter)
        .and_then(|index| {
            signature
                .parameters
                .get(index)
                .or_else(|| signature.parameters.first())
        })
        .or_else(|| signature.parameters.first());
    let active = parameter.map(|parameter| parameter.label.clone());
    let mut parts = Vec::new();
    if let Some(parameter) = parameter.and_then(|parameter| parameter.documentation.as_ref()) {
        parts.push(parameter.clone());
    }
    if let Some(documentation) = &signature.documentation {
        parts.push(documentation.clone());
    }
    Some(View {
        anchor: session.opened_at,
        label: signature.label.clone(),
        active,
        docs: Markup::join(&parts),
        position: (help.signatures.len() > 1)
            .then_some((help.active_signature + 1, help.signatures.len())),
    })
}

/// Which brackets trigger signature help, from the servers that offer it.
fn triggers(
    ctx: &AppContext,
    path: &loog_workspace_api::ResourceRef,
) -> (Vec<String>, Vec<String>) {
    let mut open = Vec::new();
    let mut again = Vec::new();
    if let Some(services) = services(ctx) {
        for info in services.capability_sources(Some(path)) {
            if let Some(options) = info.providers.signature_help.as_ref() {
                open.extend(options.trigger_characters.clone());
                again.extend(options.retrigger_characters.clone());
            }
        }
    }
    (open, again)
}

/// Watches typing for the characters that open or refresh the help, and ends it
/// when the caret leaves the call.
pub fn observe(ctx: &AppContext, app: &mut AppState, typed: Option<char>) {
    let pane = app.workspace.active_pane();
    let Some(t) = target(app, pane) else {
        if app.editor.language.signature.is_some() || app.editor.language.signature_timer.is_some()
        {
            close(app);
        }
        return;
    };
    let mut keep = true;
    let mut changed = false;
    if let Some(session) = app.editor.language.signature.as_mut() {
        if session.pane != pane
            || session.document != t.document
            || session.view != t.view
            || session.input_generation != t.input_generation
            || session.incarnation != t.stamp.incarnation
            || session.requested.language_generation != t.stamp.language_generation
        {
            keep = false;
        } else if session.last_version != t.stamp.version || session.last_cursor != t.cursor {
            changed = true;
            session.last_version = t.stamp.version;
            session.last_cursor = t.cursor;
            keep = inside_call(&t.text, session.opened_at, t.cursor);
        }
    }
    if !keep {
        close(app);
    }
    let (open, again) = triggers(ctx, &t.resource);
    let opens = typed.is_some_and(|c| open.iter().any(|s| s.chars().eq([c])));
    let refresh = app.editor.language.signature.is_some()
        && typed.is_some_and(|c| again.iter().any(|s| s.chars().eq([c])));
    if opens || refresh {
        let c = typed.unwrap();
        schedule(
            ctx,
            app,
            if opens && (app.editor.language.signature.is_none() || matches!(c, '(' | '[' | '<')) {
                Some(t.cursor.saturating_sub(c.len_utf8()))
            } else {
                None
            },
            Trigger::Character(c),
        );
    } else if changed && keep && app.editor.language.signature.is_some() {
        schedule(ctx, app, None, Trigger::ContentChange);
    }
}

/// Whether `cursor` is still inside the call opened at `opened_at`.
pub fn inside_call(text: &str, opened_at: usize, cursor: usize) -> bool {
    if cursor <= opened_at
        || cursor > text.len()
        || opened_at >= text.len()
        || !text.is_char_boundary(opened_at)
        || !text.is_char_boundary(cursor)
    {
        return false;
    }
    let (open, close) = match text[opened_at..].chars().next() {
        Some('(') => ('(', ')'),
        Some('<') => ('<', '>'),
        Some('[') => ('[', ']'),
        _ => return true,
    };
    let mut depth = 0i32;
    for c in text[opened_at..cursor].chars() {
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth <= 0 {
                return false;
            }
        }
    }
    true
}

fn schedule(ctx: &AppContext, app: &mut AppState, opened_at: Option<usize>, trigger: Trigger) {
    let Some(application) = ctx.services().and_then(|r| r.application()) else {
        return;
    };
    let timed = ctx.clone();
    let Some(expected) = target(app, app.workspace.active_pane()) else {
        return;
    };
    let scheduled = revision(app);
    app.editor.language.signature_timer = Some(Arc::new(ui_timer::after(
        application,
        DEBOUNCE,
        move || {
            timed.try_update(|app| {
                if app.editor.language.signature_revision != scheduled
                    || !target(app, app.workspace.active_pane()).is_some_and(|t| {
                        t.pane == expected.pane
                            && t.document == expected.document
                            && t.view == expected.view
                            && t.input_generation == expected.input_generation
                            && t.stamp == expected.stamp
                            && t.cursor == expected.cursor
                    })
                {
                    return false;
                }
                request_with_trigger(&timed, app, opened_at, trigger);
                true
            });
        },
    )));
}

/// Asks the servers now.
pub fn request(ctx: &AppContext, app: &mut AppState, opened_at: Option<usize>, manual: bool) {
    request_with_trigger(
        ctx,
        app,
        opened_at,
        if manual {
            Trigger::Invoked
        } else {
            Trigger::ContentChange
        },
    );
}

fn request_with_trigger(
    ctx: &AppContext,
    app: &mut AppState,
    opened_at: Option<usize>,
    trigger: Trigger,
) {
    app.editor.language.signature_timer = None;
    let pane = app.workspace.active_pane();
    let Some(t) = target(app, pane) else { return };
    let Some(services) = services(ctx) else {
        return;
    };
    if app
        .editor
        .language
        .signature
        .as_ref()
        .is_some_and(|session| {
            !current(app, session) || opened_at.is_some_and(|at| at != session.opened_at)
        })
    {
        app.editor.language.signature = None;
    }
    let mut context = json!({ "triggerKind": match trigger { Trigger::Invoked => 1, Trigger::Character(_) => 2, Trigger::ContentChange => 3 }, "isRetrigger": false });
    if let Trigger::Character(c) = trigger {
        context["triggerCharacter"] = json!(c.to_string());
    }
    let session = app.editor.language.signature.as_ref();
    let source = session.and_then(|s| s.source);
    if let Some(session) = session {
        if let (Some(help), Some(raw)) = (&session.help, &session.raw_help) {
            let mut raw = raw.clone();
            raw["activeSignature"] = json!(help.active_signature);
            context["activeSignatureHelp"] = raw;
            context["isRetrigger"] = json!(true);
        }
    }
    services.sync(app);
    let mut spec = RequestSpec::new(
        Feature::SignatureHelp,
        t.resource.clone(),
        t.lsp_stamp(),
        t.cursor,
    )
    .extra(json!({ "context": context }));
    if let Some(source) = source {
        spec = spec.target(source);
    }
    let handle = Arc::new(services.request(spec));
    let next = revision(app);
    match app.editor.language.signature.as_mut() {
        Some(session) => {
            session.handle = Some(handle);
            session.last_cursor = t.cursor;
            session.last_version = t.stamp.version;
            session.requested = t.stamp;
            session.requested_cursor = t.cursor;
            session.revision = next;
        }
        None => {
            app.editor.language.signature = Some(Session {
                revision: next,
                pane,
                document: t.document,
                view: t.view,
                input_generation: t.input_generation,
                incarnation: t.stamp.incarnation,
                opened_at: opened_at.unwrap_or_else(|| open_bracket_before(&t.text, t.cursor)),
                handle: Some(handle),
                help: None,
                last_cursor: t.cursor,
                last_version: t.stamp.version,
                requested: t.stamp,
                requested_cursor: t.cursor,
                source: None,
                raw_help: None,
                selected: None,
            })
        }
    }
}

/// The innermost unclosed bracket before `cursor`, or `cursor` itself.
pub fn open_bracket_before(text: &str, cursor: usize) -> usize {
    let mut depth = 0i32;
    for (at, c) in text[..cursor.min(text.len())].char_indices().rev() {
        match c {
            ')' => depth += 1,
            '(' => {
                if depth == 0 {
                    return at;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    cursor
}

/// Folds in the servers' answer.
pub fn tick(app: &mut AppState) -> bool {
    let Some(mut session) = app.editor.language.signature.take() else {
        return false;
    };
    if !current(app, &session) {
        close(app);
        return true;
    }
    let Some(handle) = session.handle.clone() else {
        app.editor.language.signature = Some(session);
        return false;
    };
    let t = target(app, session.pane).unwrap();
    if session.requested != t.stamp || session.requested_cursor != t.cursor {
        session.handle = None;
        app.editor.language.signature = Some(session);
        return true;
    }
    let progress = handle.poll();
    if !progress.finished {
        app.editor.language.signature = Some(session);
        return false;
    }
    session.handle = None;
    let part = progress.usable(Feature::SignatureHelp).first().cloned();
    if let Some((part, mut help)) =
        part.and_then(|part| reply::signature_help(&part.value).map(|help| (part, help)))
    {
        if session.source == Some(part.instance) {
            if let Some((old, label)) = &session.selected {
                let chosen = help
                    .signatures
                    .get(*old)
                    .filter(|s| s.label == *label)
                    .map(|_| *old)
                    .or_else(|| help.signatures.iter().position(|s| s.label == *label));
                if let Some(index) = chosen {
                    help.active_signature = index;
                    session.selected = Some((index, label.clone()));
                } else {
                    session.selected = None;
                }
            }
        } else {
            session.selected = None;
        }
        let mut raw = part.value.clone();
        // The parser discards signatures without a label; keep wire indexes aligned.
        if let Some(signatures) = raw["signatures"].as_array_mut() {
            signatures.retain(|s| s["label"].is_string());
        }
        session.source = Some(part.instance);
        session.raw_help = Some(raw);
        session.help = Some(help);
        session.revision = revision(app);
        app.editor.language.signature = Some(session);
    } else {
        close(app);
    }
    true
}

pub fn close(app: &mut AppState) {
    revision(app);
    app.editor.language.signature = None;
    app.editor.language.signature_timer = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_call_ends_at_its_closing_bracket() {
        let text = "f(a, g(b), c) + 1";
        assert!(inside_call(text, 1, 5));
        assert!(inside_call(text, 1, 11), "after a nested call closed");
        assert!(!inside_call(text, 1, 13), "after the closing bracket");
        assert!(!inside_call(text, 1, 1), "before anything was typed");
        assert!(inside_call(text, 6, 8), "the nested call itself");
    }

    #[test]
    fn stale_positions_end_the_call_instead_of_panicking() {
        // The text changed under the remembered position.
        assert!(!inside_call("é(a", 1, 3), "inside a character");
        assert!(!inside_call("(a", 5, 7));
        assert!(!inside_call("(a", 0, 9));
        assert!(!inside_call("é", 0, 1));
    }

    #[test]
    fn the_enclosing_open_bracket_skips_closed_calls() {
        let text = "f(a, g(b), ";
        assert_eq!(open_bracket_before(text, text.len()), 1);
        assert_eq!(open_bracket_before("f(a, g(b", 8), 6);
        assert_eq!(open_bracket_before("no brackets", 5), 5);
    }
}
