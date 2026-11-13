//! Background file saves. Only immutable snapshots leave the UI thread.
use super::{context::AppContext, state::CloseContinuation};
use crate::domain::document::{DiskState, FileId, SaveSnapshot};
use crate::infrastructure::document_filesystem;
use std::sync::Arc;

#[derive(Clone, Default)]
pub(crate) struct State {
    pub pending: usize,
    failures: u64,
    pub closing: bool,
    provided: std::collections::HashSet<FileId>,
}
impl State {
    pub(crate) fn resource_pending(&self, id: FileId) -> bool {
        self.provided.contains(&id)
    }
}

type Results = Vec<(SaveSnapshot, Result<DiskState, String>)>;

/// All writes use one FIFO worker, so an older save cannot overwrite a newer one.
fn submit(
    state: &AppContext,
    files: Vec<SaveSnapshot>,
    done: impl FnOnce(&AppContext, bool) + Send + 'static,
) -> bool {
    let Some(runtime) = state.services() else {
        state.update(|app| app.show_error("File saver is not available"));
        return false;
    };
    let Some(application) = runtime.application() else {
        state.update(|app| app.show_error("File saver is not available"));
        return false;
    };
    let ready = state.clone();
    let task = Box::new(move || {
        let mut results: Results = Vec::new();
        for mut saved in files {
            let result = (|| {
                let bytes = crate::features::encoding::encode(&saved.text, saved.format)?;
                document_filesystem::write(&saved.path, &bytes).map_err(|e| e.to_string())?;
                // Hash exactly the bytes written, without reading the whole file again.
                Ok(document_filesystem::state(
                    &saved.path,
                    &bytes,
                    saved.format,
                ))
            })();
            let failed = result.is_err();
            if !failed {
                // Keep the saved baseline independent of the live buffer. Copy on
                // this worker so the next UI edit need not clone it after saving.
                saved.text = std::sync::Arc::new(saved.text.as_ref().clone());
            }
            results.push((saved, result));
            if failed {
                break;
            }
        }
        application.post(move || {
            if ready.services().is_none() {
                crate::infrastructure::ui_dispatch::retire(results);
                return;
            }
            let mut success = true;
            let mut published = Vec::new();
            ready.update(|app| {
                app.editor.saving.pending -= 1;
                for (saved, result) in &results {
                    match result {
                        Ok(disk) => {
                            // Closed/reopened or renamed documents cannot receive an old baseline.
                            if app.workspace.mark_snapshot_saved(saved, disk.clone()) {
                                published.push(saved.clone());
                                if crate::features::settings::project::is_settings_file(&saved.path)
                                {
                                    app.refresh_project_settings();
                                }
                            }
                        }
                        Err(error) => {
                            success = false;
                            app.editor.saving.failures += 1;
                            app.show_error(format!(
                                "Could not save {}: {error}",
                                saved.path.display()
                            ));
                        }
                    }
                }
            });
            if let Some(services) = ready.services().and_then(|r| r.language_services()) {
                for saved in published {
                    services.saved_document(saved);
                }
            }
            done(&ready, success);
            crate::infrastructure::ui_dispatch::retire(results);
        });
    });
    match runtime.save(task) {
        Ok(()) => {
            state.update(|app| app.editor.saving.pending += 1);
            true
        }
        Err(error) => {
            state.update(|app| app.show_error(error));
            false
        }
    }
}

pub(crate) fn document(
    state: &AppContext,
    id: FileId,
    done: impl FnOnce(&AppContext, bool) + Send + 'static,
) -> bool {
    save_document(state, id, false, done)
}

/// Like [`document`], but never runs save participants.
pub(crate) fn document_without_participants(
    state: &AppContext,
    id: FileId,
    done: impl FnOnce(&AppContext, bool) + Send + 'static,
) -> bool {
    save_document(state, id, true, done)
}

