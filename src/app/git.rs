//! Source Control commands. Each reads the snapshot and root state, runs Git
//! off the UI thread, and applies the result in one root-state update.
use crate::app::context::AppContext;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use lgui::prelude::State;

use crate::app::state::{AppState, TabDrop};
use crate::features::git::actions::{
    BlockAction, BranchListing, Checkout, StashMode, SyncPlan, sync_plan,
};
use crate::features::git::diff_document::{ChangeBlock, DiffDocument};
use crate::features::git::error::GitErrorKind;
use crate::features::git::model::{HISTORY_PAGE_SIZE, HistoryRequest};
use crate::features::git::{
    ConflictSide, GitError, GitResult, GitStoreSnapshot, HeadState, OperationKind, RemoteInfo,
    RepositorySnapshot, RepositoryState,
};
use crate::infrastructure::git::{CliGitBackend, GitService};
use crate::ui::git::confirm::{GitConfirm, GitConfirmAction};
use crate::ui::git::stash_switch::StashSwitchRequest;

pub fn open_diff(state: &AppContext, request: crate::features::git::DiffRequest, untracked: bool) {
    open_diff_to(state, request, untracked, None);
}

pub(crate) fn open_diff_at(
    state: &AppContext,
    request: crate::features::git::DiffRequest,
    untracked: bool,
    drop: TabDrop,
) -> bool {
    open_diff_to(state, request, untracked, Some(drop))
}

pub fn open_changes(
    state: &AppContext,
    file: crate::features::git::DiffRequest,
    untracked: bool,
    snapshot: GitStoreSnapshot,
) {
    super::git_diff::open_changes(state, file, untracked, snapshot);
}

fn open_diff_to(
    state: &AppContext,
    request: crate::features::git::DiffRequest,
    untracked: bool,
    drop: Option<TabDrop>,
) -> bool {
    let app = state.get();
    let Some(application) = state.services().and_then(|runtime| runtime.application()) else {
        return false;
    };
    let roots = app.explorer.model.roots().to_vec();
    let active_view = (app.workspace.active_pane(), app.workspace.active());
    let destination_view = drop.map(|drop| {
        let pane = match drop {
            TabDrop::Insert { pane, .. } | TabDrop::Pane { pane, .. } => pane,
        };
        (pane, app.workspace.pane(pane).map(|pane| pane.active()))
    });
    let mut sequence = 0;
    state.update(|app| sequence = app.git.model.begin_diff_open(request.clone()));
    let worker_state = state.clone();
    let spawned = thread::Builder::new()
        .name("loog-git-open-diff".into())
        .spawn(move || {
            let result = crate::infrastructure::git::service().and_then(|service| {
                if request.target == crate::features::git::DiffTarget::Changes {
                    let content = service.refresh_diff(&request, &GitStoreSnapshot::default())?;
                    return Ok(Arc::new(DiffDocument::from_content(request, content)));
                }
                let diff = service.backend().parsed_full_diff(
                    &request.repository_root,
                    request.target,
                    std::slice::from_ref(&request.path),
                )?;
                let document = if diff.files.is_empty()
                    && untracked
                    && request.target != crate::features::git::DiffTarget::HeadToIndex
                {
                    let content = crate::infrastructure::git::service::read_untracked(
                        &request.repository_root.join(&request.path),
                    )?;
                    DiffDocument::from_content(request, content)
                } else {
                    crate::features::git::diff_document::DiffDocument::from_unified(
                        request.repository_root,
                        request.path,
                        request.target,
                        diff,
                    )
                };
                Ok(Arc::new(document))
            });
            application.post(move || {
                let mut result = Some(result);
                worker_state.update(|app| {
                    if !app.git.model.finish_diff_open(sequence) {
                        return;
                    }
                    if app.explorer.model.roots() != roots
                        || (app.workspace.active_pane(), app.workspace.active()) != active_view
                        || destination_view.is_some_and(|(pane, active)| {
                            active.is_none()
                                || app.workspace.pane(pane).map(|pane| pane.active()) != active
                        })
                    {
                        return;
                    }
                    match result.take().unwrap() {
                        Ok(document) => {
                            if let Some(drop) = drop {
                                apply_diff_drop(app, document, drop);
                            } else if document.sections.is_some() {
                                app.workspace.open_prepared_diff(document);
                            } else {
                                app.workspace.open_prepared_file_changes(document);
                            }
                        }
                        Err(error) => app.show_error(error.user_message()),
                    }
                });
                crate::infrastructure::ui_dispatch::retire(result);
            });
        })
        .is_ok();
    if !spawned {
        state.update(|app| {
            app.git.model.abandon_diff_open();
            app.show_error("Could not start Git diff worker.");
        });
    }
    spawned
}

/// Apply a successful read before making any layout changes. A closed
/// destination cancels the drop; no empty split is left on a failed read.
fn apply_diff_drop(app: &mut AppState, document: Arc<DiffDocument>, drop: TabDrop) -> bool {
    let (pane, index, split) = match drop {
        TabDrop::Insert { pane, index } => (pane, Some(index), None),
        TabDrop::Pane { pane, split } => (pane, None, split),
    };
    if app.workspace.pane(pane).is_none() {
        return false;
    }
    let pane = if let Some(direction) = split {
        let Some(new) = app.workspace.split_empty(pane, direction) else {
            return false;
        };
        new
    } else {
        pane
    };
    let Some(id) = app.workspace.open_prepared_diff_in(pane, document) else {
        return false;
    };
    if let Some(index) = index
        && let Some(position) = app
            .workspace
            .pane(pane)
            .and_then(|pane| pane.items().iter().position(|item| *item == id))
    {
        let target = if position < index { index - 1 } else { index };
        app.workspace.move_tab(pane, id, target);
    }
    app.chrome.toast = None;
    true
}

/// The active repository's change sections: conflicts, staged, unstaged and
/// untracked, as the Source Control drawer lists them.
fn sections(store: &State<GitStoreSnapshot>, repository: &RepositorySnapshot) -> [Vec<PathBuf>; 4] {
    store
        .get()
        .presentation
        .repositories
        .get(&repository.id)
        .map(|prepared| prepared.sections.clone())
        .unwrap_or_default()
}

/// Whether any of `paths` (relative to `root`) has unsaved editor changes.
fn unsaved(state: &AppContext, root: &Path, paths: &[PathBuf]) -> bool {
    let dirty = state.get().workspace.dirty_paths();
    paths
        .iter()
        .any(|path| dirty.iter().any(|dirty| dirty == &root.join(path)))
}

