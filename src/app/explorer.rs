//! Explorer commands: open, add and remove workspace folders, inline create
//! and rename, trash, reveal, and opening files from the drawers. Each
//! command keeps Workspace and Explorer changes in one root-state update;
//! disk access goes through the Explorer filesystem adapter.
use crate::app::context::AppContext;

use std::path::{Path, PathBuf};

use lgui::dialogs::{FileDialogOptions, system_file_dialogs};

use crate::app::state::{AppState, TabDrop};
use crate::app::workspace_commands::read_text_file;
use crate::app::workspace_session as workspace_persistence;
use crate::features::explorer::model::Reveal;
use crate::features::explorer::ports::FileOpsPort;
use crate::features::explorer::{ExplorerContextTarget, ExplorerCreateKind, ExplorerTargetKind};
use crate::features::explorer::{edit, roots};
use crate::infrastructure::explorer_filesystem::ExplorerFilesystem;
use crate::model::document::FileId;
use crate::ui::explorer::state::ExplorerReveal;

/// Shared Explorer click policy for local and provided files.
pub(crate) fn open_mode(clicks: u8) -> crate::app::workspace::OpenMode {
    if clicks > 1 {
        crate::app::workspace::OpenMode::Permanent
    } else {
        crate::app::workspace::OpenMode::Preview
    }
}

/// Open a file clicked in the Explorer: a single click previews it, a double
/// click keeps it open. Returns whether the editor should take focus.
pub(crate) fn open_tree_file(
    app: &mut AppState,
    path: PathBuf,
    clicks: u8,
    loader: Option<&crate::app::file_loading::FileLoader>,
) -> bool {
    if app
        .workspace
        .file_id_for_path(&path)
        .is_some_and(|id| app.workspace.open_error(id).is_none())
    {
        crate::app::file_loading::cancel(app);
        return open_tree_file_with(app, path, clicks, |_| Ok(String::new()));
    } else if crate::app::file_loading::request(app, path.clone(), open_mode(clicks), None, loader)
    {
        return true;
    }
    if crate::domain::media::is_raster(&path) || crate::domain::media::is_svg(&path) {
        let prepared = crate::app::encoding::prepare(&path)
            .unwrap_or_else(|error| crate::app::workspace::PreparedTextFile::failed(path, error));
        app.workspace
            .open_prepared_text(prepared, open_mode(clicks));
        app.chrome.toast = None;
        return true;
    }
    open_tree_file_with(app, path, clicks, read_text_file)
}

fn open_tree_file_with(
    app: &mut AppState,
    path: PathBuf,
    clicks: u8,
    read: impl FnOnce(&Path) -> Result<String, String>,
) -> bool {
    if let Some(id) = app.workspace.file_id_for_path(&path)
        && app.workspace.open_error(id).is_none()
        && app
            .workspace
            .active_items()
            .iter()
            .any(|v| app.workspace.view_document(*v) == Some(id))
    {
        app.workspace.activate_document(id);
        if clicks > 1 {
            app.workspace.promote_active_preview();
        }
        return true;
    }
    match read(&path) {
        Ok(contents) => {
            if clicks > 1 {
                app.workspace.open_path(path, contents);
            } else {
                app.workspace.preview_path(path, contents);
            }
            app.chrome.toast = None;
            true
        }
        Err(message) => {
            let mode = open_mode(clicks);
            app.workspace.open_failed(path, message, mode);
            app.chrome.toast = None;
            true
        }
    }
}

