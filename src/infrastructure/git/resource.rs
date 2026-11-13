//! Environment-independent repository operations. Filesystem interpretation is
//! supplied by an adapter; command execution uses the same injected runner.
use super::{
    backend::CliGitBackend,
    command::{GitCommand, GitCommandRunner},
};
use crate::features::git::{
    self, ChangeKind, DiffTarget, RepositoryState,
    diff::{FileDiff, UnifiedDiff},
    error::{GitError, GitErrorKind, GitResult},
    parser::{parse_resource_numstat, parse_resource_status},
    path::GitPath,
};
use loog_workspace_api::ResourceRef;
use std::{
    hash::{Hash, Hasher},
    sync::Arc,
};

pub use crate::features::git::resources::Snapshot;
pub trait RepositoryIo: Send + Sync {
    /// Interpret a Git-reported path in the selected environment and scope.
    fn resolve(&self, base: &ResourceRef, expression: &str) -> GitResult<ResourceRef>;
    fn child(&self, root: &ResourceRef, path: &GitPath) -> GitResult<ResourceRef>;
    fn read(&self, resource: &ResourceRef, limit: usize) -> GitResult<Option<Vec<u8>>>;
    fn encoding(&self, root: &ResourceRef) -> GitResult<loog_text_core::format::EncodingSettings>;
}