/// The extensions that change a file before it is saved, if any apply.
fn participants(
    state: &AppContext,
    files: &[FileId],
) -> Option<Arc<super::extensions::host::ExtensionHost>> {
    let host = state.services()?.extensions()?;
    host.participates(&state.get(), files).then_some(host)
}

fn save_document(
    state: &AppContext,
    id: FileId,
    skip: bool,
    done: impl FnOnce(&AppContext, bool) + Send + 'static,
) -> bool {
    let provided = state
        .get()
        .workspace
        .meta(id)
        .is_some_and(|meta| meta.local_path().is_none());
    if !provided && state.get().workspace.shared_save_snapshot(id).is_none() {
        return false;
    }
    if !skip && let Some(host) = participants(state, &[id]) {
        // Counted as a save in progress, so closing the window waits for the
        // participants too (design §6.5).
        state.update(|app| app.editor.saving.pending += 1);
        host.prepare_save(
            state,
            vec![id],
            Box::new(move |state, ok| {
                state.update(|app| app.editor.saving.pending -= 1);
                // The snapshot is taken now, so it holds what the participants did.
                if provided {
                    if ok {
                        resource_document(state, id, done);
                    } else {
                        done(state, false);
                    }
                    return;
                }
                match state.get().workspace.shared_save_snapshot(id) {
                    Some(saved) if ok => {
                        submit(state, vec![saved], done);
                    }
                    _ => done(state, false),
                }
            }),
        );
        return true;
    }
    if provided {
        return resource_document(state, id, done);
    }
    let Some(saved) = state.get().workspace.shared_save_snapshot(id) else {
        return false;
    };
    submit(state, vec![saved], done)
}

