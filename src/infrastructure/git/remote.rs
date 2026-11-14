//! Git for repositories in provider environments.
//!
//! A repository there is named by an encoded path (`features::git::location`). The
//! command runner and the few direct file reads ask this module where a path lives and,
//! for an environment, hand the work to the installed [`RemoteGit`], which the
//! application backs with the provider's process and file abilities. Nothing here
//! knows what kind of environment it is.
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock};

use super::command::{CancellationToken, GitCommand, GitOutput};
use crate::features::git::error::{GitError, GitErrorKind, GitResult};
use crate::features::git::location;
use loog_workspace_api::EnvironmentKey;

#[cfg(test)]
use crate::features::git::location::path_for;
#[cfg(test)]
use crate::features::git::location::token_of;

/// A directory in an environment: the environment and its own absolute path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteTarget {
    pub environment: EnvironmentKey,
    pub path: String,
}

/// What an environment must do for Git. Implementations enforce the capability,
/// trust and connection of the environment and report `RuntimeUnavailable` or
/// `PermissionDenied` when it cannot.
pub trait RemoteGit: Send + Sync {
    /// Run `command` (its arguments, input and limits) with `target` as the working
    /// directory, in the environment.
    fn run(
        &self,
        target: &RemoteTarget,
        command: &GitCommand,
        cancellation: &CancellationToken,
    ) -> GitResult<GitOutput>;

    /// The whole content of a regular file, `None` when it does not exist, within
    /// `limit` bytes.
    fn read_file(&self, target: &RemoteTarget, limit: usize) -> GitResult<Option<Vec<u8>>>;

    /// What `target` is, `None` when it does not exist.
    fn stat(&self, target: &RemoteTarget) -> GitResult<Option<RemoteStat>>;

    /// The names of the directories directly inside `target`, at most `limit`.
    fn list_directories(&self, target: &RemoteTarget, limit: usize) -> GitResult<Vec<String>>;

    /// The names of everything directly inside `target`, at most `limit`.
    fn list_names(&self, target: &RemoteTarget, limit: usize) -> GitResult<Vec<String>>;
}

/// The kind of a path in an environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteStat {
    pub directory: bool,
    pub file: bool,
    pub size: u64,
}

static INSTALLED: RwLock<Option<Arc<dyn RemoteGit>>> = RwLock::new(None);

/// Make `remote` serve every environment repository from now on.
pub fn install(remote: Arc<dyn RemoteGit>) {
    *INSTALLED.write().unwrap_or_else(PoisonError::into_inner) = Some(remote);
}

/// Remove the installed implementation.
pub fn uninstall() {
    *INSTALLED.write().unwrap_or_else(PoisonError::into_inner) = None;
}

fn installed() -> GitResult<Arc<dyn RemoteGit>> {
    INSTALLED
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .ok_or_else(|| {
            GitError::new(
                GitErrorKind::RuntimeUnavailable,
                "No environment is available for this repository.",
            )
        })
}

/// Where `path` lives: `None` for a local path, the target for an environment path,
/// and an error for an environment path whose environment this process never named.
pub fn locate(path: &Path) -> Option<GitResult<RemoteTarget>> {
    let remote = location::decode(path)?;
    Some(match location::environment_of(&remote.token) {
        Some(environment) => Ok(RemoteTarget {
            environment,
            path: remote.path,
        }),
        None => Err(GitError::new(
            GitErrorKind::RuntimeUnavailable,
            "The environment of this repository is not known.",
        )),
    })
}

/// Whether `path` names a directory in an environment.
pub fn is_remote(path: &Path) -> bool {
    location::decode(path).is_some()
}

thread_local! {
    static PROBLEM: std::cell::RefCell<Option<GitError>> = const { std::cell::RefCell::new(None) };
}

/// Note why an environment repository could not be read during this scan, so the scan can
/// go on with the others and still say it. The first one is kept.
pub fn report_problem(error: GitError) {
    PROBLEM.with(|problem| {
        problem.borrow_mut().get_or_insert(error);
    });
}

/// What [`report_problem`] noted on this thread since the last call.
pub fn take_problem() -> Option<GitError> {
    PROBLEM.with(|problem| problem.borrow_mut().take())
}

/// Run `command` in `target`.
pub fn run(
    target: &RemoteTarget,
    command: &GitCommand,
    cancellation: &CancellationToken,
) -> GitResult<GitOutput> {
    installed()?.run(target, command, cancellation)
}

/// The content of the file at `path`, which must be an environment path.
pub fn read_file(path: &Path, limit: usize) -> GitResult<Option<Vec<u8>>> {
    let target = locate(path)
        .ok_or_else(|| GitError::new(GitErrorKind::Unsupported, "Not an environment path."))??;
    installed()?.read_file(&target, limit)
}