fn count_label(paths: &[PathBuf], one: &str) -> String {
    match paths {
        [path] => format!("{one} {}…", file_name(path)),
        _ => format!("{one} {} files…", paths.len()),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

pub fn stage_paths(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    paths: Vec<PathBuf>,
) -> bool {
    let Some(repository) = active_repository(store) else {
        return false;
    };
    if paths.is_empty() {
        return false;
    }
    if paths.iter().any(|path| {
        repository
            .files
            .get(path)
            .is_some_and(|file| file.conflict.is_some())
    }) {
        return stage_conflicted(state, store, paths, false);
    }
    let message = count_label(&paths, "Staging");
    run(
        state,
        store,
        repository,
        OperationKind::Stage,
        message,
        move |service, root| service.write(root, |backend| backend.stage(root, &paths)),
    )
}

/// What marking unmerged files resolved came to.
enum Resolved {
    Staged,
    /// Git still finds conflict markers in these files; nothing was staged.
    Markers(std::collections::BTreeMap<PathBuf, usize>),
}

/// Mark unmerged `paths` resolved. Git decides whether markers are left, with
/// `git diff --check`; unless `confirmed`, leftovers ask first. The check
/// reads the file on disk, so unsaved edits must be saved before.
pub fn stage_conflicted(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    paths: Vec<PathBuf>,
    confirmed: bool,
) -> bool {
    let Some(repository) = active_repository(store) else {
        return false;
    };
    if unsaved(state, &repository.worktree_root, &paths) {
        state.update(|app| app.show_toast("Save the file before marking it resolved."));
        return false;
    }
    let message = count_label(&paths, "Marking resolved");
    let asked = paths.clone();
    run_with(
        state,
        store,
        repository,
        OperationKind::Stage,
        message,
        move |service, root| {
            service.write(root, |backend| {
                if !confirmed {
                    let left = backend.leftover_conflict_markers(root, &paths)?;
                    if !left.is_empty() {
                        return Ok(Resolved::Markers(left));
                    }
                }
                backend.stage(root, &paths)?;
                Ok(Resolved::Staged)
            })
        },
        move |app, resolved| {
            let Resolved::Markers(left) = resolved else {
                return;
            };
            let lines: usize = left.values().sum();
            let message = match left.keys().collect::<Vec<_>>().as_slice() {
                [path] => format!(
                    "{} still has {lines} conflict marker line{}.",
                    file_name(path),
                    if lines == 1 { "" } else { "s" }
                ),
                files => format!("{} files still have conflict markers.", files.len()),
            };
            app.git.ui.confirm = Some(GitConfirm {
                title: "Mark as resolved?".into(),
                message,
                confirm_label: "Mark Resolved",
                destructive: false,
                action: GitConfirmAction::StageConflicted { paths: asked },
                restore_terminal: app.terminal.ui.terminal_focused,
            });
        },
    )
}

pub fn unstage_paths(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    paths: Vec<PathBuf>,
) -> bool {
    let Some(repository) = active_repository(store) else {
        return false;
    };
    if paths.is_empty() {
        return false;
    }
    let message = count_label(&paths, "Unstaging");
    run(
        state,
        store,
        repository,
        OperationKind::Unstage,
        message,
        move |service, root| service.write(root, |backend| backend.unstage(root, &paths)),
    )
}

/// Ask before discarding: worktree changes to `tracked` files are lost and
/// `untracked` files go to the system trash.
pub fn request_discard(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    tracked: Vec<PathBuf>,
    untracked: Vec<PathBuf>,
) -> bool {
    let Some(repository) = active_repository(store) else {
        return false;
    };
    let root = repository.worktree_root;
    let all = tracked
        .iter()
        .chain(&untracked)
        .cloned()
        .collect::<Vec<_>>();
    if all.is_empty() {
        return false;
    }
    if unsaved(state, &root, &all) {
        state.update(|app| {
            app.show_toast("Save or revert the open editor changes before discarding.")
        });
        return false;
    }
    let title = match all.as_slice() {
        [path] if untracked.is_empty() => format!("Discard changes to {}?", file_name(path)),
        [path] => format!("Delete {}?", file_name(path)),
        _ => format!("Discard changes to {} files?", all.len()),
    };
    let message = match (tracked.is_empty(), untracked.is_empty()) {
        (false, true) => "Your changes will be lost. This cannot be undone.",
        (true, false) => "Untracked files are moved to the trash.",
        _ => "Changes to tracked files will be lost; untracked files are moved to the trash.",
    };
    state.update(move |app| {
        app.git.ui.confirm = Some(GitConfirm {
            title,
            message: message.to_owned(),
            confirm_label: if untracked.is_empty() {
                "Discard"
            } else {
                "Discard and Delete"
            },
            destructive: true,
            action: GitConfirmAction::Discard {
                repository: root,
                tracked,
                untracked,
            },
            restore_terminal: app.terminal.ui.terminal_focused,
        });
    });
    true
}

/// Discard confirmed changes: restore `tracked` files from the index and
/// move `untracked` files to the trash.
pub fn discard_paths(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    root: &Path,
    tracked: Vec<PathBuf>,
    untracked: Vec<PathBuf>,
) -> bool {
    let Some(repository) = store.get().repository_for_path(root).cloned() else {
        return false;
    };
    let all = tracked
        .iter()
        .chain(&untracked)
        .cloned()
        .collect::<Vec<_>>();
    let message = count_label(&all, "Discarding");
    run_with(
        state,
        store,
        repository,
        OperationKind::Restore,
        message,
        move |service, root| {
            service.write(root, |backend| {
                backend.restore_worktree(root, &tracked)?;
                let mut deleted = Vec::new();
                for path in &untracked {
                    let path = root.join(path);
                    match crate::app::explorer::move_path_to_trash(&path) {
                        Ok(true) => deleted.push(path),
                        Ok(false) => {}
                        Err(error) => return Err(GitError::new(GitErrorKind::Io, error)),
                    }
                }
                Ok(deleted)
            })
        },
        |app, deleted| {
            for path in deleted {
                crate::app::explorer::apply_deleted_path_state(app, &path);
            }
        },
    )
}

/// Commit the staged changes. With nothing staged, offer to stage every
/// tracked change first; amending needs no staged changes.
pub fn commit(state: &AppContext, store: &State<GitStoreSnapshot>) -> bool {
    if state.get().git.resources.model.offline.is_some() {
        state.update(|app| {
            app.show_error("Reconnect the repository environment before committing.")
        });
        return false;
    }
    if let Some(scope) = state.get().git.resources.model.selected {
        super::resource_git_tasks::run(
            state,
            crate::features::git::resources::Intent::Commit(scope),
        );
        return true;
    }
    // Mid-operation the commit button is Continue, and so is its shortcut.
    if let Some(repository) = active_repository(store)
        && let Some(wording) =
            crate::features::git::operation::operation_text(repository.repository_state)
    {
        if wording.continue_label.is_none() {
            return false;
        }
        let conflicted = repository
            .files
            .values()
            .any(|file| file.conflict.is_some());
        if conflicted {
            state.update(|app| app.show_toast("Resolve and stage the conflicted files first."));
            return false;
        }
        return continue_operation(state, store);
    }
    request_commit(state, store, false)
}

pub fn commit_and_push(state: &AppContext, store: &State<GitStoreSnapshot>) -> bool {
    if crate::ui::git::drawer::commit_reason(&state.get().git, &store.get()).is_some() {
        return false;
    }
    request_commit(state, store, true)
}

fn request_commit(state: &AppContext, store: &State<GitStoreSnapshot>, push: bool) -> bool {
    let app = state.get();
    let message = app.git.ui.commit_input.text().trim().to_owned();
    let amend = app.git.ui.commit_amend;
    drop(app);
    if message.is_empty() && !amend {
        state.update(|app| app.show_toast("Enter a commit message."));
        return false;
    }
    let Some(repository) = active_repository(store) else {
        return false;
    };
    let [conflicts, staged, unstaged, untracked] = sections(store, &repository);
    if !conflicts.is_empty() {
        state.update(|app| app.show_toast("Resolve and stage the conflicted files first."));
        return false;
    }
    if staged.is_empty() && !amend {
        if unstaged.is_empty() {
            let hint = if untracked.is_empty() {
                "There are no changes to commit."
            } else {
                "Stage the new files you want to commit."
            };
            state.update(move |app| app.show_toast(hint));
            return false;
        }
        let count = unstaged.len();
        state.update(move |app| {
            app.git.ui.confirm = Some(GitConfirm {
                title: "Commit all changes?".into(),
                message: format!(
                    "Nothing is staged. Stage the {count} changed file{} and commit? Untracked \
                     files are not included.",
                    if count == 1 { "" } else { "s" }
                ),
                confirm_label: "Stage and Commit",
                destructive: false,
                action: GitConfirmAction::StageAllAndCommit,
                restore_terminal: app.terminal.ui.terminal_focused,
            });
        });
        return false;
    }
    commit_paths(state, store, repository, message, Vec::new(), push)
}

/// Stage every tracked change, then commit.
pub fn commit_all(state: &AppContext, store: &State<GitStoreSnapshot>) -> bool {
    let message = state.get().git.ui.commit_input.text().trim().to_owned();
    let Some(repository) = active_repository(store) else {
        return false;
    };
    let [_, _, unstaged, _] = sections(store, &repository);
    commit_paths(state, store, repository, message, unstaged, false)
}

/// Stage `stage`, then commit with the drawer's options.
fn commit_paths(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    repository: RepositorySnapshot,
    message: String,
    stage: Vec<PathBuf>,
    push: bool,
) -> bool {
    let app = state.get();
    let amend = app.git.ui.commit_amend;
    let signoff = app.git.ui.commit_signoff;
    let publish = repository.upstream.is_none();
    let branch = match &repository.head {
        HeadState::Branch(name) => Some(name.clone()),
        _ => None,
    };
    drop(app);
    run_with(
        state,
        store,
        repository,
        OperationKind::Commit,
        if amend {
            "Amending…"
        } else {
            "Committing…"
        },
        move |service, root| {
            service.write(root, |backend| {
                backend.stage(root, &stage)?;
                backend.commit(root, &message, amend, signoff)?;
                // A rejected push must not turn a successful local commit into
                // a failed commit or leave its message ready for duplication.
                let push_error = if push {
                    let result = if publish {
                        backend.remotes(root).and_then(|remotes| {
                            let remote = publish_remote(&remotes).ok_or_else(|| {
                                GitError::new(
                                    GitErrorKind::Other,
                                    "Add a remote before publishing this branch.",
                                )
                            })?;
                            let branch = branch.as_deref().ok_or_else(|| {
                                GitError::new(
                                    GitErrorKind::Other,
                                    "Check out a branch before pushing.",
                                )
                            })?;
                            backend.push(root, Some((remote, branch)), false)
                        })
                    } else {
                        backend.push(root, None, false)
                    };
                    result.err().map(|error| error.user_message())
                } else {
                    None
                };
                Ok(push_error)
            })
        },
        |app, push_error| {
            app.git.ui.commit_amend = false;
            if let Some(error) = push_error {
                app.show_error(format!("Committed locally, but push failed: {error}"));
            }
        },
    )
}

/// The active repository, unless an open editor buffer under it has unsaved
/// changes that an operation rewriting the worktree could conflict with.
fn repository_without_unsaved(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    doing: &str,
) -> Option<RepositorySnapshot> {
    let repository = active_repository(store)?;
    if state
        .get()
        .workspace
        .has_dirty_paths_under(&repository.worktree_root)
    {
        let message = format!("Save or revert the open editor changes before {doing}.");
        state.update(move |app| app.show_toast(message));
        return None;
    }
    Some(repository)
}

/// Continue the merge, rebase, cherry-pick or revert in progress.
pub fn continue_operation(state: &AppContext, store: &State<GitStoreSnapshot>) -> bool {
    let Some(repository) = repository_without_unsaved(state, store, "continuing") else {
        return false;
    };
    let operation = repository.repository_state;
    run(
        state,
        store,
        repository,
        OperationKind::Other,
        "Continuing…",
        move |service, root| {
            service.write(root, |backend| backend.continue_operation(root, operation))
        },
    )
}

/// Ask before aborting the operation in progress, which discards its work.
pub fn request_abort(state: &AppContext, store: &State<GitStoreSnapshot>) -> bool {
    let Some(repository) = repository_without_unsaved(state, store, "aborting") else {
        return false;
    };
    let operation = repository.repository_state;
    let (title, message) = match operation {
        RepositoryState::MergeInProgress => (
            "Abort the merge?",
            "The branch returns to where it was before the merge.",
        ),
        RepositoryState::RebaseInProgress => (
            "Abort the rebase?",
            "The branch returns to where it was before the rebase.",
        ),
        RepositoryState::CherryPickInProgress => (
            "Abort the cherry-pick?",
            "The branch returns to where it was before the cherry-pick.",
        ),
        RepositoryState::RevertInProgress => (
            "Abort the revert?",
            "The branch returns to where it was before the revert.",
        ),
        RepositoryState::Bisecting => (
            "End the bisect?",
            "Git checks out the branch you started bisecting from.",
        ),
        _ => return false,
    };
    let root = repository.worktree_root;
    state.update(move |app| {
        app.git.ui.confirm = Some(GitConfirm {
            title: title.into(),
            message: format!("{message} Conflict resolutions made so far are lost."),
            confirm_label: if operation == RepositoryState::Bisecting {
                "End Bisect"
            } else {
                "Abort"
            },
            destructive: true,
            action: GitConfirmAction::AbortOperation {
                repository: root,
                operation,
            },
            restore_terminal: app.terminal.ui.terminal_focused,
        });
    });
    true
}

/// Abort a confirmed operation in progress.
pub fn abort_operation(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    root: &Path,
    operation: RepositoryState,
) -> bool {
    let Some(repository) = store.get().repository_for_path(root).cloned() else {
        return false;
    };
    run(
        state,
        store,
        repository,
        OperationKind::Other,
        "Aborting…",
        move |service, root| {
            service.write(root, |backend| backend.abort_operation(root, operation))
        },
    )
}

/// Resolve a conflicted file with one side's version and stage it.
pub fn resolve_conflict(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    path: PathBuf,
    side: ConflictSide,
) -> bool {
    let Some(repository) = active_repository(store) else {
        return false;
    };
    if unsaved(
        state,
        &repository.worktree_root,
        std::slice::from_ref(&path),
    ) {
        state.update(|app| {
            app.show_toast("Save or revert the open editor changes before resolving.")
        });
        return false;
    }
    let message = count_label(std::slice::from_ref(&path), "Resolving");
    run(
        state,
        store,
        repository,
        OperationKind::Other,
        message,
        move |service, root| {
            service.write(root, |backend| backend.resolve_conflict(root, &path, side))
        },
    )
}

/// Stash every change, untracked files included.
pub fn stash_all(state: &AppContext, store: &State<GitStoreSnapshot>) -> bool {
    let Some(repository) = repository_without_unsaved(state, store, "stashing") else {
        return false;
    };
    if repository.files.is_empty() {
        state.update(|app| app.show_toast("There are no changes to stash."));
        return false;
    }
    run(
        state,
        store,
        repository,
        OperationKind::Stash,
        "Stashing…",
        |service, root| {
            service.write(root, |backend| {
                backend.stash_push(root, None, true, false).map(|_| ())
            })
        },
    )
}

/// The current index of the stash `oid`; indices shift as stashes come and go.
fn stash_index(backend: &CliGitBackend, root: &Path, oid: &str) -> GitResult<usize> {
    backend
        .stash_list(root)?
        .into_iter()
        .find(|stash| stash.oid == oid)
        .map(|stash| stash.index)
        .ok_or_else(|| GitError::new(GitErrorKind::Other, "The stash no longer exists."))
}

/// Apply the stash `oid`, removing it when `pop`.
pub fn apply_stash(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    oid: String,
    pop: bool,
) -> bool {
    let doing = if pop {
        "popping a stash"
    } else {
        "applying a stash"
    };
    let Some(repository) = repository_without_unsaved(state, store, doing) else {
        return false;
    };
    run(
        state,
        store,
        repository,
        OperationKind::Stash,
        if pop {
            "Popping stash…"
        } else {
            "Applying stash…"
        },
        move |service, root| {
            service.write(root, |backend| {
                let index = stash_index(backend, root, &oid)?;
                backend.stash_apply(root, index, pop)
            })
        },
    )
}

pub fn request_create_tag(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    root: &Path,
    branch: Option<String>,
    target: String,
) -> bool {
    let snapshot = store.get();
    let Some(repository) = snapshot
        .repository_for_path(root)
        .filter(|repository| repository.worktree_root == root)
    else {
        return false;
    };
    if snapshot.operation.is_some() {
        return false;
    }
    if target.is_empty() {
        state.update(|app| app.show_toast("Create a commit before adding a tag."));
        return false;
    }
    let repository = repository.worktree_root.clone();
    state.update(|app| {
        app.git.ui.menu = None;
        app.git.ui.branch_picker = None;
        app.git.ui.tag_input = Default::default();
        app.git.ui.confirm = Some(GitConfirm {
            title: "New Tag".into(),
            message: if branch.is_some() {
                "Create a local tag at the selected branch's current commit."
            } else {
                "Create a local tag at the selected commit."
            }
            .into(),
            confirm_label: "Create",
            destructive: false,
            action: GitConfirmAction::CreateTag {
                repository,
                branch,
                target,
            },
            restore_terminal: app.terminal.ui.terminal_focused,
        });
    });
    true
}

pub fn create_tag(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    root: &Path,
    name: String,
    target: String,
) -> bool {
    let Some(repository) = store
        .get()
        .repository_for_path(root)
        .filter(|repository| repository.worktree_root == root)
        .cloned()
    else {
        return false;
    };
    let updated_root = repository.worktree_root.clone();
    let updated_name = name.clone();
    let updated_target = target.clone();
    run_with(
        state,
        store,
        repository,
        OperationKind::Other,
        "Creating tag…",
        move |service, root| {
            service.write(root, |backend| {
                backend.create_tag(root, &name, Some(&target))
            })
        },
        move |app, ()| {
            app.git
                .model
                .update_history_tag(&updated_root, &updated_name, Some(&updated_target));
        },
    )
}

pub fn request_delete_tag(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    root: &Path,
    name: String,
) -> bool {
    let snapshot = store.get();
    let Some(repository) = snapshot
        .repository_for_path(root)
        .filter(|repository| repository.worktree_root == root)
    else {
        return false;
    };
    if snapshot.operation.is_some() || !repository.tags.iter().any(|tag| tag.name == name) {
        return false;
    }
    let repository = repository.worktree_root.clone();
    state.update(|app| {
        app.git.ui.menu = None;
        app.git.ui.confirm = Some(GitConfirm {
            title: "Delete Tag?".into(),
            message: format!("Delete the local tag \"{name}\"?"),
            confirm_label: "Delete",
            destructive: true,
            action: GitConfirmAction::DeleteTag { repository, name },
            restore_terminal: app.terminal.ui.terminal_focused,
        });
    });
    true
}

pub fn delete_tag(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    root: &Path,
    name: String,
) -> bool {
    let Some(repository) = store
        .get()
        .repository_for_path(root)
        .filter(|repository| repository.worktree_root == root)
        .cloned()
    else {
        return false;
    };
    let updated_root = repository.worktree_root.clone();
    let updated_name = name.clone();
    run_with(
        state,
        store,
        repository,
        OperationKind::Other,
        "Deleting tag…",
        move |service, root| service.write(root, |backend| backend.delete_tag(root, &name)),
        move |app, ()| {
            app.git
                .model
                .update_history_tag(&updated_root, &updated_name, None);
        },
    )
}

pub fn request_push_tag(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    root: &Path,
    name: String,
    pos: (f32, f32),
) -> bool {
    use crate::ui::git::menu::{GitMenu, GitMenuTarget};
    static NEXT_REQUEST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let snapshot = store.get();
    let Some(repository) = snapshot
        .repository_for_path(root)
        .filter(|repository| repository.worktree_root == root)
    else {
        return false;
    };
    if snapshot.operation.is_some() || !repository.tags.iter().any(|tag| tag.name == name) {
        return false;
    }
    let root = repository.worktree_root.clone();
    let request = NEXT_REQUEST.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    state.update(|app| {
        app.git.ui.clear_hover_cards();
        app.git.ui.menu = Some(GitMenu {
            pos,
            target: GitMenuTarget::TagPushRemotes {
                repository: root.clone(),
                name,
                request,
                remotes: None,
            },
            hover: None,
        });
    });
    let Some(application) = state.services().and_then(|runtime| runtime.application()) else {
        finish_tag_remotes(state, request, Err("Git is not available.".into()));
        return true;
    };
    let worker_state = state.clone();
    let worker = thread::Builder::new()
        .name("loog-git-tag-remotes".into())
        .spawn(move || {
            let remotes = crate::infrastructure::git::service()
                .and_then(|service| service.backend().remotes(&root))
                .map(|remotes| {
                    let mut names = remotes
                        .into_iter()
                        .map(|remote| remote.name)
                        .collect::<Vec<_>>();
                    names.sort_by_key(|name| name != "origin");
                    names
                })
                .map_err(|error| error.user_message());
            application.post(move || finish_tag_remotes(&worker_state, request, remotes));
        });
    if let Err(error) = worker {
        finish_tag_remotes(
            state,
            request,
            Err(format!("Could not load remotes: {error}")),
        );
    }
    true
}

fn finish_tag_remotes(state: &AppContext, request: u64, remotes: Result<Vec<String>, String>) {
    state.try_update(|app| {
        app.git
            .ui
            .menu
            .as_mut()
            .is_some_and(|menu| menu.finish_tag_remotes(request, remotes))
    });
}

pub fn push_tag(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    root: &Path,
    name: String,
    remote: String,
) -> bool {
    let Some(repository) = store
        .get()
        .repository_for_path(root)
        .filter(|repository| {
            repository.worktree_root == root && repository.tags.iter().any(|tag| tag.name == name)
        })
        .cloned()
    else {
        return false;
    };
    let message = format!("Pushing tag {name} to {remote}…");
    let success = format!("Pushed tag {name} to {remote}.");
    run_with(
        state,
        store,
        repository,
        OperationKind::Push,
        message,
        move |service, root| service.write(root, |backend| backend.push_tag(root, &remote, &name)),
        move |app, ()| app.show_success(success),
    )
}

/// Ask before dropping the stash `oid`.
pub fn request_drop_stash(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    oid: String,
) -> bool {
    let Some(repository) = active_repository(store) else {
        return false;
    };
    let Some(stash) = repository.stashes.iter().find(|stash| stash.oid == oid) else {
        return false;
    };
    let message = format!("\"{}\" is deleted. This cannot be undone.", stash.message);
    let root = repository.worktree_root;
    state.update(move |app| {
        app.git.ui.confirm = Some(GitConfirm {
            title: "Drop this stash?".into(),
            message,
            confirm_label: "Drop",
            destructive: true,
            action: GitConfirmAction::DropStash {
                repository: root,
                oid,
            },
            restore_terminal: app.terminal.ui.terminal_focused,
        });
    });
    true
}

/// Drop a confirmed stash.
pub fn drop_stash(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    root: &Path,
    oid: String,
) -> bool {
    let Some(repository) = store.get().repository_for_path(root).cloned() else {
        return false;
    };
    run(
        state,
        store,
        repository,
        OperationKind::Stash,
        "Dropping stash…",
        move |service, root| {
            service.write(root, |backend| {
                let index = stash_index(backend, root, &oid)?;
                backend.stash_drop(root, index)
            })
        },
    )
}

/// Apply `action` to `block` of `path`'s `target` diff. The change is cut
/// from a fresh zero-context diff, so it applies exactly as Git sees it now.
pub fn apply_block(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    root: &Path,
    path: PathBuf,
    target: crate::features::git::DiffTarget,
    block: ChangeBlock,
    action: BlockAction,
) -> bool {
    let Some(repository) = store.get().repository_for_path(root).cloned() else {
        return false;
    };
    if action == BlockAction::Discard
        && unsaved(
            state,
            &repository.worktree_root,
            std::slice::from_ref(&path),
        )
    {
        state.update(|app| {
            app.show_toast("Save or revert the open editor changes before discarding.")
        });
        return false;
    }
    let (kind, message) = match action {
        BlockAction::Stage => (OperationKind::Stage, "Staging change…"),
        BlockAction::Unstage => (OperationKind::Unstage, "Unstaging change…"),
        BlockAction::Discard => (OperationKind::Restore, "Discarding change…"),
    };
    run(
        state,
        store,
        repository,
        kind,
        message,
        move |service, root| {
            service.write(root, |backend| {
                apply_block_with(backend, root, &path, target, block, action)
            })
        },
    )
}

/// [`apply_block`]'s Git work.
fn apply_block_with(
    backend: &CliGitBackend,
    root: &Path,
    path: &Path,
    target: crate::features::git::DiffTarget,
    block: ChangeBlock,
    action: BlockAction,
) -> GitResult<()> {
    let raw = backend.zero_context_diff(root, target, path)?;
    let diff = crate::features::git::diff::split_raw_diff(&raw)?;
    let hunk = diff
        .hunks
        .iter()
        .find(|hunk| {
            (
                hunk.old_start,
                hunk.old_lines,
                hunk.new_start,
                hunk.new_lines,
            ) == (
                block.old_start,
                block.old_lines,
                block.new_start,
                block.new_lines,
            )
        })
        .ok_or_else(|| {
            GitError::new(
                GitErrorKind::Conflict,
                "The change moved since the diff was shown. Try again.",
            )
        })?;
    let patch = diff.patch(hunk).ok_or_else(|| {
        GitError::new(
            GitErrorKind::Unsupported,
            "New, deleted and renamed files are staged as a whole.",
        )
    })?;
    let (to_index, reverse) = match action {
        BlockAction::Stage => (true, false),
        BlockAction::Unstage => (true, true),
        BlockAction::Discard => (false, true),
    };
    backend.apply_zero_context_patch(root, &patch, to_index, reverse)
}

/// Ask before discarding one change block.
pub fn request_discard_block(state: &AppContext, root: PathBuf, path: PathBuf, block: ChangeBlock) {
    let lines = block.new_lines.max(block.old_lines);
    let message = format!(
        "The change to {} line{} in {} will be lost. This cannot be undone.",
        lines,
        if lines == 1 { "" } else { "s" },
        file_name(&path)
    );
    state.update(move |app| {
        app.git.ui.confirm = Some(GitConfirm {
            title: "Discard this change?".into(),
            message,
            confirm_label: "Discard",
            destructive: true,
            action: GitConfirmAction::DiscardBlock {
                repository: root,
                path,
                block,
            },
            restore_terminal: app.terminal.ui.terminal_focused,
        });
    });
}

/// Load the newest commits of `root`'s HEAD, which is at `head`, into the
/// Source Control history. Read-only, so it runs beside any operation.
pub fn load_history(state: &AppContext, root: PathBuf, head: Option<String>) {
    load_history_of(state, root, head, None);
}

/// Like [`load_history`], restricted to the commits that touched `path`.
pub fn load_history_of(
    state: &AppContext,
    root: PathBuf,
    head: Option<String>,
    path: Option<PathBuf>,
) {
    let Some(application) = state.services().and_then(|runtime| runtime.application()) else {
        return;
    };
    let mut request = None;
    state.try_update(|app| {
        request = app.git.model.begin_history_of(root, head, path);
        if request.is_some() {
            app.git.ui.details_scroll = 0.0;
        }
        request.is_some()
    });
    if let Some(request) = request {
        load_history_page(state, application, request);
    }
}

pub fn load_more_history(state: &AppContext, root: PathBuf) {
    let Some(application) = state.services().and_then(|runtime| runtime.application()) else {
        return;
    };
    let mut request = None;
    state.try_update(|app| {
        request = app.git.model.begin_history_page(&root);
        // Bookkeeping only: no loading row, animation, or intermediate frame.
        false
    });
    if let Some(request) = request {
        load_history_page(state, application, request);
    }
}

fn load_history_page(
    state: &AppContext,
    application: lgui::ApplicationHandle,
    request: HistoryRequest,
) {
    let worker_state = state.clone();
    let failed_request = request.clone();
    let worker = thread::Builder::new()
        .name("loog-git-history".into())
        .spawn(move || {
            let commits = if request.head.is_none() {
                Ok(Vec::new())
            } else {
                crate::infrastructure::git::service()
                    .and_then(|service| {
                        service.backend().history(
                            &request.repository,
                            request.head.as_deref(),
                            request.path.as_deref(),
                            request.skip,
                            HISTORY_PAGE_SIZE + 1,
                        )
                    })
                    .map_err(|error| error.user_message())
            };
            application.post(move || {
                worker_state.try_update(move |app| {
                    let error = commits.as_ref().err().cloned();
                    if !app.git.model.finish_history_page(&request, commits) {
                        return false;
                    }
                    if request.skip > 0
                        && let Some(error) = error
                    {
                        app.show_error(error);
                    }
                    true
                });
            });
        });
    if let Err(error) = worker {
        state.try_update(|app| {
            let message = format!("Could not load commits: {error}");
            let accepted = app
                .git
                .model
                .finish_history_page(&failed_request, Err(message.clone()));
            if accepted && failed_request.skip > 0 {
                app.show_error(message);
            }
            accepted
        });
    }
}

/// Expand commit `oid` in the history, loading the files it changed.
/// Expanding it again collapses it.
pub fn toggle_commit(state: &AppContext, root: PathBuf, oid: String) {
    let application = state.services().and_then(|runtime| runtime.application());
    let toggled = oid.clone();
    let mut expanded = false;
    state.update(|app| {
        app.git.ui.history_stash = None;
        app.git.ui.history_files_scroll = 0.0;
        app.git.ui.history_summary_scroll = 0.0;
        app.git.ui.summary_scrollbar_dragging = false;
        expanded = app.git.model.toggle_commit(toggled);
    });
    let Some(application) = application.filter(|_| expanded) else {
        return;
    };
    // The detail pane shows the files and the details together, so both are
    // read at once and handed over in one update instead of popping in
    // one after the other.
    let need_details = state.get().git.model.commit_detail(&oid).is_none();
    let worker_state = state.clone();
    let _ = thread::Builder::new()
        .name("loog-git-commit-files".into())
        .spawn(move || {
            let (files, details) = thread::scope(|scope| {
                let details = need_details.then(|| {
                    scope.spawn(|| {
                        crate::infrastructure::git::service()
                            .and_then(|service| service.backend().commit_details(&root, &oid))
                            .map_err(|error| error.user_message())
                    })
                });
                let files = crate::infrastructure::git::service()
                    .and_then(|service| service.backend().commit_files(&root, &oid))
                    .map_err(|error| error.user_message());
                let details = details.map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|_| Err("Could not read the commit.".into()))
                });
                (files, details)
            });
            application.post(move || {
                worker_state.update(move |app| {
                    if let Some(details) = details {
                        app.git.model.cache_commit_detail(oid.clone(), details);
                    }
                    app.git.model.set_commit_files(&oid, files);
                });
            });
        });
}