/// The batch is also a barrier behind earlier saves, including when no files need writing.
pub(crate) fn close(
    state: &AppContext,
    save: bool,
    done: impl FnOnce(&AppContext, CloseContinuation) + Send + 'static,
) -> bool {
    let app = state.get();
    if app.editor.saving.closing {
        return false;
    }
    if !app.editor.saving.provided.is_empty() {
        drop(app);
        state.update(|app| {
            app.show_error("A resource save is still in progress. Close again when it finishes.")
        });
        return false;
    }
    let Some(request) = app.editor.close_request.clone() else {
        return false;
    };
    let failures = app.editor.saving.failures;
    let reviewed = app
        .workspace
        .released_by(&request.targets)
        .into_iter()
        .filter_map(|id| {
            app.workspace
                .documents
                .get(&id)
                .map(|d| (id, d.stamp(id), d.meta.resource.clone()))
        })
        .collect::<Vec<_>>();
    let ids: Vec<FileId> = if save {
        app.workspace
            .released_by(&request.targets)
            .into_iter()
            .filter(|id| app.workspace.is_dirty(*id))
            .collect()
    } else {
        Vec::new()
    };
    let files = if save {
        app.workspace
            .released_by(&request.targets)
            .into_iter()
            .filter(|id| app.workspace.is_dirty(*id))
            .filter_map(|id| app.workspace.shared_save_snapshot(id))
            .collect()
    } else {
        Vec::new()
    };
    drop(app);
    let provided: Vec<_> = ids
        .iter()
        .copied()
        .filter(|id| state.get().workspace.resource_save_snapshot(*id).is_some())
        .collect();
    let finish = move |state: &AppContext, success: bool| {
        let mut continuation = None;
        state.update(|app| {
            app.editor.saving.closing = false;
            if app.editor.close_request.as_ref() != Some(&request)
                || !success
                || save && app.editor.saving.failures != failures
            {
                return;
            }
            if !save {
                let changed = reviewed.iter().any(|(id, stamp, path)| {
                    app.workspace
                        .documents
                        .get(id)
                        .is_some_and(|d| d.stamp(*id) != *stamp || d.meta.resource != *path)
                }) || request.continuation == CloseContinuation::ExitApplication
                    && app
                        .workspace
                        .dirty_file_ids()
                        .iter()
                        .any(|id| !reviewed.iter().any(|(expected, _, _)| expected == id));
                if changed {
                    app.show_error(
                        "Files changed while waiting for saves. Review the changes before closing.",
                    );
                    return;
                }
                continuation = super::workspace_commands::discard_close_request_in(app);
                return;
            }
            let conflicted = |id| {
                app.workspace
                    .documents
                    .get(&id)
                    .is_some_and(|d| d.disk_conflict)
            };
            let still_dirty = if request.continuation == CloseContinuation::ExitApplication {
                !app.workspace.dirty_file_ids().is_empty()
                    || app.workspace.documents.keys().any(|id| conflicted(*id))
            } else {
                app.workspace
                    .released_by(&request.targets)
                    .into_iter()
                    .any(|id| app.workspace.is_dirty(id) || conflicted(id))
            };
            if still_dirty {
                app.show_error(
                    "Files changed while saving. Review the remaining changes before closing.",
                );
                return;
            }
            if request.continuation == CloseContinuation::CloseTabs {
                for (pane, view) in &request.targets {
                    app.workspace.close_item(*pane, *view);
                }
            }
            app.editor.close_request = None;
            continuation = Some(request.continuation);
        });
        if let Some(next) = continuation {
            done(state, next);
        }
    };
    let finish = move |state: &AppContext, success: bool| {
        if success && !provided.is_empty() {
            resource_batch(state, provided, Box::new(finish));
        } else {
            finish(state, success);
        }
    };
    // The extensions that change files before saving run first. This stage is
    // part of the save for closing purposes: the close barrier waits for it.
    if save && let Some(host) = participants(state, &ids) {
        state.update(|app| {
            app.editor.saving.closing = true;
            app.editor.saving.pending += 1;
        });
        host.prepare_save(
            state,
            ids.clone(),
            Box::new(move |state, ok| {
                state.update(|app| app.editor.saving.pending -= 1);
                if !ok {
                    // Nothing was written; the close request stays open.
                    state.update(|app| app.editor.saving.closing = false);
                    return;
                }
                let files = {
                    let app = state.get();
                    ids.iter()
                        .filter(|id| app.workspace.is_dirty(**id))
                        .filter_map(|id| app.workspace.shared_save_snapshot(*id))
                        .collect()
                };
                let queued = submit(state, files, finish);
                state.update(|app| app.editor.saving.closing = queued);
            }),
        );
        return true;
    }
    let queued = submit(state, files, finish);
    if queued {
        state.update(|app| app.editor.saving.closing = true);
    }
    queued
}

type ResourceDone = Box<dyn FnOnce(&AppContext, bool) + Send>;

fn resource_batch(state: &AppContext, mut ids: Vec<FileId>, done: ResourceDone) {
    let Some(id) = ids.pop() else {
        done(state, true);
        return;
    };
    resource_document(state, id, move |state, ok| {
        if ok {
            resource_batch(state, ids, done);
        } else {
            done(state, false);
        }
    });
}

