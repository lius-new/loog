use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use super::runtime::GitRuntime;
use crate::features::git::error::{GitError, GitErrorKind, GitResult};
use loog_workspace_api::ResourceRef;

#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Generous enough for `status` or `blame` in a large repository.
const READ_ONLY_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug)]
pub struct GitCommand {
    pub cwd: Option<ResourceRef>,
    pub args: Vec<OsString>,
    pub stdin: Vec<u8>,
    pub read_only: bool,
    pub allow_prompt: bool,
    pub timeout: Option<Duration>,
    /// Exit codes besides 0 that report a result rather than a failure.
    pub accepted_exit_codes: Vec<i32>,
}

impl GitCommand {
    pub fn new() -> Self {
        Self {
            cwd: None,
            args: Vec::new(),
            stdin: Vec::new(),
            read_only: false,
            allow_prompt: false,
            timeout: None,
            accepted_exit_codes: Vec::new(),
        }
    }

    pub fn cwd(mut self, cwd: impl Into<ResourceRef>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn stdin(mut self, value: impl Into<Vec<u8>>) -> Self {
        self.stdin = value.into();
        self
    }

    /// A query that changes nothing. It also gets a default timeout so a hung
    /// process cannot stall status polling for good; an explicit `timeout`
    /// still wins.
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self.timeout.get_or_insert(READ_ONLY_TIMEOUT);
        self
    }

    pub fn allow_prompt(mut self) -> Self {
        self.allow_prompt = true;
        self
    }