/// Open `path`'s diff in commit `oid`.
pub fn open_commit_diff(state: &AppContext, root: PathBuf, oid: String, path: PathBuf) {
    let Some(application) = state.services().and_then(|runtime| runtime.application()) else {
        return;
    };
    let worker_state = state.clone();
    let _ = thread::Builder::new()
        .name("loog-git-commit-diff".into())
        .spawn(move || {
            let result = crate::infrastructure::git::service().and_then(|service| {
                let diff = service.backend().commit_full_diff(&root, &oid, &path)?;
                Ok(Arc::new(DiffDocument::commit(root, path, oid, diff)))
            });
            application.post(move || {
                let mut result = Some(result);
                worker_state.update(|app| match result.take().unwrap() {
                    Ok(document) => {
                        app.workspace.open_prepared_diff(document);
                    }
                    Err(error) => app.show_error(error.user_message()),
                });
                crate::infrastructure::ui_dispatch::retire(result);
            });
        });
}

/// Open every file of commit `oid` in one tab, scrolled to `focus` if given.
pub fn open_commit_changes(state: &AppContext, root: PathBuf, oid: String, focus: Option<PathBuf>) {
    if let Some(path) = focus {
        super::git_diff::open_commit_file(state, root, oid, path);
        return;
    }
    let mut revealed = false;
    state.update(|app| {
        if let Some(id) = app.workspace.commit_changes_id(&root, &oid)
            && let Some(pane) = app.workspace.pane_of(id)
        {
            app.workspace.activate(pane, id);
            // Invalidate a slower open still in flight.
            app.git.model.cancel_diff_open();
            revealed = true;
        }
    });
    if revealed {
        return;
    }
    let app = state.get();
    let Some(application) = state.services().and_then(|runtime| runtime.application()) else {
        return;
    };
    drop(app);
    let loading = crate::features::git::DiffRequest {
        repository_root: root.clone(),
        path: PathBuf::new(),
        target: crate::features::git::DiffTarget::Commit,
    };
    let mut sequence = 0;
    state.update(|app| sequence = app.git.model.begin_diff_open(loading));
    let worker_state = state.clone();
    let spawned = thread::Builder::new()
        .name("loog-git-commit-changes".into())
        .spawn(move || {
            let result = crate::infrastructure::git::service().and_then(|service| {
                let content = service.commit_changes(&root, &oid)?;
                Ok(Arc::new(DiffDocument::commit_changes(root, oid, content)))
            });
            application.post(move || {
                let mut result = Some(result);
                worker_state.update(|app| {
                    if !app.git.model.finish_diff_open(sequence) {
                        return;
                    }
                    match result.take().unwrap() {
                        Ok(document) => {
                            app.workspace.open_prepared_commit_changes(document);
                        }
                        Err(error) => app.show_error(error.user_message()),
                    }
                });
                crate::infrastructure::ui_dispatch::retire(result);
            });
        })
        .is_ok();
    if !spawned {
        state.update(|app| {
            app.git.model.abandon_diff_open();
            app.show_error("Could not start Git diff worker.");
        });
    }
}

