//! Terminal commands: identity, model/layout ordering and runtime dispatch.
use crate::app::context::AppContext;
use crate::app::state::AppState;
use crate::app::terminal_binding::TerminalStore;
use crate::features::terminal::actions::{MenuAction, close_targets_in};
use crate::features::terminal::{SessionId, ShellKind};
use crate::infrastructure::terminal::runtime::TerminalHandle;
use lgui::core::UiFocusHandle;
use lgui::services::{Clipboard, ClipboardError};
use std::path::PathBuf;

pub(crate) fn visibility(app: &AppState) -> crate::domain::application_layout::Visibility {
    crate::domain::application_layout::Visibility {
        git: app.source_control.visible,
        files: app.drawer.visible,
        terminal: app.terminal_chrome.show_terminal,
    }
}
pub(super) fn target(
    tabs: &TerminalStore,
    id: SessionId,
) -> Option<crate::app::terminal_binding::TerminalTab> {
    tabs.get().tabs().into_iter().find(|tab| tab.id == id)
}
/// Check the completed UI round, after both stores have been updated.
fn assert_ownership(app: &AppState, tabs: &crate::app::terminal_binding::TerminalTabs) {
    debug_assert!(
        tabs.active_id()
            .is_none_or(|id| tabs.tabs().iter().any(|tab| tab.id == id))
    );
    if let Some(layout) = &app.layout.application_layout {
        let owned = layout
            .leaves()
            .into_iter()
            .flat_map(|(_, content)| match content {
                crate::domain::application_layout::Content::Terminal { tabs, active } => {
                    debug_assert!(active.is_none_or(|id| tabs.contains(&id)));
                    tabs
                }
                _ => Vec::new(),
            })
            .collect::<Vec<_>>();
        let ids = tabs
            .tabs()
            .into_iter()
            .map(|tab| tab.id)
            .collect::<std::collections::HashSet<_>>();
        debug_assert_eq!(owned.len(), ids.len());
        debug_assert_eq!(
            owned.into_iter().collect::<std::collections::HashSet<_>>(),
            ids
        );
    }
}
fn sync_layout(state: &AppContext, tabs: &TerminalStore) {
    let current = tabs.get();
    state.update(|app| {
        if let Some(layout) = &mut app.layout.application_layout {
            current.sync_layout(layout);
        }
        assert_ownership(app, &current);
    });
}
pub(crate) fn new(
    state: &AppContext,
    tabs: &TerminalStore,
    shell: ShellKind,
    cwd: Option<PathBuf>,
) -> SessionId {
    new_beside(state, tabs, shell, cwd, None)
}
/// Like [`new`], placing the session in the region of `beside` when given, so a
/// terminal created from a detached region stays in it.
fn new_beside(
    state: &AppContext,
    tabs: &TerminalStore,
    shell: ShellKind,
    cwd: Option<PathBuf>,
    beside: Option<SessionId>,
) -> SessionId {
    let mut id = 0;
    tabs.update(|tabs| id = tabs.add_at(shell, cwd));
    state.update(|app| {
        if let Some(layout) = &mut app.layout.application_layout {
            match beside {
                Some(beside) => layout.attach_beside(id, beside),
                None => layout.attach(id),
            };
        }
    });
    sync_layout(state, tabs);
    id
}
pub(crate) fn new_from_target(
    state: &AppContext,
    tabs: &TerminalStore,
    id: SessionId,
    shell: Option<ShellKind>,
) -> Option<SessionId> {
    let tab = target(tabs, id)?;
    request_new(
        state,
        tabs,
        Some(id),
        Some(shell.unwrap_or_else(|| tab.controller.shell())),
    )
}

fn new_workspace_directory(app: &AppState) -> Option<PathBuf> {
    let roots = app.explorer.model.roots();
    app.workspace
        .active_path()
        .and_then(|path| app.workspace_folder_of(path))
        .map(std::path::Path::to_path_buf)
        .or_else(|| {
            app.terminal
                .ui
                .terminal_workspace_root
                .as_ref()
                .filter(|root| roots.contains(root))
                .cloned()
        })
        .or_else(|| (roots.len() == 1).then(|| roots[0].clone()))
}

