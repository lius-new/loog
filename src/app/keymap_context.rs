//! Projects application facts for pure key-context precedence rules.
use crate::app::state::{AppState, MainSurface};
use crate::features::keymap::context::KeyContext;
pub use crate::features::keymap::context_stack::*;
fn surface_name(surface: MainSurface) -> &'static str {
    match surface {
        MainSurface::Editor => "editor",
        MainSurface::Markdown => "markdown",
        MainSurface::Image => "image",
        MainSurface::Diff => "diff",
        MainSurface::Settings => "settings",
        MainSurface::Keymap => "keymap",
        MainSurface::Extensions => "extensions",
        MainSurface::WorkspaceHome => "home",
        MainSurface::Welcome => "welcome",
        MainSurface::Empty => "empty",
    }
}
pub fn facts(app: &AppState) -> ContextFacts {
    ContextFacts {
        surface: surface_name(app.main_surface()),
        extension: app
            .workspace
            .active_path()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .map(str::to_owned),
        vim_mode: app.editor.vim.mode().context_name(),
        trust: app.chrome.trust_question().is_some(),
        highlight_menu: app.highlight_menu.visible,
        language_services_menu: app.language_services_menu.visible,
        close: app.editor.close_request.is_some(),
        clone: app.git.ui.clone_visible,
        stash: app.git.ui.stash_switch.is_some(),
        confirm: app.git.ui.confirm.is_some(),
        branch: app.git.ui.branch_picker.is_some(),
        palette: app.palette.visible,
        commit: app.git.ui.commit_input.focused,
        search: app.drawer.visible
            && app.drawer.mode == crate::ui::drawer::DrawerMode::Search
            && (app.search.ui.query.focused
                || app.search.ui.include.focused
                || app.search.ui.exclude.focused
                || app.search.ui.results_focused),
        search_results: app.search.ui.results_focused,
        other_input: app.git.ui.file_filter.focused
            || app.explorer.ui.create_input.focused
            || (app.keymap_ui.search.focused && app.main_surface() == MainSurface::Keymap)
            || (app.extensions_ui.search.focused && app.main_surface() == MainSurface::Extensions),
        terminal: app.terminal.ui.terminal_focused,
        editor: app.editor.focused
            && matches!(
                app.main_surface(),
                MainSurface::Editor | MainSurface::Markdown | MainSurface::Diff
            ),
        vim_active: crate::app::editor_vim::is_active(app),
        popup: app
            .editor
            .language
            .popup
            .as_ref()
            .is_some_and(|p| p.panel_focused)
            || crate::app::language_ui::completion_visible(app)
            || crate::app::language_ui::snippet_session::choice_visible(app),
        snippet: app.editor.language.snippet.is_some(),
        snippet_choice: crate::app::language_ui::snippet_session::has_choices(app),
        signature: crate::app::language_ui::signature::can_step(app),
        language_peek: app.editor.language.peek.is_some(),
        menu: app.editor.menu.is_some()
            || app.explorer.ui.menu.is_some()
            || app.editor.tabs.tab_context_menu.is_some()
            || app.terminal.ui.terminal_tab_context_menu.is_some()
            || app.git.ui.menu.is_some(),
    }
}
pub fn context_stack(app: &AppState) -> Vec<KeyContext> {
    crate::features::keymap::context_stack::context_stack(&facts(app))
}
pub fn workspace_context(app: &AppState) -> KeyContext {
    crate::features::keymap::context_stack::workspace_context(&facts(app))
}
pub fn editor_context(app: &AppState) -> KeyContext {
    crate::features::keymap::context_stack::editor_context(&facts(app))
}
pub fn vim_context(app: &AppState) -> KeyContext {
    crate::features::keymap::context_stack::vim_context(&facts(app))
}
#[cfg(test)]
mod tests {
    use lgui::core::{KeyModifiers, KeyState, KeyboardEvent, LogicalKey, NamedKey};

    use super::*;
    use crate::app::state::{CloseContinuation, CloseRequest};
    use crate::features::keymap::action::Action;
    use crate::features::keymap::dispatcher::dispatch_key;
    use crate::features::keymap::keymap_file::default_keymap;
    use crate::features::keymap::keystroke::Keystroke;

