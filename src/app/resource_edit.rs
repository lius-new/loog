//! Asynchronous native file operations; naming and containment belong to providers.
use super::{context::AppContext, extensions::host::ExtensionHost, state::AppState};
use crate::features::resource_tree::{Edit, EditKind, Target};
use crate::ui::explorer::commands::MenuAction;
use loog_workspace_api::{
    Answer, Capability, EnvironmentKey, Failure, FailureKind, Operation, ProviderPath, ResourceRef,
    ScopeId,
};
use std::sync::{Arc, Weak};

pub(crate) fn action(
    state: &AppContext,
    action: MenuAction,
    target: Target,
    clipboard: &dyn lgui::services::Clipboard,
) {
    if action == MenuAction::WorkspaceTrust {
        state.update(|app| {
            app.explorer.resources.trust_review = app
                .explorer
                .resources
                .root(target.scope)
                .map(|root| root.descriptor.resource.clone());
        });
        return;
    }
    if action == MenuAction::RefreshExplorer {
        refresh(state, target.scope);
        return;
    }
    if action == MenuAction::CopyPath {
        if let Err(error) = clipboard.write_text(target.path.as_str()) {
            state.update(|app| app.show_error(format!("Clipboard: {error}")));
        }
        return;
    }
    let kind = match action {
        MenuAction::NewFile => EditKind::NewFile,
        MenuAction::NewFolder => EditKind::NewFolder,
        MenuAction::Rename if target.parent.is_some() => EditKind::Rename,
        MenuAction::Delete if target.parent.is_some() => EditKind::Delete,
        _ => return,
    };
    state.update(|app| {
        if app.explorer.resources.busy.contains(&target.scope) {
            app.show_toast("Wait for the current resource operation.");
            return;
        }
        app.explorer.model.cancel_edit();
        app.explorer.ui.clear_edit();
        if kind == EditKind::Rename {
            app.explorer.ui.create_input =
                crate::ui::components::input::InputState::new_selected(&target.name);
        }
        app.explorer.resources.edit = Some(Edit { target, kind });
    });
}

struct Change {
    session: u64,
    state: AppContext,
    host: Weak<ExtensionHost>,
    edit: Edit,
    environment: EnvironmentKey,
    tracked: Vec<ProviderPath>,
}

