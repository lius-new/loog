//! Commands that change code on a server's advice: rename, code actions,
//! formatting.
use super::edit::{self, Request};
use super::hover::word_at;
use super::popup::{Item, Kind, Payload, Popup};
use super::{AppContext, AppState, Target, services, target};
use crate::domain::document::DocumentStamp;
use crate::features::language_service::reply::{
    self, Change, CodeAction, Command, DocumentEdit, TextEdit, WorkspaceEdit,
};
use crate::features::language_service::request::Feature;
use crate::features::language_service::uri;
use crate::features::providers::{Failure, RequestHandle, RequestSpec};
use lgui::prelude::UiRect;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

/// What the answer is wanted for.
#[derive(Clone)]
pub enum Wanted {
    PrepareRename,
    Rename(String),
    CodeActions,
    /// An action that arrived without its edit.
    ResolveAction {
        instance: u64,
        title: String,
    },
    Format,
    FormatSelection,
    /// Formatting triggered by a typed character, applied quietly.
    OnType,
    Command {
        title: String,
    },
}

#[derive(Clone)]
pub struct Pending {
    pub wanted: Wanted,
    pub handle: Arc<RequestHandle>,
    pub target: Target,
    pub stamps: Vec<(PathBuf, DocumentStamp)>,
    pub rect: UiRect,
}

/// Something to do once the state update that found it out is over.
pub enum Effect {
    Apply(Request),
    ApplyThen {
        request: Request,
        instance: u64,
        command: Command,
    },
    /// Run a server command: after a code action, or from a lens.
    Execute {
        instance: u64,
        command: Command,
        rect: UiRect,
    },
}

fn start(
    state: &AppContext,
    wanted: Wanted,
    feature: Feature,
    rect: UiRect,
    extra: serde_json::Value,
    range: (usize, usize),
    target_instance: Option<u64>,
) {
    let app = state.get();
    let pane = app.workspace.active_pane();
    let Some(target) = target(&app, pane) else {
        return;
    };
    let Some(services) = services(state) else {
        return;
    };
    services.sync(&app);
    let mut spec = RequestSpec::new(feature, target.path.clone(), target.lsp_stamp(), range.0)
        .range(range.1)
        .extra(extra);
    if let Some(instance) = target_instance {
        spec = spec.target(instance);
    }
    let handle = Arc::new(services.request(spec));
    let stamps = edit::stamps(&app);
    drop(app);
    state.update(|app| {
        if matches!(wanted, Wanted::CodeActions) {
            super::lightbulb::pause(app);
            super::quickfix::pause(app);
        }
        app.editor.language.refactor = Some(Pending {
            wanted,
            handle,
            target,
            stamps,
            rect,
        });
    });
}

// ---- rename ----------------------------------------------------------------

/// F2: ask where the symbol under the caret starts and ends, then let the person
/// type the new name.
pub fn begin_rename(state: &AppContext, rect: UiRect) {
    let app = state.get();
    let pane = app.workspace.active_pane();
    let Some(t) = target(&app, pane) else { return };
    drop(app);
    let (can_rename, can_prepare) = services(state)
        .map(|s| {
            let infos = s.capability_sources(Some(&t.path));
            let providers = || infos.iter().map(|i| &i.providers);
            (
                providers().any(|p| p.rename.is_some()),
                providers().any(|p| p.rename == Some(true)),
            )
        })
        .unwrap_or_default();
    if !can_rename {
        state.update(|app| app.show_toast("No language server here can rename"));
        return;
    }
    if !can_prepare {
        // Nobody can say what is renameable: offer the word under the caret.
        state.update(|app| {
            let Some(word) = word_at(&t.text, t.cursor) else {
                app.show_toast("Nothing here can be renamed");
                return;
            };
            let name = t.text[word.clone()].to_owned();
            app.editor.language.popup = Some(
                Popup::new(Kind::Rename, t.pane, t.stamp, Some(word.start), Vec::new())
                    .with_origin(t.cursor)
                    .with_text(name)
                    .titled("Rename"),
            );
        });
        return;
    }
    start(
        state,
        Wanted::PrepareRename,
        Feature::PrepareRename,
        rect,
        serde_json::Value::Null,
        (t.cursor, t.cursor),
        None,
    );
}

/// Enter in the rename box: ask the server for the edit.
pub fn commit_rename(state: &AppContext, rect: UiRect) {
    let mut chosen: Option<(String, usize)> = None;
    state.update(|app| {
        if let Some(popup) = app
            .editor
            .language
            .popup
            .take()
            .filter(|p| p.kind == Kind::Rename)
        {
            chosen = Some((popup.query.unwrap_or_default(), popup.origin));
        }
    });
    let Some((name, origin)) = chosen.filter(|(name, _)| !name.trim().is_empty()) else {
        return;
    };
    start(
        state,
        Wanted::Rename(name.trim().to_owned()),
        Feature::Rename,
        rect,
        json!({ "newName": name.trim() }),
        (origin, origin),
        None,
    );
}

