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
        Err(error) if remote::is_remote(path) => {
            remote::report_problem(error);
            Ok(None)
        }
        result => result,
    }
}

/// `--git-common-dir` is relative to the directory Git ran in unless it is the same as the
/// git dir. Older Git has no `--path-format=absolute` (it echoes the flag as if it were a
/// name), so a relative answer is made absolute here, in the spelling of that directory:
/// POSIX for an environment, native for a local path.
fn absolute_from(directory: &Path, printed: &str) -> String {
    let absolute = printed.starts_with('/') || std::path::Path::new(printed).is_absolute();
    if absolute {
        return printed.to_owned();
    }
    match remote::locate(directory) {
        Some(Ok(target)) => normalize_posix(&format!("{}/{printed}", target.path)),
        _ => directory.join(printed).to_string_lossy().into_owned(),
    }
}

/// A POSIX path with `.` and `..` steps and doubled slashes resolved.
fn normalize_posix(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    format!("/{}", parts.join("/"))
}

pub fn discover_one(
    runner: &GitCommandRunner,
    path: &Path,
) -> GitResult<Option<DiscoveredRepository>> {
    let command = GitCommand::new()
        .cwd(path)
        .args([
            "rev-parse",
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
        common_dir: spell(&absolute_from(path, &lines[2])),
        bare: lines[3] == "true",
    }))
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_paths_are_normalized_without_touching_a_disk() {
        assert_eq!(normalize_posix("/home/lius/loom/src/../.git"), "/home/lius/loom/.git");
        assert_eq!(normalize_posix("/a//b/./c"), "/a/b/c");
        assert_eq!(normalize_posix("/../x"), "/x");
    }

    #[test]
    fn a_relative_common_dir_is_made_absolute_where_git_ran() {
        let environment = loog_workspace_api::EnvironmentKey::new("fixture", "absolute-from").unwrap();
        let place = crate::features::git::location::path_for(&environment, "/home/lius/loom/src");
        assert_eq!(absolute_from(&place, "../.git"), "/home/lius/loom/.git");
        assert_eq!(absolute_from(&place, "/already/absolute"), "/already/absolute");
        // A local directory joins natively.
        let local = Path::new("repo").join("src");
        assert_eq!(
            Path::new(&absolute_from(&local, "../.git")),
            local.join("../.git")
        );
    }
}
