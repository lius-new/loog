//! Git process adapters: runtime selection, the command runner, repository
//! discovery, the CLI backend and the status poller. Parsing and repository
//! state live in `features::git`; UI code never starts `git` directly.

pub mod backend;
pub mod command;
pub mod discovery;
pub mod remote;
pub mod repo_fs;
mod encoded_diff;
pub mod runtime;
pub mod service;

pub use self::backend::CliGitBackend;
pub use self::runtime::GitRuntimeManager;
pub use self::service::{GitService, PollControl};

use std::path::PathBuf;

use crate::features::git::{GitError, GitResult, GitStoreSnapshot, PollTarget};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

/// Process-wide service.  It contains only thread-safe runtime state; UI state
/// remains in a separate `State<GitStoreSnapshot>`.
pub fn service() -> GitResult<Arc<GitService>> {
    static SERVICE: OnceLock<Result<Arc<GitService>, GitError>> = OnceLock::new();
    SERVICE
        .get_or_init(|| {
            let runtime = GitRuntimeManager::default().resolve()?;
            Ok(Arc::new(GitService::new(CliGitBackend::new(runtime))))
        })
        .clone()
}

/// Resolve the runtime and poll repository status for `roots` entirely off
/// the UI thread. This keeps slow PATH/runtime checks and the first
/// repository scan from delaying the initial frame. `control` carries the
/// active file, which changes without restarting the poller.
pub fn start_polling(
    roots: Vec<PathBuf>,
    control: PollControl,
    interval: Duration,
    publish: impl Fn(&PollTarget, GitResult<GitStoreSnapshot>) + Send + 'static,
) -> PollingHandle {
    let stopped = Arc::new(AtomicBool::new(false));
    let worker_control = control.clone();
    let worker_stopped = Arc::clone(&stopped);
    let _ = thread::Builder::new()
        .name("loog-git-status".into())
        .spawn(move || match service() {
            Ok(service) => {
                service.poll(&roots, &worker_control, &worker_stopped, interval, &publish)
            }
            Err(error) => {
                if !worker_stopped.load(Ordering::Acquire) {
                    publish(&worker_control.target(), Err(error));
                }
            }
        });
    PollingHandle { control, stopped }
}

/// Stops its poller when dropped. It does not wait for the worker: the UI
/// thread must never block on a scan in flight, and a stopped poller
/// publishes nothing further.
pub struct PollingHandle {
    control: PollControl,
    stopped: Arc<AtomicBool>,
}

impl Drop for PollingHandle {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.control.notify();
    }
}
