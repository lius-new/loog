//! Native resource consumers. Providers own IO, paths and revision semantics.
use super::{context::AppContext, extensions::host::ExtensionHost, state::AppState};
use crate::domain::workspace::{OpenMode, ResourceResolution};
use loog_workspace_api::{
    Answer, Failure, FailureKind, FileInfo, FileKind, Operation, ProviderPath, ResourceRef, ScopeId,
};
use std::sync::Arc;

pub(crate) fn directory(state: &AppContext, scope: ScopeId, path: ProviderPath, more: bool) {
    let Some(host) = state.services().and_then(|r| r.extensions()) else {
        return;
    };
    state.update(|app| {
        app.explorer.resources.directory_generation += 1;
        let generation = app.explorer.resources.directory_generation;
        let Some(directory) = app.explorer.resources.directory_mut(scope, &path) else {
            return;
        };
        if !more && directory.expanded && directory.error.is_none() {
            directory.expanded = false;
            directory.generation = generation;
            if let Some(request) = directory.pending.take() {
                host.cancel_resource(request);
            }
            return;
        }
        directory.expanded = true;
        if directory.pending.is_some() || (!more && directory.loaded && directory.error.is_none()) {
            return;
        }
        let cursor = if more { directory.cursor.clone() } else { None };
        if more && cursor.is_none() {
            return;
        }
        directory.error = None;
        directory.generation = generation;
        let target = path.clone();
        let result = host.request_resource(
            scope,
            Operation::List {
                path: path.clone(),
                cursor: cursor.clone(),
                limit: 512,
            },
            Box::new(move |app, result| {
                let Some(root) = app.explorer.resources.root_mut(scope) else {
                    return;
                };
                let total: usize = root.directories.values().map(|d| d.entries.len()).sum();
                let Some(directory) = root.directories.get_mut(&target) else {
                    return;
                };
                // Collapse cancels the request, including any result already queued.
                if directory.generation != generation || directory.pending.take().is_none() {
                    return;
                }
                match result {
                    Ok(Answer::Directory {
                        entries,
                        cursor: next,
                    }) if entries.len() <= 512
                        && total + entries.len() <= crate::features::resource_tree::MAX_ENTRIES
                        && next
                            .as_ref()
                            .is_none_or(|next| Some(next) != cursor.as_ref()) =>
                    {
                        if cursor.is_none() {
                            directory.entries.clear();
                        }
                        for entry in entries {
                            if entry.path != target
                                && !directory.entries.iter().any(|e| e.path == entry.path)
                            {
                                directory.entries.push(entry);
                            }
                        }
                        directory.cursor = next;
                        directory.loaded = true;
                        directory.error = None;
                    }
                    Ok(_) => {
                        directory.error = Some(
                            "Directory result exceeds its limit or has an invalid cursor".into(),
                        )
                    }
                    Err(error) => directory.error = Some(error.message),
                }
            }),
        );
        match result {
            Ok(request) => directory.pending = Some(request),
            Err(error) => directory.error = Some(error.message),
        }
    });
}

struct Opening {
    generation: u64,
    scope: ScopeId,
    resource: ResourceRef,
    root: ResourceRef,
    file: FileInfo,
    pane: crate::domain::identity::PaneId,
    active: Option<crate::domain::identity::ViewId>,
    input_generation: Option<u64>,
    bytes: Vec<u8>,
    revision: Option<String>,
    existing: Option<(crate::domain::identity::FileId, u64)>,
    resolution: ResourceResolution,
    mode: OpenMode,
    stamp: Option<crate::domain::document::DocumentStamp>,
    limit: usize,
    consumer: Option<TextConsumer>,
    warning: Option<String>,
}
type TextReady = Box<
    dyn FnOnce(&mut AppState, Result<crate::domain::workspace::PreparedTextFile, String>) + Send,
