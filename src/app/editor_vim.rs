//! Connects the Vim layer to the editor: the active view, keys and text,
//! pointer selections, the clipboard and the application operations Vim
//! asks for.
use crate::app::context::AppContext;

use std::ops::Range;

use lgui::prelude::UiRect;
use lgui::services::Clipboard as SystemClipboard;

use super::editor::reveal_cursor;
use crate::app::state::AppState;
use crate::ui::editor::view::page_lines;
use crate::ui::fonts;
use loog_editor_core::vim::{self, Key, Request, ScrollPosition};

/// The application as Vim's host for one key or command.
struct AppHost<'a> {
    clipboard: &'a dyn SystemClipboard,
    page_lines: usize,
    visible: Range<usize>,
    requests: Vec<Request>,
    display: std::sync::Arc<loog_editor_core::text::DisplayMap>,
    version: u64,
}

impl vim::Clipboard for AppHost<'_> {
    fn read(&mut self) -> Result<Option<String>, String> {
        self.clipboard
            .read_text()
            .map_err(|error| format!("Clipboard: {error}"))
    }

    fn write(&mut self, text: &str) -> Result<(), String> {
        self.clipboard
            .write_text(text)
            .map_err(|error| format!("Clipboard: {error}"))
    }
}

impl vim::Host for AppHost<'_> {
    fn page_lines(&self) -> usize {
        self.page_lines
    }

    fn visible_lines(&self) -> Range<usize> {
        self.display.source_line(self.visible.start)
            ..self.display.source_line(self.visible.end.saturating_sub(1)) + 1
    }

    fn display_map(
        &self,
        buffer: &loog_editor_core::text::TextBuffer,
    ) -> Option<&loog_editor_core::text::DisplayMap> {
        (buffer.version() == self.version && (self.display.wrapped() || self.display.folded()))
            .then_some(self.display.as_ref())
    }
    fn visible_display_rows(&self) -> Range<usize> {
        self.visible.clone()
    }
    fn request(&mut self, request: Request) {
        self.requests.push(request);
    }
}

/// Whether Vim drives the active view: it is enabled and a text file is
/// active in the focused pane.
pub fn is_active(app: &AppState) -> bool {
    app.editor.vim.is_enabled() && app.workspace.active_editor().is_some()
}

/// Keep Vim's state on the active view. When the active view changed, the
/// previous view leaves Vim (ending an insert) before the new one enters.
pub fn sync_view(app: &mut AppState) {
    if !app.editor.vim.is_enabled() {
        return;
    }
    let pane = app.workspace.active_pane();
    let current = app.workspace.active().and_then(|id| {
        let view = app.workspace.view(id).filter(|v| v.accepts_text())?;
        Some(vim::ViewKey {
            pane,
            document: view.document()?,
            view: id,
            input_generation: view.input_generation,
        })
    });
    if app.editor.vim.view() == current {
        return;
    }
    if let Some(previous) = app.editor.vim.view() {
        let mut editor = app.workspace.view_editor_mut(previous.pane, previous.view);
        app.editor.vim.leave_view(editor.as_mut());
    }
    if let Some(view) = current
        && let Some(mut editor) = app.workspace.active_editor_mut()
    {
        app.editor.vim.enter_view(view, &mut editor);
    }
}

/// Turn Vim on or off from Settings.
pub fn set_enabled(app: &mut AppState, enabled: bool) {
    let view = app.editor.vim.view();
    let mut editor = view.and_then(|key| app.workspace.view_editor_mut(key.pane, key.view));
    app.editor.vim.set_enabled(enabled, editor.as_mut());
    drop(editor);
    sync_view(app);
}

fn visible_lines(app: &AppState, rect: UiRect) -> Range<usize> {
    let (_, scroll_y) = app.workspace.active_scroll();
    let first = (scroll_y / fonts::editor().line_h).ceil() as usize;
    let last = ((scroll_y + rect.height()) / fonts::editor().line_h).floor() as usize;
    first..last.max(first + 1)
}