/// Release a pressed Explorer file: open it like a click when it was not
/// dragged, otherwise open it where it was dropped. Returns whether the
/// editor should take focus.
pub(crate) fn finish_file_drag(
    app: &mut AppState,
    loader: Option<&crate::app::file_loading::FileLoader>,
) -> bool {
    if loader.is_some() {
        if let Some(drag) = app.editor.tabs.file_drag.take() {
            let crate::app::state::FileDragSource::Explorer { clicks } = drag.source else {
                return false;
            };
            if !drag.active {
                return open_tree_file(app, drag.path, clicks, loader);
            }
            if let Some(drop) = drag.drop {
                return crate::app::file_loading::request(
                    app,
                    drag.path,
                    crate::app::workspace::OpenMode::Permanent,
                    Some(drop),
                    loader,
                );
            }
        }
        return false;
    }
    finish_file_drag_with(app, read_text_file)
}

fn finish_file_drag_with(
    app: &mut AppState,
    read: impl FnOnce(&Path) -> Result<String, String>,
) -> bool {
    let Some(drag) = app.editor.tabs.file_drag.take() else {
        return false;
    };
    let crate::app::state::FileDragSource::Explorer { clicks } = drag.source else {
        return false;
    };
    if !drag.active {
        return open_tree_file_with(app, drag.path, clicks, read);
    }
    let Some(drop) = drag.drop else {
        return false;
    };
    let prepared = if crate::domain::media::is_raster(&drag.path)
        || crate::domain::media::is_svg(&drag.path)
    {
        crate::app::encoding::prepare(&drag.path).unwrap_or_else(|error| {
            crate::app::workspace::PreparedTextFile::failed(drag.path, error)
        })
    } else {
        match read(&drag.path) {
            Ok(contents) => crate::app::workspace::PreparedTextFile::new(drag.path, contents),
            Err(message) => crate::app::workspace::PreparedTextFile::failed(drag.path, message),
        }
    };
    let workspace = &mut app.workspace;
    match drop {
        TabDrop::Insert { pane, index } => {
            workspace.activate_pane(pane);
            workspace.open_prepared_text(prepared, crate::app::workspace::OpenMode::Permanent);
            let id = workspace.active().unwrap();
            // `index` is an insertion point; `move_tab` takes a final index.
            if let Some(position) = workspace
                .pane(pane)
                .and_then(|pane| pane.items().iter().position(|item| *item == id))
            {
                let target = if position < index { index - 1 } else { index };
                workspace.move_tab(pane, id, target);
            }
        }
        TabDrop::Pane { pane, split: None } => {
            workspace.activate_pane(pane);
            workspace.open_prepared_text(prepared, crate::app::workspace::OpenMode::Permanent);
        }
        TabDrop::Pane {
            pane,
            split: Some(direction),
        } => {
            let Some(new) = workspace.split_empty(pane, direction) else {
                return false;
            };
            workspace.activate_pane(new);
            workspace.open_prepared_text(prepared, crate::app::workspace::OpenMode::Permanent);
        }
    }
    app.chrome.toast = None;
    true
}

/// Dispatch a drawer-file release without doing Git reads on the UI thread.
pub(crate) fn finish_drawer_file_drag(
    state: &AppContext,
    snapshot: crate::features::git::GitStoreSnapshot,
) -> bool {
    let loader = state.services().and_then(|r| r.file_loader());
    let mut changes = None;
    let mut focus = false;
    state.try_update(|app| {
        let Some(drag) = &app.editor.tabs.file_drag else {
            return false;
        };
        if matches!(
            drag.source,
            crate::app::state::FileDragSource::Changes { .. }
        ) {
            changes = app.editor.tabs.file_drag.take();
        } else {
            focus = finish_file_drag(app, loader.as_ref());
        }
        true
    });
    if let Some(drag) = changes {
        let crate::app::state::FileDragSource::Changes { request, untracked } = drag.source else {
            unreachable!("only changes drags are dispatched here");
        };
        if !drag.active {
            crate::app::git::open_changes(state, request, untracked, snapshot);
            return true;
        }
        if let Some(drop) = drag.drop {
            return crate::app::git::open_diff_at(state, request, untracked, drop);
        }
    }
    focus
}

