//! Workspace settings over generic resource capabilities. Paths remain opaque.
use super::{context::AppContext, state::AppState};
use crate::features::settings::{project::ProjectSettings, rules};
use loog_workspace_api::{
    Answer, Failure, FailureKind, Operation, ProviderPath, ResourceRef, ScopeId,
};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

const FILE_LIMIT: usize = 256 * 1024;
const CACHE_LIMIT: usize = 16 * 1024 * 1024;
static NEXT: AtomicU64 = AtomicU64::new(1);
static READERS: AtomicUsize = AtomicUsize::new(0);
struct Permit;
impl Drop for Permit {
    fn drop(&mut self) {
        READERS.fetch_sub(1, Ordering::AcqRel);
    }
}
#[derive(Clone, Default)]
pub struct State {
    entries: HashMap<ResourceRef, Entry>,
}
#[derive(Clone)]
struct Entry {
    scope: ScopeId,
    session: u64,
    generation: u64,
    settings: ProjectSettings,
    paths: Vec<ProviderPath>,
    bytes: usize,
    dirty: bool,
    busy: bool,
    error: Option<String>,
}
struct Loaded {
    settings: ProjectSettings,
    paths: Vec<ProviderPath>,
    bytes: usize,
}
type Ready = Box<dyn FnOnce(&mut AppState, Result<(), String>) + Send>;

pub(super) fn background_encoding(
    state: &AppContext,
    scope: ScopeId,
    root: &ResourceRef,
    cancelled: &dyn Fn() -> bool,
) -> Result<(crate::domain::encoding::EncodingSettings, Option<String>), String> {
    let user = state.get().settings();
    let cached = settings(&state.get(), root).encoding;
    super::resource_bytes::background_typed(state, scope, root, cancelled, |request, path| {
        Ok(match load(request, path) {
            Ok(loaded) => (
                rules::overlay(&user, &loaded.settings.overrides).encoding,
                None,
            ),
            Err(error) => (
                cached,
                Some(format!(
                    "Project settings: {error}; using the last valid settings"
                )),
            ),
        })
    })
}

pub(super) fn shell_program(
    shell: Option<crate::features::terminal::ShellKind>,
) -> Result<Option<String>, &'static str> {
    use crate::features::terminal::ShellKind;
    match shell {
        None | Some(ShellKind::System) => Ok(None),
        Some(ShellKind::Bash) => Ok(Some("bash".into())),
        Some(ShellKind::PowerShell) => Ok(Some("pwsh".into())),
        Some(ShellKind::Cmd) => Ok(Some("cmd.exe".into())),
        Some(ShellKind::Wsl(_) | ShellKind::Custom(_)) => {
            Err("Local terminal profiles cannot select a program in a resource environment")
        }
    }
}

/// Called only on a background worker. A missing directory/file is an empty
/// project layer; permission, protocol, encoding and syntax failures are errors.
fn load(
    request: &mut dyn FnMut(Operation) -> Result<Answer, Failure>,
    root: &ProviderPath,
) -> Result<Loaded, String> {
    let mut paths = Vec::new();
    let mut parent = root.clone();
    for name in [
        crate::features::settings::project::DIR_NAME,
        crate::features::settings::project::FILE_NAME,
    ] {
        match request(Operation::ChildPath {
            parent,
            name: name.into(),
        }) {
            Ok(Answer::Path { path }) => {
                paths.clear();
                paths.push(path.clone());
                parent = path;
            }
            Err(error) if error.kind == FailureKind::NotFound => {
                return Ok(Loaded {
                    settings: ProjectSettings::default(),
                    paths,
                    bytes: 0,
                });
            }
            Err(error) => return Err(error.message),
            _ => return Err("Invalid project settings path response".into()),
        }
    }
    let mut bytes = Vec::new();
    let mut revision = None;
    loop {
        let answer = request(Operation::Read {
            path: parent.clone(),
            offset: bytes.len() as u64,
            limit: 128 * 1024,
        });
        match answer {
            Err(error) if error.kind == FailureKind::NotFound && bytes.is_empty() => {
                return Ok(Loaded {
                    settings: ProjectSettings::default(),
                    paths,
                    bytes: 0,
                });
            }
            Err(error) => return Err(error.message),
            Ok(Answer::Bytes {
                bytes: chunk,
                eof,
                revision: actual,
            }) => {
                if chunk.len() > 128 * 1024
                    || bytes.len() + chunk.len() > FILE_LIMIT
                    || (!eof && chunk.is_empty())
                {
                    return Err("Project settings exceed 256 KiB or make no read progress".into());
                }
                if (!bytes.is_empty() && revision != actual) || (!eof && actual.is_none()) {
                    return Err("Project settings changed while reading".into());
                }
                revision = actual;
                bytes.extend(chunk);
                if eof {
                    break;
                }
            }
            _ => return Err("Invalid project settings read response".into()),
        }
    }
    let size = bytes.len();
    let text = String::from_utf8(bytes).map_err(|_| "Project settings are not UTF-8")?;
    let (overrides, errors) =
        rules::parse_overrides(text.trim_start_matches('\u{feff}'), rules::Scope::Project)?;
    Ok(Loaded {
        settings: ProjectSettings { overrides, errors },
        paths,
        bytes: size,
    })
}