/// Run `f` with Vim, the active editor and a host, then promote an edited
/// preview, follow the cursor and return what Vim asked the application to do.
fn with_vim<R>(
    app: &mut AppState,
    rect: UiRect,
    clipboard: &dyn SystemClipboard,
    f: impl FnOnce(&mut vim::Vim, &mut crate::model::text::EditorMut<'_>, &mut AppHost<'_>) -> R,
) -> Option<(R, Vec<Request>)> {
    if let Some(view) = app.workspace.active() {
        super::editing::configure(app, view);
    }
    sync_view(app);
    let display = super::editing::active_projection(app, rect);
    let version = app.workspace.active_editor()?.version();
    let visible = visible_lines(app, rect);
    let first = visible.start.min(display.len() - 1);
    let last = visible.end.clamp(first + 1, display.len());
    let mut host = AppHost {
        clipboard,
        page_lines: page_lines(rect),
        visible: first..last,
        display,
        version,
        requests: Vec::new(),
    };
    let was_inserting = app.editor.vim.is_inserting();
    let (result, changed) = {
        let workspace = &mut app.workspace;
        let vim = &mut app.editor.vim;
        let mut editor = workspace.active_editor_mut()?;
        let version = editor.version();
        let result = f(vim, &mut editor, &mut host);
        (result, editor.version() != version)
    };
    if was_inserting && !app.editor.vim.is_inserting() {
        super::language_ui::snippet_session::end(app);
        super::language_ui::completion::close(app);
        app.editor.language.completion_timer = None;
    }
    if changed {
        app.workspace.promote_active_preview();
    }
    app.editor.menu = None;
    app.editor.drag = None;
    let mut requests = Vec::new();
    for request in host.requests {
        match request {
            Request::Scroll(position) => scroll_cursor(app, rect, position),
            other => requests.push(other),
        }
    }
    reveal_cursor(app, rect);
    Some((result, requests))
}

/// Offer a key the keymap did not claim to Vim. Returns whether Vim used it.
pub fn handle_key(
    state: &AppContext,
    key: Key,
    rect: UiRect,
    clipboard: &dyn SystemClipboard,
) -> bool {
    let mut consumed = false;
    let mut requests = Vec::new();
    state.update(|app| {
        if !is_active(app) {
            return;
        }
        let scrolls_page = matches!(key, Key::Ctrl('d' | 'u' | 'f' | 'b'));
        let before = app.workspace.active_editor().map(|editor| {
            super::editing::active_projection(app, rect).caret_row(
                &editor,
                editor.cursor(),
                editor.selection_state().wrap_affinity,
            )
        });
        if let Some((used, asked)) = with_vim(app, rect, clipboard, |vim, editor, host| {
            vim.handle_key(editor, key, host)
        }) {
            consumed = used;
            requests = asked;
        }
        if scrolls_page {
            // Page keys scroll the view with the cursor, as Vim does.
            let after = app.workspace.active_editor().map(|editor| {
                super::editing::active_projection(app, rect).caret_row(
                    &editor,
                    editor.cursor(),
                    editor.selection_state().wrap_affinity,
                )
            });
            if let (Some(before), Some(after)) = (before, after) {
                let (x, y) = app.workspace.active_scroll();
                let delta = (after as f32 - before as f32) * fonts::editor().line_h;
                app.workspace.set_active_scroll(x, (y + delta).max(0.0));
                reveal_cursor(app, rect);
            }
        }
    });
    perform(state, requests, rect);
    consumed
}

/// Send the keys of a `vim::Keys` mapping to Vim. Returns false when Vim does
/// not drive the editor, so other bindings for the keystroke can run.
pub fn feed_keys(
    state: &AppContext,
    keys: &str,
    rect: UiRect,
    clipboard: &dyn SystemClipboard,
) -> bool {
    let keys = Key::parse(keys);
    let mut fed = false;
    let mut requests = Vec::new();
    state.update(|app| {
        if !is_active(app) {
            return;
        }
        if let Some(((), asked)) = with_vim(app, rect, clipboard, |vim, editor, host| {
            vim.feed_keys(editor, &keys, host)
        }) {
            fed = true;
            requests = asked;
        }
    });
    perform(state, requests, rect);
    fed
}