>;
pub(crate) struct TextConsumer {
    pub valid: Box<dyn Fn(&AppState) -> bool + Send>,
    pub ready: TextReady,
}
impl Opening {
    fn release(&self, app: &mut AppState) {
        if self.consumer.is_none() && app.explorer.resources.open_generation == self.generation {
            app.explorer.resources.pending_open = None;
        }
    }
    fn valid(&self, app: &AppState) -> bool {
        if let Some(consumer) = &self.consumer {
            return (consumer.valid)(app)
                && app
                    .explorer
                    .resources
                    .root(self.scope)
                    .is_some_and(|root| root.descriptor.resource == self.root);
        }
        app.explorer.resources.open_generation == self.generation
            && app.explorer.resources.root(self.scope).is_some()
            && app.editor.pending_file.is_none()
            && app.workspace.active_pane() == self.pane
            && app.workspace.active() == self.active
            && self
                .active
                .and_then(|id| app.workspace.view(id))
                .map(|v| v.input_generation)
                == self.input_generation
            && self.existing.is_none_or(|(id, incarnation)| {
                !app.editor.saving.resource_pending(id)
                    && app
                        .workspace
                        .documents
                        .get(&id)
                        .is_some_and(|d| d.incarnation == incarnation)
            })
            && (self.resolution != ResourceResolution::Reload
                || self.stamp.is_none_or(|stamp| {
                    app.workspace
                        .documents
                        .get(&stamp.document)
                        .is_some_and(|d| d.stamp(stamp.document) == stamp)
                }))
    }
}

/// Read a provided resource without opening or activating an editor. The same
/// revision checks, chunk limits and encoding path serve navigation and previews.
/// The caller's validity guard is checked before every continuation.
pub(crate) fn read_text(
    state: &AppContext,
    resource: ResourceRef,
    limit: usize,
    consumer: TextConsumer,
) {
    let Some(host) = state.services().and_then(|runtime| runtime.extensions()) else {
        return;
    };
    state.update(|app| {
        if resource.environment().is_none() || !(1..=FILE_LIMIT).contains(&limit) {
            (consumer.ready)(app, Err("Invalid resource text read".into()));
            return;
        }
        let roots = app
            .explorer
            .resources
            .roots
            .iter()
            .filter(|root| root.descriptor.resource.environment() == resource.environment())
            .take(128)
            .map(|root| root.descriptor.clone())
            .collect();
        locate_text(app, state.clone(), host, resource, roots, limit, consumer);
    });
}
fn locate_text(
    app: &mut AppState,
    state: AppContext,
    host: Arc<ExtensionHost>,
    resource: ResourceRef,
    mut roots: std::collections::VecDeque<loog_workspace_api::PresentedRoot>,
    limit: usize,
    consumer: TextConsumer,
) {
    if !(consumer.valid)(app) {
        return;
    }
    let Some(root) = roots.pop_front() else {
        (consumer.ready)(
            app,
            Err("Resource is unavailable in the connected workspace roots".into()),
        );
        return;
    };
    let ResourceRef::Provided { path, .. } = &resource else {
        unreachable!()
    };
    let path = path.clone();
    let next_host = Arc::downgrade(&host);
    let result = host.request_resource(
        root.scope,
        Operation::Stat { path: path.clone() },
        Box::new(move |app, result| {
            if !(consumer.valid)(app) {
                return;
            }
            match result {
                Err(error)
                    if matches!(
                        error.kind,
                        FailureKind::PermissionDenied
                            | FailureKind::NotFound
                            | FailureKind::Unavailable
                    ) =>
                {
                    if let Some(host) = next_host.upgrade() {
                        locate_text(app, state, host, resource, roots, limit, consumer);
                    }
                }
                Ok(Answer::Info { file })
                    if file.path == path
                        && matches!(file.kind, FileKind::File | FileKind::Symlink)
                        && file.size <= limit as u64 =>
                {
                    let opening = Opening {
                        warning: None,
                        generation: 0,
                        scope: root.scope,
                        resource,
                        root: root.resource,
                        file,
                        pane: app.workspace.active_pane(),
                        active: None,
                        input_generation: None,
                        bytes: vec![],
                        revision: None,
                        existing: None,
                        resolution: ResourceResolution::Check,
                        mode: OpenMode::Permanent,
                        stamp: None,
                        limit,
                        consumer: Some(consumer),
                    };
                    if let Some(host) = next_host.upgrade() {
                        read(app, state, host, opening);
                    }
                }
                Ok(_) => (consumer.ready)(
                    app,
                    Err("Resource is not a regular file within the requested size limit".into()),
                ),
                Err(error) => (consumer.ready)(app, Err(error.message)),
            }
        }),
    );
    if let Err(error) = result {
        app.show_error(error.message);
    }
}