// ---- code actions ----------------------------------------------------------

/// Ctrl+.: the actions the servers offer for the selection or the caret.
pub fn code_actions(state: &AppContext, rect: UiRect) {
    let app = state.get();
    let pane = app.workspace.active_pane();
    let Some(t) = target(&app, pane) else { return };
    drop(app);
    let range = t
        .selection
        .as_ref()
        .map_or((t.cursor, t.cursor), |r| (r.start, r.end));
    start(
        state,
        Wanted::CodeActions,
        Feature::CodeAction,
        rect,
        json!({ "context": { "triggerKind": 1 } }),
        range,
        None,
    );
}

/// Chooses a code action from the popup.
pub fn run_action(state: &AppContext, instance: u64, action: Arc<CodeAction>, rect: UiRect) {
    if action.disabled.is_some() {
        return;
    }
    if action.needs_resolve {
        start(
            state,
            Wanted::ResolveAction {
                instance,
                title: action.title.clone(),
            },
            Feature::CodeActionResolve,
            rect,
            action.raw.clone(),
            (0, 0),
            Some(instance),
        );
        return;
    }
    let app = state.get();
    let stamps = edit::stamps(&app);
    drop(app);
    if let Some(edit) = &action.edit {
        let request = Request {
            edit: edit.clone(),
            stamps,
            rect,
            title: action.title.clone(),
            reply: None,
        };
        if let Some(command) = &action.command {
            edit::apply_then(state, request, instance, command.clone());
        } else {
            edit::apply(state, request);
        }
    } else if let Some(command) = &action.command {
        execute(state, instance, command.clone(), rect);
    }
}

/// Asks the server to run one of its commands.
pub fn execute(state: &AppContext, instance: u64, command: Command, rect: UiRect) {
    start(
        state,
        Wanted::Command {
            title: command.title.clone(),
        },
        Feature::ExecuteCommand,
        rect,
        json!({ "command": command.command, "arguments": command.arguments }),
        (0, 0),
        Some(instance),
    );
}

// ---- formatting ------------------------------------------------------------

pub fn format(state: &AppContext, rect: UiRect, selection_only: bool) {
    let app = state.get();
    let pane = app.workspace.active_pane();
    let Some(t) = target(&app, pane) else { return };
    let options = json!({ "options": super::indentation(&app, t.document) });
    drop(app);
    match (selection_only, t.selection.clone()) {
        (true, Some(range)) => start(
            state,
            Wanted::FormatSelection,
            Feature::RangeFormatting,
            rect,
            options,
            (range.start, range.end),
            None,
        ),
        _ => start(
            state,
            Wanted::Format,
            Feature::Formatting,
            rect,
            options,
            (0, 0),
            None,
        ),
    }
}

/// A typed character may make the server want to format (`}` or `;` for example).
pub fn on_type(ctx: &AppContext, app: &mut AppState, typed: char) {
    let Some(services) = services(ctx) else {
        return;
    };
    let wanted = services
        .capability_sources(
            app.workspace
                .active_document()
                .and_then(|id| app.workspace.documents.get(&id))
                .map(|d| d.meta.path.as_path()),
        )
        .iter()
        .any(|info| {
            info.providers.on_type_formatting.as_ref().is_some_and(|f| {
                f.first.chars().eq([typed]) || f.more.iter().any(|m| m.chars().eq([typed]))
            })
        });
    if !wanted {
        return;
    }
    let pane = app.workspace.active_pane();
    let Some(t) = target(app, pane) else { return };
    services.sync(app);
    let handle = Arc::new(
        services.request(
            RequestSpec::new(
                Feature::OnTypeFormatting,
                t.path.clone(),
                t.lsp_stamp(),
                t.cursor,
            )
            .extra(json!({ "ch": typed.to_string(),
                           "options": super::indentation(app, t.document) })),
        ),
    );
    let stamps = edit::stamps(app);
    let rect = app
        .editor
        .language
        .rect
        .unwrap_or(UiRect::new(0.0, 0.0, 800.0, 600.0));
    app.editor.language.refactor = Some(Pending {
        wanted: Wanted::OnType,
        handle,
        target: t,
        stamps,
        rect,
    });
}

// ---- answers ---------------------------------------------------------------

