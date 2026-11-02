//! Keyboard commands for the language features, and the keys the open popup
//! takes for itself.
use super::actions::{self, Wanted};
use super::lists;
use super::popup::{Item, Kind, Payload, Popup, VISIBLE};
use super::save;
use super::{
    AppContext, completion, expand, hover, navigate, refactor, signature, snippet_session,
};
use crate::app::key_actions::KeyEnv;
use crate::features::keymap::action::Action;
use crate::features::language_service::request::Feature;
use lgui::prelude::UiRect;

/// Performs `action` if it is a language command, or a popup key while a popup is
/// open. `None` leaves the action to the rest of the application.
pub(crate) fn perform(env: &KeyEnv, action: Action) -> Option<bool> {
    perform_with(&env.state, env.editor_rect, action)
}

/// [`perform`] with only what it needs.
pub(crate) fn perform_with(state: &AppContext, rect: UiRect, action: Action) -> Option<bool> {
    match action {
        Action::PeekDefinition => {
            actions::begin(state, Wanted::Peek(Feature::Definition), rect, None);
            Some(true)
        }
        Action::ClosePeek => {
            state.update(super::peek::close);
            Some(true)
        }
        Action::OpenPeekTarget => {
            let revision = super::peek::view(&state.get(), state.get().workspace.active_pane())
                .map(|v| v.revision);
            if let Some(revision) = revision {
                super::peek::jump(state, revision, rect);
            }
            Some(true)
        }
        Action::Save => {
            let document = state.get().workspace.active_document();
            save::save(
                state,
                rect,
                std::sync::Arc::new(move |state| {
                    if let Some(document) = document {
                        crate::app::workspace_commands::save_document(state, document);
                    }
                }),
            );
            Some(true)
        }
        Action::ShowCallHierarchy => relation(state, lists::Relation::Callers),
        Action::ShowOutgoingCalls => relation(state, lists::Relation::Callees),
        Action::ShowSupertypes => relation(state, lists::Relation::Supertypes),
        Action::ShowSubtypes => relation(state, lists::Relation::Subtypes),
        Action::ShowCodeLenses => {
            lists::lenses(state);
            Some(true)
        }
        Action::ToggleProblems => {
            lists::problems(state);
            Some(true)
        }
        Action::GoToDefinition => goto(state, Feature::Definition, rect),
        Action::GoToDeclaration => goto(state, Feature::Declaration, rect),
        Action::GoToTypeDefinition => goto(state, Feature::TypeDefinition, rect),
        Action::GoToImplementation => goto(state, Feature::Implementation, rect),
        Action::FindAllReferences => {
            actions::begin(state, Wanted::References, rect, None);
            Some(true)
        }
        Action::ShowSignatureHelp => {
            state.update(|app| signature::request(state, app, None, true));
            Some(true)
        }
        Action::NextSignature | Action::PreviousSignature => {
            let mut moved = false;
            state.try_update(|app| {
                moved = signature::step(app, action == Action::NextSignature, None);
                moved
            });
            Some(moved)
        }
        Action::SelectLargerSyntaxNode => {
            state.update(|app| expand::grow(state, app));
            Some(true)
        }
        Action::SelectSmallerSyntaxNode => {
            state.update(expand::shrink);
            Some(true)
        }
        Action::Rename => {
            refactor::begin_rename(state, rect);
            Some(true)
        }
        Action::ToggleCodeActions => {
            refactor::code_actions(state, rect);
            Some(true)
        }
        Action::Format | Action::FormatSelections => {
            refactor::format(state, rect, action == Action::FormatSelections);
            Some(true)
        }
        Action::ShowCompletions => {
            state.update(snippet_session::dismiss_choices);
            state.update(|app| completion::request(state, app, true, None));
            Some(true)
        }
        Action::ShowSnippetChoices => {
            let mut shown = false;
            state.try_update(|app| {
                shown = snippet_session::show_choices(app);
                shown
            });
            Some(shown)
        }
        Action::NextSnippetTabstop | Action::PreviousSnippetTabstop => {
            let mut moved = false;
            state.update(|app| {
                moved = snippet_session::step(app, action == Action::NextSnippetTabstop);
            });
            Some(moved)
        }
        Action::ShowHover => {
            hover::show_at_cursor(state);
            Some(true)
        }
        Action::GoToDiagnostic | Action::GoToPreviousDiagnostic => {
            actions::go_to_diagnostic(state, action == Action::GoToDiagnostic, rect);
            Some(true)
        }
        Action::GoBack => {
            navigate::back(state, rect);
            Some(true)
        }
        Action::GoForward => {
            navigate::forward(state, rect);
            Some(true)
        }
        Action::ToggleOutline => {
            actions::begin(state, Wanted::DocumentSymbols, rect, None);
            Some(true)
        }
        Action::ToggleProjectSymbols => {
            open_workspace_symbols(state, rect);
            Some(true)
        }
        Action::MenuSelectNext
        | Action::MenuSelectPrevious
        | Action::MenuPageUp
        | Action::MenuPageDown
        | Action::MenuConfirm
        | Action::MenuConfirmTab
        | Action::MenuCancel => popup_key(state, action, rect),
        Action::Backspace => {
            if snippet_session::choice_visible(&state.get()) {
                return None;
            }
            let mut handled = false;
            state.update(|app| {
                handled = app
                    .editor
                    .language
                    .popup
                    .as_mut()
                    .is_some_and(|popup| popup.backspace());
            });
            if handled {
                workspace_query_changed(state, rect);
                Some(true)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// A language command typed in Vim's normal mode.
pub(crate) fn vim_command(
    state: &AppContext,
    command: loom_editor_core::vim::LanguageCommand,
    rect: UiRect,
) {
    use loom_editor_core::vim::LanguageCommand as C;
    let action = match command {
        C::Hover => Action::ShowHover,
        C::Definition => Action::GoToDefinition,
        C::Declaration => Action::GoToDeclaration,
        C::TypeDefinition => Action::GoToTypeDefinition,
        C::Implementation => Action::GoToImplementation,
        C::References => Action::FindAllReferences,
    };
    perform_with(state, rect, action);
}

fn relation(state: &AppContext, relation: lists::Relation) -> Option<bool> {
    lists::hierarchy(state, relation);
    Some(true)
}

fn goto(state: &AppContext, feature: Feature, rect: UiRect) -> Option<bool> {
    actions::begin(state, Wanted::Goto(feature), rect, None);
    Some(true)
}

/// The completion list takes the same keys as any popup.
fn completion_key(state: &AppContext, action: Action, rect: UiRect) -> Option<bool> {
    let mut visible = false;
    state.try_update(|app| {
        visible = super::completion_visible(app);
        false
    });
    if !visible {
        return None;
    }
    // `lsp.completion.accept_on` says which of Enter and Tab accept an item; the
    // other key does what it does without the list.
    let accept_on = state
        .get()
        .preferences
        .values()
        .language_features
        .completion
        .accept_on
        .clone();
    if (action == Action::MenuConfirm && accept_on == "tab")
        || (action == Action::MenuConfirmTab && accept_on == "enter")
    {
        return Some(false);
    }
    match action {
        Action::MenuConfirm | Action::MenuConfirmTab => state.update(|app| {
            let rect = app.editor.language.rect.unwrap_or(rect);
            if completion::accept(app, None) {
                crate::app::editor::reveal_cursor(app, rect);
            }
        }),
        Action::MenuCancel => state.update(completion::close),
        _ => {
            state.update(|app| match action {
                Action::MenuSelectNext => completion::move_selection(app, 1),
                Action::MenuSelectPrevious => completion::move_selection(app, -1),
                Action::MenuPageDown => completion::page(app, VISIBLE as isize),
                _ => completion::page(app, -(VISIBLE as isize)),
            });
            state.update(|app| completion::resolve_selected(state, app));
        }
    }
    Some(true)
}

fn popup_key(state: &AppContext, action: Action, rect: UiRect) -> Option<bool> {
    if snippet_session::choice_visible(&state.get()) {
        state.update(|app| match action {
            Action::MenuConfirm | Action::MenuConfirmTab => {
                snippet_session::accept_choice(app);
            }
            Action::MenuCancel => snippet_session::dismiss_choices(app),
            Action::MenuSelectNext => snippet_session::move_choice(app, 1),
            Action::MenuSelectPrevious => snippet_session::move_choice(app, -1),
            Action::MenuPageDown => snippet_session::move_choice(app, VISIBLE as isize),
            Action::MenuPageUp => snippet_session::move_choice(app, -(VISIBLE as isize)),
            _ => {}
        });
        return Some(true);
    }
    if let Some(done) = completion_key(state, action, rect) {
        return Some(done);
    }
    let mut open = false;
    state.update(|app| {
        let Some(popup) = app.editor.language.popup.as_mut() else {
            return;
        };
        if !popup.panel_focused {
            return;
        }
        popup.resize(rect.height());
        open = true;
        match action {
            Action::MenuSelectNext => popup.move_by(1),
            Action::MenuSelectPrevious => popup.move_by(-1),
            Action::MenuPageDown => popup.page(VISIBLE as isize),
            Action::MenuPageUp => popup.page(-(VISIBLE as isize)),
            _ => {}
        }
    });
    if !open {
        return None;
    }
    match action {
        Action::MenuConfirm | Action::MenuConfirmTab => {
            let mut renaming = false;
            state.try_update(|app| {
                renaming = app
                    .editor
                    .language
                    .popup
                    .as_ref()
                    .is_some_and(|p| p.kind == Kind::Rename);
                false
            });
            if renaming {
                refactor::commit_rename(state, rect);
            } else {
                choose(state, rect);
            }
        }
        Action::MenuCancel => state.update(|app| app.editor.language.close_popups()),
        _ => {}
    }
    Some(true)
}

/// Does what the selected row stands for, and closes the popup.
pub(crate) fn choose(state: &AppContext, rect: UiRect) {
    let mut chosen: Option<Item> = None;
    state.update(|app| {
        chosen = app
            .editor
            .language
            .popup
            .as_ref()
            .and_then(|popup| popup.selected_item().cloned());
        if let Some(Item {
            payload: Payload::Group(path),
            ..
        }) = &chosen
        {
            if let Some(popup) = app.editor.language.popup.as_mut() {
                popup.toggle_group(path.clone());
            }
            chosen = None;
            return;
        }
        if chosen.as_ref().is_some_and(|item| !item.faint) {
            if app
                .editor
                .language
                .popup
                .as_ref()
                .is_some_and(|p| p.kind.is_panel())
            {
                app.editor.language.popup.as_mut().unwrap().panel_focused = false;
            } else {
                app.editor.language.close_popups();
            }
        }
    });
    let Some(item) = chosen.filter(|item| !item.faint) else {
        return;
    };
    match item.payload {
        Payload::Place(place) => navigate::go(state, place, rect, true),
        Payload::Nothing => {}
        // The rest are performed by the features that created them.
        other => super::run_payload(state, other, rect),
    }
}

fn open_workspace_symbols(state: &AppContext, rect: UiRect) {
    let app = state.get();
    let pane = app.workspace.active_pane();
    let Some(target) = super::target(&app, pane) else {
        return;
    };
    drop(app);
    state.update(|app| {
        let mut popup = Popup::new(
            Kind::WorkspaceSymbols,
            target.pane,
            target.stamp,
            None,
            Vec::new(),
        )
        .with_field()
        .titled("Go to symbol in workspace");
        popup.local_filter = false;
        popup.empty = "Type to search the workspace".to_owned();
        app.editor.language.popup = Some(popup);
    });
    actions::begin(state, Wanted::WorkspaceSymbols, rect, Some(String::new()));
}

/// Typed text goes to the open popup's field. Returns whether it took it.
pub(crate) fn type_text(state: &AppContext, text: &str, rect: UiRect) -> bool {
    let mut taken = false;
    state.update(|app| {
        taken = app
            .editor
            .language
            .popup
            .as_mut()
            .is_some_and(|popup| popup.type_text(text));
    });
    if taken {
        workspace_query_changed(state, rect);
    }
    taken
}

/// Whether a popup with a field of its own is open (and so wants typed text).
pub(crate) fn takes_text(app: &crate::app::state::AppState) -> bool {
    if snippet_session::choice_visible(app) {
        return false;
    }
    app.editor
        .language
        .popup
        .as_ref()
        .is_some_and(|popup| popup.query.is_some() && popup.panel_focused)
}

/// A workspace symbol search asks the servers again for the new text.
fn workspace_query_changed(state: &AppContext, rect: UiRect) {
    let app = state.get();
    let Some(popup) = app
        .editor
        .language
        .popup
        .as_ref()
        .filter(|popup| popup.kind == Kind::WorkspaceSymbols)
    else {
        return;
    };
    let query = popup.query.clone().unwrap_or_default();
    drop(app);
    actions::begin(state, Wanted::WorkspaceSymbols, rect, Some(query));
}