pub struct Backend {
    runner: GitCommandRunner,
    io: Arc<dyn RepositoryIo>,
}
impl Backend {
    pub fn new(runner: GitCommandRunner, io: Arc<dyn RepositoryIo>) -> Self {
        Self { runner, io }
    }
    pub fn discover(
        &self,
        directory: &ResourceRef,
        generation: u64,
    ) -> GitResult<Option<Snapshot>> {
        let output = match self.runner.run(
            GitCommand::new()
                .cwd(directory)
                .args([
                    "rev-parse",
                    "--path-format=absolute",
                    "--show-toplevel",
                    "--absolute-git-dir",
                    "--git-common-dir",
                ])
                .read_only(),
        ) {
            Ok(output) => output.stdout_text()?,
            Err(error) if error.kind == GitErrorKind::NotRepository => return Ok(None),
            Err(error) => return Err(error),
        };
        let names: Vec<_> = output.lines().collect();
        if names.len() != 3 || names.iter().any(|name| name.is_empty()) {
            return Err(GitError::new(
                GitErrorKind::Unsupported,
                "Git repository metadata contains an ambiguous path",
            ));
        }
        let root = self.io.resolve(directory, names[0])?;
        let git_dir = self.io.resolve(&root, names[1])?;
        let common_dir = self.io.resolve(&root, names[2])?;
        if root.environment() != directory.environment()
            || git_dir.environment() != root.environment()
            || common_dir.environment() != root.environment()
        {
            return Err(GitError::new(
                GitErrorKind::PermissionDenied,
                "Git metadata resolved into another environment",
            ));
        }
        let mut identity = std::collections::hash_map::DefaultHasher::new();
        root.hash(&mut identity);
        let mut snapshot = Snapshot {
            id: git::RepositoryId(identity.finish()),
            worktree_root: root,
            git_dir,
            common_dir,
            head: Default::default(),
            upstream: None,
            ahead: 0,
            behind: 0,
            files: Default::default(),
            ignored: Default::default(),
            repository_state: RepositoryState::Normal,
            operation_step: None,
            head_oid: None,
            stashes: Default::default(),
            tags: Default::default(),
            worktrees: Default::default(),
            features: Default::default(),
            generation,
        };
        self.refresh(&mut snapshot)?;
        Ok(Some(snapshot))
    }
    pub fn refresh(&self, snapshot: &mut Snapshot) -> GitResult<()> {
        let output = self.runner.run(
            GitCommand::new()
                .cwd(&snapshot.worktree_root)
                .args([
                    "status",
                    "--porcelain=v2",
                    "-z",
                    "--branch",
                    "--untracked-files=all",
                    "--ignored=matching",
                ])
                .read_only(),
        )?;
        let mut status = parse_resource_status(&output.stdout)?;
        for staged in [false, true] {
            let mut command = GitCommand::new().cwd(&snapshot.worktree_root).args([
                "diff",
                "--numstat",
                "-z",
                "--no-ext-diff",
                "--no-textconv",
            ]);
            if staged {
                command = command.arg("--cached");
            }
            let stats = parse_resource_numstat(&self.runner.run(command.read_only())?.stdout)?;
            for (path, value) in stats {
                if let Some(file) = status.files.get_mut(&path) {
                    if staged {
                        file.index_stat = Some(value);
                    } else {
                        file.worktree_stat = Some(value);
                    }
                }
            }
        }
        snapshot.head = status.head;
        snapshot.repository_state = match &snapshot.head {
            git::HeadState::Detached(_) => RepositoryState::DetachedHead,
            git::HeadState::Unborn(_) => RepositoryState::UnbornBranch,
            _ => RepositoryState::Normal,
        };
        snapshot.head_oid = status.head_oid;
        snapshot.upstream = status.upstream;
        snapshot.ahead = status.ahead;
        snapshot.behind = status.behind;
        snapshot.files = Arc::new(status.files);
        snapshot.ignored = Arc::new(status.ignored);
        Ok(())
    }
    pub fn file_resource(&self, repository: &Snapshot, path: &GitPath) -> GitResult<ResourceRef> {
        self.io.child(&repository.worktree_root, path)
    }
    pub fn diff(
        &self,
        repository: &Snapshot,
        path: &GitPath,
        target: DiffTarget,
    ) -> GitResult<UnifiedDiff<GitPath>> {
        let file = repository.files.get(path).ok_or_else(|| {
            GitError::new(
                GitErrorKind::Conflict,
                "Refresh the repository before comparing this file",
            )
        })?;
        if file.conflict.is_some() {
            return Err(GitError::new(
                GitErrorKind::Conflict,
                "Resolve or inspect the unmerged index stages before comparing this file",
            ));
        }
        if !matches!(
            target,
            DiffTarget::HeadToIndex | DiffTarget::IndexToWorktree | DiffTarget::HeadToWorktree
        ) {
            return Err(GitError::new(
                GitErrorKind::Unsupported,
                "This comparison requires a selected repository file",
            ));
        }
        let old_name = if target == DiffTarget::IndexToWorktree {
            path
        } else {
            file.original_path.as_ref().unwrap_or(path)
        };
        let old_exists = if target == DiffTarget::IndexToWorktree {
            file.worktree != ChangeKind::Untracked && file.index != ChangeKind::Deleted
        } else {
            repository.head_oid.is_some()
                && file.index != ChangeKind::Added
                && (file.worktree != ChangeKind::Untracked || file.index == ChangeKind::Deleted)
        };
        let old = if old_exists {
            Some(self.blob(
                &repository.worktree_root,
                if target == DiffTarget::IndexToWorktree {
                    ""
                } else {
                    "HEAD"
                },
                old_name,
            )?)
        } else {
            None
        };
        let new = if target == DiffTarget::HeadToIndex {
            if file.index == ChangeKind::Deleted || file.worktree == ChangeKind::Untracked {
                None
            } else {
                Some(self.blob(&repository.worktree_root, "", path)?)
            }
        } else {
            self.io.read(
                &self.file_resource(repository, path)?,
                git::diff_text::MAX_SIDE_BYTES,
            )?
        };
        let options = self.io.encoding(&repository.worktree_root)?;
        let decode = |bytes: &Option<Vec<u8>>| {
            bytes
                .as_ref()
                .map(|bytes| {
                    loog_text_core::encoding::decode(
                        bytes,
                        None,
                        &options,
                        git::diff_text::MAX_SIDE_BYTES,
                    )
                })
                .transpose()
        };
        let mut result = FileDiff {
            old_path: old.as_ref().map(|_| old_name.clone()),
            new_path: new.as_ref().map(|_| path.clone()),
            encoded: true,
            ..Default::default()
        };
        match (decode(&old), decode(&new)) {
            (Ok(old), Ok(new)) => {
                if let (Some(old), Some(new)) = (&old, &new)
                    && old.format != new.format
                {
                    result.format_change = Some(format!(
                        "Encoding: {} → {}",
                        old.format.label(),
                        new.format.label()
                    ));
                }
                result.hunks = git::diff_text::compare(
                    old.as_ref().map_or("", |text| text.text.as_str()),
                    new.as_ref().map_or("", |text| text.text.as_str()),
                    128_000,
                )?;
            }
            (Err(error), _) | (_, Err(error)) if error == "Binary file" => result.binary = true,
            (Err(error), _) | (_, Err(error)) => {
                return Err(GitError::new(GitErrorKind::Unsupported, error));
            }
        }
        Ok(UnifiedDiff {
            files: vec![result],
        })
    }
    fn blob(&self, root: &ResourceRef, revision: &str, path: &GitPath) -> GitResult<Vec<u8>> {
        let name = path.to_str().ok_or_else(|| {
            GitError::new(GitErrorKind::Unsupported, "Git object name is not UTF-8")
        })?;
        let object = format!("{revision}:{name}");
        let size = self
            .runner
            .run(
                GitCommand::new()
                    .cwd(root)
                    .args(["cat-file", "-s", &object])
                    .read_only(),
            )?
            .stdout_text()?;
        if size
            .trim()
            .parse::<usize>()
            .ok()
            .is_none_or(|size| size > git::diff_text::MAX_SIDE_BYTES)
        {
            return Err(GitError::new(
                GitErrorKind::Unsupported,
                "Git object exceeds the text comparison budget",
            ));
        }
        let bytes = self
            .runner
            .run(
                GitCommand::new()
                    .cwd(root)
                    .args(["cat-file", "blob", &object])
                    .read_only(),
            )?
            .stdout;
        if bytes.len() > git::diff_text::MAX_SIDE_BYTES {
            return Err(GitError::new(
                GitErrorKind::Unsupported,
                "Git object changed or exceeded its budget",
            ));
        }
        Ok(bytes)
    }
    pub fn stage(&self, root: &ResourceRef, paths: &[GitPath], unstage: bool) -> GitResult<()> {
        if paths.is_empty() || paths.len() > 4096 {
            return Err(GitError::new(
                GitErrorKind::Unsupported,
                "Select between 1 and 4096 Git files",
            ));
        }
        let mut input = Vec::new();
        for path in paths {
            if path.is_empty() {
                return Err(GitError::new(
                    GitErrorKind::Unsupported,
                    "A file selection cannot include the repository root",
                ));
            }
            input.extend_from_slice(&path.bytes());
            input.push(0);
            if input.len() > 1024 * 1024 {
                return Err(GitError::new(
                    GitErrorKind::Unsupported,
                    "Git file selection exceeds 1 MiB",
                ));
            }
        }
        let command = if unstage {
            let head = self.runner.run(
                GitCommand::new()
                    .cwd(root)
                    .args(["rev-parse", "--verify", "--quiet", "HEAD"])
                    .accept_exit_code(1)
                    .read_only(),
            )?;
            if head.code == Some(1) {
                GitCommand::new().args([
                    "--literal-pathspecs",
                    "rm",
                    "--cached",
                    "-r",
                    "--ignore-unmatch",
                ])
            } else {
                GitCommand::new().args(["--literal-pathspecs", "restore", "--staged"])
            }
        } else {
            GitCommand::new().args(["--literal-pathspecs", "add"])
        };
        self.runner.run(
            command
                .cwd(root)
                .args(["--pathspec-from-file=-", "--pathspec-file-nul"])
                .stdin(input),
        )?;
        Ok(())
    }
    pub fn commit(
        &self,
        root: &ResourceRef,
        message: &str,
        amend: bool,
        signoff: bool,
    ) -> GitResult<String> {
        CliGitBackend::with_runner(self.runner.clone()).commit(root, message, amend, signoff)
    }
}
