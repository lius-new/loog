//! File creation, renaming and deletion requested by extensions (design §6.2).
//!
//! The request is checked on the UI thread against everything that must allow
//! it, the disk work runs on a background thread through the edit journal, and
//! the result comes back to the UI thread, which brings open documents and the
//! language servers up to date. A request that touches a file with unsaved
//! changes is refused, so no typing is lost.
use super::*;
use crate::features::extensions::manifest::check_package_path;
use crate::features::providers::FileChange;
use crate::infrastructure::extensions::fileops::{self, Op, Outcome as DiskOutcome};

/// What a running file operation was for.
pub(super) struct FileJob {
    extension: String,
    generation: u64,
    operation: u64,
    ops: Vec<Op>,
}

const MAX_OPS: usize = 64;
const MAX_TEXT: usize = 1024 * 1024;
const MAX_TOTAL: usize = 4 * 1024 * 1024;

fn refuse(kind: ErrorKind, message: impl Into<String>) -> ExtensionError {
    ExtensionError::new(kind, message)
}

/// A path inside the folder, as the extension named it. The folder's own path
/// (not a resolved one) is kept, so open documents keep matching.
pub(super) fn within(root: &Path, relative: &str) -> Result<PathBuf, ExtensionError> {
    let outside = || {
        refuse(
            ErrorKind::PermissionDenied,
            "the path must stay inside the folder",
        )
    };
    if check_package_path(relative).is_err() {
        return Err(outside());
    }
    // The tools' own leftovers and version control data are not for extensions.
    if relative.split('/').any(|part| {
        part == ".git" || part.starts_with(".loog-edit-") || part.starts_with(fileops::NOTE_PREFIX)
    }) {
        return Err(refuse(
            ErrorKind::PermissionDenied,
            "version control data and recovery entries cannot be changed",
        ));
    }
    let base = root
        .canonicalize()
        .map_err(|_| refuse(ErrorKind::ExpiredHandle, "the folder cannot be read"))?;
    let joined = root.join(relative);
    // The entry itself may be a link or not exist yet; the place it is in must
    // really be inside the folder once links are resolved.
    let mut ancestor = joined.parent();
    while let Some(dir) = ancestor {
        if let Ok(real) = dir.canonicalize() {
            return if real.starts_with(&base) {
                Ok(joined)
            } else {
                Err(outside())
            };
        }
        ancestor = dir.parent();
    }
    Err(outside())
}

impl ExtensionService {
    pub(super) fn file_ops_requested(
        &mut self,
        app: &AppState,
        id: &str,
        generation: u64,
        started: loog_extension_host::StartedFileOps,
    ) -> Vec<Effect> {
        let operation = started.operation;
        let fail = |error: ExtensionError| {
            vec![Effect::Submit(Job::Complete {
                id: id.to_owned(),
                generation,
                operation,
                outcome: Outcome::Failed(error),
            })]
        };
        let Some(package) = self
            .packages
            .get(id)
            .filter(|p| p.lifecycle.accepts(generation))
        else {
            return Vec::new();
        };
        if let Err(error) = self.authorize(package, "workspace.files.write") {
            return fail(error);
        }
        match self.plan_file_ops(app, &started.request) {
            Err(error) => fail(error),
            Ok(ops) => {
                self.next_file_job += 1;
                let key = self.next_file_job;
                self.file_jobs.insert(
                    key,
                    FileJob {
                        extension: id.to_owned(),
                        generation,
                        operation,
                        ops: ops.clone(),
                    },
                );
                vec![Effect::RunFileOps { key, ops }]
            }
        }
    }