    /// Treats exit `code` as a normal result, like `git diff --check` does
    /// when it finds something.
    pub fn accept_exit_code(mut self, code: i32) -> Self {
        self.accepted_exit_codes.push(code);
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

#[derive(Debug)]
pub struct GitOutput {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl GitOutput {
    pub fn stdout_text(&self) -> GitResult<String> {
        String::from_utf8(self.stdout.clone()).map_err(|error| {
            GitError::new(GitErrorKind::InvalidOutput, "Git output was not UTF-8.")
                .with_detail(error.to_string())
        })
    }
}

#[derive(Clone, Debug)]
pub struct GitCommandRunner {
    runtime: GitRuntime,
}

impl GitCommandRunner {
    pub fn new(runtime: GitRuntime) -> Self {
        Self { runtime }
    }

    pub fn git_runtime(&self) -> &GitRuntime {
        &self.runtime
    }

    pub fn run(&self, command: GitCommand) -> GitResult<GitOutput> {
        self.run_cancellable(command, &CancellationToken::default())
    }

    pub fn run_cancellable(
        &self,
        command: GitCommand,
        cancellation: &CancellationToken,
    ) -> GitResult<GitOutput> {
        if cancellation.is_cancelled() {
            return Err(GitError::new(
                GitErrorKind::Cancelled,
                "Git operation was cancelled.",
            ));
        }
        // A repository in a provider environment is named by an encoded path; its
        // `git` runs there, never here.
        if let Some(path) = command.cwd.as_ref().and_then(ResourceRef::local_path)
            && let Some(target) = super::remote::locate(path)
        {
            return super::remote::run(&target?, &command, cancellation);
        }
        let mut process = Command::new(&self.runtime.executable);
        if let Some(cwd) = &command.cwd {
            let path = cwd.local_path().ok_or_else(|| {
                GitError::new(
                    GitErrorKind::Unsupported,
                    "This Git executor only supports local resources.",
                )
            })?;
            process.current_dir(path);
        }
        if command.read_only {
            process.arg("--no-optional-locks");
        }
        process.args(&command.args);
        process
            .env("GIT_PAGER", "cat")
            .env("PAGER", "cat")
            // No terminal to edit in: `:` makes Git keep its prepared
            // message (e.g. on `rebase --continue`) without starting an
            // editor, where any other editor would hang the command.
            .env("GIT_EDITOR", ":")
            .env("GIT_CONFIG_COUNT", "3")
            .env("GIT_CONFIG_KEY_0", "color.ui")
            .env("GIT_CONFIG_VALUE_0", "false")
            .env("GIT_CONFIG_KEY_1", "diff.external")
            .env("GIT_CONFIG_VALUE_1", "")
            .env("GIT_CONFIG_KEY_2", "core.quotepath")
            .env("GIT_CONFIG_VALUE_2", "false")
            .stdin(if command.stdin.is_empty() {
                Stdio::null()
            } else {
                Stdio::piped()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if self.runtime.source == crate::features::git::types::GitRuntimeSource::Managed {
            let mut paths = vec![
                self.runtime.root.join("bin"),
                self.runtime.root.join("cmd"),
                self.runtime.root.join("mingw64").join("bin"),
                // Newer MinGit releases ship UCRT builds under `ucrt64`.
                self.runtime.root.join("ucrt64").join("bin"),
                self.runtime.root.join("usr").join("bin"),
            ];
            if let Some(existing) = std::env::var_os("PATH") {
                paths.extend(std::env::split_paths(&existing));
            }
            if let Ok(path) = std::env::join_paths(paths) {
                process.env("PATH", path);
            } else {
                process.env("PATH", self.runtime.root.join("bin"));
            }
            let exec_path = self.runtime.root.join("libexec").join("git-core");
            if exec_path.is_dir() {
                process.env("GIT_EXEC_PATH", exec_path);
            }
            let templates = self
                .runtime
                .root
                .join("share")
                .join("git-core")
                .join("templates");
            if templates.is_dir() {
                process.env("GIT_TEMPLATE_DIR", templates);
            }
        }
        if !command.allow_prompt {
            process
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("GCM_INTERACTIVE", "Never");
        }
        hide_console_window(&mut process);

        let mut child = process.spawn().map_err(|error| {
            GitError::new(
                GitErrorKind::RuntimeUnavailable,
                format!("Could not start Git: {error}"),
            )
        })?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&command.stdin)?;
        }
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdout_reader = thread::spawn(move || read_all(stdout));
        let stderr_reader = thread::spawn(move || read_all(stderr));
        let started = Instant::now();
        let status = loop {
            if cancellation.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(GitError::new(
                    GitErrorKind::Cancelled,
                    "Git operation was cancelled.",
                ));
            }
            if command
                .timeout
                .is_some_and(|timeout| started.elapsed() >= timeout)
            {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(GitError::new(
                    GitErrorKind::TimedOut,
                    "Git operation timed out.",
                ));
            }
            if let Some(status) = child.try_wait()? {
                break status;
            }
            thread::sleep(Duration::from_millis(20));
        };
        let stdout = stdout_reader
            .join()
            .map_err(|_| GitError::new(GitErrorKind::Io, "Git stdout reader failed."))??;
        let stderr = stderr_reader
            .join()
            .map_err(|_| GitError::new(GitErrorKind::Io, "Git stderr reader failed."))??;
        if !status.success()
            && !status
                .code()
                .is_some_and(|code| command.accepted_exit_codes.contains(&code))
        {
            return Err(GitError::from_stderr(&stderr, status.code()));
        }
        Ok(GitOutput {
            code: status.code(),
            stdout,
            stderr,
        })
    }
}

fn read_all<R: Read>(reader: Option<R>) -> GitResult<Vec<u8>> {
    let mut output = Vec::new();
    if let Some(mut reader) = reader {
        reader.read_to_end(&mut output)?;
    }
    Ok(output)
}

#[cfg(windows)]
pub(crate) fn hide_console_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub(crate) fn hide_console_window(_command: &mut Command) {}

pub fn nul_pathspec(paths: &[PathBuf]) -> Vec<u8> {
    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(os_str_bytes(path.as_os_str()).as_ref());
        input.push(0);
    }
    input
}

#[cfg(unix)]
fn os_str_bytes(value: &OsStr) -> Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt;
    Cow::Borrowed(value.as_bytes())
}

#[cfg(not(unix))]
fn os_str_bytes(value: &OsStr) -> Cow<'_, [u8]> {
    Cow::Owned(value.to_string_lossy().as_bytes().to_vec())
}
