//! Native change consumers. Providers own subscription and path semantics.
use super::context::AppContext;
use loog_workspace_api::{Answer, Capability, Notification, Operation, ResourceRef, ScopeId};

pub(crate) fn start(state: &AppContext) {
    let Some(host) = state.services().and_then(|runtime| runtime.extensions()) else {
        return;
    };
    state.update(|app| {
        let session = app.explorer.resources.session;
        let roots = app
            .explorer
            .resources
            .roots
            .iter()
            .filter(|root| {
                root.descriptor.capabilities.contains(&Capability::Watch)
                    && root.watch.handle.is_none()
                    && !root.watch.pending
                    && root.watch.error.is_none()
            })
            .map(|root| root.descriptor.clone())
            .collect::<Vec<_>>();
        for root in roots {
            let ResourceRef::Provided { path, .. } = root.resource else {
                continue;
            };
            let scope = root.scope;
            app.explorer
                .resources
                .root_mut(scope)
                .unwrap()
                .watch
                .pending = true;
            let submitted = host.request_resource(
                scope,
                Operation::Watch { path },
                Box::new(move |app, result| {
                    if app.explorer.resources.session != session {
                        return;
                    }
                    let Some(root) = app.explorer.resources.root_mut(scope) else {
                        return;
                    };
                    root.watch.pending = false;
                    match result {
                        Ok(Answer::Started { handle }) => {
                            root.watch.handle = Some(handle);
                            root.watch.sequence = 0;
                        }
                        error => {
                            let message = match error {
                                Err(error) => error.message,
                                _ => "Invalid filesystem subscription".into(),
                            };
                            root.watch.error = Some(message.clone());
                            app.show_error(format!("Filesystem monitoring unavailable: {message}"));
                        }
                    }
                }),
            );
            if let Err(error) = submitted {
                let watch = &mut app.explorer.resources.root_mut(scope).unwrap().watch;
                watch.pending = false;
                watch.error = Some(error.message);
            }
        }
    });
}

pub(crate) fn notify(state: &AppContext, scope: ScopeId, event: Notification) {
    let mut queued = None;
    state.update(|app| {
        let session = app.explorer.resources.session;
        let Some(root) = app.explorer.resources.root_mut(scope) else {
            return;
        };
        let watch = &mut root.watch;
        match event {
            Notification::Changed {
                watch: handle,
                sequence,
                paths,
                rescan,
            } if watch.handle == Some(handle) && sequence > watch.sequence => {
                watch.rescan |= rescan || sequence != watch.sequence + 1;
                watch.sequence = sequence;
                for path in paths {
                    if watch.rescan {
                        break;
                    }
                    if watch.changed.contains(&path) {
                        continue;
                    }
                    if watch.changed.len() >= 512
                        || watch.changed_bytes + path.as_str().len() > 128 * 1024
                    {
                        watch.rescan = true;
                        break;
                    }
                    watch.changed_bytes += path.as_str().len();
                    watch.changed.insert(path);
                }
            }
            Notification::WatchStopped {
                watch: handle,
                error,
            } if watch.handle == Some(handle) => {
                watch.handle = None;
                watch.error = Some(error.message);
                watch.rescan = true;
            }
            _ => return,
        }
        if watch.rescan {
            watch.changed.clear();
            watch.changed_bytes = 0;
        }
        if !watch.queued {
            watch.queued = true;
            queued = Some(session);
        }
    });
    if let Some(session) = queued {
        schedule(state, scope, session);
    }
}

fn schedule(state: &AppContext, scope: ScopeId, session: u64) {
    let Some(application) = state.services().and_then(|runtime| runtime.application()) else {
        return;
    };
    let state = state.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(150));
        application.post(move || flush(&state, scope, session));
    });
}

fn flush(state: &AppContext, scope: ScopeId, session: u64) {
    let Some(host) = state.services().and_then(|runtime| runtime.extensions()) else {
        return;
    };
    let mut retry = false;
    let mut directories = Vec::new();
    let mut active = None;
    state.update(|app| {
        if app.explorer.resources.session != session {
            return;
        }
        let Some(root) = app.explorer.resources.root(scope) else {
            return;
        };
        let resource = root.descriptor.resource.clone();
        if app.explorer.resources.busy.contains(&scope)
            || app.explorer.resources.pending_open.is_some()
            || app.workspace.documents.iter().any(|(id, doc)| {
                doc.meta.provider_root.as_ref() == Some(&resource)
                    && app.editor.saving.resource_pending(*id)
            })
        {
            retry = true;
            return;
        }
        let root = app.explorer.resources.root_mut(scope).unwrap();
        let paths = std::mem::take(&mut root.watch.changed);
        root.watch.changed_bytes = 0;
        let rescan = std::mem::take(&mut root.watch.rescan);
        root.watch.queued = false;
        for (path, directory) in &mut root.directories {
            if rescan
                || paths.contains(path)
                || directory
                    .entries
                    .iter()
                    .any(|entry| paths.contains(&entry.path))
            {
                if let Some(request) = directory.pending.take() {
                    host.cancel_resource(request);
                }
                directory.loaded = false;
                directory.cursor = None;
                directory.error = None;
                if directory.expanded && directories.len() < 128 {
                    directories.push(path.clone());
                }
                directory.expanded = false;
            }
        }
        let current = app.workspace.active_document();
        for (id, doc) in &mut app.workspace.documents {
            let ResourceRef::Provided { path, .. } = &doc.meta.resource else {
                continue;
            };
            if doc.meta.provider_root.as_ref() == Some(&resource)
                && (rescan || paths.contains(path))
            {
                doc.resource_check = true;
                if Some(*id) == current {
                    active = Some(*id);
                }
            }
        }
        let error = root.watch.error.clone();
        if rescan || !paths.is_empty() {
            app.search.ui.refresh += 1;
        }
        super::resource_options::changed(app, scope, &paths, rescan);
        super::resource_project::changed(app, scope, &paths, rescan);
        if let Some(error) = error {
            app.show_error(format!(
                "Filesystem monitoring paused: {error}. Refresh the folder to retry."
            ));
        }
    });
    if retry {
        schedule(state, scope, session);
        return;
    }
    super::resource_options::refresh(state);
    super::resource_project::refresh(state);
    for path in directories {
        super::resources::directory(state, scope, path, false);
    }
    if let Some(id) = active {
        super::resources::resolve(
            state,
            id,
            crate::domain::workspace::ResourceResolution::Check,
        );
    }
}