/// Copy or cut the visual selection through Vim's `"+` register. Returns
/// whether Vim handled it (it was in Visual mode).
pub fn visual_clipboard(
    app: &mut AppState,
    cut: bool,
    rect: UiRect,
    clipboard: &dyn SystemClipboard,
) -> bool {
    if !is_active(app) || !matches!(app.editor.vim.mode(), vim::Mode::Visual(_)) {
        return false;
    }
    let keys = if cut { "\"+d" } else { "\"+y" };
    with_vim(app, rect, clipboard, |vim, editor, host| {
        for key in Key::parse(keys) {
            vim.handle_key(editor, key, host);
        }
    });
    true
}

/// Text input for Vim. Returns whether Vim took it; in Normal and Visual mode
/// typed text is not a command and is left to the caller.
pub fn handle_text(app: &mut AppState, text: &str, rect: UiRect) -> bool {
    handle_input(app, text, rect, false)
}
pub fn handle_commit(app: &mut AppState, text: &str, rect: UiRect) -> bool {
    handle_input(app, text, rect, true)
}
fn handle_input(app: &mut AppState, text: &str, rect: UiRect, literal: bool) -> bool {
    if !is_active(app) {
        return false;
    }
    if let Some(view) = app.workspace.active() {
        super::editing::configure(app, view);
    }
    sync_view(app);
    let handled = {
        let workspace = &mut app.workspace;
        let vim = &mut app.editor.vim;
        let Some(mut editor) = workspace.active_editor_mut() else {
            return false;
        };
        let version = editor.version();
        let handled = if literal {
            vim.handle_commit(&mut editor, text)
        } else {
            vim.handle_text(&mut editor, text)
        };
        if editor.version() != version {
            drop(editor);
            workspace.promote_active_preview();
        }
        handled
    };
    if handled {
        reveal_cursor(app, rect);
    }
    handled
}

/// An edit the default editing commands made while Vim is typing, so `.`
/// and counts repeat it; `None` for a cursor move.
pub fn start_multi(app: &mut AppState) {
    if !is_active(app) {
        return;
    }
    if let Some(view) = app.workspace.active() {
        super::editing::configure(app, view);
    }
    sync_view(app);
    if let Some(mut editor) = app.workspace.active_editor_mut() {
        app.editor.vim.start_multi_insert(&mut editor);
    }
}
pub fn prepare_external(app: &mut AppState) {
    if !is_active(app) {
        return;
    }
    if let Some(mut editor) = app.workspace.active_editor_mut() {
        app.editor.vim.prepare_external(&mut editor);
    }
}
pub fn record_insert(app: &mut AppState, event: Option<vim::InsertEvent>) {
    if is_active(app) && app.editor.vim.is_inserting() {
        app.editor.vim.record_insert(event);
    }
}

/// After the pointer moved the cursor or selected text.
pub fn after_pointer(app: &mut AppState) {
    if !is_active(app) {
        return;
    }
    sync_view(app);
    let workspace = &mut app.workspace;
    let vim = &mut app.editor.vim;
    if let Some(mut editor) = workspace.active_editor_mut() {
        vim.after_pointer(&mut editor);
    }
}

/// After a default editing command changed the selection (select all,
/// undo), let Vim take over the selection in Normal and Visual mode.
pub fn after_command(app: &mut AppState, undo: bool) {
    if !is_active(app) || app.editor.vim.is_inserting() {
        return;
    }
    sync_view(app);
    let workspace = &mut app.workspace;
    let vim = &mut app.editor.vim;
    if let Some(mut editor) = workspace.active_editor_mut() {
        if undo {
            // Like `u`: the cursor goes to the change, without a selection.
            let selection = editor.selection_state();
            let start = selection
                .range()
                .map_or(selection.cursor, |range| range.start);
            editor.set_cursor(start);
        }
        vim.adopt_selection(&mut editor);
    }
}