fn failure_text(what: &str, failure: Option<&Failure>) -> String {
    match failure {
        Some(Failure::Unsupported) => format!("No language server here can {what}"),
        Some(Failure::Stale) => "The text changed; try again".to_owned(),
        Some(Failure::Busy) => "The language servers are busy".to_owned(),
        Some(Failure::Failed(reason)) => format!("The language server failed: {reason}"),
        None => format!("Nothing to {what}"),
    }
}

/// Takes the answer of the pending request when it is complete.
pub fn tick(app: &mut AppState) -> Vec<Effect> {
    let Some(pending) = app.editor.language.refactor.clone() else {
        return Vec::new();
    };
    let progress = pending.handle.poll();
    if !progress.finished {
        return Vec::new();
    }
    app.editor.language.refactor = None;
    let target = &pending.target;
    let same_document = super::target(app, target.pane).is_some_and(|now| now.same_view(target))
        && app.workspace.active_pane() == target.pane
        && app.workspace.pane(target.pane).and_then(|p| p.active()) == Some(target.view);
    let mut effects = Vec::new();
    match &pending.wanted {
        Wanted::PrepareRename => {
            if !same_document {
                return effects;
            }
            let found = progress
                .usable(Feature::PrepareRename)
                .first()
                .and_then(|part| reply::prepare_rename(&part.value, part.encoding));
            // A server that cannot prepare, or says the position is renameable
            // without a range, gets the word under the caret.
            let unsupported = matches!(progress.failure, Some(Failure::Unsupported));
            let word = word_at(&target.text, target.cursor);
            let (range, placeholder) = match (&found, unsupported) {
                (Some(found), _) => {
                    let range = found
                        .span
                        .map(|span| span.bytes(&target.text))
                        .or_else(|| word.clone());
                    let placeholder = found
                        .placeholder
                        .clone()
                        .or_else(|| range.as_ref().map(|r| target.text[r.clone()].to_owned()));
                    (range, placeholder)
                }
                (None, true) => (
                    word.clone(),
                    word.as_ref().map(|r| target.text[r.clone()].to_owned()),
                ),
                (None, false) => (None, None),
            };
            match (range, placeholder) {
                (Some(range), Some(placeholder)) => {
                    app.editor.language.popup = Some(
                        Popup::new(
                            Kind::Rename,
                            target.pane,
                            target.stamp,
                            Some(range.start),
                            Vec::new(),
                        )
                        .with_origin(target.cursor)
                        .with_text(placeholder)
                        .titled("Rename"),
                    );
                }
                _ => app.show_toast("Nothing here can be renamed"),
            }
        }
        Wanted::Rename(_) | Wanted::CodeActions | Wanted::Format | Wanted::FormatSelection
            if !same_document && !matches!(pending.wanted, Wanted::Rename(_)) =>
        {
            // The person moved on while the server thought.
        }
        Wanted::Rename(_) => {
            let edits: Vec<_> = progress
                .parts
                .iter()
                .filter_map(|part| {
                    reply::workspace_edit(&part.value, part.encoding)
                        .ok()
                        .filter(|e| !e.changes.is_empty())
                })
                .collect();
            match edits.into_iter().next() {
                Some(edit) => effects.push(Effect::Apply(Request {
                    edit,
                    stamps: pending.stamps.clone(),
                    rect: pending.rect,
                    title: "Rename".to_owned(),
                    reply: None,
                })),
                None => app.show_toast(failure_text("rename this", progress.failure.as_ref())),
            }
        }
        Wanted::CodeActions => {
            let mut items = Vec::new();
            for part in &progress.parts {
                for action in reply::code_actions(&part.value, part.encoding) {
                    items.push((part.instance, action));
                }
            }
            if items.is_empty() {
                app.show_toast(failure_text("fix here", progress.failure.as_ref()));
            } else {
                app.editor.language.popup = action_popup(target, items);
            }
        }
        Wanted::ResolveAction { instance, title } => {
            match progress.parts.first().and_then(|part| {
                reply::code_actions(&json!([part.value.clone()]), part.encoding).pop()
            }) {
                Some(mut action) => {
                    // Anything still missing cannot be had; do what is there.
                    action.needs_resolve = false;
                    effects.extend(action_effects(*instance, &action, &pending));
                }
                None => app.show_toast(format!("{title}: the server gave no result")),
            }
        }
        Wanted::OnType => {
            // Only while the text is as it was when asked: typing on makes the
            // answer wrong.
            let now = super::target(app, target.pane)
                .filter(|t| t.same_view(target) && t.stamp == target.stamp);
            if let (Some(part), Some(link), Some(_)) = (
                progress.usable(Feature::OnTypeFormatting).first(),
                uri::file_uri(&target.path),
                now,
            ) {
                let found = reply::text_edits(&part.value, part.encoding);
                if !found.is_empty() {
                    effects.push(Effect::Apply(Request {
                        edit: single_document_edit(link, found),
                        stamps: pending.stamps.clone(),
                        rect: pending.rect,
                        title: "Format".to_owned(),
                        reply: None,
                    }));
                }
            }
        }
        Wanted::Format | Wanted::FormatSelection => {
            let Some(part) = progress.usable(Feature::Formatting).first().cloned() else {
                app.show_toast(failure_text("format this", progress.failure.as_ref()));
                return effects;
            };
            let edits = reply::text_edits(&part.value, part.encoding);
            if edits.is_empty() {
                app.show_toast("Nothing to format");
            } else if let Some(link) = uri::file_uri(&target.path) {
                effects.push(Effect::Apply(Request {
                    edit: single_document_edit(link, edits),
                    stamps: pending.stamps.clone(),
                    rect: pending.rect,
                    title: "Format".to_owned(),
                    reply: None,
                }));
            }
        }
        Wanted::Command { title } => {
            if let Some(Failure::Failed(reason)) = &progress.failure {
                app.show_error(format!("{title}: {reason}"));
            }
        }
    }
    effects
}

