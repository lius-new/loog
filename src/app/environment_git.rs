//! Git for repositories in provider environments, served by the environment's own
//! process and file abilities. The Git code asks for a command or a file by environment
//! path (see `features::git::location`); this finds the connected root that can answer
//! and forwards the request. Nothing here names a kind of environment.
use super::{context::AppContext, resource_bytes, resource_process};
use crate::features::git::error::{GitError, GitErrorKind, GitResult};
use crate::infrastructure::git::command::{CancellationToken, GitCommand, GitOutput};
use crate::infrastructure::git::remote::{self, RemoteGit, RemoteStat, RemoteTarget};
use loog_workspace_api::{
    Answer, Capability, EnvironmentKey, Failure, FailureKind, FileKind, Operation, ProviderPath,
    ResourceRef, ScopeId,
};
use std::sync::Arc;
use std::time::Duration;

/// How long one Git command may run in an environment unless the command says.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

pub(crate) struct EnvironmentGit {
    context: AppContext,
}

/// Serve Git for provider environments until the returned closure is called.
pub(crate) fn install(context: AppContext) -> impl FnOnce() {
    remote::install(Arc::new(EnvironmentGit { context }));
    remote::uninstall
}

impl EnvironmentGit {
    /// The scope of a connected root of `environment`, one that may run processes when
    /// `process` is asked for.
    fn scope(&self, environment: &EnvironmentKey, process: bool) -> GitResult<ScopeId> {
        let app = self.context.get();
        app.explorer
            .resources
            .roots
            .iter()
            .find(|root| {
                root.descriptor.resource.environment() == Some(environment)
                    && (!process || root.descriptor.capabilities.contains(&Capability::Process))
            })
            .map(|root| root.descriptor.scope)
            .ok_or_else(|| {
                GitError::new(
                    if process {
                        GitErrorKind::PermissionDenied
                    } else {
                        GitErrorKind::RuntimeUnavailable
                    },
                    if process {
                        "Git cannot run in this folder: it is not trusted, or its environment cannot run programs."
                    } else {
                        "The environment of this repository is not connected."
                    },
                )
            })
    }

    fn resource(target: &RemoteTarget) -> GitResult<ResourceRef> {
        ResourceRef::provided(target.environment.clone(), target.path.as_str())
            .map_err(|error| GitError::new(GitErrorKind::Unsupported, error.to_string()))
    }

    /// Run `work` against the environment's file operations for `target`.
    fn files<T>(
        &self,
        target: &RemoteTarget,
        work: impl FnOnce(
            &mut dyn FnMut(Operation) -> Result<Answer, Failure>,
            &ProviderPath,
        ) -> Result<T, Failure>,
    ) -> GitResult<T> {
        let scope = self.scope(&target.environment, false)?;
        let resource = Self::resource(target)?;
        resource_bytes::background_typed(
            &self.context,
            scope,
            &resource,
            &|| false,
            |request, path| Ok(work(request, path)),
        )
        .map_err(|error| GitError::new(GitErrorKind::RuntimeUnavailable, error))?
        .map_err(failure)
    }
}

fn failure(failure: Failure) -> GitError {
    let kind = match failure.kind {
        FailureKind::Cancelled => GitErrorKind::Cancelled,
        FailureKind::UnknownOutcome => GitErrorKind::UnknownOutcome,
        FailureKind::PermissionDenied => GitErrorKind::PermissionDenied,
        FailureKind::Unavailable => GitErrorKind::RuntimeUnavailable,
        FailureKind::Unsupported | FailureKind::LimitExceeded => GitErrorKind::Unsupported,
        _ => GitErrorKind::Other,
    };
    GitError::new(kind, failure.message)
}

