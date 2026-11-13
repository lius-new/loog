//! Asynchronous EditorConfig preparation over generic resource operations.
use super::{context::AppContext, state::AppState};
use loog_editor_core::text::FileOptions;
use loog_workspace_api::{
    Answer, Failure, FailureKind, Operation, ProviderPath, ResourceRef, ScopeId,
};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

enum Step {
    Parent,
    Child,
    Read,
    Relative(String),
    Done,
}
struct Load {
    root: ProviderPath,
    file: ProviderPath,
    directory: Option<ProviderPath>,
    seen: BTreeSet<ProviderPath>,
    operation: Operation,
    step: Step,
    sources: Vec<(String, String)>,
    bytes: usize,
    paths: Arc<Mutex<BTreeSet<ProviderPath>>>,
}
impl Load {
    fn new(root: ProviderPath, file: ProviderPath) -> Self {
        Self {
            root,
            operation: Operation::ParentPath { path: file.clone() },
            file,
            directory: None,
            seen: BTreeSet::new(),
            step: Step::Parent,
            sources: vec![],
            bytes: 0,
            paths: Arc::default(),
        }
    }
    fn parent(&mut self) -> Option<FileOptions> {
        let directory = self.directory.as_ref().unwrap();
        if directory == &self.root {
            return Some(self.finish());
        }
        self.operation = Operation::ParentPath {
            path: directory.clone(),
        };
        self.step = Step::Parent;
        None
    }
    fn finish(&mut self) -> FileOptions {
        self.step = Step::Done;
        crate::infrastructure::editorconfig::from_sources(
            std::mem::take(&mut self.sources).into_iter().rev(),
        )
    }
    fn receive(&mut self, result: Result<Answer, Failure>) -> Result<Option<FileOptions>, String> {
        if matches!(self.step, Step::Done) {
            return Err("EditorConfig read already ended".into());
        }
        let answer = match result {
            Ok(answer) => answer,
            Err(error)
                if error.kind == FailureKind::NotFound
                    && matches!(self.step, Step::Child | Step::Read) =>
            {
                return Ok(self.parent());
            }
            Err(error) => return Err(error.message),
        };
        match (&self.step, answer) {
            (Step::Parent, Answer::Parent { path: None }) => Ok(Some(self.finish())),
            (Step::Parent, Answer::Parent { path: Some(path) }) => {
                if self.seen.len() >= 64 || !self.seen.insert(path.clone()) {
                    return Err("EditorConfig ancestor traversal exceeded its limit".into());
                }
                self.directory = Some(path.clone());
                self.operation = Operation::ChildPath {
                    parent: path,
                    name: ".editorconfig".into(),
                };
                self.step = Step::Child;
                Ok(None)
            }
            (Step::Child, Answer::Path { path }) => {
                let mut paths = self.paths.lock().unwrap();
                if paths.iter().map(|path| path.as_str().len()).sum::<usize>() + path.as_str().len()
                    > 128 * 1024
                {
                    return Err("EditorConfig dependency paths exceed their budget".into());
                }
                paths.insert(path.clone());
                self.operation = Operation::Read {
                    path,
                    offset: 0,
                    limit: 64 * 1024,
                };
                self.step = Step::Read;
                Ok(None)
            }
            (Step::Read, Answer::Bytes { bytes, eof, .. }) => {
                self.bytes += bytes.len();
                if !eof || bytes.len() > 64 * 1024 || self.bytes > 512 * 1024 {
                    return Err("EditorConfig sources exceed their size budget".into());
                }
                let text = String::from_utf8(bytes).map_err(|_| "EditorConfig is not UTF-8")?;
                self.operation = Operation::RelativePath {
                    base: self.directory.clone().unwrap(),
                    path: self.file.clone(),
                };
                self.step = Step::Relative(text);
                Ok(None)
            }
            (
                Step::Relative(text),
                Answer::RelativePath {
                    components: Some(parts),
                },
            ) => {
                if parts.is_empty()
                    || parts.len() > 128
                    || parts.iter().map(String::len).sum::<usize>() > 32 * 1024
                    || parts.iter().any(|part| {
                        part.is_empty()
                            || matches!(part.as_str(), "." | "..")
                            || part.contains(['/', '\0'])
                    })
                {
                    return Err("Invalid provider-relative EditorConfig path".into());
                }
                let root = crate::infrastructure::editorconfig::is_root(text);
                self.sources.push((text.clone(), parts.join("/")));
                if root {
                    Ok(Some(self.finish()))
                } else {
                    Ok(self.parent())
                }
            }
            _ => Err("Unexpected EditorConfig resource response".into()),
        }
    }
}