fn resource_document(
    state: &AppContext,
    id: FileId,
    done: impl FnOnce(&AppContext, bool) + Send + 'static,
) -> bool {
    let prepared = (|| {
        let app = state.get();
        if app.editor.saving.provided.contains(&id) {
            return Err("This resource is already being saved".to_owned());
        }
        let saved = app
            .workspace
            .resource_save_snapshot(id)
            .ok_or("Resource is offline. Connect its provider and open the file again.")?;
        if app.explorer.resources.busy.contains(&saved.scope) {
            return Err("Wait for the current resource operation before saving".into());
        }
        if app
            .workspace
            .documents
            .get(&id)
            .is_some_and(|d| d.disk_conflict)
        {
            return Err("This resource has a conflict or an unconfirmed save. Reconcile it before saving again.".into());
        }
        if !app
            .explorer
            .resources
            .root(saved.scope)
            .is_some_and(|root| {
                root.descriptor
                    .capabilities
                    .contains(&loog_workspace_api::Capability::FilesWrite)
            })
        {
            return Err("Resource is disconnected or read-only".into());
        }
        let runtime = state.services().ok_or("File saver is not available")?;
        let host = runtime
            .extensions()
            .ok_or("Resource provider is not available")?;
        let application = runtime.application().ok_or("File saver is not available")?;
        Ok((saved, runtime, host, application))
    })();
    let (saved, runtime, host, application) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            state.update(|app| app.show_error(error));
            done(state, false);
            return false;
        }
    };
    let ready = state.clone();
    let completion: Arc<std::sync::Mutex<Option<ResourceDone>>> =
        Arc::new(std::sync::Mutex::new(Some(Box::new(done))));
    let worker_done = completion.clone();
    state.update(|app| {
        if app.workspace.active_document() == Some(id) {
            app.explorer.resources.open_generation += 1;
            if let Some(request) = app.explorer.resources.pending_open.take() {
                host.cancel_resource(request);
            }
        }
        app.editor.saving.pending += 1;
        app.editor.saving.provided.insert(id);
    });
    let queued = runtime.save(Box::new(move || {
        let bytes = crate::features::encoding::encode(&saved.text, saved.format);
        let callback_application = application.clone();
        application.post(move || {
            let completion = worker_done;
            let callback_done = completion.clone();
            let callback_state = ready.clone();
            let callback_saved = saved.clone();
            let callback: super::extensions::host::ResourceContinuation =
                Box::new(move |app, result| {
                    use loog_workspace_api::Answer;
                    app.editor.saving.pending -= 1;
                    app.editor.saving.provided.remove(&id);
                    let success = match result {
                        Ok(Answer::Written { revision }) => {
                            app.workspace.mark_resource_saved(&callback_saved, revision)
                        }
                        Err(error) => {
                            app.editor.saving.failures += 1;
                            if matches!(
                                error.kind,
                                loog_workspace_api::FailureKind::Conflict
                                    | loog_workspace_api::FailureKind::UnknownOutcome
                            ) {
                                if let Some(doc) =
                                    app.workspace.documents.get_mut(&id).filter(|d| {
                                        d.incarnation == callback_saved.stamp.incarnation
                                    })
                                {
                                    doc.disk_conflict = true;
                                }
                            }
                            app.show_error(format!(
                                "Could not save {}: {error}",
                                callback_saved.resource.display()
                            ));
                            false
                        }
                        _ => false,
                    };
                    callback_application.post(move || {
                        if let Some(done) = callback_done.lock().unwrap().take() {
                            done(&callback_state, success);
                        }
                        super::resources::reconnect_active(&callback_state);
                    });
                });
            let operation = bytes.and_then(|bytes| {
                let loog_workspace_api::ResourceRef::Provided { path, .. } = saved.resource else {
                    return Err("Invalid resource identity".into());
                };
                Ok((path, bytes, saved.revision))
            });
            match operation {
                Ok((path, bytes, revision)) => {
                    if let Err(error) = super::resource_write::start(
                        &host,
                        saved.scope,
                        path,
                        bytes,
                        revision,
                        callback,
                    ) {
                        ready.update(|app| {
                            app.editor.saving.pending -= 1;
                            app.editor.saving.provided.remove(&id);
                            app.editor.saving.failures += 1;
                            app.show_error(error.message);
                        });
                        if let Some(done) = completion.lock().unwrap().take() {
                            done(&ready, false);
                        }
                    }
                }
                Err(error) => ready.update(|app| {
                    callback(
                        app,
                        Err(loog_workspace_api::Failure::new(
                            loog_workspace_api::FailureKind::Failed,
                            error,
                        )),
                    )
                }),
            }
        });
    }));
    if let Err(error) = queued {
        state.update(|app| {
            app.editor.saving.pending -= 1;
            app.editor.saving.provided.remove(&id);
            app.show_error(error);
        });
        if let Some(done) = completion.lock().unwrap().take() {
            done(state, false);
        }
        return false;
    }
    true
}