/// How long the pointer rests on a history commit before its card shows.
const COMMIT_CARD_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// Show commit `oid`'s hover card beside `anchor`, its row on screen, once
/// the pointer has rested there.
pub fn hover_commit(state: &AppContext, root: PathBuf, oid: String, anchor: lgui::prelude::UiRect) {
    let Some(application) = state.services().and_then(|runtime| runtime.application()) else {
        return;
    };
    let hovered = crate::ui::git::panel::GitHover::Commit(oid.clone());
    let worker_state = state.clone();
    let _ = thread::Builder::new()
        .name("loog-git-commit-card".into())
        .spawn(move || {
            thread::sleep(COMMIT_CARD_DELAY);
            let app = worker_state.get();
            if app.git.ui.hover.as_ref() != Some(&hovered) {
                return;
            }
            let cached = app.git.model.commit_detail(&oid).cloned();
            drop(app);
            let details = cached.unwrap_or_else(|| {
                crate::infrastructure::git::service()
                    .and_then(|service| service.backend().commit_details(&root, &oid))
                    .map_err(|error| error.user_message())
            });
            application.post(move || {
                worker_state.update(move |app| {
                    app.git
                        .model
                        .cache_commit_detail(oid.clone(), details.clone());
                    if app.git.ui.hover.as_ref() == Some(&hovered) {
                        app.git.ui.commit_card = Some(crate::ui::git::panel::CommitCard {
                            oid,
                            anchor,
                            details,
                        });
                    }
                });
            });
        });
}

/// Load tag metadata after the same dwell delay used by history commits.
pub fn hover_tag(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    root: PathBuf,
    name: String,
    anchor: lgui::prelude::UiRect,
) {
    let Some(application) = state.services().and_then(|runtime| runtime.application()) else {
        return;
    };
    let snapshot = store.get();
    let Some(tag) = snapshot
        .active()
        .filter(|repository| repository.worktree_root == root)
        .and_then(|repository| repository.tags.iter().find(|tag| tag.name == name))
        .cloned()
    else {
        return;
    };
    let sequence = state.get().git.ui.hover_sequence;
    let worker_state = state.clone();
    let store = store.clone();
    let _ = thread::Builder::new()
        .name("loog-git-tag-card".into())
        .spawn(move || {
            thread::sleep(COMMIT_CARD_DELAY);
            if !tag_hover_current(&worker_state.get(), &store.get(), &root, &tag, sequence) {
                return;
            }
            let details = crate::infrastructure::git::service()
                .and_then(|service| service.backend().tag_details(&root, &name))
                .map_err(|error| error.user_message());
            application.post(move || {
                worker_state.try_update(|app| {
                    if !tag_hover_current(app, &store.get(), &root, &tag, sequence) {
                        return false;
                    }
                    app.git.ui.tag_card = Some(crate::ui::git::panel::TagCard {
                        repository: root,
                        tag,
                        anchor,
                        details,
                    });
                    true
                });
            });
        });
}

fn tag_hover_current(
    app: &AppState,
    store: &GitStoreSnapshot,
    root: &Path,
    tag: &crate::features::git::TagInfo,
    sequence: u64,
) -> bool {
    app.git.ui.hover_sequence == sequence
        && app.git.ui.hover.as_ref()
            == Some(&crate::ui::git::panel::GitHover::Tag(tag.name.clone()))
        && !app
            .git
            .ui
            .collapsed_sections
            .contains(&crate::ui::git::panel::TAGS)
        && app.git.ui.menu.is_none()
        && app.git.ui.confirm.is_none()
        && store.active().is_some_and(|repository| {
            repository.worktree_root == root && repository.tags.contains(tag)
        })
}

/// Create a commit that undoes commit `oid`. Conflicts leave a revert in
/// progress for Source Control to continue or abort.
pub fn revert_commit(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    root: &Path,
    oid: String,
) -> bool {
    let Some(repository) = store.get().repository_for_path(root).cloned() else {
        return false;
    };
    if state
        .get()
        .workspace
        .has_dirty_paths_under(&repository.worktree_root)
    {
        state.update(|app| {
            app.show_toast("Save or revert the open editor changes before reverting.")
        });
        return false;
    }
    let short = crate::features::git::diff_document::short_oid(&oid).to_owned();
    run_with(
        state,
        store,
        repository,
        OperationKind::Other,
        "Reverting commit…",
        move |service, root| service.write(root, |backend| backend.revert(root, &oid)),
        move |app, ()| app.show_success(format!("Reverted commit {short}.")),
    )
}

/// Make the repository at `root` active in Source Control until the
/// active file changes.
pub fn select_repository(state: &AppContext, store: &State<GitStoreSnapshot>, root: PathBuf) {
    let pinned = root.clone();
    state.update(move |app| {
        let active_path = app.workspace.active_path().map(Path::to_path_buf);
        app.git.model.pin_repository(pinned, active_path);
        app.git.ui.reset_scroll();
    });
    store.update(move |snapshot| snapshot.pin_active(Some(&root)));
}

