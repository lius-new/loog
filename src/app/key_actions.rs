//! Connects the keymap to the application: resolves key events into actions,
//! performs them, and keeps the user keymap loaded.
//!
//! All bound keys are resolved in one capture-phase listener at the root, so
//! a binding wins before the focused element sees the key. Keys no binding
//! claims continue to the focused element (editor text input, terminal bytes,
//! text fields) unchanged.
use crate::app::context::AppContext;

use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;

use lgui::ApplicationHandle;
use lgui::application::ApplicationContext;
use lgui::core::{KeyboardEvent, UiEventKind, UiEventPayload, UiFocusHandle};
use lgui::prelude::{Element, State, UiRect};
use lgui::services::ServicesContextExt;

use crate::app::close_dialog as close_confirmation;
use crate::app::editor_vim as vim_input;
use crate::app::keymap_context::{self as context_stack, EDITOR, TERMINAL, VIM};
use crate::app::state::AppState;
use crate::app::terminal::{self, ClipboardShortcut};
use crate::app::workspace_commands as workspace_actions;
use crate::features::git::GitStoreSnapshot;
use crate::features::keymap::action::Action;
use crate::features::keymap::bindings as keymap;
use crate::features::keymap::context::KeyContext;
use crate::features::keymap::dispatcher::{self, Replay};
use crate::features::keymap::keystroke::Keystroke;
use crate::infrastructure::config_watcher::{self as config_watcher, ConfigWatcher};
use crate::infrastructure::keymap_filesystem::{self as keymap_file, UserKeymap};
use crate::infrastructure::settings_filesystem::{self as settings_persistence, UserSettings};
use crate::infrastructure::terminal::runtime::TerminalHandle;
use crate::ui::editor::view as editor_view;

/// Everything actions need to act on, captured once per frame.
#[derive(Clone)]
pub struct KeyEnv {
    pub state: AppContext,
    pub git_store: State<GitStoreSnapshot>,
    pub application: ApplicationContext,
    pub editor_focus: UiFocusHandle,
    /// Bounds of the main editor surface, for scrolling and page sizes.
    pub editor_rect: UiRect,
    pub terminal: Option<TerminalEnv>,
}

#[derive(Clone)]
pub struct TerminalEnv {
    pub tabs: crate::app::terminal_binding::TerminalStore,
    pub focus: UiFocusHandle,
    pub controller: Option<TerminalHandle>,
}

/// Install the keymap dispatcher on `root`.
pub fn attach(root: Element, env: KeyEnv) -> Element {
    root.on_event_capture(UiEventKind::KeyDown, move |ctx, payload| {
        if let UiEventPayload::Keyboard { event } = payload
            && handle_key_down(&env, event)
        {
            ctx.prevent_default();
            ctx.stop_propagation();
        }
    })
}

/// Resolve one key-down event. Returns whether the keymap consumed it.
pub fn handle_key_down(env: &KeyEnv, event: &KeyboardEvent) -> bool {
    if env
        .state
        .get()
        .editor
        .editing
        .menu
        .is_some_and(|(view, _)| env.state.get().workspace.active() == Some(view))
    {
        return crate::app::editing::key(&env.state, event);
    }
    if env.state.get().chrome.encoding.is_some() {
        return crate::app::encoding::key(&env.state, event);
    }
    if crate::app::image_preview::key(&env.state, event) {
        return true;
    }
    if event.is_composing {
        return false;
    }
    let Some(keystroke) = crate::ui::keymap_events::from_event(event) else {
        return false;
    };
    let mut composing = false;
    let mut workspace_picker = false;
    let mut pending = Vec::new();
    let mut stack = Vec::new();
    env.state.try_update(|app| {
        workspace_picker = app.terminal.ui.terminal_workspace_picker.is_some();
        composing = app.editor.is_composing()
            || !app.explorer.ui.create_input.preedit.is_empty()
            || !app.git.ui.commit_input.preedit.is_empty()
            || !app.git.ui.branch_picker_input.preedit.is_empty()
            || !app.git.ui.tag_input.preedit.is_empty()
            || !app.keymap_ui.search.preedit.is_empty()
            || !app.extensions_ui.search.preedit.is_empty()
            || app.search.composing();
        pending = app.keymap.pending().to_vec();
        stack = context_stack::context_stack(app);
        false
    });
    // Composition and the workspace picker handle their own input before
    // editor/terminal bindings can consume navigation or confirmation keys.
    if composing || workspace_picker {
        return false;
    }
    let keymap = keymap::current();
    let typed = keystroke.clone();
    let result = dispatcher::dispatch_key(&keymap, pending, keystroke, &stack);
    // `LOOM_LOG_KEYS=1 cargo run` traces how each keystroke resolved.
    #[cfg(debug_assertions)]
    if std::env::var_os("LOOM_LOG_KEYS").is_some() {
        eprintln!(
            "[keymap] {typed} in {} -> actions {:?}, pending {:?}, replay {}",
            context_stack::describe(&stack),
            result
                .actions
                .iter()
                .map(|action| action.name())
                .collect::<Vec<_>>(),
            result
                .pending
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            result.replay.len(),
        );
    }
    for replay in result.replay {
        run_replay(env, &stack, replay);
    }
    let waiting = !result.pending.is_empty();
    set_pending(&env.state, result.pending);
    waiting || run_actions(env, &result.actions) || vim_key(env, &stack, &typed)
}

