//! Reversible filesystem steps for a server workspace edit. Removed and
//! overwritten entries stay on their original volume until the edit commits.
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
enum Undo {
    Remove(PathBuf),
    Move { from: PathBuf, to: PathBuf },
}
#[derive(Default)]
pub(crate) struct Journal {
    undo: Vec<Undo>,
    staging: Vec<PathBuf>,
    finished: bool,
}

impl Journal {
    fn moved_staging(&mut self, from: &Path, to: &Path) {
        for path in &mut self.staging {
            if let Ok(relative) = path.strip_prefix(from) {
                *path = if relative.as_os_str().is_empty() {
                    to.to_owned()
                } else {
                    to.join(relative)
                };
            }
        }
    }
    fn parents(&mut self, path: &Path) -> io::Result<()> {
        let mut missing = Vec::new();
        let mut parent = path.parent();
        while let Some(p) = parent {
            if p.exists() {
                break;
            }
            missing.push(p.to_path_buf());
            parent = p.parent();
        }
        for p in missing.into_iter().rev() {
            fs::create_dir(&p)?;
            self.undo.push(Undo::Remove(p));
        }
        Ok(())
    }
    fn stage(&mut self, path: &Path) -> io::Result<()> {
        if fs::symlink_metadata(path).is_err_and(|e| e.kind() == io::ErrorKind::NotFound) {
            return Ok(());
        }
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("cannot replace a filesystem root"))?;
        let dir = loop {
            let candidate = parent.join(format!(
                ".loog-edit-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        };
        self.staging.push(dir.clone());
        let saved = dir.join("original");
        fs::rename(path, &saved)?;
        self.moved_staging(path, &saved);
        self.undo.push(Undo::Move {
            from: saved,
            to: path.to_path_buf(),
        });
        Ok(())
    }
    pub fn create(&mut self, path: &Path, overwrite: bool, ignore: bool) -> io::Result<bool> {
        if fs::symlink_metadata(path).is_ok() {
            if overwrite {
                self.stage(path)?;
            } else if ignore {
                return Ok(false);
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "file already exists",
                ));
            }
        }
        self.parents(path)?;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        self.undo.push(Undo::Remove(path.to_path_buf()));
        Ok(true)
    }
    pub fn rename(
        &mut self,
        from: &Path,
        to: &Path,
        overwrite: bool,
        ignore: bool,
    ) -> io::Result<bool> {
        if from == to {
            return Ok(false);
        }
        fs::symlink_metadata(from)?;
        if fs::symlink_metadata(to).is_ok() {
            if overwrite {
                self.stage(to)?;
            } else if ignore {
                return Ok(false);
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "rename target already exists",
                ));
            }
        }
        self.parents(to)?;
        fs::rename(from, to)?;
        self.moved_staging(from, to);
        self.undo.push(Undo::Move {
            from: to.to_path_buf(),
            to: from.to_path_buf(),
        });
        Ok(true)
    }
    pub fn delete(&mut self, path: &Path, recursive: bool, ignore: bool) -> io::Result<bool> {
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(e) if ignore && e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        if meta.is_dir() && !recursive && fs::read_dir(path)?.next().is_some() {
            return Err(io::Error::other(
                "cannot delete a nonempty directory without recursive",
            ));
        }
        self.stage(path)?;
        Ok(true)
    }
    pub fn rollback(&mut self) -> io::Result<()> {
        let mut error = None;
        while let Some(step) = self.undo.pop() {
            let result = match step {
                Undo::Remove(path) => {
                    if path.is_dir() {
                        fs::remove_dir(path)
                    } else {
                        fs::remove_file(path)
                    }
                }
                Undo::Move { from, to } => {
                    // Never replace something that appeared in the original place since: the
                    // moved entry stays where it is and is named in the error.
                    let result = if fs::symlink_metadata(&to).is_ok() {
                        Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            format!(
                                "cannot restore {}: {} exists now (the entry is still at {})",
                                to.display(),
                                to.display(),
                                from.display()
                            ),
                        ))
                    } else {
                        fs::rename(&from, &to)
                    };
                    if result.is_ok() {
                        self.moved_staging(&from, &to);
                    }
                    result
                }
            };
            if let Err(e) = result {
                error.get_or_insert(e);
            }
        }
        self.finished = true;
        // Failed restoration keeps staging entries for recovery.
        if let Some(error) = error {
            return Err(io::Error::new(
                error.kind(),
                format!(
                    "{error}; recovery directories: {}",
                    self.staging
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
        self.cleanup();
        Ok(())
    }
    pub fn commit(mut self) {
        self.finished = true;
        self.undo.clear();
        self.cleanup();
    }
    fn cleanup(&mut self) {
        for dir in self.staging.drain(..).rev() {
            if let Err(e) = fs::remove_dir_all(&dir)
                && e.kind() != io::ErrorKind::NotFound
            {
                eprintln!("workspace edit staging {}: {e}", dir.display());
            }
        }
    }
}
impl Drop for Journal {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.rollback();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "loog-journal-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        path
    }
    #[test]
    fn moving_a_parent_keeps_nested_staging_recoverable_and_cleans_it_after_commit() {
        for commit in [false, true] {
            let root = root();
            let dir = root.join("dir");
            let moved = root.join("moved");
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("a"), "old").unwrap();
            let mut j = Journal::default();
            j.create(&dir.join("a"), true, false).unwrap();
            j.rename(&dir, &moved, false, false).unwrap();
            if commit {
                j.commit();
            } else {
                j.rollback().unwrap();
            }
            let remaining = if commit { &moved } else { &dir };
            assert_eq!(fs::read_dir(remaining).unwrap().count(), 1);
            assert_eq!(
                fs::read_to_string(remaining.join("a")).unwrap(),
                if commit { "" } else { "old" }
            );
            assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn dropping_a_journal_restores_overwrites_and_nested_directory_deletes() {
        let root = root();
        let path = root.join("dir");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("a"), "old").unwrap();
        {
            let mut j = Journal::default();
            j.create(&path.join("a"), true, true).unwrap();
            j.delete(&path, true, false).unwrap();
        }
        assert_eq!(fs::read_to_string(path.join("a")).unwrap(), "old");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn overwrite_precedes_ignore_and_commit_removes_backups() {
        let root = root();
        let a = root.join("a");
        let b = root.join("b");
        fs::write(&a, "A").unwrap();
        fs::write(&b, "B").unwrap();
        let mut j = Journal::default();
        assert!(j.rename(&a, &b, true, true).unwrap());
        j.commit();
        assert_eq!(fs::read_to_string(&b).unwrap(), "A");
        assert!(!a.exists());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn nonrecursive_delete_and_existing_targets_fail_without_mutation() {
        let root = root();
        fs::create_dir(root.join("dir")).unwrap();
        fs::write(root.join("dir/a"), "A").unwrap();
        let mut j = Journal::default();
        assert!(j.delete(&root.join("dir"), false, false).is_err());
        assert!(j.create(&root.join("dir/a"), false, false).is_err());
        j.rollback().unwrap();
        assert_eq!(fs::read_to_string(root.join("dir/a")).unwrap(), "A");
        fs::remove_dir_all(root).unwrap();
    }
}
