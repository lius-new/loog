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
                    services.saved(saved.path, saved.text);
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
    if state.get().workspace.shared_save_snapshot(id).is_none() {
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
                .map(|d| (id, d.stamp(id), d.meta.path.clone()))
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
                        .is_some_and(|d| d.stamp(*id) != *stamp || d.meta.path != *path)
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