pub(crate) fn open(state: &AppContext, scope: ScopeId, file: FileInfo) {
    open_in_mode(state, scope, file, OpenMode::Permanent);
}
pub(crate) fn open_in_mode(state: &AppContext, scope: ScopeId, file: FileInfo, mode: OpenMode) {
    open_with(state, scope, file, ResourceResolution::Check, mode);
}
pub(crate) fn reconnect_active(state: &AppContext) {
    let id =
        {
            let app = state.get();
            app.workspace.active_document().filter(|id| {
                let doc = &app.workspace.documents[id];
                doc.meta.resource.environment().is_some()
                    && (doc.disk_conflict
                        || doc.resource_check
                        || doc
                            .provider
                            .as_ref()
                            .is_none_or(|(scope, _)| app.explorer.resources.root(*scope).is_none()))
                    && app.explorer.resources.roots.iter().any(|root| {
                        doc.meta.provider_root.as_ref() == Some(&root.descriptor.resource)
                    })
            })
        };
    if let Some(id) = id {
        resolve(state, id, ResourceResolution::Check);
    }
}
pub(crate) fn resolve(
    state: &AppContext,
    id: crate::domain::identity::FileId,
    resolution: ResourceResolution,
) {
    let target = {
        let app = state.get();
        app.workspace.documents.get(&id).and_then(|doc| {
            let ResourceRef::Provided { environment, path } = &doc.meta.resource else {
                return None;
            };
            let scope = doc
                .provider
                .as_ref()
                .map(|(scope, _)| *scope)
                .filter(|scope| app.explorer.resources.root(*scope).is_some())
                .or_else(|| {
                    app.explorer
                        .resources
                        .roots
                        .iter()
                        .find(|root| {
                            doc.meta.provider_root.as_ref() == Some(&root.descriptor.resource)
                                && root.descriptor.resource.environment() == Some(environment)
                        })
                        .map(|root| root.descriptor.scope)
                })?;
            Some((
                scope,
                FileInfo {
                    path: path.clone(),
                    name: doc.meta.name.clone(),
                    kind: FileKind::File,
                    size: 0,
                    revision: None,
                    readonly: false,
                },
            ))
        })
    };
    if let Some((scope, file)) = target {
        open_with(state, scope, file, resolution, OpenMode::Permanent);
    } else {
        state.update(|app| app.show_error("Resource is offline. Connect its provider first."));
    }
}
fn open_with(
    state: &AppContext,
    scope: ScopeId,
    file: FileInfo,
    resolution: ResourceResolution,
    mode: OpenMode,
) {
    let Some(host) = state.services().and_then(|r| r.extensions()) else {
        return;
    };
    state.update(|app| {
        if app.explorer.resources.busy.contains(&scope) {
            app.show_toast("Wait for the current resource operation before opening files.");
            return;
        }
        let Some(root) = app.explorer.resources.root(scope) else {
            return;
        };
        let ResourceRef::Provided { environment, .. } = &root.descriptor.resource else {
            return;
        };
        let resource = ResourceRef::Provided {
            environment: environment.clone(),
            path: file.path.clone(),
        };
        let root = root.descriptor.resource.clone();
        app.explorer.resources.open_generation += 1;
        if let Some(request) = app.explorer.resources.pending_open.take() {
            host.cancel_resource(request);
        }
        super::file_loading::cancel(app);
        let existing = app.workspace.file_id_for_resource(&resource);
        if let Some(id) = existing {
            app.workspace.activate_document(id);
            if mode == OpenMode::Permanent {
                app.workspace.promote_active_preview();
            }
            if resolution == ResourceResolution::Check
                && app.workspace.documents.get(&id).is_some_and(|d| {
                    d.provider.as_ref().is_some_and(|(old, _)| *old == scope)
                        && !d.disk_conflict
                        && !d.resource_check
                })
            {
                return;
            }
        }
        if !matches!(file.kind, FileKind::File | FileKind::Symlink) {
            app.show_error("This resource is not a regular file");
            return;
        }
        let limit = if crate::domain::media::is_raster(std::path::Path::new(&file.name)) {
            32 * 1024 * 1024
        } else {
            FILE_LIMIT
        };
        if file.size > limit as u64 {
            app.show_error("Resource exceeds its file size limit");
            return;
        }
        let active = app.workspace.active();
        let opening = Opening {
            warning: None,
            generation: app.explorer.resources.open_generation,
            scope,
            resource,
            root,
            file,
            pane: app.workspace.active_pane(),
            active,
            input_generation: active
                .and_then(|id| app.workspace.view(id))
                .map(|v| v.input_generation),
            bytes: Vec::new(),
            revision: None,
            existing: existing.and_then(|id| {
                app.workspace
                    .documents
                    .get(&id)
                    .map(|d| (id, d.incarnation))
            }),
            resolution,
            mode,
            stamp: existing.and_then(|id| app.workspace.documents.get(&id).map(|d| d.stamp(id))),
            limit,
            consumer: None,
        };
        read(app, state.clone(), host.clone(), opening);
    });
}

