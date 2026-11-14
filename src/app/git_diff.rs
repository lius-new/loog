//! Show the requested file before preparing the rest of a Changes overview.
use super::context::AppContext;
use crate::features::git::diff_document::{DiffDocument, DiffSection};
use crate::features::git::{
    ChangeEntry, ChangeGroup, DiffRequest, DiffTarget, GitResult, GitStoreSnapshot,
};
use crate::infrastructure::git::GitService;
use std::{path::PathBuf, sync::Arc, thread};

#[derive(Clone)]
struct Load {
    request: DiffRequest,
    revision: Option<String>,
    sequence: u64,
}

pub(super) fn open_changes(
    state: &AppContext,
    selected: DiffRequest,
    untracked: bool,
    snapshot: GitStoreSnapshot,
) {
    let entries = snapshot
        .repository_for_path(&selected.repository_root)
        .map(|repo| repo.change_entries());
    let request = DiffRequest {
        repository_root: selected.repository_root.clone(),
        path: PathBuf::new(),
        target: DiffTarget::Changes,
    };
    open(state, request, None, selected, untracked, entries, snapshot);
}

pub(super) fn open_commit_file(state: &AppContext, root: PathBuf, oid: String, path: PathBuf) {
    let entries = state
        .get()
        .git
        .model
        .history_commit()
        .filter(|commit| commit.oid == oid)
        .and_then(|commit| commit.files.as_ref())
        .and_then(|files| files.as_ref().ok())
        .map(|files| {
            files
                .iter()
                .map(|(_, path)| ChangeEntry {
                    request: DiffRequest {
                        repository_root: root.clone(),
                        path: path.clone(),
                        target: DiffTarget::Commit,
                    },
                    group: ChangeGroup::Commit,
                })
                .collect()
        });
    let request = DiffRequest {
        repository_root: root.clone(),
        path: PathBuf::new(),
        target: DiffTarget::Commit,
    };
    let selected = DiffRequest {
        repository_root: root,
        path,
        target: DiffTarget::Commit,
    };
    open(
        state,
        request,
        Some(oid),
        selected,
        false,
        entries,
        GitStoreSnapshot::default(),
    );
}