pub(crate) fn submit(state: &AppContext) {
    let Some(host) = state.services().and_then(|r| r.extensions()) else {
        return;
    };
    state.update(|app| {
        let Some(edit) = app.explorer.resources.edit.clone() else {
            return;
        };
        let scope = edit.target.scope;
        let name = app.explorer.ui.create_input.text().to_owned();
        if name.is_empty() || (edit.kind == EditKind::Delete && name != edit.target.name) {
            app.explorer.ui.create_error = Some(if edit.kind == EditKind::Delete {
                "Enter the exact name to confirm permanent deletion".into()
            } else {
                "Enter a name".into()
            });
            return;
        }
        if app.explorer.resources.busy.contains(&scope) {
            return;
        }
        let Some(root) = app.explorer.resources.root(scope) else {
            return;
        };
        if !root
            .descriptor
            .capabilities
            .contains(&Capability::FilesWrite)
        {
            app.show_error("Resource is read-only");
            return;
        }
        let Some(environment) = root.descriptor.resource.environment().cloned() else {
            return;
        };
        let same_environment = |doc: &crate::domain::workspace::OpenDocument| {
            doc.meta.resource.environment() == Some(&environment)
        };
        if app
            .workspace
            .documents
            .iter()
            .any(|(id, doc)| same_environment(doc) && app.editor.saving.resource_pending(*id))
        {
            app.show_error("Wait for pending resource saves before changing files");
            return;
        }
        if edit.kind == EditKind::Delete
            && app
                .workspace
                .documents
                .iter()
                .any(|(id, doc)| same_environment(doc) && app.workspace.is_dirty(*id))
        {
            app.show_error("Save or close modified files in this environment before deleting.");
            return;
        }
        let tracked: Vec<_> = app
            .workspace
            .documents
            .values()
            .filter(|doc| same_environment(doc))
            .filter_map(|doc| match &doc.meta.resource {
                ResourceRef::Provided { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect();
        if tracked.len() > 1024 {
            app.show_error("Too many open resources for this file operation");
            return;
        }
        let parent = if edit.kind == EditKind::Rename {
            edit.target.parent.clone()
        } else {
            Some(edit.target.path.clone())
        };
        let operation = if edit.kind == EditKind::Delete {
            Operation::Remove {
                path: edit.target.path.clone(),
                recursive: false,
            }
        } else {
            let Some(parent) = parent else { return };
            Operation::ChildPath { parent, name }
        };
        app.explorer.resources.busy.insert(scope);
        app.explorer.resources.edit = None;
        app.explorer.ui.clear_edit();
        app.explorer.resources.open_generation += 1;
        if let Some(request) = app.explorer.resources.pending_open.take() {
            host.cancel_resource(request);
        }
        let resolving = edit.kind != EditKind::Delete;
        let change = Arc::new(Change {
            session: app.explorer.resources.session,
            state: state.clone(),
            host: Arc::downgrade(&host),
            edit,
            environment,
            tracked,
        });
        send(app, change, operation, resolving);
    });
}

fn send(app: &mut AppState, change: Arc<Change>, operation: Operation, resolving: bool) {
    if app.explorer.resources.session != change.session {
        return;
    }
    let Some(host) = change.host.upgrade() else {
        finish(
            app,
            &change,
            Err(Failure::new(
                FailureKind::Unavailable,
                "Resource provider stopped",
            )),
        );
        return;
    };
    let next = change.clone();
    let result = host.request_resource(
        change.edit.target.scope,
        operation,
        Box::new(move |app, result| {
            if !resolving {
                finish(app, &next, result);
                return;
            }
            let path = match result {
                Ok(Answer::Path { path }) => path,
                Err(error) => {
                    finish(app, &next, Err(error));
                    return;
                }
                _ => {
                    finish(
                        app,
                        &next,
                        Err(Failure::new(
                            FailureKind::InvalidRequest,
                            "Invalid path result",
                        )),
                    );
                    return;
                }
            };
            let operation = match next.edit.kind {
                EditKind::NewFile => Operation::CreateFile { path },
                EditKind::NewFolder => Operation::CreateDirectory { path },
                EditKind::Rename => {
                    if path == next.edit.target.path {
                        finish(app, &next, Ok(Answer::Relocated { entries: vec![] }));
                        return;
                    }
                    Operation::Relocate {
                        source: next.edit.target.path.clone(),
                        target: path,
                        tracked: next.tracked.clone(),
                    }
                }
                EditKind::Delete => unreachable!(),
            };
            send(app, next, operation, false);
        }),
    );
    if let Err(error) = result {
        finish(app, &change, Err(error));
    }
}

fn finish(app: &mut AppState, change: &Change, result: Result<Answer, Failure>) {
    if app.explorer.resources.session != change.session {
        return;
    }
    let scope = change.edit.target.scope;
    app.explorer.resources.busy.remove(&scope);
    let mut created = None;
    let mut reconcile = None;
    let outcome: Result<(), Failure> = match result {
        Ok(Answer::Info { file }) if change.edit.kind == EditKind::NewFile => {
            created = Some(file);
            Ok(())
        }
        Ok(Answer::Relocated { entries }) if change.edit.kind == EditKind::Rename => {
            if entries.len() > change.tracked.len()
                || entries.iter().any(|e| !change.tracked.contains(&e.source))
            {
                Err(Failure::new(
                    FailureKind::UnknownOutcome,
                    "Resource was renamed but its identity mapping is invalid",
                ))
            } else {
                let result = app
                    .workspace
                    .relocate_resources(&change.environment, &entries)
                    .map_err(|error| Failure::new(FailureKind::UnknownOutcome, error));
                reconcile = app.workspace.active_document().filter(|id| {
                    app.workspace.documents.get(id).is_some_and(|doc| {
                        doc.meta.resource.environment() == Some(&change.environment)
                            && doc.disk_conflict
                    })
                });
                result
            }
        }
        Ok(Answer::Ok) if matches!(change.edit.kind, EditKind::NewFolder | EditKind::Delete) => {
            if change.edit.kind == EditKind::Delete {
                let resource = ResourceRef::Provided {
                    environment: change.environment.clone(),
                    path: change.edit.target.path.clone(),
                };
                if let Some(id) = app.workspace.file_id_for_resource(&resource) {
                    let doc = app.workspace.documents.get_mut(&id).unwrap();
                    doc.missing_on_disk = true;
                    doc.disk_conflict = true;
                    doc.provider = None;
                }
            }
            Ok(())
        }
        Ok(_) => Err(Failure::new(
            FailureKind::UnknownOutcome,
            "Invalid file operation result; check the provider before retrying",
        )),
        Err(error) => Err(error),
    };
    if let Err(error) = outcome {
        if error.kind == FailureKind::UnknownOutcome {
            for doc in app
                .workspace
                .documents
                .values_mut()
                .filter(|doc| doc.meta.resource.environment() == Some(&change.environment))
            {
                doc.disk_conflict = true;
            }
        }
        app.show_error(error.message);
    }
    if let Some(application) = change.state.services().and_then(|r| r.application()) {
        let state = change.state.clone();
        application.post(move || {
            refresh(&state, scope);
            if let Some(file) = created {
                super::resources::open(&state, scope, file);
            } else if let Some(id) = reconcile {
                super::resources::resolve(
                    &state,
                    id,
                    crate::domain::workspace::ResourceResolution::Check,
                );
            }
        });
    }
}

pub(crate) fn refresh(state: &AppContext, scope: ScopeId) {
    let Some(host) = state.services().and_then(|r| r.extensions()) else {
        return;
    };
    let mut path = None;
    state.update(|app| {
        if app.explorer.resources.busy.contains(&scope) {
            return;
        }
        let Some(root) = app.explorer.resources.root_mut(scope) else {
            return;
        };
        root.watch.error = None;
        for directory in root.directories.values() {
            if let Some(request) = directory.pending {
                host.cancel_resource(request);
            }
        }
        root.directories.clear();
        if let ResourceRef::Provided { path: target, .. } = &root.descriptor.resource {
            path = Some(target.clone());
        }
    });
    if let Some(path) = path {
        super::resources::directory(state, scope, path, false);
    }
    super::resource_watch::start(state);
}
