//! Resource-backed Markdown navigation, guarded by the originating view.
use super::{context::AppContext, state::AppState};
use crate::domain::{
    document::DocumentStamp,
    identity::{PaneId, ViewId},
};
use loog_workspace_api::{FileKind, ResourceRef};
use std::sync::atomic::{AtomicUsize, Ordering};
static WORKERS: AtomicUsize = AtomicUsize::new(0);
struct Permit;
impl Drop for Permit {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone)]
struct Origin {
    pane: PaneId,
    view: ViewId,
    stamp: DocumentStamp,
    resource: ResourceRef,
    scope: u64,
    generation: u64,
}
impl Origin {
    fn current(&self, app: &AppState) -> bool {
        app.workspace.active_pane() == self.pane
            && app.explorer.resources.open_generation == self.generation
            && app.workspace.active() == Some(self.view)
            && app
                .workspace
                .documents
                .get(&self.stamp.document)
                .is_some_and(|doc| {
                    doc.stamp(self.stamp.document) == self.stamp
                        && doc.meta.resource == self.resource
                })
            && app.explorer.resources.root(self.scope).is_some_and(|root| {
                root.descriptor.resource.environment() == self.resource.environment()
            })
    }
}
pub(crate) fn open(state: &AppContext, pane: PaneId, view: ViewId, destination: &str) {
    let Some((relative, fragment)) =
        crate::features::markdown::resources::provider_relative(destination)
    else {
        state.update(|app| app.show_error("Unsupported resource link"));
        return;
    };
    let app = state.get();
    let Some(id) = app.workspace.view_document(view) else {
        return;
    };
    let doc = &app.workspace.documents[&id];
    let Some((scope, _)) = doc.provider else {
        state.update(|app| app.show_error("Resource is offline. Connect its provider first."));
        return;
    };
    let mut generation = 0;
    state.update(|app| {
        app.explorer.resources.open_generation += 1;
        generation = app.explorer.resources.open_generation;
        if let Some(request) = app.explorer.resources.pending_open.take()
            && let Some(host) = state.services().and_then(|runtime| runtime.extensions())
        {
            host.cancel_resource(request);
        }
    });
    let origin = Origin {
        pane,
        view,
        stamp: doc.stamp(id),
        resource: doc.meta.resource.clone(),
        scope,
        generation,
    };
    let Some(application) = state.services().and_then(|runtime| runtime.application()) else {
        return;
    };
    let context = state.clone();
    if WORKERS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            (count < 4).then_some(count + 1)
        })
        .is_err()
    {
        state.update(|app| {
            app.show_error("Resource navigation is busy; retry when the current request ends")
        });
        return;
    }
    let permit = Permit;
    let worker = std::thread::Builder::new()
        .name("loog-resource-link".into())
        .spawn(move || {
            let _permit = permit;
            let resolved = super::resource_bytes::resolve_background(
                &context,
                scope,
                &origin.resource,
                &relative,
                &|| !origin.current(&context.get()),
            );
            application.post(move || {
                if !origin.current(&context.get()) {
                    return;
                }
                let (file, ancestors) = match resolved {
                    Ok(value) => value,
                    Err(error) => {
                        context.update(|app| app.show_error(error));
                        return;
                    }
                };
                if file.kind == FileKind::Directory {
                    context.update(|app| {
                        app.drawer.visible = true;
                        app.explorer.ui.scroll_x = 0.0;
                    });
                    for path in ancestors {
                        let expanded = context
                            .get()
                            .explorer
                            .resources
                            .root(scope)
                            .and_then(|root| root.directories.get(&path))
                            .is_some_and(|directory| directory.expanded);
                        if !expanded {
                            super::resources::directory(&context, scope, path, false);
                        }
                    }
                    return;
                }
                if crate::domain::media::is_raster(std::path::Path::new(&file.name)) {
                    super::resources::open(&context, scope, file);
                    return;
                }
                let Some(environment) = origin.resource.environment() else {
                    return;
                };
                let resource = ResourceRef::Provided {
                    environment: environment.clone(),
                    path: file.path,
                };
                if let Some(id) = context.get().workspace.file_id_for_resource(&resource) {
                    context.update(|app| {
                        app.workspace.activate_document(id);
                        if let Some(view) = app.workspace.active() {
                            super::markdown_view::finish_navigation(app, view, fragment);
                        }
                    });
                    return;
                }
                let guard = origin.clone();
                super::resources::read_text(
                    &context,
                    resource,
                    64 * 1024 * 1024,
                    super::resources::TextConsumer {
                        valid: Box::new(move |app| guard.current(app)),
                        ready: Box::new(move |app, result| {
                            if !origin.current(app) {
                                return;
                            }
                            match result {
                                Ok(prepared) => {
                                    app.workspace.open_prepared_text(
                                        prepared,
                                        super::workspace::OpenMode::Permanent,
                                    );
                                    if let Some(view) = app.workspace.active() {
                                        super::markdown_view::finish_navigation(
                                            app, view, fragment,
                                        );
                                    }
                                }
                                Err(error) => app.show_error(error),
                            }
                        }),
                    },
                );
            });
        });
    if worker.is_err() {
        state.update(|app| app.show_error("Could not start resource navigation"));
    }
}