    fn plan_file_ops(
        &self,
        app: &AppState,
        request: &loog_extension_host::FileOpsRequest,
    ) -> Result<Vec<Op>, ExtensionError> {
        use loog_extension_host::FileOp;
        let root = self
            .roots
            .iter()
            .find(|(id, _)| *id == request.root)
            .map(|(_, path)| path.clone())
            .ok_or_else(|| refuse(ErrorKind::ExpiredHandle, "the folder is no longer open"))?;
        if request.ops.is_empty() || request.ops.len() > MAX_OPS {
            return Err(refuse(
                ErrorKind::LimitExceeded,
                "between 1 and 64 operations are allowed",
            ));
        }
        let mut total = 0usize;
        let mut ops = Vec::new();
        for op in &request.ops {
            ops.push(match op {
                FileOp::Create(create) => {
                    let text = create.text.clone();
                    total += text.as_ref().map_or(0, String::len);
                    if text.as_ref().is_some_and(|t| t.len() > MAX_TEXT) || total > MAX_TOTAL {
                        return Err(refuse(
                            ErrorKind::LimitExceeded,
                            "the new files are too large",
                        ));
                    }
                    Op::Create {
                        path: within(&root, &create.path)?,
                        text,
                        overwrite: create.overwrite,
                    }
                }
                FileOp::Rename(rename) => Op::Rename {
                    from: within(&root, &rename.source)?,
                    to: within(&root, &rename.target)?,
                    overwrite: rename.overwrite,
                },
                FileOp::Delete(delete) => Op::Delete {
                    path: within(&root, &delete.path)?,
                    recursive: delete.recursive,
                },
            });
        }
        // Unsaved changes are never discarded or moved from under the person.
        for op in &ops {
            let touched: Vec<&PathBuf> = match op {
                Op::Create {
                    path, overwrite, ..
                } => overwrite.then_some(path).into_iter().collect(),
                Op::Rename {
                    from,
                    to,
                    overwrite,
                } => {
                    if *overwrite {
                        vec![from, to]
                    } else {
                        vec![from]
                    }
                }
                Op::Delete { path, .. } => vec![path],
            };
            for path in touched {
                let dirty = app
                    .workspace
                    .documents
                    .iter()
                    .filter(|(_, d)| d.is_text() && d.meta.path.starts_with(path))
                    .find(|(file, _)| app.workspace.is_dirty(**file));
                if let Some((_, document)) = dirty {
                    return Err(refuse(
                        ErrorKind::Conflict,
                        format!("{} has unsaved changes", document.meta.path.display()),
                    ));
                }
            }
        }
        Ok(ops)
    }

    /// The disk work finished: bring documents and servers along and answer the guest.
    pub fn file_ops_done(
        &mut self,
        app: &mut AppState,
        key: u64,
        outcome: DiskOutcome,
    ) -> Vec<Effect> {
        let Some(job) = self.file_jobs.remove(&key) else {
            return Vec::new();
        };
        let (answer, mut effects) = match outcome {
            DiskOutcome::Done => {
                let changes = apply_to_workspace(app, &job.ops);
                (
                    Outcome::Committed {
                        transaction: job.operation,
                        version: 0,
                    },
                    vec![Effect::FilesChanged(changes)],
                )
            }
            DiskOutcome::FailedRecovered { step, error } => (
                Outcome::Failed(refuse(
                    ErrorKind::ExtensionFailed,
                    format!(
                        "step {} failed and everything before it was undone: {error}",
                        step + 1
                    ),
                )),
                Vec::new(),
            ),
            DiskOutcome::FailedUnrecovered {
                step,
                error,
                recovery,
            } => {
                // The disk may be partly changed: the error says so and where to look.
                (
                    Outcome::Failed(ExtensionError::after_commit(
                        ErrorKind::ExtensionFailed,
                        format!(
                            "step {} failed ({error}) and could not be undone completely: {recovery}",
                            step + 1
                        ),
                    )),
                    vec![Effect::FilesChanged(Vec::new())],
                )
            }
        };
        if self
            .packages
            .get(&job.extension)
            .is_some_and(|p| p.lifecycle.accepts(job.generation))
        {
            effects.push(Effect::Submit(Job::Complete {
                id: job.extension,
                generation: job.generation,
                operation: job.operation,
                outcome: answer,
            }));
        }
        effects
    }
}

fn apply_to_workspace(app: &mut AppState, ops: &[Op]) -> Vec<FileChange> {
    let mut changes = Vec::new();
    for op in ops {
        match op {
            Op::Create { path, text, .. } => {
                if let Some(id) = app.workspace.file_id_for_path(path)
                    && let Some(document) = app.workspace.documents.get_mut(&id)
                {
                    document.buffer.reload(text.clone().unwrap_or_default());
                }
                changes.push(FileChange::Created(path.clone()));
            }
            Op::Rename { from, to, .. } => {
                app.workspace.close_paths_under(to);
                app.workspace.move_open_path(from, to.clone());
                changes.push(FileChange::Renamed {
                    from: from.clone(),
                    to: to.clone(),
                });
            }
            Op::Delete { path, .. } => {
                app.workspace.close_paths_under(path);
                changes.push(FileChange::Deleted(path.clone()));
            }
        }
    }
    changes
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn paths_are_relative_inside_the_folder_and_avoid_protected_names() {
        let dir = std::env::temp_dir().join(format!("loog-within-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        assert!(within(&dir, "sub/new.txt").is_ok());
        assert!(
            within(&dir, "deep/er/new.txt").is_ok(),
            "parents may not exist yet"
        );
        for bad in [
            "../x",
            "/abs",
            "a/../../x",
            "C:/x",
            ".git/config",
            "sub/.git/x",
            ".loog-edit-1/x",
            ".loog-fileops-1.txt",
            "",
        ] {
            assert!(within(&dir, bad).is_err(), "{bad}");
        }
    }
}