/// Expand or collapse an Explorer directory row, reading it on first expansion.
pub(crate) fn toggle_directory(app: &mut AppState, key: &str) {
    app.explorer.ui.reveal = None;
    app.explorer
        .model
        .toggle_directory(&ExplorerFilesystem, key);
}

/// Reread every loaded Explorer listing (deferred while an edit row is open).
pub(crate) fn refresh(app: &mut AppState) -> bool {
    app.explorer.model.refresh_all(&ExplorerFilesystem)
}

/// Reveal a document from its tab menu: show the drawer and the file's row.
pub fn reveal_file(state: &AppContext, id: FileId) {
    state.update(move |app| {
        let Some(path) = app
            .workspace
            .meta(id)
            .and_then(|meta| meta.local_path().map(Path::to_path_buf))
        else {
            return;
        };
        if !reveal_path(app, &path) {
            app.show_toast("File is outside the workspace.");
            return;
        }
        app.workspace.activate_document(id);
        app.drawer.visible = true;
        app.explorer.ui.scroll_x = 0.0;
    });
}

/// Show a folder from a path in the text: open the drawer and its row.
pub fn reveal_folder(state: &AppContext, path: &Path) {
    let path = path.to_path_buf();
    state.update(move |app| {
        if !reveal_path(app, &path) {
            app.show_toast("Folder is outside the workspace.");
            return;
        }
        app.drawer.visible = true;
        app.explorer.ui.scroll_x = 0.0;
    });
}

/// Follow the active editor in the tree after the user switched to it: expand
/// its folders and bring its row into sight, without opening the drawer or
/// moving focus. Files outside the workspace, diffs and built-in pages are
/// skipped silently, as is a tree with an open create or rename row.
pub(crate) fn auto_reveal_active(app: &mut AppState) {
    if !app.preferences.values().explorer_auto_reveal || app.explorer.model.editing() {
        return;
    }
    let Some(id) = app.workspace.active() else {
        return;
    };
    if app.workspace.is_diff(id) {
        return;
    }
    let Some(path) = app.workspace.active_path().map(Path::to_path_buf) else {
        return;
    };
    reveal_path(app, &path);
}

/// Expand the folders above `path` and ask the tree to show its row; returns
/// whether `path` lies inside the workspace.
fn reveal_path(app: &mut AppState, path: &Path) -> bool {
    let Reveal::Revealed { row } = app.explorer.model.reveal(&ExplorerFilesystem, path) else {
        return false;
    };
    app.explorer.ui.reveal = row.map(|row| {
        let depth = app
            .explorer
            .model
            .root_for_path(path)
            .and_then(|root| path.strip_prefix(root).ok())
            .map_or(0, |relative| relative.components().count());
        ExplorerReveal {
            row,
            covered: depth,
        }
    });
    true
}

pub fn choose_folder(state: &AppContext) -> bool {
    let mut options = FileDialogOptions::new().title("Open Folder");
    if let Some(directory) = state.get().explorer.model.roots().first().cloned() {
        options = options.directory(directory);
    }
    let Some(path) = system_file_dialogs().pick_folder(&options) else {
        return false;
    };

    state.update(move |app| load_folder(app, path));
    true
}

/// Add another root folder to the current file tree without replacing the
/// folders that are already open.
pub fn choose_folder_to_add(state: &AppContext) -> bool {
    let mut options = FileDialogOptions::new().title("Add Folder to Project");
    if let Some(directory) = state.get().explorer.model.roots().first().cloned() {
        options = options.directory(directory);
    }
    let Some(path) = system_file_dialogs().pick_folder(&options) else {
        return false;
    };

    state.update(move |app| add_folder(app, path));
    true
}

pub fn begin_create(state: &AppContext, parent: PathBuf, kind: ExplorerCreateKind) {
    state.update(move |app| {
        app.explorer.model.begin_create(parent, kind);
        app.explorer.ui.clear_edit();
    });
}