/// Open means add the worktree and select its repository in our single window.
pub fn open_worktree(state: &AppContext, store: &State<GitStoreSnapshot>, path: PathBuf) -> bool {
    if !path.is_dir() {
        state.update(|app| {
            app.show_error(format!(
                "Worktree is no longer available: {}",
                path.display()
            ))
        });
        return false;
    }
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    state.update(|app| crate::app::explorer::add_folder(app, path.clone()));
    select_repository(state, store, path);
    state
        .services()
        .map(|runtime| runtime.git_poll_control())
        .unwrap_or_default()
        .request_refresh();
    true
}

pub fn add_worktree_folder(state: &AppContext, path: PathBuf) {
    if !path.is_dir() {
        state.update(|app| {
            app.show_error(format!(
                "Worktree is no longer available: {}",
                path.display()
            ))
        });
        return;
    }
    state.update(|app| crate::app::explorer::add_folder(app, path));
    state
        .services()
        .map(|runtime| runtime.git_poll_control())
        .unwrap_or_default()
        .request_refresh();
}

fn default_worktree_path(repository: &Path, checkout: &Checkout) -> PathBuf {
    let branch = match checkout {
        Checkout::Track(remote) => remote
            .split_once('/')
            .map_or(remote.as_str(), |(_, name)| name),
        _ => checkout.target(),
    };
    let suffix = branch
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    let name = repository.file_name().unwrap_or_default().to_string_lossy();
    repository
        .parent()
        .unwrap_or(repository)
        .join(format!("{name}-{suffix}"))
}

fn create_worktree_message(path: &Path, checkout: &Checkout) -> String {
    format!(
        "Check out {} and add it to the workspace:\n{}",
        checkout.target(),
        crate::ui::display_path(path)
    )
}

pub fn request_create_worktree(state: &AppContext, repository: PathBuf, checkout: Checkout) {
    let path = default_worktree_path(&repository, &checkout);
    state.update(move |app| {
        app.git.ui.branch_picker = None;
        app.git.ui.confirm = Some(GitConfirm {
            title: "New Worktree".into(),
            message: create_worktree_message(&path, &checkout),
            confirm_label: "Create",
            destructive: false,
            action: GitConfirmAction::CreateWorktree {
                repository,
                path,
                checkout,
            },
            restore_terminal: app.terminal.ui.terminal_focused,
        });
    });
}

pub fn choose_worktree_location(state: &AppContext) {
    use lgui::dialogs::{FileDialogOptions, system_file_dialogs};
    let Some(confirm) = state.get().git.ui.confirm else {
        return;
    };
    let GitConfirmAction::CreateWorktree { path, .. } = confirm.action else {
        return;
    };
    let mut options = FileDialogOptions::new().title("Choose Parent Folder for Worktree");
    if let Some(parent) = path.parent() {
        options = options.directory(parent);
    }
    let Some(parent) = system_file_dialogs().pick_folder(&options) else {
        return;
    };
    let Some(name) = path.file_name() else {
        return;
    };
    let path = parent.join(name);
    state.update(move |app| {
        if let Some(confirm) = app.git.ui.confirm.as_mut()
            && let GitConfirmAction::CreateWorktree {
                path: target,
                checkout,
                ..
            } = &mut confirm.action
        {
            *target = path;
            confirm.message = create_worktree_message(target, checkout);
        }
    });
}

fn create_worktree_with(
    backend: &CliGitBackend,
    root: &Path,
    path: &Path,
    checkout: &Checkout,
) -> GitResult<()> {
    match checkout {
        Checkout::Switch(branch) => backend.add_worktree(root, path, Some(branch), false),
        Checkout::Create(branch) => backend.add_worktree(root, path, Some(branch), true),
        Checkout::Track(remote) => backend.add_tracking_worktree(root, path, remote),
        Checkout::OpenWorktree { .. } => Err(GitError::new(
            GitErrorKind::Other,
            "This branch already has a worktree. Open it instead.",
        )),
    }
}

pub fn create_worktree(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    root: &Path,
    path: PathBuf,
    checkout: Checkout,
) -> bool {
    if store.get().operation.is_some() {
        state.update(|app| app.show_toast("Wait for the running Git operation to finish."));
        return false;
    }
    let Some(repository) = store.get().repository_for_path(root).cloned() else {
        return false;
    };
    let created = path.clone();
    run_with(
        state,
        store,
        repository,
        OperationKind::Worktree,
        "Creating worktree…",
        move |service, root| {
            service.write(root, |backend| {
                create_worktree_with(backend, root, &path, &checkout)
            })
        },
        move |app, ()| {
            let path = std::fs::canonicalize(&created).unwrap_or(created);
            crate::app::explorer::add_folder(app, path.clone());
            let active_path = app.workspace.active_path().map(Path::to_path_buf);
            app.git.model.pin_repository(path, active_path);
            app.git.ui.reset_scroll();
            app.show_success("Created worktree and added it to the workspace.");
        },
    )
}

fn worktree_removal_confirmation(
    repository: PathBuf,
    path: PathBuf,
    force: bool,
    restore_terminal: bool,
) -> GitConfirm {
    GitConfirm {
        title: if force {
            "Force Delete Worktree"
        } else {
            "Delete Worktree"
        }
        .into(),
        message: if force {
            format!(
                "This worktree has modified or untracked files. Permanently delete them?\n{}",
                crate::ui::display_path(&path)
            )
        } else {
            format!(
                "Delete this worktree from disk? Its branch will be kept.\n{}",
                crate::ui::display_path(&path)
            )
        },
        confirm_label: if force { "Force Delete" } else { "Delete" },
        destructive: true,
        action: GitConfirmAction::RemoveWorktree {
            repository,
            path,
            force,
        },
        restore_terminal,
    }
}

pub fn request_remove_worktree(
    state: &AppContext,
    repository: PathBuf,
    path: PathBuf,
    force: bool,
) {
    state.update(move |app| {
        app.git.ui.confirm = Some(worktree_removal_confirmation(
            repository,
            path,
            force,
            app.terminal.ui.terminal_focused,
        ));
    });
}

/// Re-read metadata before removal so an external lock or a stale UI cannot
/// turn the wrong worktree into a deletion target.
fn remove_worktree_with(
    backend: &CliGitBackend,
    root: &Path,
    path: &Path,
    force: bool,
) -> GitResult<bool> {
    let worktrees = backend.worktrees(root)?;
    let worktree = worktrees
        .iter()
        .find(|worktree| worktree.path == path)
        .ok_or_else(|| {
            GitError::new(
                GitErrorKind::Other,
                "This worktree is no longer registered.",
            )
        })?;
    if path == root
        || worktrees.first().is_some_and(|main| main.path == path)
        || worktree.bare
        || worktree.locked.is_some()
    {
        return Err(GitError::new(
            GitErrorKind::Other,
            "The current, main, or locked worktree cannot be deleted.",
        ));
    }
    match backend.remove_worktree(root, path, force) {
        Ok(()) => Ok(true),
        Err(error) if !force && error.kind == GitErrorKind::DirtyWorktree => Ok(false),
        Err(error) => Err(error),
    }
}

pub fn remove_worktree(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    root: &Path,
    path: PathBuf,
    force: bool,
) -> bool {
    if store.get().operation.is_some() {
        state.update(|app| app.show_toast("Wait for the running Git operation to finish."));
        return false;
    }
    let Some(repository) = store.get().repository_for_path(root).cloned() else {
        return false;
    };
    if state.get().workspace.has_dirty_paths_under(&path) {
        state.update(|app| {
            app.show_toast("Save or discard editor changes before deleting this worktree.")
        });
        return false;
    }
    let source = repository.worktree_root.clone();
    let target = path.clone();
    run_with(
        state,
        store,
        repository,
        OperationKind::Worktree,
        "Deleting worktree…",
        move |service, root| {
            service.write(root, |backend| {
                remove_worktree_with(backend, root, &path, force)
            })
        },
        move |app, removed| {
            if removed {
                crate::app::explorer::forget_worktree(app, &target);
                app.show_success("Deleted worktree. Its branch was kept.");
            } else {
                app.git.ui.confirm = Some(worktree_removal_confirmation(
                    source,
                    target,
                    true,
                    app.terminal.ui.terminal_focused,
                ));
            }
        },
    )
}

/// Undo the last commit, keeping its changes staged, and offer its message
/// for the next commit.
pub fn undo_last_commit(state: &AppContext, store: &State<GitStoreSnapshot>) -> bool {
    let Some(repository) = active_repository(store) else {
        return false;
    };
    run_with(
        state,
        store,
        repository,
        OperationKind::Other,
        "Undoing last commit…",
        |service, root| service.write(root, |backend| backend.undo_commit(root)),
        |app, message| {
            if app.git.ui.commit_input.text().trim().is_empty() {
                // The input is single-line: keep the subject.
                app.git
                    .ui
                    .commit_input
                    .set_text(message.lines().next().unwrap_or_default());
            }
            app.show_success("Undid the last commit. Its changes are staged.");
        },
    )
}

/// Run the active repository's [`SyncPlan`].
pub fn sync(state: &AppContext, store: &State<GitStoreSnapshot>) -> bool {
    let Some(repository) = active_repository(store) else {
        return false;
    };
    let (Some(plan), HeadState::Branch(branch)) = (sync_plan(&repository), repository.head.clone())
    else {
        return false;
    };
    if matches!(plan, SyncPlan::Pull { .. })
        && state
            .get()
            .workspace
            .has_dirty_paths_under(&repository.worktree_root)
    {
        state.update(|app| app.show_toast("Save or discard editor changes before pulling."));
        return false;
    }
    let (kind, message) = match plan {
        SyncPlan::Publish => (OperationKind::Push, format!("Publishing {branch}…")),
        SyncPlan::Fetch => (OperationKind::Fetch, "Fetching…".to_owned()),
        SyncPlan::Pull { then_push: true } => (OperationKind::Pull, "Syncing…".to_owned()),
        SyncPlan::Pull { then_push: false } => (OperationKind::Pull, "Pulling…".to_owned()),
        SyncPlan::Push => (OperationKind::Push, "Pushing…".to_owned()),
    };
    run(
        state,
        store,
        repository,
        kind,
        message,
        move |service, root| {
            service.write(root, |backend| match plan {
                SyncPlan::Publish => {
                    let remotes = backend.remotes(root)?;
                    let remote = publish_remote(&remotes).ok_or_else(|| {
                        GitError::new(
                            GitErrorKind::Other,
                            "Add a remote before publishing this branch.",
                        )
                    })?;
                    backend.push(root, Some((remote, &branch)), false)
                }
                SyncPlan::Fetch => backend.fetch(root, None, true),
                SyncPlan::Pull { then_push } => {
                    // Plain `git pull` honors the user's pull.rebase setting.
                    backend.pull(root, false)?;
                    if then_push {
                        backend.push(root, None, false)?;
                    }
                    Ok(())
                }
                SyncPlan::Push => backend.push(root, None, false),
            })
        },
    )
}

/// The remote a new branch is published to: `origin`, else the first one.
fn publish_remote(remotes: &[RemoteInfo]) -> Option<&str> {
    remotes
        .iter()
        .find(|remote| remote.name == "origin")
        .or_else(|| remotes.first())
        .map(|remote| remote.name.as_str())
}

/// Read `repository`'s branches for the history's branch list.
pub fn load_history_branches(state: &AppContext, repository: PathBuf) {
    let Some(application) = state.services().and_then(|runtime| runtime.application()) else {
        return;
    };
    let worker_state = state.clone();
    let _ = thread::Builder::new()
        .name("loog-git-history-branches".into())
        .spawn(move || {
            let Ok(branches) = crate::infrastructure::git::service()
                .and_then(|service| service.backend().branches(&repository))
            else {
                return;
            };
            application.post(move || {
                worker_state.update(move |app| {
                    app.git.ui.history_branches = Some((repository, branches));
                });
            });
        });
}