/// What the environment path `path` is, `None` when it does not exist.
pub fn stat(path: &Path) -> GitResult<Option<RemoteStat>> {
    let target = locate(path)
        .ok_or_else(|| GitError::new(GitErrorKind::Unsupported, "Not an environment path."))??;
    installed()?.stat(&target)
}

/// The names of everything directly inside the environment path `path`.
pub fn list_names(path: &Path, limit: usize) -> GitResult<Vec<String>> {
    let target = locate(path)
        .ok_or_else(|| GitError::new(GitErrorKind::Unsupported, "Not an environment path."))??;
    installed()?.list_names(&target, limit)
}

/// The names of the directories directly inside the environment path `path`.
pub fn list_directories(path: &Path, limit: usize) -> GitResult<Vec<String>> {
    let target = locate(path)
        .ok_or_else(|| GitError::new(GitErrorKind::Unsupported, "Not an environment path."))??;
    installed()?.list_directories(&target, limit)
}

/// The arguments of `command` as text, with Git's own read-only switch first. An
/// environment's command line is text; a name that is not is refused, not altered.
pub fn arguments(command: &GitCommand) -> GitResult<Vec<String>> {
    let mut args = Vec::with_capacity(command.args.len() + 1);
    if command.read_only {
        args.push("--no-optional-locks".into());
    }
    for arg in &command.args {
        let text = arg.to_str().ok_or_else(|| {
            GitError::new(
                GitErrorKind::Unsupported,
                "This environment requires UTF-8 Git arguments.",
            )
        })?;
        // An argument that names a place in an environment, such as the folder of a new
        // worktree, is given in the environment's own spelling.
        args.push(match location::decode(Path::new(text)) {
            Some(remote) => remote.path,
            None => text.to_owned(),
        });
    }
    Ok(args)
}