fn request_new(
    state: &AppContext,
    tabs: &TerminalStore,
    target: Option<SessionId>,
    shell: Option<ShellKind>,
) -> Option<SessionId> {
    let app = state.get();
    let cwd = new_workspace_directory(&app);
    if cwd.is_none() && app.explorer.model.roots().len() > 1 {
        state.update(|app| {
            app.keymap.clear_pending();
            app.terminal.ui.terminal_shell_menu = false;
            app.terminal.ui.terminal_tab_context_menu = None;
            app.terminal.ui.terminal_workspace_picker =
                Some(crate::ui::terminal::state::WorkspacePicker {
                    target,
                    shell,
                    selected: 0,
                    scroll: 0,
                });
        });
        return None;
    }
    let shell = shell.unwrap_or_else(|| app.effective_shell_at(cwd.as_deref()));
    Some(new_beside(state, tabs, shell, cwd, target))
}

pub(crate) fn choose_workspace(
    state: &AppContext,
    tabs: &TerminalStore,
    root: PathBuf,
) -> Option<SessionId> {
    let app = state.get();
    let picker = app.terminal.ui.terminal_workspace_picker.as_ref()?;
    if !app.explorer.model.roots().contains(&root) {
        return None;
    }
    if picker.target.is_some_and(|id| target(tabs, id).is_none()) {
        state.update(|app| app.terminal.ui.terminal_workspace_picker = None);
        return None;
    }
    let shell = picker
        .shell
        .unwrap_or_else(|| app.effective_shell_at(Some(&root)));
    let id = new_beside(state, tabs, shell, Some(root.clone()), picker.target);
    state.update(|app| {
        app.terminal.ui.terminal_workspace_root = Some(root);
        app.terminal.ui.terminal_workspace_picker = None;
        app.terminal_chrome.show_terminal = true;
    });
    Some(id)
}
pub(crate) fn select(state: &AppContext, tabs: &TerminalStore, id: SessionId) -> bool {
    if target(tabs, id).is_none() {
        return false;
    }
    tabs.update(|tabs| tabs.select(id));
    sync_layout(state, tabs);
    true
}
pub(crate) fn select_panel(
    state: &AppContext,
    tabs: &TerminalStore,
    panel: crate::ui::terminal::state::PanelTab,
) {
    use crate::ui::terminal::state::PanelTab;
    state.update(|app| {
        app.terminal.ui.panel_tab = panel;
        app.terminal_chrome.show_terminal = true;
        app.terminal.ui.terminal_focused = false;
        app.terminal.ui.terminal_tab_context_menu = None;
        app.terminal.ui.terminal_shell_menu = false;
        app.terminal.ui.terminal_workspace_picker = None;
        app.git.ui.menu = None;
        app.git.ui.hover = None;
        app.git.ui.scroll_hover = None;
        app.git.ui.clear_hover_cards();
        if panel == PanelTab::Commits {
            if app.git.ui.panel_tab != crate::ui::git::panel::HISTORY {
                app.git.ui.select_panel_tab(crate::ui::git::panel::HISTORY);
            } else {
                app.git.ui.set_panel(true);
            }
        }
    });
    if panel == PanelTab::Terminal {
        let app = state.get();
        let session = app.layout.application_layout.as_ref().map_or_else(
            || tabs.get().active_id(),
            |layout| layout.default_terminal_active(),
        );
        if let Some(id) = session {
            select(state, tabs, id);
        } else {
            request_new(state, tabs, None, None);
        }
    }
}
pub(crate) fn split(state: &AppContext, tabs: &TerminalStore, id: SessionId) -> Option<SessionId> {
    target(tabs, id)?;
    let mut created = None;
    tabs.update(|tabs| created = tabs.split(id));
    if let Some(created) = created {
        state.update(|app| {
            if let Some(layout) = &mut app.layout.application_layout {
                layout.attach_split(created, id);
            }
        });
    }
    sync_layout(state, tabs);
    created
}
/// Model edit, then layout prune, then process disposal. State locks never nest.
pub(crate) fn close(
    state: &AppContext,
    tabs: &TerminalStore,
    id: SessionId,
    action: MenuAction,
    application: &std::sync::Arc<lgui::ApplicationHandle>,
) -> bool {
    if target(tabs, id).is_none() {
        return false;
    }
    let region = state
        .get()
        .layout
        .application_layout
        .as_ref()
        .and_then(|layout| {
            layout
                .terminal_region(id)
                .and_then(|region| layout.terminal_view(region))
        })
        .map(|(ids, _)| ids)
        .unwrap_or_default();
    let ids = close_targets_in(tabs.get().model(), id, action, &region);
    let mut handles = Vec::new();
    tabs.update(|tabs| {
        for id in ids {
            if let Some(handle) = tabs.close(id) {
                handles.push(handle);
            }
        }
    });
    sync_layout(state, tabs);
    for handle in handles {
        handle.terminate(application);
    }
    if tabs.get().is_empty() {
        state.update(|app| {
            app.terminal_chrome.show_terminal = false;
            app.terminal.ui.terminal_shell_menu = false;
            app.terminal.ui.terminal_focused = false;
        });
    }
    true
}
pub(crate) fn menu_action(
    state: &AppContext,
    tabs: &TerminalStore,
    id: SessionId,
    action: MenuAction,
    application: &std::sync::Arc<lgui::ApplicationHandle>,
) -> bool {
    state.update(|app| app.terminal.ui.terminal_tab_context_menu = None);
    if target(tabs, id).is_none() {
        return false;
    }
    match action {
        MenuAction::New => new_from_target(state, tabs, id, None).is_some(),
        MenuAction::Split => split(state, tabs, id).is_some(),
        _ => close(state, tabs, id, action, application),
    }
}
pub(crate) fn purpose(
    state: &AppContext,
    tabs: &TerminalStore,
    id: SessionId,
    purpose: Option<&str>,
) {
    if target(tabs, id).is_some() {
        tabs.update(|tabs| tabs.set_purpose(id, purpose));
    }
    state.update(|app| app.terminal.ui.terminal_tab_context_menu = None);
}
/// Names a session for good; blank text restores its automatic name.
pub(crate) fn set_title(tabs: &TerminalStore, id: SessionId, title: Option<&str>) {
    if target(tabs, id).is_some() {
        tabs.update(|tabs| tabs.set_title(id, title));
    }
}
pub(crate) fn begin_rename(state: &AppContext, tabs: &TerminalStore, id: SessionId) -> bool {
    let Some(tab) = target(tabs, id) else {
        return false;
    };
    let current = tab
        .title
        .clone()
        .unwrap_or_else(|| tab.controller.short_label());
    state.update(|app| {
        app.terminal.ui.terminal_tab_context_menu = None;
        app.terminal.ui.terminal_rename = Some(crate::ui::terminal::state::RenameEditor {
            target: id,
            input: crate::ui::components::input::InputState::new_selected(current),
        });
    });
    true
}
pub(crate) fn commit_rename(state: &AppContext, tabs: &TerminalStore) {
    let Some(editor) = state.get().terminal.ui.terminal_rename else {
        return;
    };
    state.update(|app| app.terminal.ui.terminal_rename = None);
    set_title(tabs, editor.target, Some(editor.input.text()));
}
pub(crate) fn cancel_rename(state: &AppContext) {
    state.update(|app| app.terminal.ui.terminal_rename = None);
}
/// Opens the find box over a session, or closes it when it is already there.
pub(crate) fn toggle_search(state: &AppContext, tabs: &TerminalStore, id: SessionId) -> bool {
    if target(tabs, id).is_none() {
        return false;
    }
    if state
        .get()
        .terminal
        .ui
        .terminal_search
        .is_some_and(|bar| bar.target == id)
    {
        close_search(state, tabs);
        return true;
    }
    close_search(state, tabs);
    state.update(|app| {
        app.terminal.ui.terminal_tab_context_menu = None;
        app.terminal.ui.terminal_search = Some(crate::ui::terminal::state::SearchBar {
            target: id,
            input: crate::ui::components::input::InputState::new(""),
            status: Default::default(),
        });
    });
    true
}
/// Searches for what the find box holds and shows the last match.
pub(crate) fn run_search(state: &AppContext, tabs: &TerminalStore) {
    let Some(bar) = state.get().terminal.ui.terminal_search else {
        return;
    };
    let Some(tab) = target(tabs, bar.target) else {
        return;
    };
    let status = if bar.input.text().is_empty() {
        tab.controller.search_clear();
        Default::default()
    } else {
        tab.controller.search(bar.input.text())
    };
    state.update(|app| {
        if let Some(bar) = app.terminal.ui.terminal_search.as_mut() {
            bar.status = status;
        }
    });
}
pub(crate) fn search_step(state: &AppContext, tabs: &TerminalStore, delta: i32) {
    let Some(bar) = state.get().terminal.ui.terminal_search else {
        return;
    };
    let Some(tab) = target(tabs, bar.target) else {
        return;
    };
    let status = tab.controller.search_step(delta);
    state.update(|app| {
        if let Some(bar) = app.terminal.ui.terminal_search.as_mut() {
            bar.status = status;
        }
    });
}
pub(crate) fn close_search(state: &AppContext, tabs: &TerminalStore) {
    let Some(bar) = state.get().terminal.ui.terminal_search else {
        return;
    };
    if let Some(tab) = target(tabs, bar.target) {
        tab.controller.search_clear();
    }
    state.update(|app| app.terminal.ui.terminal_search = None);
}
/// Fills the split the default terminal region sits in, or restores it.
pub(crate) fn toggle_maximize(state: &AppContext) -> bool {
    let mut changed = false;
    state.update(|app| {
        if let Some(layout) = app.layout.application_layout.as_mut()
            && let Some(region) = layout.default_terminal_region()
        {
            changed = layout.toggle_maximize(region);
        }
    });
    changed
}
pub(crate) fn clear_output(state: &AppContext, tabs: &TerminalStore, id: SessionId) -> bool {
    state.update(|app| app.terminal.ui.terminal_tab_context_menu = None);
    let Some(tab) = target(tabs, id) else {
        return false;
    };
    tab.controller.clear_output();
    true
}
pub(crate) fn resize_pair(tabs: &TerminalStore, left: SessionId, right: SessionId, fraction: f32) {
    if target(tabs, left).is_some() && target(tabs, right).is_some() {
        tabs.update(|tabs| tabs.resize_pair(left, right, fraction));
    }
}
/// Toggle only the default terminal region. Detached sessions retain their
/// visibility and focus when that region is hidden.
pub fn toggle_panel(
    state: &AppContext,
    terminal_tabs: &crate::app::terminal_binding::TerminalStore,
    editor_focus: &UiFocusHandle,
    terminal_focus: &UiFocusHandle,
) {
    let (opening, active_visible) = apply_toggle(state, terminal_tabs);
    if opening
        && state.get().terminal.ui.panel_tab == crate::ui::terminal::state::PanelTab::Terminal
    {
        terminal_focus.focus();
    } else if opening || !active_visible {
        editor_focus.focus();
    }
}
/// Hides the default terminal region from the panel's own collapse icon.
/// Returns false when it was not shown. The caller moves focus to the editor.
pub(crate) fn collapse_panel(
    state: &AppContext,
    terminal_tabs: &crate::app::terminal_binding::TerminalStore,
) -> bool {
    if !state.get().default_terminal_open() {
        return false;
    }
    apply_toggle(state, terminal_tabs);
    true
}
/// The state half of [`toggle_panel`]: returns whether the region is now open and
/// whether the active session is still visible.
fn apply_toggle(
    state: &AppContext,
    terminal_tabs: &crate::app::terminal_binding::TerminalStore,
) -> (bool, bool) {
    let app = state.get();
    let opening = !app.default_terminal_open();
    let default_session = app
        .layout
        .application_layout
        .as_ref()
        .and_then(|layout| layout.default_terminal_active())
        .or_else(|| {
            app.layout
                .application_layout
                .is_none()
                .then(|| terminal_tabs.get().active_id())
                .flatten()
        });
    let mut selected = default_session;
    if opening
        && selected.is_none()
        && app.terminal.ui.panel_tab == crate::ui::terminal::state::PanelTab::Terminal
    {
        selected = request_new(state, terminal_tabs, None, None);
    }
    if opening
        && app.terminal.ui.panel_tab == crate::ui::terminal::state::PanelTab::Terminal
        && let Some(id) = selected
    {
        terminal_tabs.update(|tabs| tabs.select(id));
    }
    let active = terminal_tabs.get().active_id();
    let mut active_visible = false;
    state.update(|app| {
        if opening
            && let Some(id) = selected
            && let Some(layout) = &mut app.layout.application_layout
        {
            layout.attach(id);
        }
        app.terminal_chrome.show_terminal = opening;
        let visible = visibility(app);
        active_visible = active.is_some_and(|id| {
            app.layout
                .application_layout
                .as_ref()
                .map_or(opening, |layout| layout.terminal_visible(id, visible))
        });
        let hidden_menu = app
            .terminal
            .ui
            .terminal_tab_context_menu
            .as_ref()
            .is_some_and(|menu| {
                app.layout
                    .application_layout
                    .as_ref()
                    .is_some_and(|layout| !layout.terminal_visible(menu.target, visible))
            });
        if opening || !active_visible || hidden_menu {
            app.terminal.ui.terminal_tab_context_menu = None;
            app.terminal.ui.terminal_shell_menu = false;
        }
    });
    sync_layout(state, terminal_tabs);
    (opening, active_visible)
}
/// The terminal's clipboard actions (`terminal::Copy` and friends).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardShortcut {
    Copy,
    /// Copy when text is selected and otherwise interrupt, matching Windows Terminal.
    CopyOrInterrupt,
    Paste,
    CopyOrPaste,
}