fn open(
    state: &AppContext,
    request: DiffRequest,
    revision: Option<String>,
    selected: DiffRequest,
    untracked: bool,
    entries: Option<Vec<ChangeEntry>>,
    snapshot: GitStoreSnapshot,
) {
    let mut revealed = false;
    let mut pending = None;
    state.update(|app| {
        let id = match &revision {
            Some(oid) => app
                .workspace
                .commit_changes_id(&request.repository_root, oid),
            None => {
                app.workspace
                    .diff_id_for(&request.repository_root, &request.path, request.target)
            }
        };
        if let Some(id) = id
            && let Some(pane) = app.workspace.pane_of(id)
        {
            app.workspace.activate(pane, id);
            app.workspace
                .focus_diff_section(&selected, app.git.ui.split_diff);
            // A newly listed file may not exist in an older overview yet.
            // Start a new priority read with the current listing in that case.
            if app.workspace.diff(pane).is_some_and(|doc| {
                doc.sections.as_ref().is_some_and(|sections| {
                    !sections.iter().any(|section| section.request() == selected)
                })
            }) {
                return;
            }
            if let Some(doc) = app.workspace.diff(pane)
                && let Some(sequence) = doc.loading
                && doc.sections.as_ref().is_some_and(|sections| {
                    sections
                        .iter()
                        .find(|section| section.request() == selected)
                        .is_none_or(|section| section.loading)
                })
            {
                pending = Some(Load {
                    request: request.clone(),
                    revision: revision.clone(),
                    sequence,
                });
            }
            app.git.model.cancel_diff_open();
            revealed = true;
        }
    });
    if revealed {
        if let Some(load) = pending {
            load_section(state, load, selected, untracked);
        }
        return;
    }
    let Some(application) = state.services().and_then(|runtime| runtime.application()) else {
        return;
    };
    let app = state.get();
    let roots = super::git_poll::scan_roots(&app);
    let active = (app.workspace.active_pane(), app.workspace.active());
    let mut sequence = 0;
    state.update(|app| sequence = app.git.model.begin_diff_open(request.clone()));
    let load = Load {
        request,
        revision,
        sequence,
    };
    let worker_state = state.clone();
    let spawned = thread::Builder::new()
        .name("loog-git-priority-diff".into())
        .spawn(move || {
            let result = crate::infrastructure::git::service().map(|service| {
                let (ready, error) =
                    match read_section(&service, &selected, load.revision.as_deref(), untracked) {
                        Ok(doc) => (doc, None),
                        Err(error) => {
                            let mut doc = DiffDocument::from_unified(
                                selected.repository_root.clone(),
                                selected.path.clone(),
                                selected.target,
                                Default::default(),
                            );
                            doc.revision = load.revision.clone();
                            (Arc::new(doc), Some(error.user_message()))
                        }
                    };
                let mut document = preview(&load, entries, ready, untracked);
                if let Some(error) = error {
                    // As in a complete overview, a single failed file must not
                    // prevent the other sections from opening.
                    if let Some(section) = Arc::make_mut(&mut document)
                        .sections
                        .as_mut()
                        .unwrap()
                        .iter_mut()
                        .find(|section| section.request() == selected)
                    {
                        section.error = Some(error);
                    }
                }
                (service, document)
            });
            let next_application = application.clone();
            application.post(move || {
                let mut result = Some(result);
                let mut completion = None;
                worker_state.update(|app| {
                    if !app.git.model.finish_diff_open(sequence)
                        || super::git_poll::scan_roots(app) != roots
                        || (app.workspace.active_pane(), app.workspace.active()) != active
                    {
                        return;
                    }
                    match result.take().unwrap() {
                        Ok((service, document)) => {
                            let loading = document.loading.is_some();
                            if load.revision.is_some() {
                                app.workspace.open_prepared_commit_changes(document);
                            } else {
                                app.workspace.open_prepared_diff(document);
                            }
                            app.workspace
                                .focus_diff_section(&selected, app.git.ui.split_diff);
                            if loading {
                                completion = Some(service);
                            }
                        }
                        Err(error) => app.show_error(error.user_message()),
                    }
                });
                crate::infrastructure::ui_dispatch::retire(result);
                if let Some(service) = completion {
                    complete(worker_state, next_application, service, load, snapshot);
                }
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

fn read_section(
    service: &GitService,
    request: &DiffRequest,
    revision: Option<&str>,
    untracked: bool,
) -> GitResult<Arc<DiffDocument>> {
    let paths = std::slice::from_ref(&request.path);
    let diff = if let Some(revision) = revision {
        service
            .backend()
            .commit_file_diff(&request.repository_root, revision, &request.path)?
    } else {
        service
            .backend()
            .parsed_diff(&request.repository_root, request.target, paths)?
    };
    let mut doc = if untracked && diff.files.is_empty() {
        let content = crate::infrastructure::git::service::read_untracked(
            &request.repository_root.join(&request.path),
        )?;
        DiffDocument::from_content(request.clone(), content)
    } else {
        DiffDocument::from_unified(
            request.repository_root.clone(),
            request.path.clone(),
            request.target,
            diff,
        )
    };
    doc.revision = revision.map(str::to_owned);
    Ok(Arc::new(doc))
}

fn preview(
    load: &Load,
    entries: Option<Vec<ChangeEntry>>,
    ready: Arc<DiffDocument>,
    untracked: bool,
) -> Arc<DiffDocument> {
    let known = entries.is_some();
    let ready_request = ready.request();
    let mut found = false;
    let mut sections = Vec::new();
    for entry in entries.unwrap_or_default() {
        let loading = entry.request != ready_request;
        let document = if loading {
            let mut doc = DiffDocument::from_unified(
                entry.request.repository_root,
                entry.request.path,
                entry.request.target,
                Default::default(),
            );
            doc.revision = load.revision.clone();
            Arc::new(doc)
        } else {
            found = true;
            ready.clone()
        };
        sections.push(DiffSection {
            document,
            group: entry.group,
            error: None,
            loading,
        });
    }
    if !found {
        let group = if load.revision.is_some() {
            ChangeGroup::Commit
        } else if untracked {
            ChangeGroup::Untracked
        } else if ready.target == DiffTarget::HeadToIndex {
            ChangeGroup::Staged
        } else if ready.target == DiffTarget::HeadToWorktree {
            ChangeGroup::Conflicts
        } else {
            ChangeGroup::Unstaged
        };
        sections.push(DiffSection {
            document: ready,
            group,
            error: None,
            loading: false,
        });
    }
    let pending = !known || !found || sections.iter().any(|section| section.loading);
    let mut doc = DiffDocument::from_unified(
        load.request.repository_root.clone(),
        PathBuf::new(),
        load.request.target,
        Default::default(),
    );
    doc.revision = load.revision.clone();
    doc.sections = Some(sections);
    doc.loading = pending.then_some(load.sequence);
    Arc::new(doc)
}

#[cfg(test)]
#[path = "git_diff_tests.rs"]
mod tests;

fn complete(
    state: AppContext,
    application: lgui::ApplicationHandle,
    service: Arc<GitService>,
    load: Load,
    snapshot: GitStoreSnapshot,
) {
    let failure_state = state.clone();
    let failure_load = load.clone();
    let spawned = thread::Builder::new()
        .name("loog-git-complete-diff".into())
        .spawn(move || {
            let result = if let Some(oid) = &load.revision {
                service
                    .commit_changes(&load.request.repository_root, oid)
                    .map(|content| {
                        Arc::new(DiffDocument::commit_changes(
                            load.request.repository_root.clone(),
                            oid.clone(),
                            content,
                        ))
                    })
            } else {
                service
                    .refresh_diff(&load.request, &snapshot)
                    .map(|content| {
                        Arc::new(DiffDocument::from_content(load.request.clone(), content))
                    })
            }
            .map_err(|error| error.user_message());
            application.post(move || publish(&state, &load, result));
        })
        .is_ok();
    if !spawned {
        publish(
            &failure_state,
            &failure_load,
            Err("Could not start Git diff worker.".into()),
        );
    }
}

fn publish(state: &AppContext, load: &Load, result: Result<Arc<DiffDocument>, String>) {
    let mut retired = Vec::new();
    let mut result = Some(result);
    state.try_update(|app| {
        let Some(current) =
            app.workspace
                .loading_diff(&load.request, load.revision.as_deref(), load.sequence)
        else {
            return false;
        };
        let next = match result.take().unwrap() {
            Ok(doc) => doc,
            Err(error) => {
                let mut doc = (*current).clone();
                doc.loading = None;
                for section in doc
                    .sections
                    .as_mut()
                    .unwrap()
                    .iter_mut()
                    .filter(|s| s.loading)
                {
                    section.loading = false;
                    section.error = Some(error.clone());
                }
                app.show_error(error);
                Arc::new(doc)
            }
        };
        retired = app
            .workspace
            .update_loading_diff(next, load.sequence, app.git.ui.split_diff);
        !retired.is_empty()
    });
    crate::infrastructure::ui_dispatch::retire((retired, result));
}

fn load_section(state: &AppContext, load: Load, request: DiffRequest, untracked: bool) {
    let Some(application) = state.services().and_then(|runtime| runtime.application()) else {
        return;
    };
    let state = state.clone();
    let _ = thread::Builder::new()
        .name("loog-git-next-diff".into())
        .spawn(move || {
            let result = crate::infrastructure::git::service()
                .and_then(|service| {
                    read_section(&service, &request, load.revision.as_deref(), untracked)
                })
                .map_err(|error| error.user_message());
            application.post(move || {
                let mut result = Some(result);
                let mut retired = Vec::new();
                state.try_update(|app| {
                    let Some(current) = app.workspace.loading_diff(
                        &load.request,
                        load.revision.as_deref(),
                        load.sequence,
                    ) else {
                        return false;
                    };
                    let mut next = (*current).clone();
                    let sections = next.sections.as_mut().unwrap();
                    let Some(section) = sections.iter_mut().find(|s| s.request() == request) else {
                        return false;
                    };
                    if !section.loading {
                        return false;
                    }
                    match result.take().unwrap() {
                        Ok(doc) => section.document = doc,
                        Err(error) => section.error = Some(error),
                    }
                    section.loading = false;
                    retired = app.workspace.update_loading_diff(
                        Arc::new(next),
                        load.sequence,
                        app.git.ui.split_diff,
                    );
                    !retired.is_empty()
                });
                crate::infrastructure::ui_dispatch::retire((retired, result));
            });
        });
}
