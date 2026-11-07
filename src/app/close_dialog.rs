//! Dirty-close coordination. Disk work completes before tabs or the window close.
use crate::app::context::AppContext;
use crate::app::state::CloseContinuation;
use crate::app::workspace_commands as workspace_actions;
use crate::features::keymap::action::Action;
use crate::ui::chrome::{CloseBinding, CloseCommand, CloseOutcome};
use lgui::core::{UiFocusHandle, UiId};
use lgui::prelude::{Element, UiRect};
use lgui::window::WindowManager;

pub fn message(state: &AppContext) -> Option<String> {
    let snapshot = state.get();
    let request = snapshot.editor.close_request.as_ref()?;
    if snapshot.editor.saving.closing {
        return Some("Saving changes… Please wait.".into());
    }
    let dirty = snapshot
        .workspace
        .released_by(&request.targets)
        .into_iter()
        .filter(|id| snapshot.workspace.is_dirty(*id))
        .collect::<Vec<_>>();
    if dirty.len() == 1 {
        let name = snapshot
            .workspace
            .meta(dirty[0])
            .map(|m| m.name.as_str())
            .unwrap_or("this file");
        Some(format!(
            "Do you want to save the changes you made to {name}?"
        ))
    } else {
        Some(format!(
            "Do you want to save changes to {} files?",
            dirty.len()
        ))
    }
}

pub(crate) fn finish(
    state: &AppContext,
    continuation: CloseContinuation,
    windows: &WindowManager,
    focus: Option<&UiFocusHandle>,
) {
    if continuation == CloseContinuation::ExitApplication {
        if workspace_actions::save_editor_session(state) {
            windows.close("loog");
        }
    } else if let Some(focus) = focus {
        focus.focus();
    }
}

fn run(
    state: &AppContext,
    command: CloseCommand,
    windows: WindowManager,
    focus: UiFocusHandle,
) -> CloseOutcome {
    if state.get().editor.saving.closing {
        return CloseOutcome {
            exit: false,
            resolved: false,
        };
    }
    match command {
        CloseCommand::Save => {
            workspace_actions::save_close_request(state, move |state, next| {
                finish(state, next, &windows, Some(&focus))
            });
        }
        CloseCommand::Discard => {
            workspace_actions::discard_close_request_after_saves(state, move |state, next| {
                finish(state, next, &windows, Some(&focus))
            });
        }
        CloseCommand::Cancel => workspace_actions::cancel_close_request(state),
    }
    CloseOutcome {
        exit: false,
        resolved: state.get().editor.close_request.is_none(),
    }
}

pub fn binding(state: &AppContext, windows: WindowManager, focus: UiFocusHandle) -> CloseBinding {
    let reader = state.clone();
    let writer = state.clone();
    let busy = state.clone();
    CloseBinding::new(
        move || message(&reader),
        move || busy.get().editor.saving.closing,
        move |command| run(&writer, command, windows.clone(), focus.clone()),
    )
}

pub fn render(
    rect: UiRect,
    state: AppContext,
    id: UiId,
    focus: UiFocusHandle,
    windows: WindowManager,
) -> Element {
    crate::ui::close_confirmation::render(rect, binding(&state, windows, focus.clone()), id, focus)
}

pub fn perform(
    state: &AppContext,
    editor_focus: &UiFocusHandle,
    windows: &WindowManager,
    action: Action,
) -> bool {
    let command = match action {
        Action::DialogConfirm => CloseCommand::Save,
        Action::DialogSecondary => CloseCommand::Discard,
        Action::DialogCancel => CloseCommand::Cancel,
        _ => return false,
    };
    let outcome = run(state, command, windows.clone(), editor_focus.clone());
    if outcome.resolved {
        editor_focus.focus();
    }
    true
}
