use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::command::{GitCommand, GitCommandRunner};
use super::remote;
use super::repo_fs;
use crate::features::git::error::{GitErrorKind, GitResult};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredRepository {
    pub worktree_root: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    pub bare: bool,
}

pub fn discover(
    runner: &GitCommandRunner,
    workspace_roots: &[PathBuf],
) -> GitResult<Vec<DiscoveredRepository>> {
    let mut seen = HashSet::new();
    let mut repositories = Vec::new();
    for root in workspace_roots {
        if let Some(repository) = discover_in(runner, root)?
            && seen.insert(repository.worktree_root.clone())
        {
            repositories.push(repository);
        }
        for child in repo_fs::child_repositories(root) {
            if let Some(repository) = discover_in(runner, &child)?
                && seen.insert(repository.worktree_root.clone())
            {
                repositories.push(repository);
            }
        }
    }
    repositories.sort_by(|left, right| left.worktree_root.cmp(&right.worktree_root));
    Ok(repositories)
}

/// Fast startup discovery. Workspace roots that contain `.git` are resolved
/// without spawning a process; only roots opened from inside a parent
/// repository fall back to `rev-parse`. Nested repositories are discovered by
/// the later detailed refresh.
pub fn discover_fast(
    runner: &GitCommandRunner,
    workspace_roots: &[PathBuf],
) -> GitResult<Vec<DiscoveredRepository>> {
    let mut seen = HashSet::new();
    let mut repositories = Vec::new();
    for root in workspace_roots {
        let root = repo_fs::canonical(root.clone());
        // One `rev-parse` resolves an environment root, whose `.git` file would name a
        // directory in the environment's own spelling.
        let repository = if !remote::is_remote(&root) && repo_fs::exists(&root.join(".git")) {
            Some(from_git_marker(&root)?)
        } else {
            discover_in(runner, &root)?
        };
        if let Some(repository) = repository
            && seen.insert(repository.worktree_root.clone())
        {
            repositories.push(repository);
        }
    }
    Ok(repositories)
}

fn from_git_marker(root: &Path) -> GitResult<DiscoveredRepository> {
    let marker = root.join(".git");
    let git_dir = if repo_fs::is_dir(&marker) {
        marker
    } else {
        let contents = repo_fs::read_to_string(&marker).ok_or_else(|| {
            crate::features::git::error::GitError::new(
                GitErrorKind::InvalidOutput,
                "The .git file could not be read.",
            )
        })?;
        let value = contents
            .trim()
            .strip_prefix("gitdir:")
            .map(str::trim)
            .ok_or_else(|| {
                crate::features::git::error::GitError::new(
                    GitErrorKind::InvalidOutput,
                    "The .git file does not contain a gitdir path.",
                )
            })?;
        let path = PathBuf::from(value);
        if path.is_absolute() {
            path
        } else {
            root.join(path)
        }
    };
    let git_dir = repo_fs::canonical(git_dir);
    let common_dir = repo_fs::read_to_string(&git_dir.join("commondir"))
        .map(|value| {
            let path = PathBuf::from(value.trim());
            if path.is_absolute() {
                path
            } else {
                git_dir.join(path)
            }
        })
        .map(repo_fs::canonical)
        .unwrap_or_else(|| git_dir.clone());
    Ok(DiscoveredRepository {
        worktree_root: root.to_path_buf(),
        git_dir,
        common_dir,
        bare: false,
    })
}

/// [`discover_one`], except that an environment that cannot answer (disconnected, no `git`
/// there) finds nothing instead of failing the scan of the local repositories too.
fn discover_in(runner: &GitCommandRunner, path: &Path) -> GitResult<Option<DiscoveredRepository>> {
    match discover_one(runner, path) {
        Err(_) if remote::is_remote(path) => Ok(None),
        result => result,
    }
}

pub fn discover_one(
    runner: &GitCommandRunner,
    path: &Path,
) -> GitResult<Option<DiscoveredRepository>> {
    let command = GitCommand::new()
        .cwd(path)
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--absolute-git-dir",
            "--git-common-dir",
            "--is-bare-repository",
        ])
        .read_only();
    let output = match runner.run(command) {
        Ok(output) => output,
        Err(error) if error.kind == GitErrorKind::NotRepository => return Ok(None),
        Err(error) => return Err(error),
    };
    let lines = output
        .stdout_text()?
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if lines.len() < 4 {
        return Ok(None);
    }
    // Git prints the environment's own paths; the model spells them as environment paths.
    let spell = |line: &str| repo_fs::spell(path, line);
    Ok(Some(DiscoveredRepository {
        worktree_root: spell(&lines[0]),
        git_dir: spell(&lines[1]),
        common_dir: spell(&lines[2]),
        bare: lines[3] == "true",
    }))
}

