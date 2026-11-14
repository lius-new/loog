//! Source Control scheduling over generic resource execution and file IO.
use super::{context::AppContext, resource_git, state::AppState};
use crate::features::git::{
    DiffTarget, GitError, GitResult,
    diff_document::{DiffDocument, ResourceDiffIdentity},
    error::GitErrorKind,
    path::GitPath,
    resources::{Entry, Snapshot},
};
use loog_workspace_api::{Answer, Capability, Operation, ResourceRef, ScopeId};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

static WORKERS: AtomicUsize = AtomicUsize::new(0);
static WRITES: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<ResourceRef>>> =
    std::sync::OnceLock::new();
struct WriteGuard(ResourceRef);
impl WriteGuard {
    fn acquire(root: &ResourceRef) -> GitResult<Self> {
        if WRITES
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .insert(root.clone())
        {
            Ok(Self(root.clone()))
        } else {
            Err(GitError::new(
                GitErrorKind::Locked,
                "A Git write is already running in this repository",
            ))
        }
    }
}
impl Drop for WriteGuard {
    fn drop(&mut self) {
        WRITES.get().unwrap().lock().unwrap().remove(&self.0);
    }
}
struct Permit;
impl Drop for Permit {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}
fn permit() -> Option<Permit> {
    WORKERS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < 4).then_some(n + 1)
        })
        .ok()
        .map(|_| Permit)
}

pub(super) fn sync(app: &mut AppState) {
    let previous_selection = app.git.resources.model.selected;
    let roots = &app.explorer.resources.roots;
    let active = app
        .workspace
        .active_meta()
        .map(|meta| meta.resource.clone());
    let document = app
        .workspace
        .active_document()
        .and_then(|id| app.workspace.documents.get(&id));
    let scope = document.and_then(|document| document.provider.as_ref().map(|(scope, _)| *scope));
    let model = &mut app.git.resources.model;
    let had_entries = !model.entries.is_empty();
    if let Some(scope) = model.selected
        && !roots.iter().any(|root| root.descriptor.scope == scope)
        && let Some(entry) = model.entries.get(&scope)
    {
        model.offline = Some((entry.root.resource.clone(), entry.root.name.clone()));
    }
    model
        .entries
        .retain(|scope, _| roots.iter().any(|root| root.descriptor.scope == *scope));
    app.git.resources.messages.retain(|resource, _| {
        roots
            .iter()
            .any(|root| &root.descriptor.resource == resource)
            || model
                .offline
                .as_ref()
                .is_some_and(|(root, _)| root == resource)
            || app
                .explorer
                .resources
                .bookmarks
                .iter()
                .any(|bookmark| &bookmark.resource == resource)
    });
    for root in roots {
        let descriptor = &root.descriptor;
        app.git
            .resources
            .messages
            .entry(descriptor.resource.clone())
            .or_default();
        let entry = model
            .entries
            .entry(descriptor.scope)
            .or_insert_with(|| Entry {
                root: descriptor.clone(),
                target: descriptor.resource.clone(),
                target_is_file: false,
                sequence: 0,
                busy: false,
                dirty: true,
                repository: None,
                groups: Default::default(),
                error: None,
            });
        if entry.root.capabilities != descriptor.capabilities {
            entry.sequence += 1;
            entry.busy = false;
            entry.dirty = true;
        }
        entry.root = descriptor.clone();
        if model
            .offline
            .as_ref()
            .is_some_and(|(resource, _)| resource == &descriptor.resource)
        {
            model.selected = Some(descriptor.scope);
            model.offline = None;
        }
        if !descriptor.capabilities.contains(&Capability::Process) {
            entry.repository = None;
            entry.error = Some("Trust this folder to allow Git execution.".into());
        }
    }
    if model.active != active {
        model.active = active.clone();
        if let Some(scope) = scope {
            model.selected = Some(scope);
            model.offline = None;
            if let Some(entry) = model.entries.get_mut(&scope) {
                let target = active.unwrap_or_else(|| entry.root.resource.clone());
                if entry.target != target {
                    entry.target = target;
                    entry.target_is_file = true;
                    entry.dirty = true;
                    entry.sequence += 1;
                    entry.busy = false;
                    entry.repository = None;
                }
            }
        } else if let Some(identity) = app
            .workspace
            .active_diff()
            .and_then(|d| d.provided.as_ref())
        {
            model.selected = model
                .entries
                .iter()
                .find(|(_, entry)| {
                    entry
                        .repository
                        .as_ref()
                        .is_some_and(|repo| repo.worktree_root == identity.repository)
                })
                .map(|(scope, _)| *scope);
            if let Some(scope) = model.selected
                && let Some(entry) = model.entries.get_mut(&scope)
                && entry.target != identity.repository
            {
                entry.target = identity.repository.clone();
                entry.target_is_file = false;
                entry.sequence += 1;
                entry.busy = false;
            }
        } else if active
            .as_ref()
            .is_some_and(|resource| resource.local_path().is_some())
        {
            model.selected = None;
            model.offline = None;
        } else if let Some(meta) = app.workspace.active_meta()
            && meta.resource.environment().is_some()
            && model.selected.is_none()
        {
            model.offline = Some((
                meta.provider_root
                    .clone()
                    .unwrap_or_else(|| meta.resource.clone()),
                meta.name.clone(),
            ));
        }
    }
    if model
        .selected
        .is_some_and(|scope| !model.entries.contains_key(&scope))
    {
        model.selected = None;
    }
    if !had_entries
        && model.offline.is_none()
        && model.active.is_none()
        && app.explorer.model.roots().is_empty()
        && model.selected.is_none()
    {
        model.selected = model.entries.keys().next().copied();
    }
    if previous_selection != model.selected {
        clear_overlays(app);
    }
}