pub fn cancel_edit(state: &AppContext) {
    state.update(|app| {
        app.explorer.model.cancel_edit();
        app.explorer.ui.clear_edit();
    });
}

pub fn copy_item(state: &AppContext, path: &Path, cut: bool) {
    copy_items(state, &[path.to_path_buf()], cut);
}

/// Put one or more items on the system file clipboard.
pub fn copy_items(state: &AppContext, paths: &[PathBuf], cut: bool) {
    let action = if cut { "Cut" } else { "Copied" };
    let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    match ExplorerFilesystem.set_file_clipboard(&refs, cut) {
        Ok(()) => {
            let name = match paths {
                [path] => path
                    .file_name()
                    .map(|value| value.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string()),
                _ => format!("{} items", paths.len()),
            };
            state.update(move |app| app.show_success(format!("{action} {name}.")));
        }
        Err(error) => state.update(move |app| app.show_error(error)),
    }
}

/// Move every path to the Trash. Nothing is deleted when any of them has
/// unsaved changes.
pub fn delete_items(state: &AppContext, paths: Vec<PathBuf>) {
    let dirty = {
        let snapshot = state.get();
        paths
            .iter()
            .any(|path| snapshot.workspace.has_dirty_paths_under(path))
    };
    if dirty {
        state.update(|app| {
            app.show_toast("Save or close modified files before deleting these items.")
        });
        return;
    }
    let mut moved = 0usize;
    for path in paths {
        match move_path_to_trash(&path) {
            Ok(false) => {}
            Ok(true) => {
                moved += 1;
                tell_servers(
                    state,
                    crate::features::providers::FileChange::Deleted(path.clone()),
                );
                state.update(move |app| apply_deleted_path_state(app, &path));
            }
            Err(error) => {
                state.update(move |app| app.show_error(error));
                break;
            }
        }
    }
    if moved > 1 {
        state.update(move |app| app.show_success(format!("Moved {moved} items to Trash.")));
    }
}

/// Copy the paths of several items, one per line.
pub(crate) fn copy_paths(
    state: &AppContext,
    clipboard: &dyn lgui::services::Clipboard,
    targets: &[PathBuf],
    relative: bool,
) {
    let snapshot = state.get();
    let mut lines = Vec::with_capacity(targets.len());
    for target in targets {
        if relative {
            let Some(root) = snapshot.explorer.model.root_for_path(target) else {
                drop(snapshot);
                state.update(|app| app.show_toast("Path is outside the workspace."));
                return;
            };
            lines.push(crate::ui::display_path(
                target.strip_prefix(root).unwrap_or(target),
            ));
        } else {
            lines.push(crate::ui::display_path(target));
        }
    }
    drop(snapshot);
    let label = if relative { "relative paths" } else { "paths" };
    match clipboard.write_text(&lines.join("\n")) {
        Ok(()) => state.update(move |app| app.show_success(format!("Copied {label}."))),
        Err(error) => state.update(move |app| app.show_error(format!("Clipboard: {error}"))),
    }
}

pub fn begin_rename(state: &AppContext, path: PathBuf, kind: ExplorerTargetKind) {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        state.update(|app| app.show_toast("This name cannot be edited as text."));
        return;
    };
    let name = name.to_owned();
    state.update(move |app| {
        app.explorer.model.begin_rename(path, kind);
        app.explorer.ui.create_input.set_text(name);
        app.explorer.ui.create_error = None;
    });
}