type Ready =
    Box<dyn FnOnce(&mut AppState, Result<FileOptions, String>, BTreeSet<ProviderPath>) + Send>;
type Callback = Box<dyn FnOnce(&mut AppState, Result<FileOptions, String>) + Send>;
struct Request {
    load: Load,
    deadline: Instant,
    ready: Arc<Mutex<Option<Callback>>>,
    pending: Arc<Mutex<Option<loog_workspace_api::RequestId>>>,
    _timer: super::ui_timer::Timer,
}
fn complete(
    app: &mut AppState,
    ready: &Arc<Mutex<Option<Callback>>>,
    result: Result<FileOptions, String>,
) {
    let ready = ready.lock().unwrap().take();
    if let Some(ready) = ready {
        ready(app, result);
    }
}
pub(super) fn read(
    app: &mut AppState,
    state: &AppContext,
    scope: ScopeId,
    resource: &ResourceRef,
    ready: Ready,
) {
    let paths = Arc::<Mutex<BTreeSet<ProviderPath>>>::default();
    let observed = paths.clone();
    let ready: Callback = Box::new(move |app, result| {
        let paths = observed.lock().unwrap().clone();
        ready(app, result, paths);
    });
    let Some(root) = app.explorer.resources.root(scope) else {
        ready(app, Err("Resource root is unavailable".into()));
        return;
    };
    let (
        ResourceRef::Provided {
            path: file,
            environment,
        },
        ResourceRef::Provided {
            path: root,
            environment: owner,
        },
    ) = (resource, &root.descriptor.resource)
    else {
        ready(app, Err("Expected a provided EditorConfig resource".into()));
        return;
    };
    if environment != owner {
        ready(
            app,
            Err("EditorConfig resource belongs to another environment".into()),
        );
        return;
    }
    let mut load = Load::new(root.clone(), file.clone());
    load.paths = paths;
    let Some(application) = state.services().and_then(|runtime| runtime.application()) else {
        ready(app, Err("Resource runtime is unavailable".into()));
        return;
    };
    let ready = Arc::new(Mutex::new(Some(ready)));
    let pending = Arc::new(Mutex::new(None));
    let expired = ready.clone();
    let cancel = pending.clone();
    let context = state.clone();
    let timer = super::ui_timer::after(application, Duration::from_secs(15), move || {
        context.update(|app| {
            if let Some(request) = cancel.lock().unwrap().take()
                && let Some(host) = context.services().and_then(|runtime| runtime.extensions())
            {
                host.cancel_resource(request);
            }
            complete(
                app,
                &expired,
                Err("EditorConfig preparation timed out".into()),
            );
        });
    });
    let request = Request {
        load,
        deadline: Instant::now() + Duration::from_secs(15),
        ready,
        pending,
        _timer: timer,
    };
    advance(app, state.clone(), scope, request);
}