fn clear_overlays(app: &mut AppState) {
    app.git.ui.menu = None;
    app.git.ui.confirm = None;
    app.git.ui.branch_picker = None;
    app.git.ui.stash_switch = None;
    app.git.ui.clear_hover_cards();
}

pub(super) fn tick(context: &AppContext) {
    context.update(sync);
    let app = context.get();
    if app.git.resources.paused
        || (!app.source_control.visible
            && app
                .workspace
                .active_diff()
                .is_none_or(|diff| diff.provided.is_none()))
    {
        return;
    }
    for (scope, entry) in &app.git.resources.model.entries {
        if !entry.busy && entry.root.capabilities.contains(&Capability::Process) {
            refresh(context, *scope);
        }
    }
}

pub(super) fn start(context: AppContext) -> impl FnOnce() {
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let pending = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _ = std::thread::Builder::new()
        .name("loog-resource-git-poll".into())
        .spawn(move || {
            while matches!(
                stopped.recv_timeout(std::time::Duration::from_secs(2)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ) {
                let Some(application) = context.services().and_then(|r| r.application()) else {
                    break;
                };
                if pending.swap(true, Ordering::AcqRel) {
                    continue;
                }
                let next = context.clone();
                let pending = pending.clone();
                application.post(move || {
                    pending.store(false, Ordering::Release);
                    tick(&next);
                });
            }
        });
    move || {
        let _ = stop.send(());
    }
}

pub(super) fn refresh(context: &AppContext, scope: ScopeId) {
    let Some(application) = context.services().and_then(|r| r.application()) else {
        return;
    };
    let Some(permit) = permit() else {
        return;
    };
    let mut captured = None;
    context.update(|app| {
        if let Some(entry) = app.git.resources.model.entries.get_mut(&scope)
            && !entry.busy
            && entry.root.capabilities.contains(&Capability::Process)
        {
            entry.busy = true;
            entry.dirty = false;
            entry.sequence += 1;
            captured = Some(entry.clone());
        }
    });
    let Some(entry) = captured else {
        return;
    };
    let state = context.clone();
    let sequence = entry.sequence;
    let target = entry.target.clone();
    let app = context.get();
    let open_diff = app.workspace.active().and_then(|view| {
        app.workspace
            .diff_record(view)
            .filter(|record| record.content.provided.is_some())
            .map(|record| (view, record.content.clone()))
    });
    let spawned = std::thread::Builder::new()
        .name("loog-resource-git-status".into())
        .spawn(move || {
            let _permit = permit;
            let result = discover(&state, scope, &entry);
            let refreshed =
                if let (Ok(Some(repository)), Some((view, previous))) = (&result, open_diff) {
                    previous
                        .provided
                        .as_ref()
                        .filter(|identity| identity.repository == repository.worktree_root)
                        .map(|identity| {
                            let backend = resource_git::backend(
                                state.clone(),
                                scope,
                                entry.root.resource.clone(),
                            );
                            let diff = if repository.files.contains_key(&identity.path) {
                                backend.diff(repository, &identity.path, previous.target)
                            } else {
                                Ok(Default::default())
                            };
                            let next = diff.map(|diff| {
                                Arc::new(DiffDocument::from_resource(
                                    identity.clone(),
                                    previous.target,
                                    diff,
                                ))
                            });
                            (view, previous.clone(), next)
                        })
                } else {
                    None
                };
            let groups = result
                .as_ref()
                .ok()
                .and_then(Option::as_ref)
                .map(crate::features::git::resources::groups)
                .unwrap_or_default();
            application.post(move || {
                state.update(|app| {
                    if !app
                        .git
                        .resources
                        .model
                        .accepts(scope, sequence, &entry.target)
                    {
                        return;
                    }
                    let entry = app.git.resources.model.entries.get_mut(&scope).unwrap();
                    entry.busy = false;
                    entry.groups = groups;
                    match result {
                        Ok(repository) => {
                            entry.repository = repository.map(Arc::new);
                            entry.error = None;
                        }
                        Err(error) => {
                            entry.repository = None;
                            entry.error = Some(error.user_message());
                        }
                    }
                    if let Some((view, previous, next)) = refreshed {
                        match next {
                            Ok(next) => {
                                app.workspace.refresh_resource_diff(view, &previous, next);
                            }
                            Err(error) => {
                                app.git
                                    .resources
                                    .model
                                    .entries
                                    .get_mut(&scope)
                                    .unwrap()
                                    .error = Some(error.user_message());
                            }
                        }
                    }
                });
            });
        });
    if spawned.is_err() {
        finish_error(
            context,
            scope,
            sequence,
            &target,
            "Could not start Git worker",
        );
    }
}

fn discover(context: &AppContext, scope: ScopeId, entry: &Entry) -> GitResult<Option<Snapshot>> {
    let directory = if entry.target_is_file {
        super::resource_bytes::background_typed(
            context,
            scope,
            &entry.target,
            &|| false,
            |request, path| match request(Operation::ParentPath { path: path.clone() })
                .map_err(|e| e.message)?
            {
                Answer::Parent { path: Some(path) } => ResourceRef::provided(
                    entry.target.environment().unwrap().clone(),
                    path.as_str(),
                )
                .map_err(|e| e.to_string()),
                _ => Err("Git file has no accessible parent directory".into()),
            },
        )
        .map_err(|message| GitError::new(GitErrorKind::Io, message))?
    } else {
        entry.target.clone()
    };
    resource_git::backend(context.clone(), scope, entry.root.resource.clone())
        .discover(&directory, entry.sequence)
}

fn finish_error(
    context: &AppContext,
    scope: ScopeId,
    sequence: u64,
    target: &ResourceRef,
    error: &str,
) {
    context.update(|app| {
        if app.git.resources.model.accepts(scope, sequence, target) {
            let entry = app.git.resources.model.entries.get_mut(&scope).unwrap();
            entry.busy = false;
            entry.error = Some(error.into());
        }
    });
}

use crate::features::git::resources::Intent as Command;
pub(crate) fn run(context: &AppContext, command: Command) {
    match command {
        Command::Select(scope) => context.update(|app| {
            app.git.resources.model.selected = scope;
            app.git.resources.model.offline = None;
            app.git.resources.scroll = 0.0;
            clear_overlays(app);
        }),
        Command::Refresh(scope) => refresh(context, scope),
        Command::Diff(scope, path, target) => open_diff(context, scope, path, target),
        Command::Stage(scope, paths, unstage) => mutate(
            context,
            scope,
            move |backend, root| backend.stage(root, &paths, unstage),
            None,
        ),
        Command::Commit(scope) => {
            let app = context.get();
            let message = app
                .git
                .resources
                .model
                .entries
                .get(&scope)
                .and_then(|entry| app.git.resources.messages.get(&entry.root.resource))
                .map(|input| input.text().to_owned())
                .unwrap_or_default();
            if message.trim().is_empty() {
                return;
            }
            let captured = message.clone();
            mutate(
                context,
                scope,
                move |backend, root| backend.commit(root, &captured, false, false).map(|_| ()),
                Some(message),
            );
        }
    }
}

fn mutate(
    context: &AppContext,
    scope: ScopeId,
    work: impl FnOnce(&crate::infrastructure::git::resource::Backend, &ResourceRef) -> GitResult<()>
    + Send
    + 'static,
    message: Option<String>,
) {
    let Some(application) = context.services().and_then(|r| r.application()) else {
        return;
    };
    let Some(permit) = permit() else {
        return;
    };
    let Some(entry) = context
        .get()
        .git
        .resources
        .model
        .entries
        .get(&scope)
        .filter(|e| !e.busy)
        .cloned()
    else {
        return;
    };
    let Some(repository) = entry.repository.clone() else {
        return;
    };
    let sequence = entry.sequence + 1;
    context.update(|app| {
        let entry = app.git.resources.model.entries.get_mut(&scope).unwrap();
        entry.sequence = sequence;
        entry.busy = true;
        entry.error = None;
    });
    let state = context.clone();
    let target = entry.target.clone();
    let spawned = std::thread::Builder::new()
        .name("loog-resource-git-write".into())
        .spawn(move || {
            let _permit = permit;
            let backend = resource_git::backend(state.clone(), scope, entry.root.resource);
            let result = WriteGuard::acquire(&repository.worktree_root)
                .and_then(|_guard| work(&backend, &repository.worktree_root));
            application.post(move || {
                state.update(|app| {
                    if !app
                        .git
                        .resources
                        .model
                        .accepts(scope, sequence, &entry.target)
                    {
                        return;
                    }
                    let entry = app.git.resources.model.entries.get_mut(&scope).unwrap();
                    entry.busy = false;
                    entry.dirty = true;
                    if let Err(error) = &result {
                        entry.error = Some(error.user_message());
                        app.show_error(error.user_message());
                    } else if let Some(message) = message
                        && let Some(input) =
                            app.git.resources.messages.get_mut(&entry.root.resource)
                        && input.text() == message
                    {
                        *input = Default::default();
                    }
                });
                if result.is_ok() {
                    refresh(&state, scope);
                }
            });
        });
    if spawned.is_err() {
        finish_error(
            context,
            scope,
            sequence,
            &target,
            "Could not start Git worker",
        );
    }
}

fn open_diff(context: &AppContext, scope: ScopeId, path: GitPath, target: DiffTarget) {
    let Some(application) = context.services().and_then(|r| r.application()) else {
        return;
    };
    let Some(permit) = permit() else {
        return;
    };
    let app = context.get();
    let Some(entry) = app.git.resources.model.entries.get(&scope).cloned() else {
        return;
    };
    let Some(repository) = entry.repository.clone() else {
        return;
    };
    let pane = app.workspace.active_pane();
    let view = app.workspace.active();
    let mut opening = 0;
    context.update(|app| {
        app.git.resources.model.opening += 1;
        opening = app.git.resources.model.opening;
    });
    let state = context.clone();
    let spawned = std::thread::Builder::new()
        .name("loog-resource-git-diff".into())
        .spawn(move || {
            let _permit = permit;
            let backend = resource_git::backend(state.clone(), scope, entry.root.resource);
            let result = (|| {
                let diff = backend.diff(&repository, &path, target)?;
                let file = backend.file_resource(&repository, &path)?;
                Ok::<_, GitError>(Arc::new(DiffDocument::from_resource(
                    ResourceDiffIdentity {
                        repository: repository.worktree_root.clone(),
                        path,
                        file,
                    },
                    target,
                    diff,
                )))
            })();
            application.post(move || {
                state.update(|app| {
                    if app.git.resources.model.opening != opening
                        || app.workspace.active_pane() != pane
                        || app.workspace.active() != view
                        || !app
                            .git
                            .resources
                            .model
                            .entries
                            .get(&scope)
                            .is_some_and(|e| {
                                e.repository
                                    .as_ref()
                                    .is_some_and(|r| r.worktree_root == repository.worktree_root)
                            })
                    {
                        return;
                    }
                    match result {
                        Ok(document) => {
                            app.workspace.open_prepared_diff(document);
                        }
                        Err(error) => app.show_error(error.user_message()),
                    }
                })
            });
        });
    if spawned.is_err() {
        context.update(|app| app.show_error("Could not start Git diff worker"));
    }
}

pub(super) fn open_file(context: &AppContext, resource: ResourceRef) -> bool {
    let app = context.get();
    let Some(identity) = app
        .workspace
        .active_diff()
        .and_then(|d| d.provided.as_ref())
    else {
        return false;
    };
    if identity.file != resource {
        return false;
    }
    let Some(scope) = app
        .git
        .resources
        .model
        .entries
        .iter()
        .find(|(_, e)| {
            e.repository
                .as_ref()
                .is_some_and(|r| r.worktree_root == identity.repository)
        })
        .map(|(scope, _)| *scope)
    else {
        return false;
    };
    let Some(host) = context.services().and_then(|r| r.extensions()) else {
        return false;
    };
    let ResourceRef::Provided { path, .. } = resource else {
        return false;
    };
    let state = context.clone();
    let view = app.workspace.active();
    host.request_resource(
        scope,
        Operation::Stat { path },
        Box::new(move |app, result| {
            if app.workspace.active() != view {
                return;
            }
            match result {
                Ok(Answer::Info { file }) => {
                    let next = state.clone();
                    if let Some(application) = state.services().and_then(|r| r.application()) {
                        application.post(move || super::resources::open(&next, scope, file));
                    }
                }
                Err(error) => app.show_error(error.message),
                _ => app.show_error("Invalid Git file metadata"),
            }
        }),
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnected_git_selection_stays_offline_until_reconnected_or_explicitly_changed() {
        let root = |scope: ScopeId, environment: &str| loog_workspace_api::PresentedRoot {
            scope,
            resource: ResourceRef::provided(
                loog_workspace_api::EnvironmentKey::new("fixture", environment).unwrap(),
                "opaque:root",
            )
            .unwrap(),
            name: environment.into(),
            capabilities: vec![Capability::FilesRead, Capability::Process],
            badge: None,
        };
        let (first, second) = (root(1, "first"), root(2, "second"));
        let mut app = AppState::new();
        app.explorer
            .resources
            .sync(vec![first.clone(), second.clone()]);
        sync(&mut app);
        app.git.resources.model.selected = Some(1);
        app.git
            .resources
            .messages
            .get_mut(&first.resource)
            .unwrap()
            .set_text("keep this draft");
        app.explorer.resources.sync(vec![second.clone()]);
        sync(&mut app);
        sync(&mut app);
        assert_eq!(app.git.resources.model.selected, None);
        assert_eq!(
            app.git.resources.model.offline.as_ref().unwrap().0,
            first.resource
        );
        let mut reconnected = first.clone();
        reconnected.scope = 3;
        app.explorer.resources.sync(vec![second, reconnected]);
        sync(&mut app);
        assert_eq!(app.git.resources.model.selected, Some(3));
        assert!(app.git.resources.model.offline.is_none());
        assert_eq!(
            app.git.resources.messages[&first.resource].text(),
            "keep this draft"
        );

        let context = crate::app::test_support::live(app);
        run(&context, Command::Select(None));
        context.update(sync);
        assert!(context.get().git.resources.model.selected.is_none());
        assert!(context.get().git.resources.model.offline.is_none());
    }
}