const FILE_LIMIT: usize = 64 * 1024 * 1024;
fn read(app: &mut AppState, state: AppContext, host: Arc<ExtensionHost>, mut opening: Opening) {
    if !opening.valid(app) {
        opening.release(app);
        return;
    }
    let operation = Operation::Read {
        path: opening.file.path.clone(),
        offset: opening.bytes.len() as u64,
        limit: 128 * 1024,
    };
    let detached = opening.consumer.is_some();
    let generation = opening.generation;
    let next_host = Arc::downgrade(&host);
    let result = host.request_resource(
        opening.scope,
        operation,
        Box::new(move |app, result| {
            if !opening.valid(app) {
                opening.release(app);
                return;
            }
            let outcome = match result {
                Ok(Answer::Bytes {
                    bytes,
                    revision,
                    eof,
                }) => {
                    if opening.bytes.len() + bytes.len() > opening.limit || bytes.len() > 128 * 1024
                    {
                        Err(Failure::new(
                            FailureKind::LimitExceeded,
                            "File exceeds the editor size limit",
                        ))
                    } else if !opening.bytes.is_empty() && opening.revision != revision {
                        Err(Failure::new(
                            FailureKind::Conflict,
                            "File changed while opening; open it again",
                        ))
                    } else if bytes.is_empty() && !eof {
                        Err(Failure::new(
                            FailureKind::InvalidRequest,
                            "Provider returned no read progress",
                        ))
                    } else {
                        opening.revision = revision;
                        opening.bytes.extend(bytes);
                        Ok(eof)
                    }
                }
                Ok(_) => Err(Failure::new(
                    FailureKind::InvalidRequest,
                    "Invalid read result",
                )),
                Err(error) => Err(error),
            };
            match outcome {
                Ok(false) => {
                    if let Some(host) = next_host.upgrade() {
                        read(app, state, host, opening);
                    } else {
                        opening.release(app);
                        app.show_error("Resource provider stopped");
                    }
                }
                Ok(true) => prepare(app, state, opening),
                Err(error) => {
                    opening.release(app);
                    if let Some(consumer) = opening.consumer.take() {
                        (consumer.ready)(app, Err(error.to_string()));
                        return;
                    }
                    if let Some((id, _)) = opening.existing {
                        if let Some(document) = app.workspace.documents.get_mut(&id) {
                            document.disk_conflict = true;
                            if error.kind == FailureKind::NotFound {
                                document.missing_on_disk = true;
                            }
                        }
                    }
                    app.show_error(format!(
                        "Could not open {}: {error}",
                        opening.resource.display()
                    ));
                }
            }
        }),
    );
    match result {
        Ok(request) if !detached => app.explorer.resources.pending_open = Some(request),
        Ok(_) => {}
        Err(error) => {
            if !detached && app.explorer.resources.open_generation == generation {
                app.explorer.resources.pending_open = None;
            }
            app.show_error(error.message);
        }
    }
}