    fn secondary() -> KeyModifiers {
        if cfg!(target_os = "macos") {
            KeyModifiers::META
        } else {
            KeyModifiers::CONTROL
        }
    }

    fn event(key: LogicalKey, modifiers: KeyModifiers) -> KeyboardEvent {
        KeyboardEvent {
            state: KeyState::Down,
            key,
            modifiers,
            ..Default::default()
        }
    }

    fn character(value: &str, modifiers: KeyModifiers) -> KeyboardEvent {
        event(LogicalKey::Character(value.into()), modifiers)
    }

    fn named(key: NamedKey, modifiers: KeyModifiers) -> KeyboardEvent {
        event(LogicalKey::Named(key), modifiers)
    }

    /// The first action the default keymap runs for `event` in `app`.
    fn action(app: &AppState, event: KeyboardEvent) -> Option<Action> {
        let keystroke = crate::ui::keymap_events::from_event(&event).unwrap();
        dispatch_key(
            &default_keymap(),
            Vec::new(),
            keystroke,
            &context_stack(app),
        )
        .actions
        .first()
        .copied()
    }

    fn editor_app() -> AppState {
        let mut app = AppState::new();
        app.workspace
            .open_path("main.rs".into(), "fn main() {}".into());
        app.editor.focused = true;
        app
    }

