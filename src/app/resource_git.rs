//! Git command policy composed with the generic resource process adapter.
//! Repository and file identity must remain with the captured environment.
use super::{context::AppContext, resource_process};
use crate::features::git::{GitError, GitResult, error::GitErrorKind, types::GitRuntimeSource};
use crate::infrastructure::git::{
    command::{CancellationToken, GitCommand, GitCommandRunner, GitExecutor, GitOutput},
    runtime::GitRuntime,
};
use loog_workspace_api::{
    Answer, Failure, FailureKind, FileKind, Operation, ProviderPath, ResourceRef, ScopeId,
};
use std::{sync::Arc, time::Duration};

struct Executor {
    context: AppContext,
    scope: ScopeId,
    root: ResourceRef,
}

pub(super) fn backend(
    context: AppContext,
    scope: ScopeId,
    root: ResourceRef,
) -> crate::infrastructure::git::resource::Backend {
    let command = runner(context.clone(), scope, root);
    crate::infrastructure::git::resource::Backend::new(command, Arc::new(Io { context, scope }))
}
struct Io {
    context: AppContext,
    scope: ScopeId,
}
impl Io {
    fn with<T>(
        &self,
        resource: &ResourceRef,
        work: impl FnOnce(
            &mut dyn FnMut(Operation) -> Result<Answer, Failure>,
            &ProviderPath,
        ) -> Result<T, Failure>,
    ) -> GitResult<T> {
        super::resource_bytes::background_typed(
            &self.context,
            self.scope,
            resource,
            &|| false,
            |request, path| Ok(work(request, path)),
        )
        .map_err(|error| GitError::new(GitErrorKind::RuntimeUnavailable, error))?
        .map_err(failure)
    }
    fn identity(base: &ResourceRef, path: ProviderPath) -> Result<ResourceRef, Failure> {
        Ok(ResourceRef::Provided {
            environment: base
                .environment()
                .ok_or_else(|| {
                    Failure::new(
                        FailureKind::InvalidRequest,
                        "Expected a provided Git resource",
                    )
                })?
                .clone(),
            path,
        })
    }
}
impl crate::infrastructure::git::resource::RepositoryIo for Io {
    fn resolve(&self, base: &ResourceRef, expression: &str) -> GitResult<ResourceRef> {
        self.with(base, |request, path| {
            let Answer::Path { path } = request(Operation::ResolvePath {
                base: path.clone(),
                relative: expression.into(),
            })?
            else {
                return Err(Failure::new(
                    FailureKind::InvalidRequest,
                    "Invalid Git directory resolution",
                ));
            };
            Self::identity(base, path)
        })
    }
    fn child(
        &self,
        root: &ResourceRef,
        relative: &crate::features::git::path::GitPath,
    ) -> GitResult<ResourceRef> {
        self.with(root, |request, path| {
            let mut path = path.clone();
            for name in relative.components() {
                let name = name.to_str().ok_or_else(|| {
                    Failure::new(
                        FailureKind::Unsupported,
                        "This environment requires UTF-8 Git filenames",
                    )
                })?;
                let Answer::Path { path: child } = request(Operation::ChildPath {
                    parent: path,
                    name: name.into(),
                })?
                else {
                    return Err(Failure::new(
                        FailureKind::InvalidRequest,
                        "Invalid Git file resolution",
                    ));
                };
                path = child;
            }
            Self::identity(root, path)
        })
    }
    fn read(&self, resource: &ResourceRef, limit: usize) -> GitResult<Option<Vec<u8>>> {
        self.with(resource, |request, path| {
            let file = match request(Operation::Stat { path: path.clone() }) {
                Ok(Answer::Info { file }) => file,
                Err(error) if error.kind == FailureKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
                _ => {
                    return Err(Failure::new(
                        FailureKind::InvalidRequest,
                        "Invalid Git worktree metadata",
                    ));
                }
            };
            if file.path != *path || file.kind != FileKind::File || file.size > limit as u64 {
                return Err(Failure::new(
                    FailureKind::Unsupported,
                    "Git text comparison requires a regular file within its read budget",
                ));
            }
            super::resource_bytes::read_file(
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
    fn encoding(&self, root: &ResourceRef) -> GitResult<loog_text_core::format::EncodingSettings> {
        let (settings, error) =
            super::resource_project::background_encoding(&self.context, self.scope, root, &|| {
                false
            })
            .map_err(|error| GitError::new(GitErrorKind::Io, error))?;
        if let Some(error) = error {
            return Err(GitError::new(GitErrorKind::Other, error));
        }
        Ok(settings)
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
impl std::fmt::Debug for Executor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceGitExecutor")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}
pub(super) fn runner(context: AppContext, scope: ScopeId, root: ResourceRef) -> GitCommandRunner {
    // This is a command name resolved in the provider's environment. The local
    // runner is never selected for this instance, including on disconnect.
    let runtime = GitRuntime {
        executable: "git".into(),
        root: Default::default(),
        source: GitRuntimeSource::RemoteEnvironment,
        version: Default::default(),
        capabilities: Default::default(),
    };
    GitCommandRunner::with_executor(
        runtime,
        Arc::new(Executor {
            context,
            scope,
            root,
        }),
    )
}
impl GitExecutor for Executor {
    fn run(&self, command: GitCommand, cancellation: &CancellationToken) -> GitResult<GitOutput> {
        let cwd = command.cwd.as_ref().unwrap_or(&self.root);
        if cwd.environment().is_none() || cwd.environment() != self.root.environment() {
            return Err(GitError::new(
                GitErrorKind::PermissionDenied,
                "Git command belongs to another environment",
            ));
        }
        let mut args = Vec::with_capacity(command.args.len() + 1);
        if command.read_only {
            args.push("--no-optional-locks".into());
        }
        for arg in &command.args {
            args.push(
                arg.to_str()
                    .ok_or_else(|| {
                        GitError::new(
                            GitErrorKind::Unsupported,
                            "This environment requires UTF-8 Git arguments",
                        )
                    })?
                    .to_owned(),
            );
        }
        let env = [
            ("GIT_PAGER", "cat"),
            ("PAGER", "cat"),
            ("GIT_EDITOR", ":"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GCM_INTERACTIVE", "Never"),
            ("GIT_CONFIG_COUNT", "3"),
            ("GIT_CONFIG_KEY_0", "color.ui"),
            ("GIT_CONFIG_VALUE_0", "false"),
            ("GIT_CONFIG_KEY_1", "diff.external"),
            ("GIT_CONFIG_VALUE_1", ""),
            ("GIT_CONFIG_KEY_2", "core.quotepath"),
            ("GIT_CONFIG_VALUE_2", "false"),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
        let output = resource_process::run(
            &self.context,
            self.scope,
            resource_process::Command {
                program: "git".into(),
                args,
                cwd: cwd.clone(),
                env,
                input: command.stdin,
                timeout: command.timeout.unwrap_or(Duration::from_secs(120)),
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
}