fn prepare(app: &mut AppState, state: AppContext, mut opening: Opening) {
    let context = state.clone();
    super::resource_project::read(
        app,
        &state,
        opening.scope,
        Box::new(move |app, result| {
            if !opening.valid(app) {
                opening.release(app);
                return;
            }
            if let Err(error) = result {
                // Keep the configuration file itself editable when malformed.
                // The last valid layer (or the user layer) remains in effect.
                opening.warning = Some(format!(
                    "Project settings could not be read: {error}. Using the last valid settings."
                ));
            }
            let Some(host) = context.services().and_then(|runtime| runtime.extensions()) else {
                opening.release(app);
                return;
            };
            let ResourceRef::Provided { path: base, .. } = &opening.root else {
                opening.release(app);
                return;
            };
            let generation = opening.generation;
            let detached = opening.consumer.is_some();
            let result = host.request_resource(
                opening.scope,
                Operation::RelativePath {
                    base: base.clone(),
                    path: opening.file.path.clone(),
                },
                Box::new(move |app, result| {
                    if !opening.valid(app) {
                        opening.release(app);
                        return;
                    }
                    let relative = match result {
                        Ok(Answer::RelativePath {
                            components: Some(parts),
                        }) if !parts.is_empty()
                            && parts.len() <= 128
                            && parts.iter().map(String::len).sum::<usize>() <= 32 * 1024
                            && parts.iter().all(|part| {
                                !part.is_empty()
                                    && !matches!(part.as_str(), "." | "..")
                                    && !part.contains(['/', '\0'])
                            }) =>
                        {
                            Some(parts)
                        }
                        _ => None,
                    };
                    decode(app, context, opening, relative);
                }),
            );
            match result {
                Ok(request) if !detached => app.explorer.resources.pending_open = Some(request),
                Ok(_) => {}
                Err(error) => {
                    if !detached && app.explorer.resources.open_generation == generation {
                        app.explorer.resources.pending_open = None;
                    }
                    app.show_error(error.message);
                }
            }
        }),
    );
}