fn scroll_cursor(app: &mut AppState, rect: UiRect, position: ScrollPosition) {
    if app.workspace.active_editor().is_none() {
        return;
    }
    let map = super::editing::active_projection(app, rect);
    let editor = app.workspace.active_editor().unwrap();
    let row = map.caret_row(
        &editor,
        editor.cursor(),
        editor.selection_state().wrap_affinity,
    );
    let top = row as f32 * fonts::editor().line_h;
    let height = rect.height();
    let y = match position {
        ScrollPosition::Top => top,
        ScrollPosition::Center => top - (height - fonts::editor().line_h) / 2.0,
        ScrollPosition::Bottom => top + fonts::editor().line_h - height,
    };
    let (x, _) = app.workspace.active_scroll();
    app.workspace.set_active_scroll(x, y.max(0.0));
}

/// Carry out what Vim asked of the application.
fn perform(state: &AppContext, requests: Vec<Request>, rect: UiRect) {
    for request in requests {
        match request {
            Request::Save => {
                let document = state.get().workspace.active_document();
                crate::app::language_ui::save::save(
                    state,
                    rect,
                    std::sync::Arc::new(move |state| {
                        if let Some(document) = document {
                            crate::app::workspace_commands::save_document(state, document);
                        }
                    }),
                )
            }
            Request::Quit { force } => quit(state, force),
            Request::SaveAndQuit => {
                let document = state.get().workspace.active_document();
                let view = state.get().workspace.active();
                crate::app::language_ui::save::save(
                    state,
                    rect,
                    std::sync::Arc::new(move |state| {
                        if let Some(document) = document {
                            crate::app::saving::document(state, document, move |state, saved| {
                                if saved
                                    && state.get().workspace.active_document() == Some(document)
                                    && state.get().workspace.active() == view
                                {
                                    quit(state, false);
                                }
                            });
                        }
                    }),
                )
            }
            Request::Scroll(_) => {}
            Request::Fold { open, all } => {
                use super::folding::FoldAction;
                let action = match (open, all) {
                    (false, false) => FoldAction::Fold,
                    (true, false) => FoldAction::Unfold,
                    (false, true) => FoldAction::FoldAll,
                    (true, true) => FoldAction::UnfoldAll,
                };
                super::folding::command(state, action, rect);
            }
            Request::Language(command) => {
                crate::app::language_ui::keys::vim_command(state, command, rect)
            }
        }
    }
}