pub(super) fn project<'a>(app: &'a AppState, root: &ResourceRef) -> Option<&'a ProjectSettings> {
    app.editor
        .editing
        .resource_project
        .entries
        .get(root)
        .map(|entry| &entry.settings)
}
pub(super) fn settings(app: &AppState, root: &ResourceRef) -> rules::Settings {
    let user = app.settings();
    project(app, root).map_or(user.clone(), |project| {
        rules::overlay(&user, &project.overrides)
    })
}

pub(super) fn read(app: &mut AppState, state: &AppContext, scope: ScopeId, ready: Ready) {
    let Some(root) = app
        .explorer
        .resources
        .root(scope)
        .map(|root| root.descriptor.resource.clone())
    else {
        ready(app, Err("Project settings root is disconnected".into()));
        return;
    };
    let Some(application) = state.services().and_then(|runtime| runtime.application()) else {
        ready(app, Err("Project settings runtime is unavailable".into()));
        return;
    };
    if READERS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            (count < 4).then_some(count + 1)
        })
        .is_err()
    {
        ready(
            app,
            Err("Project settings reader limit reached; retry the operation".into()),
        );
        return;
    }
    let permit = Permit;
    let session = app.explorer.resources.session;
    let generation = NEXT.fetch_add(1, Ordering::Relaxed);
    let context = state.clone();
    let ready = Arc::new(Mutex::new(Some(ready)));
    let callback = ready.clone();
    let result = std::thread::Builder::new()
        .name("resource-project-settings".into())
        .spawn(move || {
            let valid = || {
                let app = context.get();
                app.explorer.resources.session == session
                    && app
                        .explorer
                        .resources
                        .root(scope)
                        .is_some_and(|entry| entry.descriptor.resource == root)
            };
            let loaded =
                super::resource_bytes::background_typed(&context, scope, &root, &|| !valid(), load);
            drop(permit);
            application.post(move || {
                context.update(|app| {
                    let result = if app.explorer.resources.session == session
                        && app
                            .explorer
                            .resources
                            .root(scope)
                            .is_some_and(|entry| entry.descriptor.resource == root)
                    {
                        store(app, root, scope, session, generation, loaded)
                    } else {
                        Err("Project settings root changed while reading".into())
                    };
                    let ready = callback.lock().unwrap().take();
                    if let Some(ready) = ready {
                        ready(app, result);
                    }
                })
            });
        });
    if let Err(error) = result {
        let ready = ready.lock().unwrap().take();
        if let Some(ready) = ready {
            ready(app, Err(error.to_string()));
        }
    }
}
fn store(
    app: &mut AppState,
    root: ResourceRef,
    scope: ScopeId,
    session: u64,
    generation: u64,
    loaded: Result<Loaded, String>,
) -> Result<(), String> {
    let state = &mut app.editor.editing.resource_project;
    state.entries.retain(|resource, entry| {
        app.explorer
            .resources
            .roots
            .iter()
            .any(|r| &r.descriptor.resource == resource && r.descriptor.scope == entry.scope)
            && entry.session == session
    });
    if state
        .entries
        .get(&root)
        .is_some_and(|entry| entry.generation > generation)
    {
        return Ok(());
    }
    let total: usize = state
        .entries
        .iter()
        .filter(|(key, _)| *key != &root)
        .map(|(_, entry)| entry.bytes)
        .sum();
    let loaded = loaded.and_then(|mut loaded| {
        loaded.bytes += loaded
            .paths
            .iter()
            .map(|path| path.as_str().len())
            .sum::<usize>();
        if (!state.entries.contains_key(&root) && state.entries.len() >= 128)
            || total + loaded.bytes > CACHE_LIMIT
        {
            Err("Project settings cache exceeded its root or byte budget".into())
        } else {
            Ok(loaded)
        }
    });
    if !state.entries.contains_key(&root) && state.entries.len() >= 128 {
        return Err("Project settings root limit reached".into());
    }
    let entry = state.entries.entry(root.clone()).or_insert_with(|| Entry {
        scope,
        session,
        generation,
        settings: ProjectSettings::default(),
        paths: vec![],
        bytes: 0,
        dirty: false,
        busy: false,
        error: None,
    });
    entry.generation = generation;
    let failure = loaded.as_ref().err().cloned();
    let warning = match loaded {
        Ok(loaded) => {
            entry.settings = loaded.settings;
            entry.paths = loaded.paths;
            entry.bytes = loaded.bytes;
            entry.settings.errors.first().cloned()
        }
        Err(error) => Some(error),
    };
    let fresh = entry.error != warning;
    entry.error = warning.clone();
    if fresh && let Some(warning) = &warning {
        app.show_error(format!("{} project settings: {warning}", root.display()));
    }
    languages(app);
    super::editing::configure_all(app);
    failure.map_or(Ok(()), Err)
}
pub(super) fn changed(
    app: &mut AppState,
    scope: ScopeId,
    paths: &HashSet<ProviderPath>,
    rescan: bool,
) {
    for entry in app
        .editor
        .editing
        .resource_project
        .entries
        .values_mut()
        .filter(|entry| entry.scope == scope)
    {
        entry.dirty |=
            rescan || entry.paths.is_empty() || entry.paths.iter().any(|path| paths.contains(path));
    }
}
pub(super) fn refresh(state: &AppContext) {
    state.update(|app| {
        let entries = &mut app.editor.editing.resource_project.entries;
        entries.retain(|root, entry| {
            entry.session == app.explorer.resources.session
                && app
                    .explorer
                    .resources
                    .root(entry.scope)
                    .is_some_and(|r| &r.descriptor.resource == root)
        });
        let mut pending = Vec::new();
        for (root, entry) in entries
            .iter_mut()
            .filter(|(_, entry)| entry.dirty && !entry.busy)
            .take(2)
        {
            entry.dirty = false;
            entry.busy = true;
            pending.push((root.clone(), entry.scope, entry.session));
        }
        for (root, scope, session) in pending {
            let context = state.clone();
            read(
                app,
                state,
                scope,
                Box::new(move |app, _| {
                    if let Some(entry) = app
                        .editor
                        .editing
                        .resource_project
                        .entries
                        .get_mut(&root)
                        .filter(|entry| entry.scope == scope && entry.session == session)
                    {
                        entry.busy = false;
                    }
                    if let Some(application) =
                        context.services().and_then(|runtime| runtime.application())
                    {
                        application.post(move || refresh(&context));
                    }
                }),
            );
        }
    });
}