pub fn finish_rename(state: &AppContext) {
    let snapshot = state.get();
    let Some(request) = snapshot.explorer.model.rename_request().cloned() else {
        return;
    };
    let raw_name = snapshot.explorer.ui.create_input.text().to_owned();
    drop(snapshot);

    let target = match edit::validate_rename_path(&ExplorerFilesystem, &request.path, &raw_name) {
        Ok(target) => target,
        Err(message) => {
            state.update(move |app| {
                app.explorer.ui.create_error = Some(message.clone());
                app.show_error(message);
            });
            return;
        }
    };
    if target == request.path {
        cancel_edit(state);
        return;
    }
    if let Err(error) = ExplorerFilesystem.rename(&request.path, &target) {
        let message = format!("Could not rename {}: {error}", request.path.display());
        state.update(move |app| {
            app.explorer.ui.create_error = Some(message.clone());
            app.show_error(message);
        });
        return;
    }

    tell_servers(
        state,
        crate::features::providers::FileChange::Renamed {
            from: request.path.clone(),
            to: target.clone(),
        },
    );
    state.update(move |app| apply_renamed_path_state(app, &request.path, &target));
}

/// Language servers that asked to hear about file changes are told.
fn tell_servers(state: &AppContext, change: crate::features::providers::FileChange) {
    if let Some(services) = state.services().and_then(|r| r.language_services()) {
        services.files_changed(change);
    }
}

fn apply_renamed_path_state(app: &mut AppState, old_path: &Path, new_path: &Path) {
    app.workspace.move_open_paths_under(old_path, new_path);
    let roots_changed = app
        .explorer
        .model
        .apply_rename(&ExplorerFilesystem, old_path, new_path);
    app.explorer.ui.hovered_path = None;
    app.explorer.ui.clear_edit();
    app.chrome.toast = None;
    if roots_changed {
        persist_workspace(app);
    }
}

pub fn delete_item(state: &AppContext, path: PathBuf) {
    if state.get().workspace.has_dirty_paths_under(&path) {
        state.update(|app| {
            app.show_toast("Save or close modified files before deleting this item.")
        });
        return;
    }
    match move_path_to_trash(&path) {
        Ok(false) => {}
        Ok(true) => {
            tell_servers(
                state,
                crate::features::providers::FileChange::Deleted(path.clone()),
            );
            state.update(move |app| apply_deleted_path_state(app, &path))
        }
        Err(error) => state.update(move |app| app.show_error(error)),
    }
}

/// Git worktree removal shares the Explorer's platform trash operation.
pub(crate) fn move_path_to_trash(path: &Path) -> Result<bool, String> {
    ExplorerFilesystem.move_to_trash(path)
}

/// Show a file or folder in the platform file manager.
pub(crate) fn reveal_item(state: &AppContext, target: &ExplorerContextTarget) {
    if let Err(error) = ExplorerFilesystem.reveal(&target.path, target.kind) {
        state.update(move |app| {
            app.show_error(format!("Could not reveal item: {error}"));
        });
    }
}

pub(crate) fn apply_deleted_path_state(app: &mut AppState, path: &Path) {
    app.workspace.close_paths_under(path);
    let folders_changed = app.explorer.model.apply_delete(&ExplorerFilesystem, path);
    app.explorer.ui.hovered_path = None;
    app.explorer.ui.clear_edit();
    app.show_success("Moved item to Trash.");
    if folders_changed {
        persist_workspace(app);
    }
}

/// Remove a root from the current workspace without touching it on disk.
pub fn remove_root(state: &AppContext, root: PathBuf) {
    let display = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    state.update(move |app| {
        if remove_folder_state(app, &root) {
            persist_workspace(app);
            app.show_success(format!("Removed {display} from workspace."));
        }
    });
}

fn remove_folder_state(app: &mut AppState, root: &Path) -> bool {
    let Some(edit_cancelled) = app.explorer.model.remove_root(root) else {
        return false;
    };
    if app
        .explorer
        .ui
        .hovered_path
        .as_deref()
        .is_some_and(|path| Path::new(path).starts_with(root))
    {
        app.explorer.ui.hovered_path = None;
    }
    if edit_cancelled {
        app.explorer.ui.clear_edit();
    }
    app.explorer.ui.reset_scroll();
    true
}