/// List `repository`'s branches into the open branch picker opened as
/// `sequence`. Read-only, so it runs beside any write operation.
pub fn load_branches(state: &AppContext, repository: PathBuf, sequence: u64) {
    let Some(application) = state.services().and_then(|runtime| runtime.application()) else {
        return;
    };
    let worker_state = state.clone();
    let spawned = thread::Builder::new()
        .name("loog-git-branches".into())
        .spawn(move || {
            let result = crate::infrastructure::git::service().and_then(|service| {
                let branches = service.backend().branches(&repository)?;
                let worktrees = service.backend().worktrees(&repository)?;
                Ok((branches, worktrees))
            });
            application.post(move || {
                worker_state.update(move |app| {
                    if let Some(picker) = app
                        .git
                        .ui
                        .branch_picker
                        .as_mut()
                        .filter(|picker| picker.sequence == sequence)
                    {
                        picker.listing = BranchListing::from_result(result);
                    }
                });
            });
        })
        .is_ok();
    if !spawned {
        state.update(move |app| {
            if let Some(picker) = app.git.ui.branch_picker.as_mut() {
                picker.listing = BranchListing::Failed("Could not start Git worker.".into());
            }
        });
    }
}

/// Fetch every remote, then reload the open branch picker's list.
pub fn fetch_branches(state: &AppContext, store: &State<GitStoreSnapshot>) -> bool {
    let Some(picker) = state.get().git.ui.branch_picker else {
        return false;
    };
    let Some(repository) = store.get().repository_for_path(&picker.repository).cloned() else {
        return false;
    };
    let sequence = picker.sequence;
    run_with(
        state,
        store,
        repository,
        OperationKind::Fetch,
        "Fetching…",
        |service, root| {
            service.write(root, |backend| {
                backend.fetch(root, None, true)?;
                let branches = backend.branches(root)?;
                let worktrees = backend.worktrees(root)?;
                Ok((branches, worktrees))
            })
        },
        move |app, branches| {
            if let Some(picker) = app
                .git
                .ui
                .branch_picker
                .as_mut()
                .filter(|picker| picker.sequence == sequence)
            {
                picker.listing = BranchListing::from_result(Ok(branches));
            }
        },
    )
}

#[derive(Debug, PartialEq, Eq)]
pub enum CheckoutOutcome {
    Switched,
    /// Switched; the changes wait in the stash.
    Stashed,
    /// Switched, but reapplying the changes conflicted. The stash is kept.
    CarryConflicted,
    /// Refused: these files have changes the switch would overwrite.
    Blocked(Vec<String>),
}

/// Check out `checkout` with `backend`, handling local changes per `mode`.
/// A switch that fails after stashing restores the stash, so a failure
/// never leaves the changes stashed away.
pub fn checkout_with(
    backend: &CliGitBackend,
    root: &Path,
    checkout: &Checkout,
    mode: StashMode,
) -> GitResult<CheckoutOutcome> {
    let switch = || match checkout {
        Checkout::Switch(name) => backend.switch(root, name),
        Checkout::Create(name) => backend.create_branch(root, name, None),
        Checkout::Track(remote) => backend.switch_tracking(root, remote),
        Checkout::OpenWorktree { .. } => Err(GitError::new(
            GitErrorKind::Other,
            "Open the existing worktree instead of switching branches.",
        )),
    };
    if mode == StashMode::Refuse {
        return match switch() {
            Ok(()) => Ok(CheckoutOutcome::Switched),
            Err(error) if error.kind == GitErrorKind::DirtyWorktree => {
                Ok(CheckoutOutcome::Blocked(overwritten_files(&error)))
            }
            Err(error) => Err(error),
        };
    }
    // `git stash push` succeeds without stashing when there is nothing to
    // stash; only a new top entry is ours to pop.
    let top = |backend: &CliGitBackend| {
        backend
            .stash_list(root)
            .map(|stashes| stashes.first().map(|stash| stash.oid.clone()))
    };
    let before = top(backend)?;
    let message = format!("Loog: before switching to {}", checkout.target());
    backend.stash_push(root, Some(&message), true, false)?;
    let stashed = top(backend)? != before;
    if let Err(error) = switch() {
        if stashed && let Err(restore) = backend.stash_pop(root) {
            return Err(GitError::new(
                GitErrorKind::Other,
                "Could not switch branches; your changes are kept in the stash.",
            )
            .with_detail(format!(
                "{}\n{}",
                error.user_message(),
                restore.user_message()
            )));
        }
        return Err(error);
    }
    if !stashed {
        return Ok(CheckoutOutcome::Switched);
    }
    if mode == StashMode::Keep {
        return Ok(CheckoutOutcome::Stashed);
    }
    Ok(match backend.stash_pop(root) {
        Ok(()) => CheckoutOutcome::Switched,
        Err(_) => CheckoutOutcome::CarryConflicted,
    })
}

/// The files Git lists, tab-indented, as blocking an operation.
fn overwritten_files(error: &GitError) -> Vec<String> {
    error
        .detail
        .as_deref()
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.strip_prefix('\t'))
        .map(|path| path.trim().to_owned())
        .filter(|path| !path.is_empty())
        .collect()
}

/// Check out `checkout` in the repository at `root`.
pub fn checkout(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    root: &Path,
    checkout: Checkout,
    mode: StashMode,
) -> bool {
    if let Checkout::OpenWorktree { path, .. } = checkout {
        return open_worktree(state, store, path);
    }
    let Some(repository) = store.get().repository_for_path(root).cloned() else {
        return false;
    };
    if state
        .get()
        .workspace
        .has_dirty_paths_under(&repository.worktree_root)
    {
        state.update(|app| {
            app.show_toast("Save or discard editor changes before switching branches.")
        });
        return false;
    }
    if store.get().operation.is_some() {
        state.update(|app| app.show_toast("Wait for the running Git operation to finish."));
        return false;
    }
    let target = checkout.target().to_owned();
    let message = match (&checkout, mode) {
        (Checkout::Create(name), _) => format!("Creating {name}…"),
        (_, StashMode::Refuse) => format!("Switching to {target}…"),
        (_, StashMode::Keep | StashMode::Carry) => format!("Stashing and switching to {target}…"),
    };
    let repository_root = repository.worktree_root.clone();
    let blocked_checkout = checkout.clone();
    run_with(
        state,
        store,
        repository,
        OperationKind::Checkout,
        message,
        move |service, root| {
            service.write(root, |backend| {
                checkout_with(backend, root, &checkout, mode)
            })
        },
        move |app, outcome| match outcome {
            CheckoutOutcome::Switched => {}
            CheckoutOutcome::Stashed => {
                app.show_success(format!(
                    "Switched to {target}. Your changes are in the stash."
                ));
            }
            CheckoutOutcome::CarryConflicted => {
                app.show_error(format!(
                    "Switched to {target}, but your changes conflict with it. Resolve the \
                     conflicts; the stash was kept."
                ));
            }
            CheckoutOutcome::Blocked(files) => {
                app.git.ui.stash_switch = Some(StashSwitchRequest {
                    repository: repository_root,
                    checkout: blocked_checkout,
                    files,
                    restore_terminal: app.terminal.ui.terminal_focused,
                });
            }
        },
    )
}

fn active_repository(store: &State<GitStoreSnapshot>) -> Option<RepositorySnapshot> {
    store.get().active().cloned()
}

fn run(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    repository: RepositorySnapshot,
    kind: OperationKind,
    message: impl Into<String>,
    action: impl FnOnce(&GitService, &std::path::Path) -> crate::features::git::GitResult<()>
    + Send
    + 'static,
) -> bool {
    run_with(state, store, repository, kind, message, action, |_, ()| {})
}