impl RemoteGit for EnvironmentGit {
    fn run(
        &self,
        target: &RemoteTarget,
        command: &GitCommand,
        cancellation: &CancellationToken,
    ) -> GitResult<GitOutput> {
        let scope = self.scope(&target.environment, true)?;
        let output = resource_process::run(
            &self.context,
            scope,
            resource_process::Command {
                program: "git".into(),
                args: remote::arguments(command)?,
                cwd: Self::resource(target)?,
                env: remote::environment_variables(command),
                input: command.stdin.clone(),
                timeout: command.timeout.unwrap_or(DEFAULT_TIMEOUT),
            },
            &|| cancellation.is_cancelled(),
        )
        .map_err(failure)?;
        if output.code != Some(0)
            && !output
                .code
                .is_some_and(|code| command.accepted_exit_codes.contains(&code))
        {
            return Err(GitError::from_stderr(&output.stderr, output.code));
        }
        Ok(GitOutput {
            code: output.code,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    fn read_file(&self, target: &RemoteTarget, limit: usize) -> GitResult<Option<Vec<u8>>> {
        self.files(target, |request, path| {
            let file = match request(Operation::Stat { path: path.clone() }) {
                Ok(Answer::Info { file }) => file,
                Err(error) if error.kind == FailureKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
                _ => {
                    return Err(Failure::new(
                        FailureKind::InvalidRequest,
                        "Invalid file metadata",
                    ));
                }
            };
            if file.path != *path || file.kind != FileKind::File || file.size > limit as u64 {
                return Err(Failure::new(
                    FailureKind::Unsupported,
                    "Git reads a regular file within its size budget",
                ));
            }
            resource_bytes::read_file(
                &mut |operation| request(operation).map_err(|error| error.message),
                file.path,
                file.revision,
                file.size,
                limit,
            )
            .map(Some)
            .map_err(|error| Failure::new(FailureKind::Failed, error))
        })
    }

    fn stat(&self, target: &RemoteTarget) -> GitResult<Option<RemoteStat>> {
        self.files(target, |request, path| {
            match request(Operation::Stat { path: path.clone() }) {
                Ok(Answer::Info { file }) => Ok(Some(RemoteStat {
                    directory: file.kind == FileKind::Directory,
                    file: file.kind == FileKind::File,
                    size: file.size,
                })),
                Err(error) if error.kind == FailureKind::NotFound => Ok(None),
                Err(error) => Err(error),
                _ => Err(Failure::new(
                    FailureKind::InvalidRequest,
                    "Invalid file metadata",
                )),
            }
        })
    }

    fn list_names(&self, target: &RemoteTarget, limit: usize) -> GitResult<Vec<String>> {
        self.list_matching(target, limit, |_| true)
    }

    fn list_directories(&self, target: &RemoteTarget, limit: usize) -> GitResult<Vec<String>> {
        self.list_matching(target, limit, |kind| kind == FileKind::Directory)
    }
}

impl EnvironmentGit {
    fn list_matching(
        &self,
        target: &RemoteTarget,
        limit: usize,
        keep: impl Fn(FileKind) -> bool,
    ) -> GitResult<Vec<String>> {
        self.files(target, |request, path| {
            let mut names = Vec::new();
            let mut cursor = None;
            loop {
                let Answer::Directory {
                    entries,
                    cursor: next,
                } = request(Operation::List {
                    path: path.clone(),
                    cursor: cursor.take(),
                    limit: 256,
                })?
                else {
                    return Err(Failure::new(
                        FailureKind::InvalidRequest,
                        "Invalid directory listing",
                    ));
                };
                names.extend(
                    entries
                        .into_iter()
                        .filter(|entry| keep(entry.kind.clone()))
                        .map(|entry| entry.name),
                );
                match next {
                    Some(next) if names.len() < limit => cursor = Some(next),
                    _ => break,
                }
            }
            names.truncate(limit);
            Ok(names)
        })
    }
}

/// Open a file of an environment in the editor, as the Explorer does: look it up, then
/// open it by the scope of the connected root it belongs to.
pub(crate) fn open_file(context: &AppContext, resource: ResourceRef) -> bool {
    let ResourceRef::Provided { environment, path } = resource else {
        return false;
    };
    let scope = context
        .get()
        .explorer
        .resources
        .roots
        .iter()
        .find_map(|root| {
            (root.descriptor.resource.environment() == Some(&environment))
                .then_some(root.descriptor.scope)
        });
    let (Some(scope), Some(host)) = (
        scope,
        context.services().and_then(|runtime| runtime.extensions()),
    ) else {
        return false;
    };
    let state = context.clone();
    host.request_resource(
        scope,
        Operation::Stat { path },
        Box::new(move |app, result| match result {
            Ok(Answer::Info { file }) => {
                if let Some(application) =
                    state.services().and_then(|runtime| runtime.application())
                {
                    let next = state.clone();
                    application.post(move || super::resources::open(&next, scope, file));
                }
            }
            Err(error) => app.show_error(error.message),
            _ => app.show_error("Invalid file metadata"),
        }),
    )
    .is_ok()
}

/// The text copied for an environment path: the environment's own absolute path, or the
/// path relative to the connected root that holds it. `None` when `path` is not an
/// environment path, or is outside every connected root when a relative one is asked.
pub(crate) fn copy_text(
    context: &AppContext,
    path: &std::path::Path,
    relative: bool,
) -> Option<String> {
    let (environment, provider) = crate::features::git::location::resolve(path)?;
    if !relative {
        return Some(provider);
    }
    context
        .get()
        .explorer
        .resources
        .roots
        .iter()
        .filter_map(|root| match &root.descriptor.resource {
            ResourceRef::Provided {
                environment: root_environment,
                path: root_path,
            } if *root_environment == environment => {
                Some(root_path.as_str().trim_end_matches('/').to_owned())
            }
            _ => None,
        })
        .filter_map(|root| {
            let rest = provider.strip_prefix(&root)?;
            (rest.is_empty() || rest.starts_with('/'))
                .then(|| rest.trim_start_matches('/').to_owned())
        })
        .map(|rest| {
            if rest.is_empty() {
                ".".to_owned()
            } else {
                rest
            }
        })
        .next()
}