pub fn finish_create(state: &AppContext) {
    let snapshot = state.get();
    let Some(request) = snapshot.explorer.model.create_request().cloned() else {
        return;
    };
    let raw_name = snapshot.explorer.ui.create_input.text().to_owned();
    drop(snapshot);

    let force_folder = raw_name.ends_with(['/', '\\']);
    let relative = match edit::validate_create_path(&ExplorerFilesystem, &request.parent, &raw_name)
    {
        Ok(relative) => relative,
        Err(message) => {
            state.update(move |app| {
                app.explorer.ui.create_error = Some(message.clone());
                app.show_error(message);
            });
            return;
        }
    };
    let target = request.parent.join(relative);
    let kind = if force_folder {
        ExplorerCreateKind::Folder
    } else {
        request.kind
    };
    let result = match kind {
        ExplorerCreateKind::File => ExplorerFilesystem.create_file(&target),
        ExplorerCreateKind::Folder => ExplorerFilesystem.create_folder(&target),
    };
    if let Err(error) = result {
        let message = format!("Could not create {}: {error}", target.display());
        state.update(move |app| {
            app.explorer.ui.create_error = Some(message.clone());
            app.show_error(message);
        });
        return;
    }

    tell_servers(
        state,
        crate::features::providers::FileChange::Created(target.clone()),
    );
    state.update(move |app| {
        app.explorer
            .model
            .complete_create(&ExplorerFilesystem, &request.parent, &target, kind);
        if kind == ExplorerCreateKind::File {
            let format = crate::infrastructure::document_filesystem::options(&target).new_file;
            let id = app.workspace.open_path(target, String::new());
            let _ = app.workspace.set_encoding(id, format);
        }
        app.explorer.ui.clear_edit();
        app.chrome.toast = None;
    });
}

/// Reopen a folder selected from the welcome screen's recent-workspace list.
pub fn open_recent_folder(state: &AppContext, path: PathBuf) -> bool {
    if !path.is_dir() {
        let display = path.display().to_string();
        state.update(move |app| {
            app.explorer.model.forget_recent(&path);
            persist_workspace(app);
            app.show_error(format!(
                "Workspace folder is no longer available: {display}"
            ));
        });
        return false;
    }

    state.update(move |app| load_folder(app, path));
    true
}

pub(crate) fn load_folder(app: &mut AppState, path: PathBuf) {
    let path = normalized_folder(path);
    replace_folder_state(app, path);
    persist_workspace(app);
}

fn replace_folder_state(app: &mut AppState, path: PathBuf) {
    app.explorer.model.open_root(&ExplorerFilesystem, path);
    app.explorer.ui.reset_scroll();
    app.drawer.visible = true;
    app.chrome.welcome_hover = None;
    app.chrome.toast = None;
}

pub(crate) fn add_folder(app: &mut AppState, path: PathBuf) {
    let path = normalized_folder(path);
    add_folder_state(app, path);
    persist_workspace(app);
}

/// Forget a worktree after Git successfully removed it from disk.
pub(crate) fn forget_worktree(app: &mut AppState, root: &Path) {
    remove_folder_state(app, root);
    app.explorer.model.forget_recent(root);
    app.git.model.unpin(root);
    persist_workspace(app);
}

fn add_folder_state(app: &mut AppState, path: PathBuf) -> bool {
    let was_empty = app.explorer.model.roots().is_empty();
    if !app.explorer.model.add_root(&ExplorerFilesystem, path) {
        return false;
    }
    if was_empty {
        app.explorer.ui.reset_scroll();
    }
    app.drawer.visible = true;
    app.chrome.welcome_hover = None;
    app.chrome.toast = None;
    true
}