/// [`run`], handing the action's result to `on_success` on the UI thread.
fn run_with<T: Send + 'static>(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    repository: RepositorySnapshot,
    kind: OperationKind,
    message: impl Into<String>,
    action: impl FnOnce(&GitService, &std::path::Path) -> crate::features::git::GitResult<T>
    + Send
    + 'static,
    on_success: impl FnOnce(&mut AppState, T) + Send + 'static,
) -> bool {
    if store.get().operation.is_some() {
        return false;
    }
    let Some(tasks) = super::git_tasks::GitTasks::for_state(state, store) else {
        return false;
    };
    let root = repository.worktree_root;
    tasks.run(
        kind,
        message,
        move || crate::infrastructure::git::service().and_then(|service| action(&service, &root)),
        on_success,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::features::git::{RepositoryFeatures, RepositoryId, RepositoryState, UpstreamState};

    fn dropped_diff(path: &str) -> Arc<DiffDocument> {
        Arc::new(DiffDocument::added(
            "repo".into(),
            path.into(),
            crate::features::git::DiffTarget::IndexToWorktree,
            "added\n",
        ))
    }

    #[test]
    fn diff_drop_moves_the_unique_view_in_each_split_direction() {
        use crate::model::pane_layout::Direction;
        for direction in [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ] {
            let mut app = AppState::new();
            let keep = app
                .workspace
                .test_open_view("keep.txt".into(), String::new());
            let original = app.workspace.open_prepared_diff(dropped_diff("a.txt"));
            let pane = app.workspace.active_pane();
            assert!(apply_diff_drop(
                &mut app,
                dropped_diff("a.txt"),
                TabDrop::Pane {
                    pane,
                    split: Some(direction),
                }
            ));
            assert_eq!(app.workspace.pane_ids().len(), 2);
            assert_eq!(app.workspace.pane(pane).unwrap().items(), &[keep]);
            assert_ne!(app.workspace.active_pane(), pane);
            assert_eq!(app.workspace.active(), Some(original));
            assert_eq!(
                app.workspace
                    .diff(app.workspace.active_pane())
                    .unwrap()
                    .path,
                Path::new("a.txt")
            );
            assert_eq!(app.workspace.git_poll_target().diffs.len(), 1);
        }
    }

    #[test]
    fn diff_drop_inserts_and_reuses_the_unique_matching_view() {
        use crate::model::pane_layout::Direction;
        let mut app = AppState::new();
        let keep = app
            .workspace
            .test_open_view("keep.txt".into(), String::new());
        let original = app.workspace.open_prepared_diff(dropped_diff("a.txt"));
        let source = app.workspace.active_pane();
        let target = app.workspace.split_empty(source, Direction::Right).unwrap();
        app.workspace.activate_pane(target);
        let first = app
            .workspace
            .test_open_view("first.txt".into(), String::new());
        let second = app
            .workspace
            .test_open_view("second.txt".into(), String::new());
        assert!(apply_diff_drop(
            &mut app,
            dropped_diff("a.txt"),
            TabDrop::Insert {
                pane: target,
                index: 1,
            }
        ));
        let diff = app.workspace.active().unwrap();
        assert_eq!(diff, original);
        assert_eq!(
            app.workspace.pane(target).unwrap().items(),
            &[first, diff, second]
        );
        assert_eq!(app.workspace.pane(source).unwrap().items(), &[keep]);
        assert!(apply_diff_drop(
            &mut app,
            dropped_diff("a.txt"),
            TabDrop::Pane {
                pane: target,
                split: None,
            }
        ));
        assert_eq!(app.workspace.active(), Some(diff));
        assert_eq!(app.workspace.pane(target).unwrap().items().len(), 3);
        assert!(apply_diff_drop(
            &mut app,
            dropped_diff("a.txt"),
            TabDrop::Insert {
                pane: target,
                index: 3,
            }
        ));
        assert_eq!(
            app.workspace.pane(target).unwrap().items(),
            &[first, second, diff]
        );
    }

    #[test]
    fn diff_drop_to_a_closed_pane_leaves_the_current_layout_intact() {
        use crate::model::pane_layout::Direction;
        let mut app = AppState::new();
        let current = app
            .workspace
            .test_open_view("current.txt".into(), "keep".into());
        let source = app.workspace.active_pane();
        let target = app.workspace.split_empty(source, Direction::Right).unwrap();
        app.workspace.close_pane(target);
        assert!(!apply_diff_drop(
            &mut app,
            dropped_diff("a.txt"),
            TabDrop::Pane {
                pane: target,
                split: Some(Direction::Down),
            }
        ));
        assert_eq!(app.workspace.pane_ids(), vec![source]);
        assert_eq!(app.workspace.active(), Some(current));
    }

    #[test]
    fn diff_drop_async_reads_preserve_comparison_and_ignore_failed_or_stale_destinations() {
        use crate::features::git::diff_document::DiffRowKind;
        use crate::features::git::{DiffRequest, DiffTarget};
        use crate::model::pane_layout::Direction;
        use lgui::application::{AppView, ApplicationHandle};
        use lgui::core::UiScale;
        use lgui::prelude::{UiRect, VisualStyle, panel};
        use lgui::session::UiSession;
        use std::sync::Mutex;
        use std::time::{Duration, Instant};

        let (root, backend) = crate::infrastructure::git::backend::tests::required_repository();
        std::fs::write(root.join("file.txt"), "base\n").unwrap();
        backend.stage(&root, &["file.txt".into()]).unwrap();
        backend.commit(&root, "base", false, false).unwrap();
        std::fs::write(root.join("file.txt"), "staged\n").unwrap();
        backend.stage(&root, &["file.txt".into()]).unwrap();
        std::fs::write(root.join("file.txt"), "working\n").unwrap();
        std::fs::write(root.join("new.txt"), "untracked\n").unwrap();

        for scenario in [
            "staged",
            "working",
            "untracked",
            "failed",
            "closed",
            "navigated",
        ] {
            let tasks = Arc::new(Mutex::new(Vec::new()));
            let queue = tasks.clone();
            let application =
                ApplicationHandle::new(move |task| queue.lock().unwrap().push(task), || {});
            let runtime = Arc::new(crate::app::runtime::AppRuntime::default());
            runtime.set_application(application);
            let exposed = Arc::new(Mutex::new(None::<AppContext>));
            let output = exposed.clone();
            let roots = vec![root.clone()];
            let viewport = UiRect::new(0.0, 0.0, 600.0, 400.0);
            let view: AppView = Arc::new(move |cx| {
                let state = AppContext::detached(cx.state_with(|| {
                    let mut app = AppState::new();
                    app.explorer.model.restore(roots.clone(), Vec::new());
                    app.workspace
                        .test_open_view("current.txt".into(), "keep".into());
                    app
                }))
                .with_runtime(&runtime);
                *output.lock().unwrap() = Some(state);
                panel(viewport, VisualStyle::default())
            });
            let mut session = UiSession::new();
            session.render_view(&view, viewport, UiScale::ONE);
            let state = exposed.lock().unwrap().clone().unwrap();
            let mut target = state.get().workspace.active_pane();
            if scenario == "closed" {
                state.update(|app| {
                    target = app.workspace.split_empty(target, Direction::Right).unwrap()
                });
            }
            let request = DiffRequest {
                repository_root: if scenario == "failed" {
                    root.join("missing")
                } else {
                    root.clone()
                },
                path: if scenario == "untracked" {
                    "new.txt".into()
                } else {
                    "file.txt".into()
                },
                target: if scenario == "staged" {
                    DiffTarget::HeadToIndex
                } else {
                    DiffTarget::IndexToWorktree
                },
            };
            assert!(open_diff_at(
                &state,
                request.clone(),
                scenario == "untracked",
                TabDrop::Pane {
                    pane: target,
                    split: Some(Direction::Right),
                }
            ));
            if scenario == "closed" {
                state.update(|app| app.workspace.close_pane(target));
            } else if scenario == "navigated" {
                state.update(|app| {
                    app.workspace
                        .test_open_view("another.txt".into(), "another".into());
                });
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let completion = tasks.lock().unwrap().pop();
                if let Some(completion) = completion {
                    completion();
                }
                // Language timers can post before the diff worker's completion.
                if state.get().git.model.diff_loading().is_none() {
                    break;
                }
                assert!(Instant::now() < deadline, "diff worker did not respond");
                std::thread::sleep(Duration::from_millis(5));
            }
            let app = state.get();
            assert!(app.git.model.diff_loading().is_none());
            if matches!(scenario, "failed" | "closed" | "navigated") {
                assert_eq!(app.workspace.pane_ids().len(), 1);
                assert!(app.workspace.diff(app.workspace.active_pane()).is_none());
                assert_eq!(
                    app.workspace.active_editor().unwrap().text(),
                    if scenario == "navigated" {
                        "another"
                    } else {
                        "keep"
                    }
                );
            } else {
                assert_eq!(app.workspace.pane_ids().len(), 2);
                let diff = app.workspace.diff(app.workspace.active_pane()).unwrap();
                assert_eq!(diff.request(), request);
                assert!(
                    diff.rows
                        .iter()
                        .any(|row| row.kind == DiffRowKind::Addition && row.text == scenario)
                );
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn worktree_defaults_are_siblings_with_safe_branch_names() {
        assert_eq!(
            default_worktree_path(
                Path::new("projects/loog"),
                &Checkout::Create("feature/x".into())
            ),
            PathBuf::from("projects/loog-feature-x")
        );
        assert_eq!(
            default_worktree_path(
                Path::new("projects/loog"),
                &Checkout::Track("origin/feature/x".into())
            ),
            PathBuf::from("projects/loog-feature-x")
        );
    }

    #[test]
    fn real_repository_manages_worktrees_without_switching_the_source() {
        use crate::infrastructure::git::backend::GitBackend;
        use crate::infrastructure::git::command::GitCommand;
        let (root, backend) = crate::infrastructure::git::backend::tests::repository()
            .expect("Git runtime for worktree integration test");
        let root = std::fs::canonicalize(root).unwrap();
        std::fs::write(root.join("file.txt"), "base\n").unwrap();
        backend.stage(&root, &[PathBuf::from("file.txt")]).unwrap();
        backend.commit(&root, "base", false, false).unwrap();
        let path = root.join("topic worktree");
        let checkout = Checkout::Create("feature/topic".into());
        create_worktree_with(&backend, &root, &path, &checkout).unwrap();
        let path = std::fs::canonicalize(path).unwrap();
        let branches = backend.branches(&root).unwrap();
        assert!(
            branches
                .iter()
                .any(|branch| branch.current && branch.name == "main")
        );
        assert_eq!(
            branches
                .iter()
                .find(|branch| branch.name == "feature/topic")
                .unwrap()
                .worktree_path
                .as_ref(),
            Some(&path)
        );
        let discovered = backend.discover(std::slice::from_ref(&root)).unwrap();
        let source = discovered
            .iter()
            .find(|repo| repo.worktree_root == root)
            .unwrap();
        let snapshot = backend.status(source, RepositoryId(1), 1).unwrap();
        assert_eq!(snapshot.worktrees.len(), 2);
        // External tools can override Git's single-checkout guard. Listing
        // must preserve both directories even though worktreepath is singular.
        let duplicate = root.join("topic review worktree");
        backend
            .runner()
            .run(
                GitCommand::new().cwd(&root).args([
                    std::ffi::OsString::from("worktree"),
                    "add".into(),
                    "--force".into(),
                    "--".into(),
                    crate::features::git::types::comparable_path(&duplicate)
                        .as_os_str()
                        .to_owned(),
                    "feature/topic".into(),
                ]),
            )
            .unwrap();
        assert_eq!(
            backend
                .worktrees(&root)
                .unwrap()
                .iter()
                .filter(|worktree| worktree.branch.as_deref() == Some("feature/topic"))
                .count(),
            2
        );
        let duplicate = std::fs::canonicalize(duplicate).unwrap();
        assert!(remove_worktree_with(&backend, &root, &duplicate, false).unwrap());
        assert!(remove_worktree_with(&backend, &root, &root, true).is_err());
        assert!(remove_worktree_with(&backend, &path, &root, true).is_err());
        assert!(remove_worktree_with(&backend, &path, &path, true).is_err());

        backend
            .runner()
            .run(
                GitCommand::new().cwd(&root).args([
                    std::ffi::OsString::from("worktree"),
                    "lock".into(),
                    "--reason".into(),
                    "in use".into(),
                    crate::features::git::types::comparable_path(&path)
                        .as_os_str()
                        .to_owned(),
                ]),
            )
            .unwrap();
        assert_eq!(
            backend.worktrees(&root).unwrap()[1].locked.as_deref(),
            Some("in use")
        );
        assert!(remove_worktree_with(&backend, &root, &path, true).is_err());
        backend
            .runner()
            .run(
                GitCommand::new().cwd(&root).args([
                    std::ffi::OsString::from("worktree"),
                    "unlock".into(),
                    crate::features::git::types::comparable_path(&path)
                        .as_os_str()
                        .to_owned(),
                ]),
            )
            .unwrap();

        std::fs::write(path.join("file.txt"), "modified\n").unwrap();
        std::fs::write(path.join("untracked.txt"), "untracked\n").unwrap();
        assert!(!remove_worktree_with(&backend, &root, &path, false).unwrap());
        assert!(path.join("untracked.txt").is_file());
        assert!(remove_worktree_with(&backend, &root, &path, true).unwrap());
        assert!(!path.exists());
        let branches = backend.branches(&root).unwrap();
        assert!(
            branches
                .iter()
                .any(|branch| branch.name == "feature/topic" && branch.worktree_path.is_none())
        );
        // The kept branch can be checked out in another worktree and removed cleanly.
        create_worktree_with(
            &backend,
            &root,
            &path,
            &Checkout::Switch("feature/topic".into()),
        )
        .unwrap();
        assert!(remove_worktree_with(&backend, &root, &path, false).unwrap());

        backend
            .runner()
            .run(
                GitCommand::new()
                    .cwd(&root)
                    .args(["config", "remote.origin.url", "."]),
            )
            .unwrap();
        backend
            .runner()
            .run(GitCommand::new().cwd(&root).args([
                "config",
                "remote.origin.fetch",
                "+refs/heads/*:refs/remotes/origin/*",
            ]))
            .unwrap();
        backend
            .runner()
            .run(GitCommand::new().cwd(&root).args([
                "update-ref",
                "refs/remotes/origin/release/1.0",
                "HEAD",
            ]))
            .unwrap();
        let remote_path = root.join("remote worktree");
        create_worktree_with(
            &backend,
            &root,
            &remote_path,
            &Checkout::Track("origin/release/1.0".into()),
        )
        .unwrap();
        let tracking = backend
            .branches(&root)
            .unwrap()
            .into_iter()
            .find(|branch| branch.name == "release/1.0")
            .unwrap();
        assert_eq!(tracking.upstream.as_deref(), Some("origin/release/1.0"));
        assert!(
            backend
                .branches(&root)
                .unwrap()
                .iter()
                .any(|branch| branch.name == "main" && branch.current)
        );
        let remote_path = std::fs::canonicalize(remote_path).unwrap();
        assert!(remove_worktree_with(&backend, &root, &remote_path, false).unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }

    fn repository(head: HeadState, upstream: bool, ahead: u32, behind: u32) -> RepositorySnapshot {
        RepositorySnapshot {
            id: RepositoryId(1),
            worktree_root: PathBuf::from("repo"),
            git_dir: PathBuf::from("repo/.git"),
            common_dir: PathBuf::from("repo/.git"),
            head,
            upstream: upstream.then(|| UpstreamState {
                name: "origin/main".into(),
            }),
            ahead,
            behind,
            files: Arc::new(BTreeMap::new()),
            ignored: Arc::new(BTreeSet::new()),
            repository_state: RepositoryState::Normal,
            operation_step: None,
            head_oid: None,
            stashes: Default::default(),
            tags: Default::default(),
            worktrees: Default::default(),
            features: RepositoryFeatures::default(),
            generation: 0,
        }
    }

    #[test]
    fn tag_hover_rejects_stale_rows_repositories_and_closed_sections() {
        use crate::ui::git::panel::{GitHover, TAGS};
        let mut repository = repository(HeadState::Branch("main".into()), false, 0, 0);
        let root = repository.worktree_root.clone();
        let tag = crate::features::git::TagInfo {
            name: "v1.0".into(),
            commit: Some("abc".into()),
        };
        repository.tags = Arc::new(vec![tag.clone()]);
        let mut store = GitStoreSnapshot {
            active_repository: Some(repository.id),
            repositories: Arc::new([(repository.id, repository)].into_iter().collect()),
            ..Default::default()
        };
        let mut app = AppState::new();
        app.git.ui.hover = Some(GitHover::Tag(tag.name.clone()));
        app.git.ui.collapsed_sections.remove(&TAGS);
        assert!(tag_hover_current(&app, &store, &root, &tag, 0));
        app.git.ui.clear_hover_cards();
        assert!(!tag_hover_current(&app, &store, &root, &tag, 0));
        assert!(tag_hover_current(&app, &store, &root, &tag, 1));
        app.git.ui.collapsed_sections.insert(TAGS);
        assert!(!tag_hover_current(&app, &store, &root, &tag, 1));
        app.git.ui.collapsed_sections.remove(&TAGS);
        assert!(!tag_hover_current(
            &app,
            &store,
            Path::new("other"),
            &tag,
            1
        ));
        store.active_repository = None;
        store.repositories = Arc::default();
        assert!(!tag_hover_current(&app, &store, &root, &tag, 1));
    }

    #[test]
    fn sync_plan_follows_the_upstream_state() {
        let branch = || HeadState::Branch("main".into());
        assert_eq!(
            sync_plan(&repository(branch(), false, 0, 0)),
            Some(SyncPlan::Publish)
        );
        assert_eq!(
            sync_plan(&repository(branch(), true, 0, 0)),
            Some(SyncPlan::Fetch)
        );
        assert_eq!(
            sync_plan(&repository(branch(), true, 2, 0)),
            Some(SyncPlan::Push)
        );
        assert_eq!(
            sync_plan(&repository(branch(), true, 0, 3)),
            Some(SyncPlan::Pull { then_push: false })
        );
        assert_eq!(
            sync_plan(&repository(branch(), true, 2, 3)),
            Some(SyncPlan::Pull { then_push: true })
        );
        assert_eq!(
            sync_plan(&repository(HeadState::Detached("abc".into()), false, 0, 0)),
            None
        );
        assert_eq!(
            sync_plan(&repository(HeadState::Unborn("main".into()), false, 0, 0)),
            None
        );
    }

    #[test]
    fn publishes_to_origin_or_the_first_remote() {
        let remote = |name: &str| RemoteInfo {
            name: name.into(),
            fetch_url: None,
            push_url: None,
        };
        assert_eq!(publish_remote(&[]), None);
        assert_eq!(
            publish_remote(&[remote("fork"), remote("origin")]),
            Some("origin")
        );
        assert_eq!(
            publish_remote(&[remote("fork"), remote("upstream")]),
            Some("fork")
        );
    }

    #[test]
    fn lists_the_files_git_says_would_be_overwritten() {
        let error = GitError::from_stderr(
            b"error: Your local changes to the following files would be overwritten by checkout:\n\
\tsrc/main.rs\n\tREADME.md\nPlease commit your changes or stash them before you switch branches.\nAborting\n",
            Some(1),
        );
        assert_eq!(error.kind, GitErrorKind::DirtyWorktree);
        assert_eq!(overwritten_files(&error), ["src/main.rs", "README.md"]);
    }

    #[test]
    fn real_repository_stages_unstages_and_discards_single_changes() {
        use crate::features::git::DiffTarget;
        let Some((root, backend)) = crate::infrastructure::git::backend::tests::repository() else {
            return;
        };
        let path = PathBuf::from("lines.txt");
        let file = root.join(&path);
        // CRLF content must survive patches cut from it, so keep it CRLF in
        // the index whatever the machine's `core.autocrlf`.
        backend
            .runner()
            .run(
                crate::infrastructure::git::command::GitCommand::new()
                    .cwd(&root)
                    .args(["config", "core.autocrlf", "false"]),
            )
            .unwrap();
        std::fs::write(&file, "a\r\nb\r\nc\r\nd\r\ne\r\nf\r\ng\r\nh\r\n").unwrap();
        backend.stage(&root, std::slice::from_ref(&path)).unwrap();
        backend.commit(&root, "lines", false, false).unwrap();
        // Change b, insert after d, delete g: three separate blocks.
        std::fs::write(&file, "a\r\nB\r\nc\r\nd\r\nd2\r\ne\r\nf\r\nh\r\n").unwrap();
        let document = |target| {
            DiffDocument::from_unified(
                root.clone(),
                path.clone(),
                target,
                backend
                    .parsed_full_diff(&root, target, std::slice::from_ref(&path))
                    .unwrap(),
            )
        };
        let staged = |backend: &CliGitBackend| {
            backend
                .diff(&root, DiffTarget::HeadToIndex, std::slice::from_ref(&path))
                .unwrap()
        };
        let worktree = document(DiffTarget::IndexToWorktree);
        assert_eq!(worktree.blocks.len(), 3);

        // Stage the insertion only.
        apply_block_with(
            &backend,
            &root,
            &path,
            DiffTarget::IndexToWorktree,
            worktree.blocks[1],
            BlockAction::Stage,
        )
        .unwrap();
        let cached = staged(&backend);
        assert!(cached.contains("\n+d2\r\n"), "{cached:?}");
        assert!(
            !cached.contains("\n+B") && !cached.contains("\n-g"),
            "{cached:?}"
        );

        // A block that no longer matches is refused.
        assert_eq!(
            apply_block_with(
                &backend,
                &root,
                &path,
                DiffTarget::IndexToWorktree,
                worktree.blocks[1],
                BlockAction::Stage,
            )
            .unwrap_err()
            .kind,
            GitErrorKind::Conflict
        );

        // Unstage it again from the index's own diff.
        let index = document(DiffTarget::HeadToIndex);
        apply_block_with(
            &backend,
            &root,
            &path,
            DiffTarget::HeadToIndex,
            index.blocks[0],
            BlockAction::Unstage,
        )
        .unwrap();
        assert!(staged(&backend).is_empty());

        // Discard the first change from the worktree; the others stay.
        apply_block_with(
            &backend,
            &root,
            &path,
            DiffTarget::IndexToWorktree,
            worktree.blocks[0],
            BlockAction::Discard,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "a\r\nb\r\nc\r\nd\r\nd2\r\ne\r\nf\r\nh\r\n"
        );

        // History lists the commit's files and its diff.
        let head = backend.history(&root, None, None, 0, 1).unwrap()[0]
            .oid
            .clone();
        assert_eq!(
            backend.commit_files(&root, &head).unwrap(),
            [(crate::features::git::ChangeKind::Added, path.clone())]
        );
        let shown = backend.commit_full_diff(&root, &head, &path).unwrap();
        assert_eq!(shown.files[0].hunks[0].new_lines, 8);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn real_repository_switches_with_stashed_changes() {
        let Some((root, backend)) = crate::infrastructure::git::backend::tests::repository() else {
            return;
        };
        let file = root.join("file.txt");
        let commit = |message: &str| {
            backend.stage(&root, &[PathBuf::from("file.txt")]).unwrap();
            backend.commit(&root, message, false, false).unwrap();
        };
        let current = || {
            backend
                .branches(&root)
                .unwrap()
                .into_iter()
                .find(|branch| branch.current)
                .map(|branch| branch.name)
                .unwrap()
        };
        std::fs::write(&file, "base\n").unwrap();
        commit("base");
        backend.create_branch(&root, "other", None).unwrap();
        std::fs::write(&file, "other\n").unwrap();
        commit("other");
        backend.switch(&root, "main").unwrap();
        let other = Checkout::Switch("other".into());
        let main = Checkout::Switch("main".into());

        // Nothing to stash: the stash modes just switch.
        assert_eq!(
            checkout_with(&backend, &root, &other, StashMode::Carry).unwrap(),
            CheckoutOutcome::Switched
        );
        assert!(backend.stash_list(&root).unwrap().is_empty());
        backend.switch(&root, "main").unwrap();

        // A change the switch would overwrite blocks it.
        std::fs::write(&file, "local\n").unwrap();
        assert_eq!(
            checkout_with(&backend, &root, &other, StashMode::Refuse).unwrap(),
            CheckoutOutcome::Blocked(vec!["file.txt".into()])
        );
        assert_eq!(current(), "main");

        // Keep: switched, the change waits in the stash.
        assert_eq!(
            checkout_with(&backend, &root, &other, StashMode::Keep).unwrap(),
            CheckoutOutcome::Stashed
        );
        assert_eq!(current(), "other");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "other\n");
        assert_eq!(backend.stash_list(&root).unwrap().len(), 1);

        // Carry: back on main the stashed change reapplies cleanly.
        backend.stash_drop(&root, 0).unwrap();
        backend.switch(&root, "main").unwrap();
        std::fs::write(root.join("new.txt"), "untracked\n").unwrap();
        assert_eq!(
            checkout_with(&backend, &root, &other, StashMode::Carry).unwrap(),
            CheckoutOutcome::Switched
        );
        assert_eq!(current(), "other");
        assert_eq!(
            std::fs::read_to_string(root.join("new.txt")).unwrap(),
            "untracked\n"
        );
        assert!(backend.stash_list(&root).unwrap().is_empty());

        // Carry onto a branch the change conflicts with keeps the stash.
        std::fs::write(&file, "conflicting\n").unwrap();
        assert_eq!(
            checkout_with(&backend, &root, &main, StashMode::Carry).unwrap(),
            CheckoutOutcome::CarryConflicted
        );
        assert_eq!(current(), "main");
        assert_eq!(backend.stash_list(&root).unwrap().len(), 1);

        // A failed switch restores what it stashed.
        backend
            .runner()
            .run(
                crate::infrastructure::git::command::GitCommand::new()
                    .cwd(&root)
                    .args(["reset", "--hard"]),
            )
            .unwrap();
        backend.stash_drop(&root, 0).unwrap();
        std::fs::write(&file, "kept\n").unwrap();
        let missing = Checkout::Switch("missing".into());
        assert!(checkout_with(&backend, &root, &missing, StashMode::Keep).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "kept\n");
        assert!(backend.stash_list(&root).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }
}