fn run_clipboard_shortcut(
    controller: &TerminalHandle,
    shortcut: ClipboardShortcut,
    clipboard: &dyn Clipboard,
) -> Result<(), ClipboardError> {
    match shortcut {
        ClipboardShortcut::Copy => copy_selection(controller, clipboard).map(|_| ()),
        ClipboardShortcut::CopyOrInterrupt => copy_selection(controller, clipboard).map(|copied| {
            if !copied {
                controller.write(b"\x03");
            }
        }),
        ClipboardShortcut::CopyOrPaste => match copy_selection(controller, clipboard)? {
            true => Ok(()),
            false => paste_clipboard(controller, clipboard),
        },
        ClipboardShortcut::Paste => paste_clipboard(controller, clipboard),
    }
}

fn copy_selection(
    controller: &TerminalHandle,
    clipboard: &dyn Clipboard,
) -> Result<bool, ClipboardError> {
    let Some(value) = controller.selected_text() else {
        return Ok(false);
    };
    // A failed copy keeps the selection so the user can retry.
    clipboard.write_text(&value)?;
    controller.clear_selection();
    Ok(true)
}

fn paste_clipboard(
    controller: &TerminalHandle,
    clipboard: &dyn Clipboard,
) -> Result<(), ClipboardError> {
    if let Some(value) = clipboard.read_text()? {
        controller.paste(&value);
    }
    Ok(())
}