pub(super) fn languages(app: &mut AppState) -> bool {
    let registry = crate::domain::language::Registry::builtin();
    let user = app.preferences.values().file_associations.clone();
    let projects = &app.editor.editing.resource_project.entries;
    let mut changed = false;
    for document in app
        .workspace
        .documents
        .values_mut()
        .filter(|doc| doc.is_text() && doc.meta.resource.environment().is_some())
    {
        let project: Vec<rules::FileAssociation> = document
            .meta
            .provider_root
            .as_ref()
            .and_then(|root| projects.get(root))
            .and_then(|entry| entry.settings.overrides.get("file_associations"))
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();
        let language = resolve_language(
            &registry,
            &project,
            &user,
            &document.meta.name,
            document.meta.provider_relative.as_deref(),
        );
        let generation = document.meta.language_generation;
        document.meta.set_language(language);
        changed |= generation != document.meta.language_generation;
    }
    changed
}
pub(super) fn language(
    app: &AppState,
    root: &ResourceRef,
    name: &str,
    relative: Option<&[String]>,
) -> crate::domain::language::ResolvedLanguage {
    let project: Vec<rules::FileAssociation> = project(app, root)
        .and_then(|project| project.overrides.get("file_associations"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    resolve_language(
        &crate::domain::language::Registry::builtin(),
        &project,
        &app.preferences.values().file_associations,
        name,
        relative,
    )
}
fn resolve_language(
    registry: &crate::domain::language::Registry,
    project: &[rules::FileAssociation],
    user: &[rules::FileAssociation],
    name: &str,
    relative: Option<&[String]>,
) -> crate::domain::language::ResolvedLanguage {
    let parts: Vec<_> = relative
        .map(|parts| parts.iter().map(String::as_str).collect())
        .unwrap_or_else(|| vec![name]);
    project
        .iter()
        .chain(user)
        .find_map(|association| {
            let glob = crate::domain::language::Glob::parse(&association.pattern, false).ok()?;
            if (relative.is_none() && !glob.matches_without_folder()) || !glob.matches(&parts) {
                return None;
            }
            registry
                .language_by_name(&association.language)
                .map(|language| registry.default_resolved(language))
        })
        .unwrap_or_else(|| registry.resolve_filename(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn path(name: &str) -> ProviderPath {
        ProviderPath::new(name).unwrap()
    }
    fn read_text(text: &[u8]) -> Result<Loaded, String> {
        load(
            &mut |operation| {
                Ok(match operation {
                    Operation::ChildPath { parent, name } => {
                        assert_eq!(
                            (parent.as_str(), name.as_str()),
                            if name == ".loog" {
                                ("opaque:root", ".loog")
                            } else {
                                ("opaque:directory", "settings.json")
                            }
                        );
                        Answer::Path {
                            path: path(if name == ".loog" {
                                "opaque:directory"
                            } else {
                                "opaque:settings"
                            }),
                        }
                    }
                    Operation::Read { path, offset, .. } => {
                        assert_eq!(path.as_str(), "opaque:settings");
                        assert_eq!(offset, 0);
                        Answer::Bytes {
                            bytes: text.to_vec(),
                            eof: true,
                            revision: Some("first".into()),
                        }
                    }
                    _ => panic!("unexpected settings operation"),
                })
            },
            &path("opaque:root"),
        )
    }
    #[test]
    fn opaque_project_settings_use_jsonc_and_project_allowlist() {
        let loaded =
            read_text(b"// project\n{\"editor\":{\"indent_size\":3},\"theme\":\"ember\"}").unwrap();
        assert_eq!(loaded.settings.overrides["editor"]["indent_size"], 3);
        assert!(!loaded.settings.overrides.contains_key("theme"));
        assert_eq!(loaded.settings.errors.len(), 1);
        assert_eq!(loaded.paths, [path("opaque:settings")]);
        assert!(read_text(b"{bad json}").is_err());
        assert!(read_text(&[255]).is_err());
        assert!(read_text(&vec![b' '; FILE_LIMIT + 1]).is_err());
    }
    #[test]
    fn absent_settings_track_uncreated_directory_but_permission_errors_do_not_mean_empty() {
        for kind in [FailureKind::NotFound, FailureKind::PermissionDenied] {
            let mut step = 0;
            let result = load(
                &mut |_| {
                    step += 1;
                    if step == 1 {
                        Ok(Answer::Path {
                            path: path("opaque:uncreated-directory"),
                        })
                    } else {
                        Err(Failure::new(kind.clone(), "fixture"))
                    }
                },
                &path("opaque:root"),
            );
            if kind == FailureKind::NotFound {
                let loaded = result.unwrap();
                assert!(loaded.settings.overrides.is_empty());
                assert_eq!(loaded.paths, [path("opaque:uncreated-directory")]);
            } else {
                assert!(result.is_err());
            }
        }
    }
    #[test]
    fn associations_match_provider_components_and_project_precedes_user() {
        let project = vec![rules::FileAssociation {
            pattern: "src/*.cfg".into(),
            language: "json".into(),
        }];
        let user = vec![rules::FileAssociation {
            pattern: "*.cfg".into(),
            language: "rust".into(),
        }];
        let registry = crate::domain::language::Registry::builtin();
        let name = "目录\\文件.cfg";
        assert_eq!(
            resolve_language(
                &registry,
                &project,
                &user,
                name,
                Some(&["src".into(), name.into()])
            )
            .name(),
            "json"
        );
        assert_eq!(
            resolve_language(
                &registry,
                &project,
                &user,
                name,
                Some(&["other".into(), name.into()])
            )
            .name(),
            "rust"
        );
        assert_eq!(
            resolve_language(&registry, &project, &user, name, None).name(),
            "rust"
        );
    }
    #[test]
    fn shell_selection_never_expands_local_profiles_in_a_provider() {
        use crate::features::terminal::ShellKind;
        assert_eq!(shell_program(None).unwrap(), None);
        assert_eq!(
            shell_program(Some(ShellKind::Bash)).unwrap().as_deref(),
            Some("bash")
        );
        assert!(shell_program(Some(ShellKind::Wsl(1))).is_err());
        assert!(shell_program(Some(ShellKind::Custom(1))).is_err());
    }
    #[test]
    fn same_path_settings_are_isolated_by_environment_and_older_reads_cannot_replace_newer() {
        let root = |environment| {
            ResourceRef::provided(
                loog_workspace_api::EnvironmentKey::new("fixture", environment).unwrap(),
                "opaque:root",
            )
            .unwrap()
        };
        let (first, second) = (root("first"), root("second"));
        let mut app = AppState::new();
        app.explorer.resources.sync(
            vec![(first.clone(), 1), (second.clone(), 2)]
                .into_iter()
                .map(|(resource, scope)| loog_workspace_api::PresentedRoot {
                    scope,
                    resource,
                    name: "Project".into(),
                    capabilities: vec![],
                    badge: None,
                })
                .collect(),
        );
        store(
            &mut app,
            first.clone(),
            1,
            0,
            2,
            read_text(br#"{"editor":{"indent_size":3}}"#),
        )
        .unwrap();
        store(
            &mut app,
            second.clone(),
            2,
            0,
            3,
            read_text(br#"{"editor":{"indent_size":7}}"#),
        )
        .unwrap();
        store(
            &mut app,
            first.clone(),
            1,
            0,
            1,
            read_text(br#"{"editor":{"indent_size":9}}"#),
        )
        .unwrap();
        assert_eq!(settings(&app, &first).editor.indent_size, 3);
        assert_eq!(settings(&app, &second).editor.indent_size, 7);
        assert!(
            store(
                &mut app,
                first.clone(),
                1,
                0,
                4,
                Err("invalid changed config".into())
            )
            .is_err()
        );
        assert_eq!(settings(&app, &first).editor.indent_size, 3);
        assert_eq!(settings(&app, &second).editor.indent_size, 7);
        store(&mut app, first.clone(), 1, 0, 5, read_text(b"{}")).unwrap();
        assert_eq!(
            settings(&app, &first).editor.indent_size,
            app.settings().editor.indent_size
        );
        assert_eq!(settings(&app, &second).editor.indent_size, 7);
    }
}