/// Open paths given on the command line, after the previous session has been
/// restored. The first folder replaces the workspace like Open Folder and any
/// further folders are added to it; files open as tabs, the last one active.
/// Unreadable files keep a tab describing why they could not be opened.
pub(crate) fn open_launch_paths(
    app: &mut AppState,
    paths: Vec<PathBuf>,
    loader: Option<&crate::app::file_loading::FileLoader>,
) {
    // Launch files use the same metadata-first restoration as session tabs.
    // Reading them here would still block the very first frame.
    let mut replaced_workspace = false;
    for path in paths {
        let path = std::path::absolute(&path).unwrap_or(path);
        if path.is_dir() {
            let path = normalized_folder(path);
            if replaced_workspace {
                add_folder_state(app, path);
            } else {
                replace_folder_state(app, path);
                replaced_workspace = true;
            }
        } else {
            if loader.is_some() {
                if let Some(id) = app
                    .workspace
                    .file_id_for_path(&path)
                    .filter(|id| app.workspace.open_error(*id).is_none())
                {
                    app.workspace.activate_document(id);
                } else {
                    crate::app::file_loading::request(
                        app,
                        path,
                        crate::app::workspace::OpenMode::Permanent,
                        None,
                        loader,
                    );
                }
            } else {
                app.workspace.open_deferred(path);
            }
        }
    }
    if replaced_workspace {
        persist_workspace(app);
    }
}

/// Apply launch paths to the state; returns whether the workspace folders
/// changed and need to be persisted.
#[cfg(test)]
fn open_launch_paths_with(
    app: &mut AppState,
    paths: Vec<PathBuf>,
    mut read: impl FnMut(&Path) -> Result<String, String>,
) -> bool {
    let mut replaced_workspace = false;
    for path in paths {
        // Relative to the launching shell's directory. `absolute` keeps the
        // plain spelling (no `\\?\` prefix), so session tabs still match.
        let path = std::path::absolute(&path).unwrap_or(path);
        if path.is_dir() {
            let path = normalized_folder(path);
            if replaced_workspace {
                add_folder_state(app, path);
            } else {
                replace_folder_state(app, path);
                replaced_workspace = true;
            }
            continue;
        }
        match read(&path) {
            Ok(contents) => {
                app.workspace.open_path(path, contents);
            }
            Err(message) => {
                app.workspace.open_failed(
                    path,
                    message,
                    crate::app::workspace::OpenMode::Permanent,
                );
            }
        }
    }
    replaced_workspace
}

/// Populate directory caches for folders restored from the previous session.
pub(crate) fn hydrate_workspace_folders(app: &mut AppState) {
    app.explorer.model.hydrate(&ExplorerFilesystem);
}

fn normalized_folder(path: PathBuf) -> PathBuf {
    roots::normalized(&ExplorerFilesystem, path)
}

fn persist_workspace(app: &mut AppState) {
    if let Err(error) =
        workspace_persistence::save(app.explorer.model.roots(), app.explorer.model.recents())
    {
        app.show_error(format!("Could not remember workspace folders: {error}"));
    }
}

#[cfg(test)]
#[path = "explorer_tests.rs"]
mod tests;

/// Copy `target` to the clipboard, absolute or relative to its workspace root.
pub(crate) fn copy_path(
    state: &AppContext,
    clipboard: &dyn lgui::services::Clipboard,
    target: &Path,
    relative: bool,
) {
    let value = if relative {
        let snapshot = state.get();
        let Some(root) = snapshot.explorer.model.root_for_path(target) else {
            state.update(|app| app.show_toast("Path is outside the workspace."));
            return;
        };
        let relative = target.strip_prefix(root).unwrap_or(target);
        if relative.as_os_str().is_empty() {
            ".".to_owned()
        } else {
            crate::ui::display_path(relative)
        }
    } else {
        crate::ui::display_path(target)
    };
    let label = if relative { "relative path" } else { "path" };
    match clipboard.write_text(&value) {
        Ok(()) => state.update(move |app| app.show_success(format!("Copied {label}."))),
        Err(error) => state.update(move |app| app.show_error(format!("Clipboard: {error}"))),
    }
}