pub fn report_clipboard_error(state: &AppContext, result: Result<(), ClipboardError>) {
    if let Err(error) = result {
        state.update(|app| app.show_error(format!("Clipboard: {error}")));
    }
}

pub(crate) fn protocol_input(tabs: &TerminalStore, id: SessionId, bytes: &[u8]) -> bool {
    let Some(tab) = target(tabs, id) else {
        return false;
    };
    tab.controller.write_protocol(bytes);
    true
}
pub(crate) fn write(tabs: &TerminalStore, id: SessionId, bytes: &[u8]) -> bool {
    let Some(tab) = target(tabs, id) else {
        return false;
    };
    tab.controller.write(bytes);
    true
}
pub(crate) fn clipboard(
    tabs: &TerminalStore,
    id: SessionId,
    shortcut: ClipboardShortcut,
    clipboard: &dyn Clipboard,
) -> Result<(), ClipboardError> {
    let Some(tab) = target(tabs, id) else {
        return Ok(());
    };
    run_clipboard_shortcut(&tab.controller, shortcut, clipboard)
}
pub(crate) fn mouse_position(tabs: &TerminalStore, id: SessionId, row: u16, col: u16) {
    if let Some(tab) = target(tabs, id) {
        tab.controller.set_mouse_position(row, col);
    }
}
pub(crate) fn begin_selection(tabs: &TerminalStore, id: SessionId, row: u16, col: u16) {
    if let Some(tab) = target(tabs, id) {
        tab.controller.begin_selection(row, col);
    }
}
/// The link at a cell of the session's screen, if there is one.
pub(crate) fn url_at(tabs: &TerminalStore, id: SessionId, row: u16, cell: u16) -> Option<String> {
    target(tabs, id)?.controller.url_at(row, cell)
}
/// Underlines the link at `cell`, or none. Returns whether anything changed.
pub(crate) fn link_hover(tabs: &TerminalStore, id: SessionId, cell: Option<(u16, u16)>) -> bool {
    target(tabs, id).is_some_and(|tab| tab.controller.set_link_hover(cell))
}
pub(crate) fn press_selection(
    tabs: &TerminalStore,
    id: SessionId,
    row: u16,
    col: u16,
    cell: u16,
    extend: bool,
) {
    if let Some(tab) = target(tabs, id) {
        tab.controller.press_selection(row, col, cell, extend);
    }
}
pub(crate) fn extend_selection(tabs: &TerminalStore, id: SessionId, row: u16, col: u16) {
    if let Some(tab) = target(tabs, id) {
        tab.controller.extend_selection(row, col);
    }
}
/// Scrolls history while a selection drag rests outside the screen and drags
/// the selection head along. Returns whether anything moved.
pub(crate) fn selection_scroll_tick(state: &AppContext, tabs: &TerminalStore) -> bool {
    let Some(drag) = state.get().terminal.ui.selection_drag else {
        return false;
    };
    let rows = crate::ui::terminal::view::selection_scroll_rows(drag.content, drag.point.y);
    if rows == 0 {
        return false;
    }
    let Some(tab) = target(tabs, drag.session) else {
        return false;
    };
    tab.controller.scroll(rows);
    let (row, col) =
        crate::ui::terminal::view::cell_at(drag.content, drag.point, drag.rows, drag.cols);
    tab.controller.extend_selection(row, col);
    true
}
pub(crate) fn wheel(
    tabs: &TerminalStore,
    id: SessionId,
    delta: f32,
    modifiers: crate::features::terminal::input::Modifiers,
) {
    if let Some(tab) = target(tabs, id) {
        tab.controller.wheel(delta, modifiers);
    }
}

pub(crate) fn set_scrollback(tabs: &TerminalStore, id: SessionId, rows: usize) {
    if let Some(tab) = target(tabs, id) {
        tab.controller.set_scrollback(rows);
    }
}

pub(crate) fn shell_menu(state: &AppContext, tabs: &TerminalStore, id: SessionId) -> bool {
    if !select(state, tabs, id) {
        return false;
    }
    state.update(|app| {
        app.terminal.ui.terminal_shell_menu = !app.terminal.ui.terminal_shell_menu;
        if app.terminal.ui.terminal_shell_menu {
            app.terminal.ui.terminal_shell_scroll = 0;
            app.refresh_shells();
        }
    });
    true
}