fn decode(app: &mut AppState, state: AppContext, opening: Opening, relative: Option<Vec<String>>) {
    let Some(runtime) = state.services() else {
        opening.release(app);
        return;
    };
    let Some(application) = runtime.application() else {
        opening.release(app);
        return;
    };
    let options = super::resource_project::settings(app, &opening.root).encoding;
    let explicit = opening
        .existing
        .and_then(|(id, _)| app.workspace.documents.get(&id))
        .and_then(|doc| doc.meta.read_override);
    let language = super::resource_project::language(
        app,
        &opening.root,
        &opening.file.name,
        relative.as_deref(),
    );
    let detached = opening.consumer.is_some();
    let generation = opening.generation;
    let image =
        !detached && crate::domain::media::is_raster(std::path::Path::new(&opening.file.name));
    let result = runtime.save(Box::new(move || {
        let prepared = if image {
            crate::infrastructure::image_filesystem::decode(&opening.bytes).map(|image| {
                crate::domain::workspace::PreparedTextFile::provided_image(
                    opening.resource.clone(),
                    opening.file.name.clone(),
                    opening.scope,
                    opening.revision.clone(),
                    image,
                )
                .with_provider_root(Some(opening.root.clone()))
            })
        } else {
            crate::features::encoding::decode(&opening.bytes, explicit, &options, opening.limit)
                .map(|decoded| {
                    crate::domain::workspace::PreparedTextFile::provided(
                        opening.resource.clone(),
                        opening.file.name.clone(),
                        opening.scope,
                        opening.revision.clone(),
                        decoded,
                    )
                    .with_provider_root(Some(opening.root.clone()))
                })
                .map_err(|error| error.to_string())
        };
        let prepared = prepared.map(|prepared| {
            prepared
                .with_resource_language(relative, language)
                .with_read_override(explicit)
        });
        application.post(move || {
            state.update(|app| {
                if !opening.valid(app) {
                    opening.release(app);
                    crate::infrastructure::ui_dispatch::retire(prepared);
                    return;
                }
                match prepared {
                    Ok(prepared) if image => finish_open(app, opening, Ok(prepared)),
                    Ok(prepared) => {
                        let resource = opening.resource.clone();
                        super::resource_options::read(
                            app,
                            &state,
                            opening.scope,
                            &resource,
                            Box::new(move |app, options, paths| {
                                let current = opening.valid(app);
                                let resource = opening.resource.clone();
                                let scope = opening.scope;
                                let warning = opening
                                    .valid(app)
                                    .then(|| options.as_ref().err().cloned())
                                    .flatten();
                                let options = options.unwrap_or_else(|_| {
                                    opening
                                        .existing
                                        .and_then(|(id, _)| app.workspace.documents.get(&id))
                                        .map(|doc| doc.buffer.file_options())
                                        .unwrap_or_default()
                                });
                                finish_open(app, opening, Ok(prepared.with_file_options(options)));
                                if current {
                                    super::resource_options::remember(app, scope, resource, paths);
                                }
                                if let Some(error) = warning {
                                    app.show_error(format!(
                                        "Could not read resource editing options: {error}"
                                    ));
                                }
                            }),
                        );
                    }
                    Err(error) => finish_open(app, opening, Err(error.to_string())),
                }
            });
        });
    }));
    if let Err(error) = result {
        if !detached && app.explorer.resources.open_generation == generation {
            app.explorer.resources.pending_open = None;
        }
        app.show_error(error);
    }
}

fn finish_open(
    app: &mut AppState,
    mut opening: Opening,
    prepared: Result<crate::domain::workspace::PreparedTextFile, String>,
) {
    // Keep the open lifecycle through decoding and resource configuration so
    // watch reconciliation cannot replace an explicit conflict resolution.
    opening.release(app);
    if !opening.valid(app) {
        crate::infrastructure::ui_dispatch::retire(prepared);
        return;
    }
    if let Some(consumer) = opening.consumer.take() {
        (consumer.ready)(app, prepared);
        return;
    }
    match prepared {
        Ok(prepared) => {
            if let Some((id, _)) = opening.existing {
                if let Err(error) = app
                    .workspace
                    .resolve_resource(id, prepared, opening.resolution)
                {
                    app.show_error(error);
                    return;
                }
            } else {
                app.workspace.open_prepared_text(prepared, opening.mode);
            }
            app.editor.drag = None;
            app.editor.ime = None;
            app.chrome.toast = None;
            if let Some(warning) = opening.warning {
                app.show_error(warning);
            }
        }
        Err(error) => app.show_error(format!(
            "Could not open {}: {error}",
            opening.resource.display()
        )),
    }
}