fn action_effects(instance: u64, action: &CodeAction, pending: &Pending) -> Vec<Effect> {
    let mut effects = Vec::new();
    if let Some(edit) = &action.edit {
        let request = Request {
            edit: edit.clone(),
            stamps: pending.stamps.clone(),
            rect: pending.rect,
            title: action.title.clone(),
            reply: None,
        };
        match &action.command {
            Some(command) => effects.push(Effect::ApplyThen {
                request,
                instance,
                command: command.clone(),
            }),
            None => effects.push(Effect::Apply(request)),
        }
    } else if let Some(command) = &action.command {
        effects.push(Effect::Execute {
            instance,
            command: command.clone(),
            rect: pending.rect,
        });
    }
    effects
}

pub(super) fn action_popup(target: &Target, mut items: Vec<(u64, CodeAction)>) -> Option<Popup> {
    if items.is_empty() {
        return None;
    }
    // Preferred fixes first, then fixes before refactorings.
    items.sort_by_key(|(_, action)| {
        (
            action.disabled.is_some(),
            !action.preferred,
            !action
                .kind
                .as_deref()
                .is_some_and(|k| k.starts_with("quickfix")),
        )
    });
    let rows = items
        .into_iter()
        .map(|(instance, action)| action_item(instance, action))
        .collect();
    Some(
        Popup::new(
            Kind::CodeActions,
            target.pane,
            target.stamp,
            Some(target.cursor),
            rows,
        )
        .titled("Code actions"),
    )
}

fn action_item(instance: u64, action: CodeAction) -> Item {
    let badge = match action.kind.as_deref() {
        Some(kind) if kind.starts_with("quickfix") => "fx",
        Some(kind) if kind.starts_with("refactor") => "rf",
        Some(kind) if kind.starts_with("source") => "sr",
        _ => "",
    };
    let detail = action.disabled.clone().unwrap_or_default();
    Item {
        label: if action.preferred {
            format!("\u{2605} {}", action.title)
        } else {
            action.title.clone()
        },
        detail,
        badge,
        faint: action.disabled.is_some(),
        payload: Payload::Action {
            instance,
            action: Arc::new(action),
        },
    }
}

/// An edit that replaces ranges of one document.
pub fn single_document_edit(uri: String, edits: Vec<TextEdit>) -> WorkspaceEdit {
    WorkspaceEdit {
        changes: vec![Change::Edit(DocumentEdit {
            uri,
            version: None,
            edits,
        })],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edit_for_one_document_is_a_workspace_edit() {
        let edit = single_document_edit("file:///a".into(), Vec::new());
        assert_eq!(edit.changes.len(), 1);
    }

    #[test]
    fn disabled_actions_are_faint_and_say_why() {
        let action = reply::code_actions(
            &json!([{ "title": "Extract", "disabled": { "reason": "select an expression" },
                      "command": { "title": "c", "command": "x" } }]),
            crate::features::language_service::capabilities::PositionEncoding::Utf16,
        )
        .pop()
        .unwrap();
        let item = action_item(1, action);
        assert!(item.faint);
        assert_eq!(item.detail, "select an expression");
    }

    #[test]
    fn preferred_actions_are_starred() {
        let action = reply::code_actions(
            &json!([{ "title": "Fix it", "kind": "quickfix", "isPreferred": true, "data": 1 }]),
            crate::features::language_service::capabilities::PositionEncoding::Utf16,
        )
        .pop()
        .unwrap();
        let item = action_item(1, action);
        assert!(item.label.starts_with('\u{2605}'));
        assert_eq!(item.badge, "fx");
    }
}