#[derive(Clone, Default)]
pub struct State {
    entries: HashMap<ResourceRef, Tracked>,
    generation: u64,
}
#[derive(Clone)]
struct Tracked {
    scope: ScopeId,
    incarnation: u64,
    paths: BTreeSet<ProviderPath>,
    dirty: bool,
    busy: bool,
    generation: u64,
}
pub(super) fn remember(
    app: &mut AppState,
    scope: ScopeId,
    resource: ResourceRef,
    paths: BTreeSet<ProviderPath>,
) {
    let Some(id) = app.workspace.file_id_for_resource(&resource) else {
        return;
    };
    let incarnation = app.workspace.documents[&id].incarnation;
    let state = &mut app.editor.editing.resource_options;
    state.entries.retain(|resource, tracked| {
        app.workspace
            .file_id_for_resource(resource)
            .is_some_and(|id| app.workspace.documents[&id].incarnation == tracked.incarnation)
    });
    let bytes = state
        .entries
        .iter()
        .filter(|(key, _)| *key != &resource)
        .flat_map(|(_, tracked)| &tracked.paths)
        .chain(&paths)
        .map(|path| path.as_str().len())
        .sum::<usize>();
    if (!state.entries.contains_key(&resource) && state.entries.len() >= 128)
        || bytes > 2 * 1024 * 1024
    {
        app.show_error("EditorConfig monitoring exceeded its document or path budget");
        return;
    }
    state.generation += 1;
    state.entries.insert(
        resource,
        Tracked {
            scope,
            incarnation,
            paths,
            dirty: false,
            busy: false,
            generation: state.generation,
        },
    );
}
pub(super) fn changed(
    app: &mut AppState,
    scope: ScopeId,
    paths: &HashSet<ProviderPath>,
    rescan: bool,
) {
    for tracked in app
        .editor
        .editing
        .resource_options
        .entries
        .values_mut()
        .filter(|tracked| tracked.scope == scope)
    {
        tracked.dirty |= rescan || tracked.paths.iter().any(|path| paths.contains(path));
    }
}
/// Refresh at most four documents at a time. A second event while a read is in
/// flight remains dirty and is reread after that response, never lost.
pub(super) fn refresh(state: &AppContext) {
    state.update(|app| drive(app, state));
}
fn drive(app: &mut AppState, state: &AppContext) {
    app.editor
        .editing
        .resource_options
        .entries
        .retain(|resource, tracked| {
            app.workspace
                .file_id_for_resource(resource)
                .is_some_and(|id| app.workspace.documents[&id].incarnation == tracked.incarnation)
                && app.explorer.resources.root(tracked.scope).is_some()
        });
    let entries = &mut app.editor.editing.resource_options.entries;
    let available = 4usize.saturating_sub(entries.values().filter(|tracked| tracked.busy).count());
    let mut next = Vec::new();
    for (resource, tracked) in entries
        .iter_mut()
        .filter(|(_, tracked)| tracked.dirty && !tracked.busy)
        .take(available)
    {
        tracked.dirty = false;
        tracked.busy = true;
        next.push((resource.clone(), tracked.clone()));
    }
    for (resource, tracked) in next {
        let context = state.clone();
        let target = resource.clone();
        read(
            app,
            state,
            tracked.scope,
            &resource,
            Box::new(move |app, result, paths| {
                let bytes = app
                    .editor
                    .editing
                    .resource_options
                    .entries
                    .iter()
                    .filter(|(key, _)| *key != &target)
                    .flat_map(|(_, entry)| &entry.paths)
                    .chain(&paths)
                    .map(|path| path.as_str().len())
                    .sum::<usize>();
                let Some(current) = app
                    .editor
                    .editing
                    .resource_options
                    .entries
                    .get_mut(&target)
                    .filter(|entry| {
                        entry.generation == tracked.generation && entry.scope == tracked.scope
                    })
                else {
                    return;
                };
                current.busy = false;
                if !paths.is_empty() && bytes <= 2 * 1024 * 1024 {
                    current.paths = paths;
                }
                let result = if bytes > 2 * 1024 * 1024 {
                    Err("EditorConfig monitoring exceeded its path budget".into())
                } else {
                    result
                };
                if let Some(id) = app.workspace.file_id_for_resource(&target)
                    && app.workspace.documents[&id].incarnation == tracked.incarnation
                {
                    match result {
                        Ok(options) => {
                            app.workspace
                                .documents
                                .get_mut(&id)
                                .unwrap()
                                .buffer
                                .initialize_file_options(options);
                            super::editing::configure_all(app);
                        }
                        Err(error) => app.show_error(format!(
                            "Could not refresh resource editing options: {error}"
                        )),
                    }
                }
                // Continue on a later turn even if request admission was rejected.
                if let Some(application) =
                    context.services().and_then(|runtime| runtime.application())
                {
                    application.post(move || refresh(&context));
                }
            }),
        );
    }
}
fn advance(app: &mut AppState, state: AppContext, scope: ScopeId, mut request: Request) {
    if request.ready.lock().unwrap().is_none() {
        return;
    }
    let Some(host) = state.services().and_then(|runtime| runtime.extensions()) else {
        complete(
            app,
            &request.ready,
            Err("Workspace provider is unavailable".into()),
        );
        return;
    };
    if Instant::now() >= request.deadline || app.explorer.resources.root(scope).is_none() {
        complete(
            app,
            &request.ready,
            Err("EditorConfig preparation expired or its root closed".into()),
        );
        return;
    }
    let ready = request.ready.clone();
    let pending = request.pending.clone();
    let result = host.request_resource(
        scope,
        request.load.operation.clone(),
        Box::new(move |app, result| {
            request.pending.lock().unwrap().take();
            if request.ready.lock().unwrap().is_none() {
                return;
            }
            if Instant::now() >= request.deadline || app.explorer.resources.root(scope).is_none() {
                complete(
                    app,
                    &request.ready,
                    Err("EditorConfig preparation expired or its root closed".into()),
                );
                return;
            }
            match request.load.receive(result) {
                Ok(Some(options)) => complete(app, &request.ready, Ok(options)),
                Ok(None) => advance(app, state, scope, request),
                Err(error) => complete(app, &request.ready, Err(error)),
            }
        }),
    );
    match result {
        Ok(id) => *pending.lock().unwrap() = Some(id),
        Err(error) => complete(app, &ready, Err(error.message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn path(value: &str) -> ProviderPath {
        ProviderPath::new(value).unwrap()
    }
    fn source(load: &mut Load, directory: &str, text: &str, parts: &[&str]) -> Option<FileOptions> {
        assert!(
            load.receive(Ok(Answer::Parent {
                path: Some(path(directory))
            }))
            .unwrap()
            .is_none()
        );
        assert!(
            load.receive(Ok(Answer::Path {
                path: path("opaque:configuration")
            }))
            .unwrap()
            .is_none()
        );
        assert!(
            load.receive(Ok(Answer::Bytes {
                bytes: text.as_bytes().to_vec(),
                revision: Some("v1".into()),
                eof: true
            }))
            .unwrap()
            .is_none()
        );
        load.receive(Ok(Answer::RelativePath {
            components: Some(parts.iter().map(|part| (*part).into()).collect()),
        }))
        .unwrap()
    }
    #[test]
    fn provider_names_and_ancestor_precedence_preserve_target_backslashes() {
        let mut load = Load::new(path("opaque:root"), path("opaque:file"));
        assert!(
            source(
                &mut load,
                "opaque:child",
                "[*]\nindent_size=2\n",
                &["中文\\file.rs"]
            )
            .is_none()
        );
        let options = source(&mut load, "opaque:root", "\u{feff}root=true\n[*]\nindent_style=space\nindent_size=4\n[child/中文\\\\file.rs]\ntab_width=7\n", &["child", "中文\\file.rs"]).unwrap();
        assert_eq!(options.indent_size, Some(2));
        assert_eq!(options.tab_width, Some(7));
        assert_eq!(options.use_tabs, Some(false));
    }
    #[test]
    fn missing_configurations_walk_but_denied_oversized_and_cyclic_reads_fail() {
        let mut load = Load::new(path("root"), path("file"));
        load.receive(Ok(Answer::Parent {
            path: Some(path("child")),
        }))
        .unwrap();
        assert!(
            load.receive(Err(Failure::new(FailureKind::NotFound, "absent")))
                .unwrap()
                .is_none()
        );
        assert!(
            load.receive(Ok(Answer::Parent {
                path: Some(path("child"))
            }))
            .is_err()
        );
        let mut load = Load::new(path("root"), path("file"));
        load.receive(Ok(Answer::Parent {
            path: Some(path("root")),
        }))
        .unwrap();
        assert!(
            load.receive(Err(Failure::new(FailureKind::PermissionDenied, "denied")))
                .is_err()
        );
        load.receive(Ok(Answer::Path {
            path: path("config"),
        }))
        .unwrap();
        assert!(
            load.receive(Ok(Answer::Bytes {
                bytes: vec![],
                revision: None,
                eof: false
            }))
            .is_err()
        );
    }
}