/// The variables every Git process runs with, here as for a local one: no pager, no
/// editor, no color, no external diff, quoted paths off. Unlike a local one it never
/// prompts, whatever the command asks for: an environment has no screen to prompt on,
/// so a command that needs a password fails with Git's own message.
pub fn environment_variables(_command: &GitCommand) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = [
        ("GIT_PAGER", "cat"),
        ("PAGER", "cat"),
        ("GIT_EDITOR", ":"),
        ("GIT_CONFIG_COUNT", "3"),
        ("GIT_CONFIG_KEY_0", "color.ui"),
        ("GIT_CONFIG_VALUE_0", "false"),
        ("GIT_CONFIG_KEY_1", "diff.external"),
        ("GIT_CONFIG_VALUE_1", ""),
        ("GIT_CONFIG_KEY_2", "core.quotepath"),
        ("GIT_CONFIG_VALUE_2", "false"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect();
    env.push(("GIT_TERMINAL_PROMPT".into(), "0".into()));
    env.push(("GCM_INTERACTIVE".into(), "Never".into()));
    env
}

#[cfg(test)]
pub(crate) mod fake {
    //! A stand-in environment for tests: it runs the real `git` against a local
    //! directory that plays the part of the environment's filesystem.
    use super::*;
    use std::process::{Command, Stdio};
    use std::sync::Mutex;

    pub static SERIAL: Mutex<()> = Mutex::new(());

    pub struct LocalBacked {
        /// The directory the environment's `/` stands for.
        pub root: PathBuf,
        pub commands: Mutex<Vec<Vec<String>>>,
    }

    /// The spelling Git prints for a directory. The fake takes its root off Git's output as
    /// text, so a root spelled differently (a symlinked `/var` on macOS, an 8.3 short name
    /// such as `RUNNER~1` in a Windows temp directory) would never match and the
    /// environment's paths would come back with the whole local path in front.
    pub fn real_path(path: &Path) -> PathBuf {
        let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let text = real.to_string_lossy();
        match text.strip_prefix(r"\\?\") {
            Some(plain) => PathBuf::from(plain),
            None => real,
        }
    }

    impl LocalBacked {
        fn dir(&self, path: &str) -> PathBuf {
            let mut dir = self.root.clone();
            for part in path.split('/').filter(|part| !part.is_empty()) {
                dir.push(part);
            }
            dir
        }
    }

    impl RemoteGit for LocalBacked {
        fn run(
            &self,
            target: &RemoteTarget,
            command: &GitCommand,
            _: &CancellationToken,
        ) -> GitResult<GitOutput> {
            let args = arguments(command)?;
            self.commands.lock().unwrap().push(args.clone());
            // An absolute argument is a place in the environment, whose root is `self.root`.
            let local_root = self.root.to_string_lossy().replace("\\", "/");
            let local_args: Vec<String> = args
                .iter()
                .map(|arg| {
                    if arg.starts_with('/') {
                        format!("{local_root}{arg}")
                    } else {
                        arg.clone()
                    }
                })
                .collect();
            let mut process = Command::new("git");
            process
                .current_dir(self.dir(&target.path))
                .args(&local_args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            for (key, value) in environment_variables(command) {
                process.env(key, value);
            }
            let mut child = process.spawn()?;
            {
                use std::io::Write;
                if let Some(mut stdin) = child.stdin.take() {
                    stdin.write_all(&command.stdin)?;
                }
            }
            let mut output = child.wait_with_output()?;
            // A real environment prints its own paths; this one prints this machine's,
            // so the part that stands for the environment's `/` is taken off.
            let prefix = self.root.to_string_lossy().replace('\\', "/");
            let printed = String::from_utf8_lossy(&output.stdout).replace(&prefix, "");
            output.stdout = printed.into_bytes();
            if !output.status.success()
                && !output
                    .status
                    .code()
                    .is_some_and(|code| command.accepted_exit_codes.contains(&code))
            {
                return Err(GitError::from_stderr(&output.stderr, output.status.code()));
            }
            Ok(GitOutput {
                code: output.status.code(),
                stdout: output.stdout,
                stderr: output.stderr,
            })
        }

        fn read_file(&self, target: &RemoteTarget, limit: usize) -> GitResult<Option<Vec<u8>>> {
            match std::fs::read(self.dir(&target.path)) {
                Ok(bytes) if bytes.len() <= limit => Ok(Some(bytes)),
                Ok(_) => Err(GitError::new(GitErrorKind::Unsupported, "File too large.")),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error.into()),
            }
        }

        fn stat(&self, target: &RemoteTarget) -> GitResult<Option<RemoteStat>> {
            match std::fs::metadata(self.dir(&target.path)) {
                Ok(meta) => Ok(Some(RemoteStat {
                    directory: meta.is_dir(),
                    file: meta.is_file(),
                    size: meta.len(),
                })),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error.into()),
            }
        }

        fn list_names(&self, target: &RemoteTarget, limit: usize) -> GitResult<Vec<String>> {
            let mut names: Vec<String> = std::fs::read_dir(self.dir(&target.path))?
                .flatten()
                .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
                .collect();
            names.sort();
            names.truncate(limit);
            Ok(names)
        }

        fn list_directories(&self, target: &RemoteTarget, limit: usize) -> GitResult<Vec<String>> {
            let mut names: Vec<String> = std::fs::read_dir(self.dir(&target.path))?
                .flatten()
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
                .collect();
            names.sort();
            names.truncate(limit);
            Ok(names)
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn an_environment_keeps_one_token_and_a_path_finds_it_again() {
        let first = environment_key("registry-one");
        let second = environment_key("registry-two");
        assert_eq!(token_of(&first), token_of(&first));
        assert_ne!(token_of(&first), token_of(&second));
        let path = path_for(&first, "/srv/repo");
        let target = locate(&path.join("src")).unwrap().unwrap();
        assert_eq!(target.environment, first);
        assert_eq!(target.path, "/srv/repo/src");
        assert!(locate(Path::new("some/local/dir")).is_none());
        assert!(is_remote(&path) && !is_remote(Path::new("some/local/dir")));
    }

    #[test]
    fn an_environment_path_nobody_registered_is_an_error_not_a_local_path() {
        let stranger = location::encode("0000-feed", "/x");
        let error = locate(&stranger).unwrap().unwrap_err();
        assert_eq!(error.kind, GitErrorKind::RuntimeUnavailable);
    }

    /// An installed stand-in environment whose filesystem is the parent of a fresh local
    /// repository, so the repository is at `/<name>` there. Uninstalls on drop.
    pub(crate) struct Environment {
        _serial: std::sync::MutexGuard<'static, ()>,
        pub key: EnvironmentKey,
        pub fake: Arc<fake::LocalBacked>,
        /// The repository as the Git model spells it.
        pub repository: PathBuf,
        pub local: PathBuf,
        pub backend: super::super::backend::CliGitBackend,
    }

    impl Drop for Environment {
        fn drop(&mut self) {
            uninstall();
        }
    }

    pub(crate) fn environment(name: &str) -> Option<Environment> {
        let serial = fake::SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let (local, backend) = super::super::backend::tests::repository()?;
        let fake = Arc::new(fake::LocalBacked {
            root: fake::real_path(local.parent()?),
            commands: Default::default(),
        });
        install(fake.clone());
        let key = environment_key(name);
        let repository = path_for(&key, &format!("/{}", local.file_name()?.to_str()?));
        Some(Environment {
            _serial: serial,
            key,
            fake,
            repository,
            local,
            backend,
        })
    }

    fn environment_key(name: &str) -> EnvironmentKey {
        EnvironmentKey::new("fixture", name).unwrap()
    }

    #[test]
    fn the_runner_sends_an_environment_path_to_the_environment() {
        let Some(env) = environment("runner-dispatch") else {
            return;
        };
        let output = env
            .backend
            .runner()
            .run(
                GitCommand::new()
                    .cwd(env.repository.clone())
                    .read_only()
                    .args(["rev-parse", "--is-inside-work-tree"]),
            )
            .unwrap();
        assert_eq!(output.stdout_text().unwrap().trim(), "true");
        assert_eq!(
            env.fake.commands.lock().unwrap().last().unwrap(),
            &["--no-optional-locks", "rev-parse", "--is-inside-work-tree"]
        );
    }

    fn git(env: &Environment, args: &[&str]) {
        env.backend
            .runner()
            .run(GitCommand::new().cwd(&env.local).args(args.iter().copied()))
            .unwrap();
    }

    #[test]
    fn an_environment_repository_is_discovered_and_its_status_read_like_a_local_one() {
        use crate::features::git::types::ChangeKind;
        let Some(env) = environment("status") else {
            return;
        };
        std::fs::write(env.local.join("a.txt"), "one\n").unwrap();
        git(&env, &["add", "a.txt"]);
        git(&env, &["commit", "-m", "first"]);
        std::fs::write(env.local.join("a.txt"), "two\n").unwrap();
        std::fs::write(env.local.join("b.txt"), "new\n").unwrap();

        let service = crate::infrastructure::git::GitService::new(env.backend.clone());
        let snapshot = service
            .refresh(std::slice::from_ref(&env.repository), None)
            .unwrap();
        let repository = snapshot.repositories.values().next().expect("repository");
        // Discovery asks in a form every Git understands: `--path-format` needs 2.31, and an
        // older Git echoes it back as if it were a name.
        assert!(
            env.fake
                .commands
                .lock()
                .unwrap()
                .iter()
                .all(|args| !args.iter().any(|arg| arg.starts_with("--path-format")))
        );
        // Identity is the environment path, and paths inside it join as usual.
        assert_eq!(repository.worktree_root, env.repository);
        assert_eq!(
            repository.files[Path::new("a.txt")].worktree,
            ChangeKind::Modified
        );
        assert_eq!(
            repository.files[Path::new("b.txt")].worktree,
            ChangeKind::Untracked
        );
        assert_eq!(
            snapshot
                .repository_for_path(&env.repository.join("a.txt"))
                .map(|found| found.id),
            Some(repository.id)
        );
    }

    #[test]
    fn diffs_and_untracked_content_of_an_environment_repository_come_from_the_environment() {
        use crate::features::git::types::{DiffContent, DiffTarget};
        let Some(env) = environment("diffs") else {
            return;
        };
        std::fs::write(
            env.local.join("a.txt"),
            "one
",
        )
        .unwrap();
        git(&env, &["add", "a.txt"]);
        git(&env, &["commit", "-m", "first"]);
        std::fs::write(
            env.local.join("a.txt"),
            "two
",
        )
        .unwrap();
        std::fs::write(
            env.local.join("b.txt"),
            "fresh
",
        )
        .unwrap();

        let diff = env
            .backend
            .parsed_full_diff(
                &env.repository,
                DiffTarget::IndexToWorktree,
                &["a.txt".into()],
            )
            .unwrap();
        assert_eq!(diff.files.len(), 1);
        let content =
            crate::infrastructure::git::service::read_untracked(&env.repository.join("b.txt"))
                .unwrap();
        assert!(
            matches!(content, DiffContent::Untracked(text) if text == "fresh
")
        );
    }

    #[test]
    fn writes_to_an_environment_repository_run_there_with_their_input() {
        use crate::features::git::types::ChangeKind;
        let Some(env) = environment("writes") else {
            return;
        };
        let backend = &env.backend;
        let a = PathBuf::from("a.txt");
        std::fs::write(
            env.local.join("a.txt"),
            "one
two
",
        )
        .unwrap();
        backend
            .stage(&env.repository, std::slice::from_ref(&a))
            .unwrap();
        let commit = backend
            .commit(
                env.repository.clone(),
                "first

body",
                false,
                false,
            )
            .unwrap();
        assert!(commit.contains("first"));
        // The message travelled as input and landed in the log of the repository.
        let log = backend
            .runner()
            .run(
                GitCommand::new()
                    .cwd(&env.local)
                    .args(["log", "-1", "--format=%B"]),
            )
            .unwrap()
            .stdout_text()
            .unwrap();
        assert_eq!(
            log.trim(),
            "first

body"
        );

        std::fs::write(
            env.local.join("a.txt"),
            "one
TWO
",
        )
        .unwrap();
        backend
            .stage(&env.repository, std::slice::from_ref(&a))
            .unwrap();
        let status = |expect_index: ChangeKind, expect_worktree: ChangeKind| {
            let snapshot = crate::infrastructure::git::GitService::new(backend.clone())
                .refresh(std::slice::from_ref(&env.repository), None)
                .unwrap();
            let file =
                snapshot.repositories.values().next().unwrap().files[Path::new("a.txt")].clone();
            assert_eq!((file.index, file.worktree), (expect_index, expect_worktree));
        };
        status(ChangeKind::Modified, ChangeKind::Unmodified);
        backend
            .unstage(&env.repository, std::slice::from_ref(&a))
            .unwrap();
        status(ChangeKind::Unmodified, ChangeKind::Modified);
        backend
            .restore_worktree(&env.repository, std::slice::from_ref(&a))
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(env.local.join("a.txt")).unwrap(),
            "one
two
"
        );

        // A patch is applied from its input.
        let patch = "--- a/a.txt
+++ b/a.txt
@@ -2,1 +2,1 @@
-two
+three
";
        backend
            .apply_zero_context_patch(&env.repository, patch, false, false)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(env.local.join("a.txt")).unwrap(),
            "one
three
"
        );
        // Untracked files are deleted by Git, there being no trash in an environment.
        std::fs::write(env.local.join("junk.txt"), "x").unwrap();
        backend
            .clean_untracked(&env.repository, &[PathBuf::from("junk.txt")])
            .unwrap();
        assert!(!env.local.join("junk.txt").exists());

        // A second commit can be taken back; the first has no parent.
        backend
            .stage(&env.repository, std::slice::from_ref(&a))
            .unwrap();
        backend
            .commit(env.repository.clone(), "second", false, false)
            .unwrap();
        let undone = backend.undo_commit(&env.repository).unwrap();
        assert!(undone.contains("second"));
    }

    #[test]
    fn history_branches_tags_stashes_and_worktrees_of_an_environment_repository() {
        let Some(env) = environment("refs") else {
            return;
        };
        let backend = &env.backend;
        let repository = env.repository.as_path();
        std::fs::write(
            env.local.join("a.txt"),
            "one
",
        )
        .unwrap();
        git(&env, &["add", "a.txt"]);
        git(&env, &["commit", "-m", "first"]);
        std::fs::write(
            env.local.join("a.txt"),
            "two
",
        )
        .unwrap();
        git(&env, &["commit", "-am", "second"]);

        let history = backend.history(repository, None, None, 0, 10).unwrap();
        assert_eq!(
            history
                .iter()
                .map(|c| c.subject.as_str())
                .collect::<Vec<_>>(),
            ["second", "first"]
        );

        backend.create_branch(repository, "feature", None).unwrap();
        let branches = backend.branches(repository).unwrap();
        assert!(branches.iter().any(|branch| branch.name == "feature"));
        // The checkout of the current branch is named by its environment path.
        let current = branches.iter().find(|branch| branch.current).unwrap();
        assert_eq!(current.worktree_path.as_deref(), Some(repository));

        backend.create_tag(repository, "v1", None).unwrap();
        assert!(
            backend
                .tags(repository)
                .unwrap()
                .iter()
                .any(|tag| tag.name == "v1")
        );

        std::fs::write(
            env.local.join("a.txt"),
            "three
",
        )
        .unwrap();
        backend
            .stash_push(repository, Some("wip"), false, false)
            .unwrap();
        assert_eq!(backend.stash_list(repository).unwrap().len(), 1);
        backend.stash_pop(repository).unwrap();

        // A worktree is created where the environment says, from an environment path.
        let sibling = repository.with_file_name("sibling-worktree");
        backend
            .add_worktree(repository, &sibling, Some("main"), false)
            .unwrap();
        let worktrees = backend.worktrees(repository).unwrap();
        assert!(
            worktrees.iter().any(|worktree| worktree.path == sibling),
            "{worktrees:?}"
        );
        let _ = std::fs::remove_dir_all(env.local.with_file_name("sibling-worktree"));
    }

    #[test]
    fn network_blame_and_clone_work_for_an_environment_repository() {
        let Some(env) = environment("network") else {
            return;
        };
        let backend = &env.backend;
        let repository = env.repository.as_path();
        std::fs::write(
            env.local.join("a.txt"),
            "one
",
        )
        .unwrap();
        git(&env, &["add", "a.txt"]);
        git(&env, &["commit", "-m", "first"]);
        let bare = env.fake.root.join(format!(
            "{}-bare.git",
            env.local.file_name().unwrap().to_str().unwrap()
        ));
        let bare_url = format!("/{}", bare.file_name().unwrap().to_str().unwrap());
        let made = std::process::Command::new("git")
            .args(["init", "--bare"])
            .arg(&bare)
            .output()
            .unwrap();
        assert!(made.status.success());

        backend.add_remote(repository, "origin", &bare_url).unwrap();
        backend
            .push(repository, Some(("origin", "main")), false)
            .unwrap();
        backend.fetch(repository, Some("origin"), true).unwrap();
        backend.pull(repository, false).unwrap();
        let remotes = backend.remotes(repository).unwrap();
        assert!(remotes.iter().any(|remote| remote.name == "origin"));

        let blame = backend.blame(repository, Path::new("a.txt")).unwrap();
        assert_eq!(blame.len(), 1);

        // A clone lands where the environment says, from the folder that holds it.
        let name = format!("{}-clone", env.local.file_name().unwrap().to_str().unwrap());
        let target = crate::features::git::location::path_for(&env.key, &format!("/{name}"));
        crate::infrastructure::git::GitService::new(backend.clone())
            .clone_repository(&bare_url, &target, &CancellationToken::default())
            .unwrap();
        assert!(env.fake.root.join(&name).join(".git").exists());
        let _ = std::fs::remove_dir_all(env.fake.root.join(&name));
        let _ = std::fs::remove_dir_all(&bare);
    }

    #[test]
    fn an_environment_that_cannot_answer_does_not_take_the_local_repositories_with_it() {
        let _serial = fake::SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        uninstall();
        let Some((local, backend)) = super::super::backend::tests::repository() else {
            return;
        };
        std::fs::write(local.join("a.txt"), "x").unwrap();
        let gone = path_for(&environment_key("disconnected"), "/srv/app");
        let service = crate::infrastructure::git::GitService::new(backend);
        for fast in [true, false] {
            let roots = [gone.clone(), local.clone()];
            let snapshot = if fast {
                service.refresh(&roots, None)
            } else {
                service.refresh(&roots[..], Some(&local))
            }
            .unwrap();
            assert_eq!(snapshot.repositories.len(), 1);
            assert_eq!(
                snapshot.repositories.values().next().unwrap().worktree_root,
                crate::infrastructure::git::repo_fs::canonical(local.clone())
            );
        }
    }

    #[test]
    fn without_an_environment_the_runner_reports_it_and_does_not_fall_back_to_local() {
        let _serial = fake::SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        uninstall();
        let Some((local, backend)) = super::super::backend::tests::repository() else {
            return;
        };
        let key = environment_key("nothing-installed");
        let remote = path_for(
            &key,
            &format!("/{}", local.file_name().unwrap().to_str().unwrap()),
        );
        let error = backend
            .runner()
            .run(GitCommand::new().cwd(remote).args(["status"]))
            .unwrap_err();
        assert_eq!(error.kind, GitErrorKind::RuntimeUnavailable);
    }

    #[test]
    fn arguments_are_text_and_the_environment_matches_a_local_run() {
        let command = GitCommand::new().read_only().args(["status", "-z"]);
        assert_eq!(
            arguments(&command).unwrap(),
            ["--no-optional-locks", "status", "-z"]
        );
        // A path in an environment is passed as the environment spells it.
        let place = path_for(&environment_key("argument-paths"), "/srv/wt/feature");
        let command = GitCommand::new()
            .args(["worktree".to_owned(), "add".to_owned()])
            .arg(place.into_os_string());
        assert_eq!(
            arguments(&command).unwrap(),
            ["worktree", "add", "/srv/wt/feature"]
        );
        let env = environment_variables(&command);
        assert!(env.contains(&("GIT_TERMINAL_PROMPT".into(), "0".into())));
        // A command that may prompt locally still cannot in an environment.
        let prompting = environment_variables(&GitCommand::new().allow_prompt());
        assert!(prompting.contains(&("GIT_TERMINAL_PROMPT".into(), "0".into())));
    }
}

#[cfg(all(test, windows))]
mod wsl_probe {
    //! A manual probe: the Git code against a real WSL distribution, to catch commands an
    //! older Git there does not know. Run with
    //! `LOOG_WSL_PROBE=<distro>:<folder> cargo test wsl_probe -- --ignored --nocapture`.
    use super::*;
    use std::process::{Command, Stdio};

    struct Wsl(String);

    impl Wsl {
        fn sh(&self, directory: &str, script: &str) -> std::io::Result<std::process::Output> {
            Command::new("wsl.exe")
                .args(["-d", &self.0, "--cd", directory, "--", "sh", "-c", script])
                .output()
        }
    }

    impl RemoteGit for Wsl {
        fn run(
            &self,
            target: &RemoteTarget,
            command: &GitCommand,
            _: &CancellationToken,
        ) -> GitResult<GitOutput> {
            let args = arguments(command)?;
            let mut process = Command::new("wsl.exe");
            process
                .args(["-d", &self.0, "--cd", &target.path, "--exec", "git"])
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = process.spawn()?;
            {
                use std::io::Write;
                if let Some(mut stdin) = child.stdin.take() {
                    stdin.write_all(&command.stdin)?;
                }
            }
            let output = child.wait_with_output()?;
            if !output.status.success()
                && !output
                    .status
                    .code()
                    .is_some_and(|code| command.accepted_exit_codes.contains(&code))
            {
                return Err(GitError::from_stderr(&output.stderr, output.status.code()));
            }
            Ok(GitOutput {
                code: output.status.code(),
                stdout: output.stdout,
                stderr: output.stderr,
            })
        }
        fn read_file(&self, target: &RemoteTarget, limit: usize) -> GitResult<Option<Vec<u8>>> {
            let out = self.sh("/", &format!("cat '{}'", target.path))?;
            if !out.status.success() {
                return Ok(None);
            }
            Ok((out.stdout.len() <= limit).then_some(out.stdout))
        }
        fn stat(&self, target: &RemoteTarget) -> GitResult<Option<RemoteStat>> {
            let p = &target.path;
            let out = self.sh(
                "/",
                &format!("if [ -d '{p}' ]; then echo d; elif [ -f '{p}' ]; then echo f; fi"),
            )?;
            let text = String::from_utf8_lossy(&out.stdout);
            Ok(match text.trim() {
                "d" => Some(RemoteStat {
                    directory: true,
                    file: false,
                    size: 0,
                }),
                "f" => Some(RemoteStat {
                    directory: false,
                    file: true,
                    size: 0,
                }),
                _ => None,
            })
        }
        fn list_directories(&self, target: &RemoteTarget, limit: usize) -> GitResult<Vec<String>> {
            let out = self.sh(&target.path, "for d in */; do echo \"${d%/}\"; done")?;
            Ok(String::from_utf8_lossy(&out.stdout)
                .lines()
                .take(limit)
                .map(str::to_owned)
                .collect())
        }
        fn list_names(&self, target: &RemoteTarget, limit: usize) -> GitResult<Vec<String>> {
            let out = self.sh(&target.path, "ls -A")?;
            Ok(String::from_utf8_lossy(&out.stdout)
                .lines()
                .take(limit)
                .map(str::to_owned)
                .collect())
        }
    }

    #[test]
    #[ignore]
    fn write_operations_against_a_real_wsl_distribution() {
        let Ok(spec) = std::env::var("LOOG_WSL_PROBE") else {
            return;
        };
        let (distro, _) = spec.split_once(':').unwrap();
        let _serial = fake::SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let wsl = Wsl(distro.to_owned());
        let folder = format!("/tmp/loog-probe-{}", std::process::id());
        let make = wsl.sh("/", &format!("rm -rf {folder} && mkdir -p {folder} && cd {folder} && git init -q -b main && git config user.name probe && git config user.email probe@example.invalid && printf 'one\ntwo\n' > a.txt")).unwrap();
        assert!(
            make.status.success(),
            "{}",
            String::from_utf8_lossy(&make.stderr)
        );
        install(Arc::new(Wsl(distro.to_owned())));
        let Some((_, backend)) = super::super::backend::tests::repository() else {
            return;
        };
        let key = EnvironmentKey::new("probe", "wsl-write").unwrap();
        let root = crate::features::git::location::path_for(&key, &folder);
        let a = PathBuf::from("a.txt");
        let mut results: Vec<(String, Result<String, GitError>)> = Vec::new();
        let mut step =
            |name: &str, result: Result<String, GitError>| results.push((name.to_owned(), result));
        step(
            "stage",
            backend
                .stage(&root, std::slice::from_ref(&a))
                .map(|()| String::new()),
        );
        step(
            "commit",
            backend
                .commit(
                    root.clone(),
                    "first

body",
                    false,
                    false,
                )
                .map(|t| t.lines().next().unwrap_or("").to_owned()),
        );
        let _ = wsl.sh(&folder, "printf 'one\nTWO\n' > a.txt; printf new > b.txt");
        let snapshot = crate::infrastructure::git::GitService::new(backend.clone())
            .refresh(std::slice::from_ref(&root), None);
        step(
            "status",
            snapshot.map(|s| {
                format!(
                    "{} files",
                    s.repositories.values().next().map_or(0, |r| r.files.len())
                )
            }),
        );
        step(
            "stage again",
            backend
                .stage(&root, std::slice::from_ref(&a))
                .map(|()| String::new()),
        );
        step(
            "unstage",
            backend
                .unstage(&root, std::slice::from_ref(&a))
                .map(|()| String::new()),
        );
        step(
            "restore",
            backend
                .restore_worktree(&root, std::slice::from_ref(&a))
                .map(|()| String::new()),
        );
        step(
            "patch",
            backend
                .apply_zero_context_patch(
                    &root,
                    "--- a/a.txt
+++ b/a.txt
@@ -2,1 +2,1 @@
-two
+three
",
                    false,
                    false,
                )
                .map(|()| String::new()),
        );
        step(
            "clean",
            backend
                .clean_untracked(&root, &[PathBuf::from("b.txt")])
                .map(|()| String::new()),
        );
        step(
            "stash push",
            backend.stash_push(&root, Some("wip"), false, false),
        );
        step("stash pop", backend.stash_pop(&root).map(|_| String::new()));
        step(
            "create branch",
            backend
                .create_branch(&root, "feature", None)
                .map(|()| String::new()),
        );
        step(
            "switch",
            backend.switch(&root, "main").map(|()| String::new()),
        );
        step(
            "tag",
            backend
                .create_tag(&root, "v1", None)
                .map(|()| String::new()),
        );
        step(
            "worktree add",
            backend
                .add_worktree(
                    &root,
                    &root.with_file_name(format!("loog-probe-{}-wt", std::process::id())),
                    Some("feature"),
                    false,
                )
                .map(|()| String::new()),
        );
        step(
            "worktrees",
            backend.worktrees(&root).map(|v| v.len().to_string()),
        );
        let _ = wsl.sh(
            "/",
            &format!("git -C {folder} worktree prune; rm -rf {folder} {folder}-wt"),
        );
        uninstall();
        for (name, result) in results {
            match result {
                Ok(text) => println!("ok   {name}: {text}"),
                Err(error) => println!(
                    "FAIL {name}: {:?} {}",
                    error.kind,
                    error
                        .detail
                        .clone()
                        .unwrap_or_default()
                        .lines()
                        .last()
                        .unwrap_or_default()
                ),
            }
        }
    }

    #[test]
    #[ignore]
    fn the_git_code_against_a_real_wsl_distribution() {
        let Ok(spec) = std::env::var("LOOG_WSL_PROBE") else {
            return;
        };
        let (distro, folder) = spec.split_once(':').unwrap();
        let _serial = fake::SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        install(Arc::new(Wsl(distro.to_owned())));
        let Some((_, backend)) = super::super::backend::tests::repository() else {
            return;
        };
        let key = EnvironmentKey::new("probe", "wsl").unwrap();
        let root = crate::features::git::location::path_for(&key, folder);
        let service = crate::infrastructure::git::GitService::new(backend);
        for fast in [true, false] {
            let result = if fast {
                service.refresh(std::slice::from_ref(&root), None)
            } else {
                service.refresh(std::slice::from_ref(&root), Some(&root.join("README.md")))
            };
            match result {
                Ok(snapshot) => {
                    println!(
                        "fast={fast}: repositories={} error={:?}",
                        snapshot.repositories.len(),
                        snapshot.last_error
                    );
                    for repository in snapshot.repositories.values() {
                        println!(
                            "  root={:?} branch={:?} files={}",
                            repository.worktree_root,
                            repository.head,
                            repository.files.len()
                        );
                    }
                }
                Err(error) => println!("fast={fast}: ERROR {error:?}"),
            }
        }
        let history = service.backend().history(&root, None, None, 0, 3);
        println!("history: {} commits", history.as_ref().map_or(0, Vec::len));
        let backend = service.backend();
        let head = history
            .as_ref()
            .ok()
            .and_then(|h| h.first())
            .map(|c| c.oid.clone())
            .unwrap_or_default();
        let report = |name: &str, result: Result<String, GitError>| match result {
            Ok(text) => println!("ok   {name}: {text}"),
            Err(error) => println!(
                "FAIL {name}: {:?} {}",
                error.kind,
                error
                    .detail
                    .clone()
                    .unwrap_or_default()
                    .lines()
                    .last()
                    .unwrap_or_default()
            ),
        };
        report(
            "branches",
            backend.branches(&root).map(|v| v.len().to_string()),
        );
        report("tags", backend.tags(&root).map(|v| v.len().to_string()));
        report(
            "remotes",
            backend.remotes(&root).map(|v| v.len().to_string()),
        );
        report(
            "stash_list",
            backend.stash_list(&root).map(|v| v.len().to_string()),
        );
        report(
            "worktrees",
            backend.worktrees(&root).map(|v| v.len().to_string()),
        );
        report(
            "commit_details",
            backend
                .commit_details(&root, &head)
                .map(|d| format!("{d:?}").chars().take(60).collect::<String>()),
        );
        report(
            "blame",
            backend
                .blame(&root, std::path::Path::new("README.md"))
                .map(|v| v.len().to_string()),
        );
        report(
            "commit_files",
            backend
                .commit_files(&root, &head)
                .map(|v| v.len().to_string()),
        );
        report(
            "conflict_stages",
            backend.conflict_stages(&root).map(|v| v.len().to_string()),
        );
        report(
            "parsed_full_diff",
            backend
                .parsed_full_diff(
                    &root,
                    crate::features::git::types::DiffTarget::HeadToWorktree,
                    &[std::path::PathBuf::from("README.md")],
                )
                .map(|d| d.files.len().to_string()),
        );
        report(
            "show",
            backend.show(&root, &head).map(|t| t.len().to_string()),
        );
        uninstall();
    }
}
