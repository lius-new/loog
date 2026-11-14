//! The few direct reads the Git code makes of a repository's files, for a repository
//! that is local and for one in an environment alike. A local path goes to `std::fs`;
//! an environment path (see `features::git::location`) goes to the installed
//! environment. Callers do not care which.
use std::path::{Path, PathBuf};

use super::remote;
use crate::features::git::error::GitResult;

/// Whether `path` exists. An environment that cannot answer says no.
pub fn exists(path: &Path) -> bool {
    if remote::is_remote(path) {
        return matches!(remote::stat(path), Ok(Some(_)));
    }
    path.exists()
}

/// Whether `path` is a regular file.
pub fn is_file(path: &Path) -> bool {
    if remote::is_remote(path) {
        return matches!(remote::stat(path), Ok(Some(stat)) if stat.file);
    }
    path.is_file()
}

/// Whether `path` is a directory.
pub fn is_dir(path: &Path) -> bool {
    if remote::is_remote(path) {
        return matches!(remote::stat(path), Ok(Some(stat)) if stat.directory);
    }
    path.is_dir()
}

/// The text of a small file, `None` when it is missing, unreadable or not text.
pub fn read_to_string(path: &Path) -> Option<String> {
    read(path, 1024 * 1024)
        .ok()
        .flatten()
        .and_then(|bytes| String::from_utf8(bytes).ok())
}

/// The bytes of a file within `limit`, `None` when it does not exist.
pub fn read(path: &Path, limit: usize) -> GitResult<Option<Vec<u8>>> {
    if remote::is_remote(path) {
        return remote::read_file(path, limit);
    }
    match std::fs::read(path) {
        Ok(bytes) if bytes.len() <= limit => Ok(Some(bytes)),
        Ok(_) => Err(crate::features::git::error::GitError::new(
            crate::features::git::error::GitErrorKind::Unsupported,
            "The file is larger than Git reads it.",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// One spelling per repository, matching workspace folders (canonicalized when opened).
/// Git prints `D:/dir` on Windows while `fs::canonicalize` yields `\\?\D:\dir`; mixing
/// them gives the fast and detailed scans different repository ids. An environment path
/// is already one spelling, since Git prints it that way there.
pub fn canonical(path: PathBuf) -> PathBuf {
    if remote::is_remote(&path) {
        return path;
    }
    std::fs::canonicalize(&path).unwrap_or(path)
}

/// The path Git printed (`line`) for the repository `near`, spelled the way the model
/// does: in the environment's own terms for an environment repository, else canonical.
pub fn spell(near: &Path, line: &str) -> PathBuf {
    match remote::locate(near) {
        Some(Ok(target)) => crate::features::git::location::path_for(&target.environment, line),
        _ => canonical(PathBuf::from(line)),
    }
}

/// What a directory holds, read once. In an environment each existence check is a round
/// trip, so a caller that asks about several names of one directory lists it first;
/// locally a check is cheap and this reads nothing.
pub struct Names {
    dir: PathBuf,
    listed: Option<std::collections::HashSet<String>>,
}

impl Names {
    pub fn of(dir: &Path) -> Self {
        let listed = remote::is_remote(dir).then(|| {
            remote::list_names(dir, 4096)
                .unwrap_or_default()
                .into_iter()
                .collect()
        });
        Self {
            dir: dir.to_path_buf(),
            listed,
        }
    }

    /// Whether the directory holds `name`.
    pub fn has(&self, name: &str) -> bool {
        match &self.listed {
            Some(names) => names.contains(name),
            None => self.dir.join(name).exists(),
        }
    }
}

/// The directories directly inside `root` that contain a `.git`, which is how nested
/// repositories are found.
pub fn child_repositories(root: &Path) -> Vec<PathBuf> {
    if remote::is_remote(root) {
        return remote::list_directories(root, 512)
            .unwrap_or_default()
            .into_iter()
            .map(|name| root.join(name))
            .filter(|path| exists(&path.join(".git")))
            .collect();
    }
    std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|kind| kind.is_dir())
                .map(|_| entry.path())
                .filter(|path| path.join(".git").exists())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::git::remote::tests as env;

    #[test]
    fn a_local_path_reads_the_disk() {
        let dir = std::env::temp_dir().join(format!("loog-repo-fs-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::write(dir.join("f.txt"), "hello").unwrap();
        assert!(exists(&dir) && is_dir(&dir) && is_file(&dir.join("f.txt")));
        assert!(!exists(&dir.join("missing")));
        assert_eq!(read_to_string(&dir.join("f.txt")).as_deref(), Some("hello"));
        assert_eq!(read(&dir.join("missing"), 10).unwrap(), None);
        assert!(read(&dir.join("f.txt"), 2).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_environment_path_reads_the_environment_not_the_disk() {
        let Some(environment) = env::environment("repo-fs") else {
            return;
        };
        std::fs::write(environment.local.join("note.txt"), "remote text").unwrap();
        let file = environment.repository.join("note.txt");
        assert!(exists(&file) && is_file(&file) && !is_dir(&file));
        assert!(is_dir(&environment.repository));
        assert_eq!(read_to_string(&file).as_deref(), Some("remote text"));
        assert!(!exists(&environment.repository.join("absent")));
        // The environment's spelling is never looked up on this machine's disk.
        assert!(!file.exists());
        assert_eq!(canonical(file.clone()), file);
    }
}