/// `:q` closes the active tab. Unsaved changes stop it unless forced or the
/// document is still open in another tab.
fn quit(state: &AppContext, force: bool) {
    state.update(move |app| {
        let pane = app.workspace.active_pane();
        let Some(document) = app.workspace.active() else {
            return;
        };
        let shared = app
            .workspace
            .tabs_of(app.workspace.view_document(document).unwrap())
            .len()
            > 1;
        if !force && !shared && app.workspace.is_view_dirty(document) {
            app.editor
                .vim
                .show_message("E37: No write since last change (add ! to override)", true);
            return;
        }
        sync_view(app);
        let view = app.editor.vim.view();
        let mut editor = view.and_then(|key| app.workspace.view_editor_mut(key.pane, key.view));
        app.editor.vim.leave_view(editor.as_mut());
        drop(editor);
        app.workspace.close_item(pane, document);
        sync_view(app);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::pane_layout::Direction;
    use lgui::services::ClipboardError;

    #[test]
    fn p1_source_presentation_generation_ends_vim_insert_even_after_round_trip() {
        use crate::domain::view::Presentation;
        let mut app = AppState::new();
        app.workspace.open_path("mode.md".into(), "abc".into());
        let view = app.workspace.active().unwrap();
        app.editor.vim.options.enabled = true;
        keys(&mut app, "i");
        assert_eq!(app.editor.vim.mode(), vim::Mode::Insert);
        app.workspace.set_presentation(view, Presentation::Markdown);
        app.workspace.set_presentation(view, Presentation::Source);
        sync_view(&mut app);
        assert_eq!(app.editor.vim.mode(), vim::Mode::Normal);
        assert_eq!(app.editor.vim.view().unwrap().input_generation, 2);
        app.workspace.set_presentation(view, Presentation::Markdown);
        sync_view(&mut app);
        assert!(app.editor.vim.view().is_none());
        assert!(!is_active(&app));
    }

    struct NoClipboard;

    impl SystemClipboard for NoClipboard {
        fn read_text(&self) -> Result<Option<String>, ClipboardError> {
            Ok(None)
        }
        fn write_text(&self, _: &str) -> Result<(), ClipboardError> {
            Ok(())
        }
    }

    const RECT: UiRect = UiRect {
        left: 0.0,
        top: 0.0,
        right: 600.0,
        bottom: 400.0,
    };

    fn keys(app: &mut AppState, keys: &str) {
        for key in Key::parse(keys) {
            let used = with_vim(app, RECT, &NoClipboard, |vim, editor, host| {
                vim.handle_key(editor, key, host)
            });
            if used.is_some_and(|(used, _)| !used)
                && let Key::Char(c) = key
            {
                handle_text(app, &c.to_string(), RECT);
            }
        }
    }

    fn text(app: &AppState) -> String {
        app.workspace.active_editor().unwrap().text().to_owned()
    }

    #[test]
    fn switching_panes_ends_the_insert_and_shares_text_and_history() {
        let mut app = AppState::new();
        app.workspace
            .open_path("shared.txt".into(), "one\ntwo".into());
        app.editor.vim.options.enabled = true;
        let left = app.workspace.active_pane();
        let right = app.workspace.split(left, Direction::Right).unwrap();
        app.workspace.activate_pane(left);
        keys(&mut app, "jA!");
        assert_eq!(app.editor.vim.mode(), vim::Mode::Insert);
        keys(&mut app, "?");

        // The other pane: the left pane's insert ends, this pane starts in
        // Normal mode and sees the shared text.
        app.workspace.activate_pane(right);
        sync_view(&mut app);
        assert_eq!(app.editor.vim.mode(), vim::Mode::Normal);
        assert_eq!(app.editor.vim.view().map(|view| view.pane), Some(right));
        assert_eq!(text(&app), "one\ntwo!?");
        keys(&mut app, "ggI> <Esc>");
        assert_eq!(text(&app), "> one\ntwo!?");
        // The left view's cursor followed the edit made in the right pane.
        assert_eq!(app.workspace.editor(left).unwrap().cursor(), 11);

        // One history: undo from the right pane reverses its insert, then
        // the left pane's insert as a single step.
        keys(&mut app, "u");
        assert_eq!(text(&app), "one\ntwo!?");
        keys(&mut app, "u");
        assert_eq!(text(&app), "one\ntwo");
        app.workspace.activate_pane(left);
        keys(&mut app, "<C-r>");
        assert_eq!(text(&app), "one\ntwo!?");
        assert_eq!(app.workspace.editor(left).unwrap().cursor(), 7);
    }

    #[test]
    fn clicking_leaves_visual_mode_and_dragging_enters_it() {
        let mut app = AppState::new();
        app.workspace.open_path("click.txt".into(), "abcdef".into());
        app.editor.vim.options.enabled = true;
        keys(&mut app, "vl");
        assert!(matches!(app.editor.vim.mode(), vim::Mode::Visual(_)));
        app.workspace.active_editor_mut().unwrap().set_cursor(4);
        after_pointer(&mut app);
        assert_eq!(app.editor.vim.mode(), vim::Mode::Normal);
        app.workspace
            .active_editor_mut()
            .unwrap()
            .select_range(1..3);
        after_pointer(&mut app);
        assert!(matches!(app.editor.vim.mode(), vim::Mode::Visual(_)));
        keys(&mut app, "d");
        assert_eq!(text(&app), "adef");
    }
}
