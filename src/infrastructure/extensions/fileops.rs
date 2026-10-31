//! File creation, renaming and deletion for extensions (design §6.2).
//!
//! The steps run in order on disk through the edit journal, which keeps
//! replaced and removed entries aside until everything succeeded. A failure
//! rolls every step back; if the rollback itself cannot finish, the entries it
//! kept are named so the person can restore them by hand. A small note file in
//! the folder records the operation while it runs, so an interrupted run
//! (a crash, a power cut) leaves a trace that is reported at the next start.
use crate::infrastructure::edit_journal::Journal;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Prefix of the note an unfinished run leaves in the folder.
pub const NOTE_PREFIX: &str = ".loom-fileops-";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Create {
        path: PathBuf,
        text: Option<String>,
        overwrite: bool,
    },
    Rename {
        from: PathBuf,
        to: PathBuf,
        overwrite: bool,
    },
    Delete {
        path: PathBuf,
        recursive: bool,
    },
}

impl Op {
    fn describe(&self) -> String {
        match self {
            Op::Create { path, .. } => format!("create {}", path.display()),
            Op::Rename { from, to, .. } => format!("rename {} -> {}", from.display(), to.display()),
            Op::Delete { path, .. } => format!("delete {}", path.display()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every step was done.
    Done,
    /// A step failed and everything done before it was undone.
    FailedRecovered { step: usize, error: String },
    /// A step failed and the undo could not finish. `recovery` names what to
    /// look at; the disk may be partly changed.
    FailedUnrecovered {
        step: usize,
        error: String,
        recovery: String,
    },
}

/// Run `ops`. `between` is called after each successful step with its number;
/// tests use it to disturb the disk.
pub fn run(ops: &[Op], mut between: impl FnMut(usize)) -> Outcome {
    let note = write_note(ops);
    let mut journal = Journal::default();
    for (step, op) in ops.iter().enumerate() {
        let result = match op {
            Op::Create {
                path,
                text,
                overwrite,
            } => journal
                .create(path, *overwrite, false)
                .and_then(|_| match text {
                    Some(text) => std::fs::write(path, text).map(|()| true),
                    None => Ok(true),
                }),
            Op::Rename {
                from,
                to,
                overwrite,
            } => journal.rename(from, to, *overwrite, false),
            Op::Delete { path, recursive } => journal.delete(path, *recursive, false),
        };
        if let Err(error) = result {
            return match journal.rollback() {
                Ok(()) => {
                    remove_note(&note);
                    Outcome::FailedRecovered {
                        step,
                        error: format!("{}: {error}", op.describe()),
                    }
                }
                // The note stays: it is the record of what was in progress.
                Err(unrecovered) => Outcome::FailedUnrecovered {
                    step,
                    error: format!("{}: {error}", op.describe()),
                    recovery: format!(
                        "{unrecovered}{}",
                        note.as_ref()
                            .map(|n| format!("; the operation is recorded in {}", n.display()))
                            .unwrap_or_default()
                    ),
                },
            };
        }
        between(step);
    }
    journal.commit();
    remove_note(&note);
    Outcome::Done
}

fn first_directory(ops: &[Op]) -> Option<PathBuf> {
    let path = match ops.first()? {
        Op::Create { path, .. } | Op::Delete { path, .. } => path,
        Op::Rename { from, .. } => from,
    };
    path.parent().map(Path::to_path_buf)
}

fn write_note(ops: &[Op]) -> Option<PathBuf> {
    let dir = first_directory(ops).filter(|dir| dir.is_dir())?;
    let note = dir.join(format!(
        "{NOTE_PREFIX}{}-{}.txt",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let text = std::iter::once("An extension's file operation was in progress. Entries it set aside are in `.loom-edit-*` folders next to the files it touched.".to_owned())
        .chain(ops.iter().map(Op::describe))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&note, text).ok().map(|()| note)
}

fn remove_note(note: &Option<PathBuf>) {
    if let Some(note) = note {
        let _ = std::fs::remove_file(note);
    }
}

/// Notes left behind by runs that never finished, directly inside `root`
/// and one level below.
pub fn unfinished_notes(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut look = |dir: &Path| {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with(NOTE_PREFIX) {
                    found.push(entry.path());
                }
            }
        }
    };
    look(root);
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten().filter(|e| e.path().is_dir()) {
            look(&entry.path());
        }
    }
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("loom-fileops-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn steps_run_in_order_and_leave_no_leftovers() {
        let dir = folder("ok");
        fs::write(dir.join("old.txt"), "old").unwrap();
        fs::write(dir.join("gone.txt"), "x").unwrap();
        let ops = [
            Op::Create {
                path: dir.join("new.txt"),
                text: Some("hello".into()),
                overwrite: false,
            },
            Op::Rename {
                from: dir.join("old.txt"),
                to: dir.join("moved/old.txt"),
                overwrite: false,
            },
            Op::Delete {
                path: dir.join("gone.txt"),
                recursive: false,
            },
        ];
        assert_eq!(run(&ops, |_| {}), Outcome::Done);
        assert_eq!(fs::read_to_string(dir.join("new.txt")).unwrap(), "hello");
        assert_eq!(
            fs::read_to_string(dir.join("moved/old.txt")).unwrap(),
            "old"
        );
        assert_eq!(
            entries(&dir),
            ["moved", "new.txt"],
            "no backups or notes remain"
        );
    }

    #[test]
    fn a_failing_step_undoes_everything_before_it() {
        let dir = folder("rollback");
        fs::write(dir.join("a.txt"), "A").unwrap();
        fs::write(dir.join("keep.txt"), "K").unwrap();
        let ops = [
            Op::Create {
                path: dir.join("created.txt"),
                text: None,
                overwrite: false,
            },
            Op::Rename {
                from: dir.join("a.txt"),
                to: dir.join("b.txt"),
                overwrite: false,
            },
            Op::Delete {
                path: dir.join("keep.txt"),
                recursive: false,
            },
            // Fails: the target of the third step's sibling does not exist.
            Op::Rename {
                from: dir.join("missing.txt"),
                to: dir.join("c.txt"),
                overwrite: false,
            },
        ];
        let outcome = run(&ops, |_| {});
        assert!(
            matches!(outcome, Outcome::FailedRecovered { step: 3, .. }),
            "{outcome:?}"
        );
        assert_eq!(entries(&dir), ["a.txt", "keep.txt"]);
        assert_eq!(fs::read_to_string(dir.join("keep.txt")).unwrap(), "K");
    }

    #[test]
    fn an_existing_target_is_refused_unless_overwrite_is_asked_for() {
        let dir = folder("overwrite");
        fs::write(dir.join("a.txt"), "A").unwrap();
        let refuse = [Op::Create {
            path: dir.join("a.txt"),
            text: Some("new".into()),
            overwrite: false,
        }];
        assert!(matches!(
            run(&refuse, |_| {}),
            Outcome::FailedRecovered { .. }
        ));
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "A");
        let replace = [Op::Create {
            path: dir.join("a.txt"),
            text: Some("new".into()),
            overwrite: true,
        }];
        assert_eq!(run(&replace, |_| {}), Outcome::Done);
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "new");
        assert_eq!(entries(&dir), ["a.txt"]);
    }

    #[test]
    fn a_directory_needs_recursive_to_be_deleted_when_it_has_content() {
        let dir = folder("recursive");
        fs::create_dir(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/file"), "x").unwrap();
        let plain = [Op::Delete {
            path: dir.join("sub"),
            recursive: false,
        }];
        assert!(matches!(
            run(&plain, |_| {}),
            Outcome::FailedRecovered { .. }
        ));
        assert!(dir.join("sub/file").exists());
        let deep = [Op::Delete {
            path: dir.join("sub"),
            recursive: true,
        }];
        assert_eq!(run(&deep, |_| {}), Outcome::Done);
        assert!(!dir.join("sub").exists());
    }

    #[test]
    fn when_the_undo_cannot_finish_the_recovery_place_is_named_and_the_note_stays() {
        let dir = folder("unrecovered");
        fs::write(dir.join("a.txt"), "A").unwrap();
        fs::write(dir.join("b.txt"), "B").unwrap();
        let ops = [
            Op::Rename {
                from: dir.join("a.txt"),
                to: dir.join("c.txt"),
                overwrite: false,
            },
            // Step 1 fails because b.txt was removed under the run's feet.
            Op::Rename {
                from: dir.join("b.txt"),
                to: dir.join("d.txt"),
                overwrite: false,
            },
        ];
        // Between the steps something else takes the renamed file's place back.
        let outcome = run(&ops, |step| {
            if step == 0 {
                fs::write(dir.join("a.txt"), "someone else's file").unwrap();
                fs::remove_file(dir.join("b.txt")).unwrap();
            }
        });
        let Outcome::FailedUnrecovered { step, recovery, .. } = &outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(*step, 1);
        assert!(recovery.contains(NOTE_PREFIX), "{recovery}");
        let notes = unfinished_notes(&dir);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(fs::read_to_string(&notes[0]).unwrap().contains("rename"));
        // The other writer's file was not overwritten by the undo.
        assert_eq!(
            fs::read_to_string(dir.join("a.txt")).unwrap(),
            "someone else's file"
        );
        assert_eq!(fs::read_to_string(dir.join("c.txt")).unwrap(), "A");
    }

    #[test]
    fn notes_are_found_in_the_folder_and_one_level_below() {
        let dir = folder("notes");
        fs::create_dir(dir.join("sub")).unwrap();
        fs::write(dir.join(format!("{NOTE_PREFIX}1-1.txt")), "x").unwrap();
        fs::write(dir.join(format!("sub/{NOTE_PREFIX}2-2.txt")), "x").unwrap();
        assert_eq!(unfinished_notes(&dir).len(), 2);
    }
}