/// Offer a keystroke no binding claimed to Vim, when it drives the editor.
fn vim_key(env: &KeyEnv, stack: &[KeyContext], keystroke: &Keystroke) -> bool {
    if !stack.iter().any(|context| context.primary() == Some(VIM)) {
        return false;
    }
    let Some(key) = crate::app::editor_keys::vim_key(keystroke) else {
        return false;
    };
    let clipboard = env.application.clipboard();
    vim_input::handle_key(&env.state, key, env.editor_rect, clipboard.as_ref())
}

/// Resolve a sequence that timed out, if `seq` still identifies it.
pub fn flush_pending(env: &KeyEnv, seq: u64) {
    let mut pending = Vec::new();
    let mut stack = Vec::new();
    env.state.try_update(|app| {
        if app.keymap.pending_seq() != seq || app.keymap.pending().is_empty() {
            return false;
        }
        pending = app.keymap.take_pending();
        app.keymap.bump_pending();
        stack = context_stack::context_stack(app);
        true
    });
    if pending.is_empty() {
        return;
    }
    for replay in dispatcher::flush(&keymap::current(), pending, &stack) {
        run_replay(env, &stack, replay);
    }
}

/// Wait out the pending-sequence timeout on a worker thread and flush on the
/// UI thread. Dropping the returned guard (a newer keystroke) cancels it.
pub fn start_pending_timer(env: KeyEnv, seq: u64) -> impl FnOnce() {
    let (stop_sender, stop_receiver) = mpsc::channel::<()>();
    let handle = env.application.try_resource::<ApplicationHandle>();
    let worker = thread::Builder::new()
        .name("loom-key-sequence-timeout".to_owned())
        .spawn(move || {
            if let Err(RecvTimeoutError::Timeout) =
                stop_receiver.recv_timeout(dispatcher::PENDING_TIMEOUT)
            {
                match handle {
                    Some(handle) => handle.post(move || flush_pending(&env, seq)),
                    None => flush_pending(&env, seq),
                }
            }
        })
        .ok();
    move || {
        let _ = stop_sender.send(());
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

fn set_pending(state: &AppContext, pending: Vec<Keystroke>) {
    state.try_update(move |app| {
        if app.keymap.pending() == pending {
            return false;
        }
        app.keymap.set_pending(pending);
        app.keymap.bump_pending();
        true
    });
}

/// Run actions in precedence order until one applies (Zed's fall-through
/// for actions that propagate).
fn run_actions(env: &KeyEnv, actions: &[Action]) -> bool {
    actions.iter().any(|&action| perform(env, action))
}

fn run_replay(env: &KeyEnv, stack: &[KeyContext], replay: Replay) {
    if run_actions(env, &replay.actions) || vim_key(env, stack, &replay.keystroke) {
        return;
    }
    let Some(text) = replay.keystroke.text() else {
        return;
    };
    match stack.last().and_then(KeyContext::primary) {
        Some(EDITOR | VIM) => {
            let rect = env.editor_rect;
            env.state
                .update(move |app| crate::app::editor::insert_text(app, &text, rect));
        }
        Some(TERMINAL) => {
            if let Some(controller) = env
                .terminal
                .as_ref()
                .and_then(|terminal| terminal.controller.as_ref())
            {
                if let Some(terminal) = &env.terminal {
                    terminal::write(&terminal.tabs, controller.id(), text.as_bytes());
                }
            }
        }
        _ => {}
    }
}

/// Perform one action. Returns `false` when it does not apply right now, so
/// the next binding for the keystroke gets a chance.
pub fn perform(env: &KeyEnv, action: Action) -> bool {
    if let Some(done) = crate::app::language_ui::keys::perform(env, action) {
        return done;
    }
    if let Some(service) = env.state.services().and_then(|runtime| runtime.markdown()) {
        match service.card_key_action(action, env.application.clipboard().as_ref()) {
            Ok(true) => return true,
            Err(error) => {
                env.state
                    .update(|app| app.show_error(format!("Clipboard: {error}")));
                return true;
            }
            _ => {}
        }
    }
    if crate::app::markdown_view::key_action(&env.state, action, env.application.clipboard()) {
        return true;
    }
    if crate::app::diff_view::key_action(&env.state, action, env.application.clipboard()) {
        return true;
    }
    match action {
        Action::Fold => {
            super::folding::command(
                &env.state,
                super::folding::FoldAction::Fold,
                env.editor_rect,
            );
            true
        }
        Action::Unfold => {
            super::folding::command(
                &env.state,
                super::folding::FoldAction::Unfold,
                env.editor_rect,
            );
            true
        }
        Action::FoldAll => {
            super::folding::command(
                &env.state,
                super::folding::FoldAction::FoldAll,
                env.editor_rect,
            );
            true
        }
        Action::UnfoldAll => {
            super::folding::command(
                &env.state,
                super::folding::FoldAction::UnfoldAll,
                env.editor_rect,
            );
            true
        }
        Action::SelectNextOccurrence | Action::AddCursorAbove | Action::AddCursorBelow => {
            env.state.update(|app| {
                super::multicursor::command(
                    app,
                    action == Action::SelectNextOccurrence,
                    action == Action::AddCursorBelow,
                    env.editor_rect,
                )
            });
            true
        }
        Action::ToggleSoftWrap => {
            env.state.update(|app| {
                app.editor.language.rect = Some(env.editor_rect);
                super::editing::toggle_wrap(app);
                super::editor::reveal_cursor(app, env.editor_rect);
            });
            true
        }
        Action::NoAction => false,
        Action::SearchToggleCase
        | Action::SearchToggleWord
        | Action::SearchToggleRegex
        | Action::SearchToggleFilters => {
            env.state.update(|app| match action {
                Action::SearchToggleCase => {
                    app.search.ui.case_sensitive = !app.search.ui.case_sensitive
                }
                Action::SearchToggleWord => app.search.ui.whole_word = !app.search.ui.whole_word,
                Action::SearchToggleRegex => app.search.ui.regex = !app.search.ui.regex,
                _ => crate::app::search::toggle_filters(&mut app.search),
            });
            true
        }
        Action::SearchRefresh | Action::SearchCancel => {
            if action == Action::SearchRefresh {
                crate::app::search::refresh(&env.state);
            } else {
                crate::app::search::cancel(&env.state);
            }
            true
        }
        Action::SearchNext | Action::SearchPrevious | Action::SearchOpen => {
            if action != Action::SearchOpen {
                env.state.update(|app| {
                    crate::app::search::move_selection(
                        app,
                        if action == Action::SearchNext { 1 } else { -1 },
                    )
                });
            }
            if let Some(application) = env.application.try_resource::<ApplicationHandle>() {
                crate::app::search::navigate_selected(
                    &env.state,
                    application.as_ref().clone(),
                    env.editor_rect,
                    env.editor_focus.clone(),
                    action == Action::SearchOpen,
                    action == Action::SearchOpen,
                );
            }
            true
        }
        Action::ToggleTerminal => {
            match &env.terminal {
                Some(terminal) => crate::app::terminal::toggle_panel(
                    &env.state,
                    &terminal.tabs,
                    &env.editor_focus,
                    &terminal.focus,
                ),
                None => env.state.update(|app| app.apply(action)),
            }
            true
        }
        Action::RunExtension(command) => {
            match env
                .state
                .services()
                .and_then(|runtime| runtime.extensions())
            {
                Some(host) => host.run_command(&env.state, &command.id()),
                None => env
                    .state
                    .update(|app| app.show_error("Extensions are not available")),
            }
            true
        }
        Action::VimKeys(keys) => {
            let clipboard = env.application.clipboard();
            vim_input::feed_keys(
                &env.state,
                &keys.keys(),
                env.editor_rect,
                clipboard.as_ref(),
            )
        }
        Action::OpenKeymapFile => {
            open_keymap_file(env);
            true
        }
        Action::AllowExtensionTools => {
            change_tools(env, true);
            true
        }
        Action::RevokeExtensionTools => {
            change_tools(env, false);
            true
        }
        Action::TrustWorkspace => {
            change_trust(env, true);
            true
        }
        Action::RevokeWorkspaceTrust => {
            change_trust(env, false);
            true
        }
        Action::ReloadExtensions => {
            match env
                .state
                .services()
                .and_then(|runtime| runtime.extensions())
            {
                Some(host) => host.reload_all(&env.state),
                None => env
                    .state
                    .update(|app| app.show_error("Extensions are not available")),
            }
            true
        }
        Action::ImportExtension => {
            super::extensions_view::import_package(&env.state);
            true
        }
        Action::RetryExtensions | Action::StopExtensions => {
            match env
                .state
                .services()
                .and_then(|runtime| runtime.extensions())
            {
                Some(host) if matches!(action, Action::RetryExtensions) => {
                    host.retry_failed(&env.state)
                }
                Some(host) => host.stop_all(&env.state),
                None => env
                    .state
                    .update(|app| app.show_error("Extensions are not available")),
            }
            true
        }
        Action::ShowExtensions => {
            let report = env
                .state
                .services()
                .and_then(|runtime| runtime.extensions())
                .map(|host| host.report())
                .unwrap_or_else(|| "Extensions are not available".to_owned());
            env.state.update(|app| app.show_toast(report));
            true
        }
        Action::OpenProjectSettingsFile => {
            if open_project_settings(&env.state) {
                env.editor_focus.focus();
            }
            true
        }
        Action::OpenSettingsFile => {
            open_settings_file(env);
            true
        }
        Action::SaveWithoutExtensions => {
            crate::app::workspace_commands::save_active_document_without_participants(&env.state);
            true
        }
        Action::Save => {
            workspace_actions::save_active_document(&env.state);
            true
        }
        Action::TerminalCopy | Action::TerminalCopyOrInterrupt | Action::TerminalPaste => {
            let Some(controller) = env
                .terminal
                .as_ref()
                .and_then(|terminal| terminal.controller.as_ref())
            else {
                return false;
            };
            let shortcut = match action {
                Action::TerminalCopy => ClipboardShortcut::Copy,
                Action::TerminalCopyOrInterrupt => ClipboardShortcut::CopyOrInterrupt,
                _ => ClipboardShortcut::Paste,
            };
            let clipboard = env.application.clipboard();
            let result = terminal::clipboard(
                &env.terminal.as_ref().unwrap().tabs,
                controller.id(),
                shortcut,
                clipboard.as_ref(),
            );
            terminal::report_clipboard_error(&env.state, result);
            if let Some(handle) = env.application.try_resource::<ApplicationHandle>() {
                handle.request_frame();
            }
            true
        }
        Action::DialogConfirm | Action::DialogCancel | Action::DialogSecondary => {
            let (close_request, clone_dialog, stash_switch, git_confirm) =
                read(&env.state, |app| {
                    (
                        app.editor.close_request.is_some(),
                        app.git.ui.clone_visible,
                        app.git.ui.stash_switch.is_some(),
                        app.git.ui.confirm.is_some(),
                    )
                });
            if close_request {
                close_confirmation::perform(
                    &env.state,
                    &env.editor_focus,
                    &env.application.windows(),
                    action,
                )
            } else if clone_dialog {
                crate::app::git_view::perform_clone(&env.state, action)
            } else if stash_switch {
                crate::app::git_view::perform_stash_switch(&env.state, &env.git_store, action)
            } else if git_confirm {
                crate::app::git_view::perform_confirm(&env.state, &env.git_store, action)
            } else {
                false
            }
        }
        Action::MenuConfirm => crate::app::git_view::confirm_branch(&env.state, &env.git_store),
        Action::GoToConflict | Action::GoToPreviousConflict => crate::app::git_conflict::go_to(
            &env.state,
            &env.git_store,
            action == Action::GoToConflict,
            env.editor_rect,
        ),
        Action::GitCommit => {
            crate::app::git::commit(&env.state, &env.git_store);
            true
        }
        _ => {
            if let Some(command) = action.editor_command(editor_view::page_lines(env.editor_rect)) {
                crate::app::editor::execute_command(
                    &env.state,
                    command,
                    env.editor_rect,
                    env.application.clipboard(),
                );
            } else {
                env.state.update(move |app| app.apply(action));
            }
            true
        }
    }
}

/// Read from the state without cloning all of it.
fn read<R: Default>(state: &AppContext, f: impl FnOnce(&AppState) -> R) -> R {
    let mut out = R::default();
    state.try_update(|app| {
        out = f(app);
        false
    });
    out
}

fn open_keymap_file(env: &KeyEnv) {
    if open_user_keymap(&env.state) {
        env.editor_focus.focus();
    }
}

/// Open `keymap.json` in a tab, creating it from the template first.
pub fn open_user_keymap(state: &AppContext) -> bool {
    match keymap_file::ensure_user_file() {
        Ok(path) => {
            let loader = state.services().and_then(|r| r.file_loader());
            state.update(move |app| {
                crate::app::explorer::open_launch_paths(app, vec![path], loader.as_ref())
            });
            true
        }
        Err(error) => {
            state.update(move |app| app.show_error(format!("Could not open keymap.json: {error}")));
            false
        }
    }
}

// ---- User keymap -------------------------------------------------------------

/// Load `keymap.json` over the defaults and install the result. Problems are
/// reported in a toast; a file that does not parse keeps the previous keymap.
pub fn reload_user_keymap(state: &AppContext) {
    let user = match keymap_file::load_user() {
        UserKeymap::Missing => Vec::new(),
        UserKeymap::Loaded(outcome) => {
            if let Some(first) = outcome.errors.first() {
                let message = match outcome.errors.len() {
                    1 => format!("keymap.json: {first}"),
                    count => format!("keymap.json: {first} (and {} more)", count - 1),
                };
                state.update(move |app| app.show_error(message));
            }
            outcome.bindings
        }
        UserKeymap::Invalid(error) => {
            state
                .update(move |app| app.show_error(format!("keymap.json was not applied: {error}")));
            return;
        }
    };
    keymap::install(keymap_file::build(user));
    state.update(|app| app.keymap.reloaded());
}

/// Reloads the user keymap whenever `keymap.json` changes on disk.
pub fn watch_user_keymap(state: AppContext) -> Option<ConfigWatcher> {
    config_watcher::watch(keymap_file::FILE_NAME, move || reload_user_keymap(&state))
}

// ---- User settings -----------------------------------------------------------

fn open_settings_file(env: &KeyEnv) {
    if open_user_settings(&env.state) {
        env.editor_focus.focus();
    }
}

/// Open `settings.json` in a tab, creating it from the template first.
pub fn open_user_settings(state: &AppContext) -> bool {
    match settings_persistence::ensure_user_file() {
        Ok(path) => {
            let loader = state.services().and_then(|r| r.file_loader());
            state.update(move |app| {
                crate::app::explorer::open_launch_paths(app, vec![path], loader.as_ref())
            });
            true
        }
        Err(error) => {
            state.update(move |app| {
                app.show_error(format!("Could not open settings.json: {error}"))
            });
            false
        }
    }
}

/// The current project's workspace folder: the one
/// holding the active file, otherwise the first.
pub fn project_settings_root(app: &AppState) -> Option<std::path::PathBuf> {
    app.workspace
        .active_path()
        .and_then(|path| app.workspace_folder_of(path))
        .or_else(|| {
            app.explorer
                .model
                .roots()
                .first()
                .map(std::path::PathBuf::as_path)
        })
        .map(std::path::Path::to_path_buf)
}

/// Open the current project's `.loom/settings.json`, creating it first.
pub fn open_project_settings(state: &AppContext) -> bool {
    let Some(root) = project_settings_root(&state.get()) else {
        state.update(|app| app.show_error("Open a folder to add project settings."));
        return false;
    };
    match crate::infrastructure::project_settings::ensure(&root) {
        Ok(path) => {
            let loader = state.services().and_then(|r| r.file_loader());
            state.update(move |app| {
                crate::app::explorer::open_launch_paths(app, vec![path], loader.as_ref())
            });
            true
        }
        Err(error) => {
            state.update(move |app| {
                app.show_error(format!("Could not open project settings: {error}"))
            });
            false
        }
    }
}

/// Apply `settings.json` to the running app. Ignored values are reported in
/// a toast; a file that does not parse keeps the current settings.
pub fn reload_user_settings(state: &AppContext) {
    let outcome = match settings_persistence::read() {
        UserSettings::Missing => settings_persistence::LoadOutcome::default(),
        UserSettings::Loaded(outcome) => outcome,
        UserSettings::Invalid(error) => {
            state.update(move |app| {
                app.show_error(format!("settings.json was not applied: {error}"))
            });
            return;
        }
    };
    if let Some(first) = outcome.errors.first() {
        let message = match outcome.errors.len() {
            1 => format!("settings.json: {first}"),
            count => format!("settings.json: {first} (and {} more)", count - 1),
        };
        state.update(move |app| app.show_error(message));
    }
    let settings = outcome.settings;
    state.try_update(move |app| app.apply_settings(settings));
}

/// Reloads settings whenever `settings.json` changes, except for the app's
/// own writes from the Settings page.
pub fn watch_user_settings(state: AppContext) -> Option<ConfigWatcher> {
    config_watcher::watch(settings_persistence::FILE_NAME, move || {
        let own_write = settings_persistence::current_file_is_own_write();
        if !own_write {
            reload_user_settings(&state);
        }
    })
}

// ---- Labels ------------------------------------------------------------------

/// The label of the binding that triggers `action` in `stack`, if any.
pub fn shortcut_label(action: Action, stack: &[KeyContext]) -> Option<String> {
    keymap::current()
        .binding_for_action(action, stack)
        .map(|binding| binding.label())
}

/// Shortcut label for an editor action, as seen from the focused editor.
pub fn editor_shortcut_label(app: &AppState, action: Action) -> Option<String> {
    shortcut_label(
        action,
        &[
            context_stack::workspace_context(app),
            context_stack::editor_context(app),
        ],
    )
}

/// Trust or stop trusting the folder of the active file (the innermost
/// workspace folder that contains it, else its own directory). Servers that
/// were refused are started again, and revoking stops the running ones.
fn change_trust(env: &KeyEnv, trust: bool) {
    let app = env.state.get();
    let Some(path) = app.workspace.active_path().map(|p| p.to_path_buf()) else {
        env.state
            .update(|app| app.show_error("Open a file in the folder first"));
        return;
    };
    let root = app
        .workspace_folder_of(&path)
        .map(|folder| folder.to_path_buf())
        .or_else(|| path.parent().map(|parent| parent.to_path_buf()));
    let Some(root) = root else {
        return;
    };
    let Some(runtime) = env.state.services() else {
        return;
    };
    let result = if trust {
        runtime.trust().trust(&root)
    } else {
        runtime.trust().revoke(&root)
    };
    match result {
        Ok(()) => {
            if let Some(services) = runtime.language_services() {
                services.restart();
            }
            if !trust && let Some(host) = runtime.extensions() {
                host.revoke_tools(&root);
            }
            let verb = if trust {
                "Trusted"
            } else {
                "No longer trusting"
            };
            env.state
                .update(|app| app.show_toast(format!("{verb} {}", root.display())));
        }
        Err(error) => env
            .state
            .update(|app| app.show_error(format!("Could not save the trust decision: {error}"))),
    }
}

/// Allow or stop extensions running programs from the folder of the active
/// file. The message says what the permission means: no sandbox.
fn change_tools(env: &KeyEnv, allow: bool) {
    let app = env.state.get();
    let Some(path) = app.workspace.active_path().map(|p| p.to_path_buf()) else {
        env.state
            .update(|app| app.show_error("Open a file in the folder first"));
        return;
    };
    let Some(root) = app
        .workspace_folder_of(&path)
        .map(|folder| folder.to_path_buf())
        .or_else(|| path.parent().map(|parent| parent.to_path_buf()))
    else {
        return;
    };
    let Some(host) = env
        .state
        .services()
        .and_then(|runtime| runtime.extensions())
    else {
        env.state
            .update(|app| app.show_error("Extensions are not available"));
        return;
    };
    let message = if allow {
        let count = host.allow_tools(&root);
        format!(
            "{count} extension(s) may now run programs from {} with your user rights; this is not a sandbox. The folder must also be trusted.",
            root.display()
        )
    } else {
        host.revoke_tools(&root);
        format!(
            "Extensions can no longer run programs from {}",
            root.display()
        )
    };
    env.state.update(|app| app.show_toast(message));
}
