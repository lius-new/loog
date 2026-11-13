//! Native project-settings command, using only generic resource operations.
use super::{context::AppContext, state::AppState};
use loog_workspace_api::{
    Answer, Failure, FailureKind, FileInfo, FileKind, Operation, ProviderPath, ResourceRef,
};
use std::sync::atomic::{AtomicUsize, Ordering};
static WORKERS: AtomicUsize = AtomicUsize::new(0);
struct Permit;
impl Drop for Permit {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Origin {
    root: ResourceRef,
    scope: u64,
    session: u64,
    generation: u64,
    pane: crate::domain::identity::PaneId,
    view: Option<crate::domain::identity::ViewId>,
    input: Option<u64>,
}
impl Origin {
    fn current(&self, app: &AppState) -> bool {
        app.explorer.resources.session == self.session
            && app.explorer.resources.open_generation == self.generation
            && app
                .explorer
                .resources
                .root(self.scope)
                .is_some_and(|root| root.descriptor.resource == self.root)
            && app.workspace.active_pane() == self.pane
            && app.workspace.active() == self.view
            && self
                .view
                .and_then(|view| app.workspace.view(view))
                .map(|view| view.input_generation)
                == self.input
    }
}
pub(super) fn open(state: &AppContext, root: ResourceRef) {
    let Some(application) = state.services().and_then(|runtime| runtime.application()) else {
        return;
    };
    let mut origin = None;
    state.update(|app| {
        let Some(scope) = app
            .explorer
            .resources
            .roots
            .iter()
            .find(|entry| entry.descriptor.resource == root)
            .map(|entry| entry.descriptor.scope)
        else {
            app.show_error("Connect the resource environment before opening project settings");
            return;
        };
        app.explorer.resources.open_generation += 1;
        if let Some(request) = app.explorer.resources.pending_open.take()
            && let Some(host) = state.services().and_then(|runtime| runtime.extensions())
        {
            host.cancel_resource(request);
        }
        let view = app.workspace.active();
        origin = Some(Origin {
            root,
            scope,
            session: app.explorer.resources.session,
            generation: app.explorer.resources.open_generation,
            pane: app.workspace.active_pane(),
            view,
            input: view
                .and_then(|view| app.workspace.view(view))
                .map(|view| view.input_generation),
        });
    });
    let Some(origin) = origin else {
        return;
    };
    if WORKERS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            (count < 4).then_some(count + 1)
        })
        .is_err()
    {
        state.update(|app| {
            app.show_error("Project settings operations are busy; retry after they finish")
        });
        return;
    }
    let permit = Permit;
    let context = state.clone();
    let submitted = std::thread::Builder::new().name("resource-project-command".into()).spawn(move || {
        let result = super::resource_bytes::background_typed(&context, origin.scope, &origin.root, &|| !origin.current(&context.get()), ensure);
        drop(permit);
        application.post(move || {
            if !origin.current(&context.get()) { return; }
            match result {
                Ok(file) => super::resources::open(&context, origin.scope, file),
                Err(error) => context.update(|app| app.show_error(format!("Could not open project settings: {error}. A file or folder may have been created; refresh before retrying."))),
            }
        });
    });
    if let Err(error) = submitted {
        state.update(|app| app.show_error(error.to_string()));
    }
}
fn ensure(
    request: &mut dyn FnMut(Operation) -> Result<Answer, Failure>,
    root: &ProviderPath,
) -> Result<FileInfo, String> {
    let child = |request: &mut dyn FnMut(Operation) -> Result<Answer, Failure>,
                 parent: ProviderPath,
                 name: &str| {
        match request(Operation::ChildPath {
            parent,
            name: name.into(),
        })
        .map_err(|e| e.message)?
        {
            Answer::Path { path } => Ok(path),
            _ => Err("Invalid settings path response".to_string()),
        }
    };
    let directory = child(
        request,
        root.clone(),
        crate::features::settings::project::DIR_NAME,
    )?;
    match request(Operation::Stat {
        path: directory.clone(),
    }) {
        Ok(Answer::Info { file }) if file.path == directory && file.kind == FileKind::Directory => {
        }
        Err(error) if error.kind == FailureKind::NotFound => {
            if !matches!(
                request(Operation::CreateDirectory {
                    path: directory.clone()
                })
                .map_err(|e| e.message)?,
                Answer::Ok
            ) {
                return Err("Invalid settings folder creation receipt".into());
            }
        }
        Err(error) => return Err(error.message),
        _ => return Err("Project settings parent is not a directory".into()),
    }
    let path = child(
        request,
        directory,
        crate::features::settings::project::FILE_NAME,
    )?;
    match request(Operation::Stat { path: path.clone() }) {
        Ok(Answer::Info { file })
            if file.path == path && matches!(file.kind, FileKind::File | FileKind::Symlink) =>
        {
            return Ok(file);
        }
        Err(error) if error.kind == FailureKind::NotFound => {}
        Err(error) => return Err(error.message),
        _ => return Err("Project settings resource is not a file".into()),
    }
    let Answer::Info { file } =
        request(Operation::CreateFile { path: path.clone() }).map_err(|e| e.message)?
    else {
        return Err("Invalid settings file creation receipt".into());
    };
    if file.path != path {
        return Err("Created settings file has the wrong identity".into());
    }
    let Some(revision) = file.revision else {
        return Err(
            "Created empty settings file; provider has no revision for safe initialization".into(),
        );
    };
    if !matches!(
        request(Operation::Write {
            path: path.clone(),
            bytes: crate::features::settings::project::template().into_bytes(),
            expected_revision: Some(revision)
        })
        .map_err(|e| e.message)?,
        Answer::Written { .. }
    ) {
        return Err("Invalid settings initialization receipt".into());
    }
    match request(Operation::Stat { path: path.clone() }).map_err(|e| e.message)? {
        Answer::Info { file } if file.path == path && file.kind == FileKind::File => Ok(file),
        _ => Err("Invalid initialized settings metadata".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn existing_settings_are_never_rewritten_or_parsed_by_the_open_command() {
        let path = |value| ProviderPath::new(value).unwrap();
        let file = ensure(
            &mut |operation| {
                Ok(match operation {
                    Operation::ChildPath { name, .. } => Answer::Path {
                        path: path(if name == ".loog" { "directory" } else { "file" }),
                    },
                    Operation::Stat { path } => Answer::Info {
                        file: FileInfo {
                            kind: if path.as_str() == "directory" {
                                FileKind::Directory
                            } else {
                                FileKind::File
                            },
                            path,
                            name: "settings.json".into(),
                            size: 128,
                            revision: Some("existing".into()),
                            readonly: true,
                        },
                    },
                    _ => panic!("Opening existing settings must not mutate them"),
                })
            },
            &path("root"),
        )
        .unwrap();
        assert_eq!(file.revision.as_deref(), Some("existing"));
    }
}
