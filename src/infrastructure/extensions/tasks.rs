//! Local tool runs for extensions (design §6.4, §9).
//!
//! A task is a program and an argument list, never a shell command line. It
//! runs with a small allow-listed environment, in a directory inside the
//! granted root, and is owned by a kill-on-close job so that cancelling,
//! timing out or closing Loom ends the whole process tree. This is not an
//! operating system sandbox: the program can still touch whatever the user can.
use crate::infrastructure::process_job::Job;
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Output kept per stream for one task; the rest is read and thrown away.
pub const OUTPUT_BUDGET: usize = 1024 * 1024;
/// One delivered chunk is at most this long.
const CHUNK: usize = 8 * 1024;
/// How often a running task is checked for cancellation and its deadline.
const POLL: Duration = Duration::from_millis(15);

/// Environment variables passed through from Loom's own environment.
const INHERITED: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SystemRoot",
    "windir",
    "ComSpec",
    "TEMP",
    "TMP",
    "TMPDIR",
    "HOME",
    "USERPROFILE",
    "LOCALAPPDATA",
    "APPDATA",
    "ProgramData",
    "ProgramFiles",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "LANG",
    "LC_ALL",
];

/// Names an extension may set itself.
pub const SETTABLE: &[&str] = &[
    "LANG",
    "LC_ALL",
    "NO_COLOR",
    "FORCE_COLOR",
    "CI",
    "RUST_BACKTRACE",
    "CARGO_TERM_COLOR",
];

#[derive(Clone, Debug)]
pub struct Spec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Extra variables; names must be in [`SETTABLE`] (checked by the caller).
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskEvent {
    Output {
        task: u64,
        channel: Channel,
        text: String,
        truncated: bool,
    },
    /// Sent exactly once per started task.
    Exited {
        task: u64,
        code: Option<i32>,
        cancelled: bool,
        timed_out: bool,
        output_truncated: bool,
        error: Option<String>,
    },
}

type Sink = Arc<dyn Fn(TaskEvent) + Send + Sync>;

struct Running {
    cancel: Arc<AtomicBool>,
}

/// Starts, cancels and tracks tasks. Dropping it cancels what is still running.
#[derive(Clone)]
pub struct Runner {
    sink: Sink,
    running: Arc<Mutex<HashMap<u64, Running>>>,
}

impl Runner {
    pub fn new(sink: impl Fn(TaskEvent) + Send + Sync + 'static) -> Self {
        Self {
            sink: Arc::new(sink),
            running: Arc::default(),
        }
    }

    pub fn running(&self) -> usize {
        self.running.lock().unwrap().len()
    }