    #[test]
    fn stacks_follow_focus_and_modals() {
        let app = editor_app();
        let stack = context_stack(&app);
        assert_eq!(describe(&stack[1..]), "Editor extension=rs");
        assert_eq!(stack[0].get("surface"), Some("editor"));

        let mut terminal = editor_app();
        terminal.editor.focused = false;
        terminal.terminal.ui.terminal_focused = true;
        terminal.editor.menu = Some((0.0, 0.0));
        let primaries = context_stack(&terminal)
            .iter()
            .map(|context| context.primary().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(primaries, [WORKSPACE, TERMINAL, MENU]);

        let mut dialog = editor_app();
        dialog.git.ui.clone_visible = true;
        assert_eq!(describe(&context_stack(&dialog)), "Dialog CloneRepository");
    }

    #[test]
    fn branch_picker_owns_navigation_and_leaves_typing_to_its_filter() {
        let mut app = editor_app();
        app.git.ui.branch_picker = Some(crate::ui::git::branch_picker::BranchPicker {
            repository: "repo".into(),
            repository_name: "repo".into(),
            anchor_x: 0.0,
            listing: crate::features::git::actions::BranchListing::Loading,
            collapsed_worktrees: Default::default(),
            selected: 0,
            scroll: 0.0,
            query: String::new(),
            sequence: 1,
            restore_terminal: false,
        });
        assert_eq!(describe(&context_stack(&app)), "Dialog BranchPicker");
        let none = KeyModifiers::empty();
        for (key, expected) in [
            (NamedKey::ArrowUp, Action::MenuSelectPrevious),
            (NamedKey::ArrowDown, Action::MenuSelectNext),
            (NamedKey::Enter, Action::MenuConfirm),
            (NamedKey::Escape, Action::MenuCancel),
        ] {
            assert_eq!(action(&app, named(key, none)), Some(expected), "{key:?}");
        }
        assert_eq!(action(&app, character("a", none)), None);
        assert_eq!(action(&app, character("w", secondary())), None);
    }

    #[test]
    fn command_palette_opens_from_the_workspace_and_owns_navigation() {
        let mut app = editor_app();
        let none = KeyModifiers::empty();
        assert_eq!(
            action(&app, character("P", secondary() | KeyModifiers::SHIFT)),
            Some(Action::OpenCommandPalette)
        );
        app.palette.visible = true;
        assert_eq!(describe(&context_stack(&app)), "Dialog CommandPalette");
        for (key, expected) in [
            (NamedKey::ArrowUp, Action::MenuSelectPrevious),
            (NamedKey::ArrowDown, Action::MenuSelectNext),
            (NamedKey::Enter, Action::MenuConfirm),
            (NamedKey::Tab, Action::MenuConfirmTab),
            (NamedKey::Escape, Action::MenuCancel),
        ] {
            assert_eq!(action(&app, named(key, none)), Some(expected), "{key:?}");
        }
        assert_eq!(action(&app, character("a", none)), None);
    }

    #[test]
    fn workspace_shortcuts_match_the_previous_hardcoded_keymap() {
        let app = AppState::new();
        let s = secondary();
        let cases = [
            (character("b", s), Action::ToggleDrawer),
            (
                character("G", s | KeyModifiers::SHIFT),
                Action::ToggleSourceControl,
            ),
            (character("`", s), Action::ToggleTerminal),
            (character("w", s), Action::CloseActiveItem),
            (character(",", s), Action::OpenSettings),
            (character("1", s), Action::ActivateItem(0)),
            (character("8", s), Action::ActivateItem(7)),
            (character("9", s), Action::ActivateLastItem),
            (
                named(NamedKey::Tab, KeyModifiers::CONTROL),
                Action::ActivateNextItem,
            ),
            (
                named(NamedKey::Tab, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
                Action::ActivatePrevItem,
            ),
            (
                named(NamedKey::Escape, KeyModifiers::empty()),
                Action::CloseOverlay,
            ),
        ];
        for (event, expected) in cases {
            assert_eq!(
                action(&app, event.clone()),
                Some(expected),
                "{:?}",
                event.key
            );
        }
        // Ctrl+P stays unbound.
        assert_eq!(action(&app, character("p", s)), None);
    }

    #[test]
    fn workspace_search_keys_use_input_context_and_do_not_reach_editor() {
        let mut app = editor_app();
        assert_eq!(
            action(&app, character("F", secondary() | KeyModifiers::SHIFT)),
            Some(Action::OpenSearch)
        );
        app.apply(Action::OpenSearch);
        app.search.ui.query.focused = true;
        assert_eq!(
            action(&app, named(NamedKey::ArrowDown, KeyModifiers::empty())),
            Some(Action::SearchNext)
        );
        assert_eq!(
            action(&app, named(NamedKey::Escape, KeyModifiers::empty())),
            Some(Action::SearchCancel)
        );
        assert_eq!(action(&app, character("a", secondary())), None);
        app.search.ui.query.focused = false;
        app.search.ui.results_focused = true;
        assert_eq!(
            action(&app, named(NamedKey::ArrowUp, KeyModifiers::empty())),
            Some(Action::SearchPrevious)
        );
        assert_eq!(
            action(&app, named(NamedKey::Enter, KeyModifiers::empty())),
            Some(Action::SearchOpen)
        );
    }

    #[test]
    fn a_focused_changes_view_binds_copy_select_all_and_clear() {
        let mut app = AppState::new();
        app.workspace
            .open_diff(crate::features::git::diff_selection_tests::overview());
        assert_eq!(app.main_surface(), MainSurface::Diff);
        // Without focus the shortcuts belong to whatever has it.
        assert_eq!(action(&app, character("c", secondary())), None);
        app.editor.focused = true;
        assert_eq!(
            action(&app, character("c", secondary())),
            Some(Action::Copy)
        );
        assert_eq!(
            action(&app, character("a", secondary())),
            Some(Action::SelectAll)
        );
        assert_eq!(
            action(&app, named(NamedKey::Escape, KeyModifiers::empty())),
            Some(Action::Cancel)
        );
        // Editing commands stay out of a read-only diff.
        assert_eq!(action(&app, character("x", secondary())), None);
    }

    #[test]
    fn editor_shortcuts_match_the_previous_command_table() {
        let app = editor_app();
        let s = secondary();
        let none = KeyModifiers::empty();
        let shift = KeyModifiers::SHIFT;
        let cases = [
            (character("s", s), Action::Save),
            (character("z", s), Action::Undo),
            (character("Z", s | shift), Action::Redo),
            (character("y", s), Action::Redo),
            (character("a", s), Action::SelectAll),
            (character("c", s), Action::Copy),
            (character("x", s), Action::Cut),
            (character("v", s), Action::Paste),
            (named(NamedKey::ArrowLeft, none), Action::MoveLeft),
            (
                named(NamedKey::ArrowLeft, s),
                Action::MoveToPreviousWordStart,
            ),
            (
                named(NamedKey::ArrowRight, s | shift),
                Action::SelectToNextWordEnd,
            ),
            (named(NamedKey::Home, s | shift), Action::SelectToBeginning),
            (named(NamedKey::PageDown, shift), Action::SelectPageDown),
            (
                named(NamedKey::Backspace, s),
                Action::DeleteToPreviousWordStart,
            ),
            (named(NamedKey::Delete, none), Action::Delete),
            (named(NamedKey::Enter, none), Action::Newline),
            (named(NamedKey::Tab, none), Action::Tab),
            (named(NamedKey::Tab, shift), Action::Backtab),
            (named(NamedKey::Escape, none), Action::Cancel),
        ];
        for (event, expected) in cases {
            assert_eq!(
                action(&app, event.clone()),
                Some(expected),
                "{:?}",
                event.key
            );
        }
        // Workspace bindings still apply from inside the editor.
        assert_eq!(action(&app, character("b", s)), Some(Action::ToggleDrawer));
        // Ctrl+Up has no binding and reaches the editor as an unbound key.
        assert_eq!(action(&app, named(NamedKey::ArrowUp, s)), None);
    }

    #[test]
    fn font_size_shortcuts_target_the_focused_editor_or_terminal() {
        for terminal in [false, true] {
            let mut app = editor_app();
            app.terminal.ui.terminal_focused = terminal;
            app.editor.focused = !terminal;
            let s = secondary();
            for (event, expected, size) in [
                (character("=", s), Action::IncreaseFontSize, 13),
                (
                    character("+", s | KeyModifiers::SHIFT),
                    Action::IncreaseFontSize,
                    14,
                ),
                (character("-", s), Action::DecreaseFontSize, 13),
                (character("0", s), Action::ResetFontSize, 12),
            ] {
                let resolved = action(&app, event);
                assert_eq!(resolved, Some(expected));
                app.apply(resolved.unwrap());
                let prefs = app.preferences.values();
                assert_eq!(prefs.editor_font.size, if terminal { 12 } else { size });
                assert_eq!(prefs.terminal_font.size, if terminal { size } else { 12 });
                assert_eq!(prefs.ui_scale, None);
            }
        }
    }

    #[test]
    fn terminal_keeps_shell_keys_and_clipboard_conventions() {
        let mut app = AppState::new();
        app.terminal.ui.terminal_focused = true;
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(
            action(&app, character("c", ctrl)),
            Some(Action::TerminalCopyOrInterrupt)
        );
        assert_eq!(
            action(&app, character("C", ctrl | KeyModifiers::SHIFT)),
            Some(Action::TerminalCopy)
        );
        assert_eq!(
            action(&app, character("v", ctrl)),
            Some(Action::TerminalPaste)
        );
        assert_eq!(
            action(&app, named(NamedKey::Insert, KeyModifiers::SHIFT)),
            Some(Action::TerminalPaste)
        );
        assert_eq!(action(&app, character("d", ctrl)), None);
        assert_eq!(
            action(&app, named(NamedKey::Escape, KeyModifiers::empty())),
            None
        );
        assert_eq!(action(&app, named(NamedKey::Tab, ctrl)), None);
        if !cfg!(target_os = "macos") {
            assert_eq!(action(&app, character("w", ctrl)), None);
            assert_eq!(
                action(&app, character("b", ctrl)),
                Some(Action::ToggleDrawer)
            );
            // Ctrl+K is the shell's kill-line, not the start of a sequence.
            let keystroke = Keystroke::parse("ctrl-k").unwrap();
            assert!(
                dispatch_key(
                    &default_keymap(),
                    Vec::new(),
                    keystroke,
                    &context_stack(&app)
                )
                .is_unbound()
            );
        }
    }

    #[test]
    fn inputs_and_dialogs_own_their_keys() {
        let mut input = AppState::new();
        input.git.ui.commit_input.focused = true;
        assert_eq!(
            action(&input, named(NamedKey::Enter, KeyModifiers::empty())),
            None
        );
        assert_eq!(
            action(&input, named(NamedKey::Enter, secondary())),
            Some(Action::GitCommit)
        );
        assert_eq!(
            action(&input, named(NamedKey::Escape, KeyModifiers::empty())),
            None
        );

        let mut closing = editor_app();
        closing.editor.close_request = Some(CloseRequest {
            targets: Vec::new(),
            continuation: CloseContinuation::CloseTabs,
        });
        assert_eq!(
            action(&closing, named(NamedKey::Enter, KeyModifiers::empty())),
            Some(Action::DialogConfirm)
        );
        assert_eq!(
            action(&closing, character("d", secondary())),
            Some(Action::DialogSecondary)
        );
        // Modal: workspace shortcuts do not reach through the dialog.
        assert_eq!(action(&closing, character("b", secondary())), None);
    }

    #[test]
    fn sequences_open_the_keymap_page() {
        let app = AppState::new();
        let keymap = default_keymap();
        let stack = context_stack(&app);
        let first = dispatch_key(
            &keymap,
            Vec::new(),
            Keystroke::parse("secondary-k").unwrap(),
            &stack,
        );
        assert!(!first.pending.is_empty());
        let second = dispatch_key(
            &keymap,
            first.pending,
            Keystroke::parse("secondary-s").unwrap(),
            &stack,
        );
        assert_eq!(second.actions, [Action::OpenKeymap]);
    }
}

#[cfg(test)]
mod vim_tests {
    use super::*;
    use crate::features::keymap::action::Action;
    use crate::features::keymap::bindings::BindingSource;
    use crate::features::keymap::dispatcher::dispatch_key;
    use crate::features::keymap::keymap_file;
    use crate::features::keymap::keystroke::Keystroke;

    fn vim_app() -> AppState {
        let mut app = AppState::new();
        app.workspace
            .open_path("main.rs".into(), "fn main() {}".into());
        app.editor.focused = true;
        app.editor.vim.options.enabled = true;
        crate::app::editor_vim::sync_view(&mut app);
        app
    }

    fn ks(source: &str) -> Keystroke {
        Keystroke::parse(source).unwrap()
    }

    #[test]
    fn vim_takes_conflicting_keys_and_leaves_application_shortcuts() {
        let app = vim_app();
        let stack = context_stack(&app);
        assert_eq!(
            describe(&stack[1..]),
            "Editor extension=rs > Vim mode=normal"
        );
        let keymap = keymap_file::default_keymap();
        for key in ["escape", "ctrl-v", "ctrl-r", "ctrl-d", "left", "enter", "j"] {
            let result = dispatch_key(&keymap, Vec::new(), ks(key), &stack);
            assert!(result.is_unbound(), "{key} goes to Vim");
        }
        let save = dispatch_key(&keymap, Vec::new(), ks("secondary-s"), &stack);
        assert_eq!(save.actions.first(), Some(&Action::Save));
    }

    #[test]
    fn user_mappings_are_key_sequences_in_vim_contexts() {
        let mut app = vim_app();
        let user = keymap_file::parse(
            r#"[{ "context": "Vim && mode == insert", "bindings": { "j k": ["vim::Keys", "<Esc>"] } }]"#,
            BindingSource::User,
        )
        .unwrap();
        assert!(user.errors.is_empty());
        let keymap = keymap_file::build(user.bindings);
        let normal = context_stack(&app);
        assert!(dispatch_key(&keymap, Vec::new(), ks("j"), &normal).is_unbound());

        let mut editor = app.workspace.active_editor_mut().unwrap();
        let mut host = NoHost;
        app.editor.vim.handle_key(
            &mut editor,
            loog_editor_core::vim::Key::Char('i'),
            &mut host,
        );
        drop(editor);
        let insert = context_stack(&app);
        let first = dispatch_key(&keymap, Vec::new(), ks("j"), &insert);
        assert_eq!(first.pending, [ks("j")]);
        let second = dispatch_key(&keymap, first.pending, ks("k"), &insert);
        let Some(Action::VimKeys(keys)) = second.actions.first() else {
            panic!("jk maps to vim::Keys");
        };
        assert_eq!(keys.keys(), "<Esc>");
        // Another key after `j` types the `j` and then itself.
        let first = dispatch_key(&keymap, Vec::new(), ks("j"), &insert);
        let other = dispatch_key(&keymap, first.pending, ks("x"), &insert);
        assert_eq!(other.replay.len(), 1);
        assert!(other.replay[0].actions.is_empty() && other.is_unbound());
    }

    struct NoHost;

    impl loog_editor_core::vim::Clipboard for NoHost {
        fn read(&mut self) -> Result<Option<String>, String> {
            Ok(None)
        }
        fn write(&mut self, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    impl loog_editor_core::vim::Host for NoHost {
        fn page_lines(&self) -> usize {
            10
        }
        fn visible_lines(&self) -> std::ops::Range<usize> {
            0..10
        }
        fn request(&mut self, _: loog_editor_core::vim::Request) {}
    }
}