    /// Start `spec` as `task`. A program that cannot start is reported as an
    /// exit with an error, still exactly once.
    pub fn start(&self, task: u64, spec: Spec) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.running.lock().unwrap().insert(
            task,
            Running {
                cancel: cancel.clone(),
            },
        );
        let (sink, running) = (self.sink.clone(), self.running.clone());
        let spawned = std::thread::Builder::new()
            .name("loom-extension-task".into())
            .spawn({
                let (sink, running) = (sink.clone(), running.clone());
                move || {
                    let exit = run(task, &spec, &cancel, &sink);
                    running.lock().unwrap().remove(&task);
                    sink(exit);
                }
            });
        if let Err(error) = spawned {
            running.lock().unwrap().remove(&task);
            sink(TaskEvent::Exited {
                task,
                code: None,
                cancelled: false,
                timed_out: false,
                output_truncated: false,
                error: Some(format!("could not start a thread: {error}")),
            });
        }
    }

    /// Stop `task` and its process tree. Its exit is still reported once.
    pub fn cancel(&self, task: u64) {
        if let Some(running) = self.running.lock().unwrap().get(&task) {
            running.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn cancel_all(&self) {
        for running in self.running.lock().unwrap().values() {
            running.cancel.store(true, Ordering::Relaxed);
        }
    }
}

pub(super) fn command(spec: &Spec) -> Command {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in INHERITED {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    for (name, value) in &spec.env {
        command.env(name, value);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // no console window
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

/// Read a pipe until it closes, delivering chunks and honoring the budget.
fn pump(task: u64, channel: Channel, mut pipe: impl Read, sink: &Sink) -> bool {
    let mut kept = 0usize;
    let mut truncated = false;
    let mut buffer = vec![0u8; CHUNK];
    // A multi-byte character can straddle two reads; keep its tail for the next.
    let mut carry: Vec<u8> = Vec::new();
    loop {
        let read = match pipe.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        if kept >= OUTPUT_BUDGET {
            truncated = true;
            continue;
        }
        carry.extend_from_slice(&buffer[..read]);
        let valid = match std::str::from_utf8(&carry) {
            Ok(_) => carry.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => carry.len(),
        };
        let text = String::from_utf8_lossy(&carry[..valid]).into_owned();
        carry.drain(..valid);
        kept += text.len();
        let cut = kept > OUTPUT_BUDGET;
        let mut text = text;
        if cut {
            let mut end = text.len() - (kept - OUTPUT_BUDGET);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            truncated = true;
        }
        if !text.is_empty() || cut {
            sink(TaskEvent::Output {
                task,
                channel,
                text,
                truncated: cut,
            });
        }
    }
    truncated
}

fn run(task: u64, spec: &Spec, cancel: &Arc<AtomicBool>, sink: &Sink) -> TaskEvent {
    let failed = |error: String| TaskEvent::Exited {
        task,
        code: None,
        cancelled: false,
        timed_out: false,
        output_truncated: false,
        error: Some(error),
    };
    let mut child = match command(spec).spawn() {
        Ok(child) => child,
        Err(error) => return failed(format!("{}: {error}", spec.program)),
    };
    let mut tree = Job::adopt(&child);
    let readers: Vec<_> = [child.stdout.take().map(|pipe| (Channel::Stdout, pipe))]
        .into_iter()
        .flatten()
        .map(|(channel, pipe)| {
            let sink = sink.clone();
            std::thread::spawn(move || pump(task, channel, pipe, &sink))
        })
        .chain(child.stderr.take().map(|pipe| {
            let sink = sink.clone();
            std::thread::spawn(move || pump(task, Channel::Stderr, pipe, &sink))
        }))
        .collect();

    let deadline = Instant::now() + spec.timeout;
    let (mut cancelled, mut timed_out) = (false, false);
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {}
            Err(_) => break None,
        }
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
        } else if Instant::now() >= deadline {
            timed_out = true;
        }
        if cancelled || timed_out {
            // Closing the job ends the whole tree; the direct kill covers a refused job.
            let _ = child.kill();
            drop(tree.take());
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(POLL);
    };
    // Helpers may inherit pipe handles and outlive a normally exiting parent.
    drop(tree);
    // Join every reader (no short circuit) so none outlives the task.
    let truncated: Vec<bool> = readers
        .into_iter()
        .map(|reader| reader.join().unwrap_or(false))
        .collect();
    let output_truncated = truncated.contains(&true);
    TaskEvent::Exited {
        task,
        code,
        cancelled,
        timed_out,
        output_truncated,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{Receiver, channel};

    fn runner() -> (Runner, Receiver<TaskEvent>) {
        let (sender, events) = channel();
        let sender = Mutex::new(sender);
        (
            Runner::new(move |event| {
                let _ = sender.lock().unwrap().send(event);
            }),
            events,
        )
    }

    fn shell(script: &str) -> Spec {
        let (program, args): (&str, Vec<String>) = if cfg!(windows) {
            ("cmd", vec!["/c".into(), script.into()])
        } else {
            ("sh", vec!["-c".into(), script.into()])
        };
        Spec {
            program: program.into(),
            args,
            cwd: std::env::temp_dir(),
            env: Vec::new(),
            timeout: Duration::from_secs(30),
        }
    }

    fn sleeping() -> &'static str {
        if cfg!(windows) {
            "ping -n 30 127.0.0.1 >nul"
        } else {
            "sleep 30"
        }
    }

    /// Everything up to and including the exit.
    fn until_exit(events: &Receiver<TaskEvent>) -> Vec<TaskEvent> {
        let mut seen = Vec::new();
        loop {
            let event = events
                .recv_timeout(Duration::from_secs(20))
                .expect("the task ended");
            let done = matches!(event, TaskEvent::Exited { .. });
            seen.push(event);
            if done {
                return seen;
            }
        }
    }

    fn output(events: &[TaskEvent], wanted: Channel) -> String {
        events
            .iter()
            .filter_map(|e| match e {
                TaskEvent::Output { channel, text, .. } if *channel == wanted => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn output_arrives_in_pieces_and_the_exit_is_reported_once_with_its_code() {
        let (runner, events) = runner();
        runner.start(1, shell("echo hello&& echo oops 1>&2&& exit 3"));
        let seen = until_exit(&events);
        assert!(output(&seen, Channel::Stdout).contains("hello"));
        assert!(output(&seen, Channel::Stderr).contains("oops"));
        let TaskEvent::Exited {
            code,
            cancelled,
            timed_out,
            error,
            ..
        } = seen.last().unwrap()
        else {
            panic!()
        };
        assert_eq!(
            (*code, *cancelled, *timed_out, error.is_none()),
            (Some(3), false, false, true)
        );
        assert!(
            events.recv_timeout(Duration::from_millis(200)).is_err(),
            "nothing after the exit"
        );
        assert_eq!(runner.running(), 0);
    }

    #[test]
    fn cancelling_ends_the_whole_process_tree_promptly() {
        let (runner, events) = runner();
        let started = Instant::now();
        runner.start(2, shell(sleeping()));
        std::thread::sleep(Duration::from_millis(300));
        runner.cancel(2);
        let seen = until_exit(&events);
        assert!(started.elapsed() < Duration::from_secs(10));
        let TaskEvent::Exited {
            cancelled, code, ..
        } = seen.last().unwrap()
        else {
            panic!()
        };
        assert!(*cancelled && code.is_none());
    }

    #[test]
    fn a_task_past_its_deadline_is_stopped_and_marked() {
        let (runner, events) = runner();
        let mut spec = shell(sleeping());
        spec.timeout = Duration::from_millis(200);
        runner.start(3, spec);
        let seen = until_exit(&events);
        let TaskEvent::Exited {
            timed_out,
            cancelled,
            ..
        } = seen.last().unwrap()
        else {
            panic!()
        };
        assert!(*timed_out && !*cancelled);
    }

    #[test]
    fn a_program_that_does_not_exist_is_an_exit_with_an_error() {
        let (runner, events) = runner();
        runner.start(
            4,
            Spec {
                program: "loom-no-such-program".into(),
                ..shell("")
            },
        );
        let seen = until_exit(&events);
        let TaskEvent::Exited { error, code, .. } = seen.last().unwrap() else {
            panic!()
        };
        assert!(
            error
                .as_deref()
                .is_some_and(|e| e.contains("loom-no-such-program")),
            "{error:?}"
        );
        assert!(code.is_none());
    }

    #[test]
    fn the_environment_is_cleared_except_what_is_allowed() {
        // SAFETY: tests in this module only read this variable through the child.
        unsafe { std::env::set_var("LOOM_TASK_SECRET", "leaked") };
        let script = if cfg!(windows) {
            "echo [%LOOM_TASK_SECRET%][%NO_COLOR%]"
        } else {
            "echo [$LOOM_TASK_SECRET][$NO_COLOR]"
        };
        let (runner, events) = runner();
        let mut spec = shell(script);
        spec.env = vec![("NO_COLOR".into(), "1".into())];
        runner.start(5, spec);
        let text = output(&until_exit(&events), Channel::Stdout);
        assert!(!text.contains("leaked"), "{text}");
        assert!(text.contains("1]"), "{text}");
    }

    #[test]
    fn output_beyond_the_budget_is_cut_and_marked_but_the_task_still_finishes() {
        let (runner, events) = runner();
        // About 3 MiB of output.
        let script = if cfg!(windows) {
            "for /L %i in (1,1,60000) do @echo 0123456789012345678901234567890123456789012345678901234567890123456789"
        } else {
            "i=0; while [ $i -lt 60000 ]; do echo 0123456789012345678901234567890123456789012345678901234567890123456789; i=$((i+1)); done"
        };
        runner.start(6, shell(script));
        let seen = until_exit(&events);
        let kept = output(&seen, Channel::Stdout).len();
        assert!(kept <= OUTPUT_BUDGET, "kept {kept}");
        let TaskEvent::Exited {
            output_truncated,
            code,
            ..
        } = seen.last().unwrap()
        else {
            panic!()
        };
        assert!(*output_truncated);
        assert_eq!(*code, Some(0));
    }

    #[test]
    fn a_multibyte_character_split_across_reads_is_not_corrupted() {
        // Shell output uses the machine's code page on Windows. Feed actual
        // UTF-8 bytes one at a time so this tests the promised split-read case.
        struct Bytes(std::io::Cursor<Vec<u8>>);
        impl Read for Bytes {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                self.0.read(&mut buffer[..1])
            }
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = seen.clone();
        let sink: Sink = Arc::new(move |event| sink_seen.lock().unwrap().push(event));
        assert!(!pump(
            7,
            Channel::Stdout,
            Bytes(std::io::Cursor::new("中文测试".as_bytes().to_vec())),
            &sink
        ));
        assert_eq!(output(&seen.lock().unwrap(), Channel::Stdout), "中文测试");
    }
}
