use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::backend::{CliGitBackend, GitBackend};
use super::command::CancellationToken;
use crate::features::git::error::{GitError, GitResult};
use crate::features::git::snapshot::{
    GitStoreSnapshot, PollTarget, binary_content, collect_line_changes, repository_id,
};
use crate::features::git::types::*;

/// What the UI tells the status poller between scans. Tab changes retarget
/// line decorations and open diffs without restarting the worker.
#[derive(Clone, Debug, Default)]
pub struct PollControl {
    shared: Arc<(Mutex<PollRequest>, Condvar)>,
}

#[derive(Debug, Default)]
struct PollRequest {
    target: PollTarget,
    revision: u64,
    wake: bool,
    paused: bool,
}

impl PollControl {
    pub(super) fn target(&self) -> PollTarget {
        self.shared
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .target
            .clone()
    }
    /// Stop interval scans while the window is in the background. Resuming
    /// scans immediately, since anything may have changed meanwhile.
    pub fn set_paused(&self, paused: bool) {
        let (request, signal) = &*self.shared;
        let mut request = request.lock().unwrap_or_else(PoisonError::into_inner);
        if request.paused != paused {
            request.paused = paused;
            request.wake |= !paused;
            signal.notify_all();
        }
    }

    /// Retarget line decorations and open diffs without restarting the worker.
    pub fn set_target(&self, target: PollTarget) {
        let (request, signal) = &*self.shared;
        let mut request = request.lock().unwrap_or_else(PoisonError::into_inner);
        if request.target != target {
            request.target = target;
            request.revision += 1;
            request.wake = true;
            signal.notify_all();
        }
    }

    /// Invalidate an in-flight scan and refresh even if the window is paused.
    pub fn request_refresh(&self) {
        let (request, signal) = &*self.shared;
        let mut request = request.lock().unwrap_or_else(PoisonError::into_inner);
        request.revision += 1;
        request.wake = true;
        signal.notify_all();
    }

    /// The path for the scan about to start. That scan satisfies any pending
    /// wake request.
    fn begin_scan(&self) -> (u64, PollTarget) {
        let (request, _) = &*self.shared;
        let mut request = request.lock().unwrap_or_else(PoisonError::into_inner);
        request.wake = false;
        (request.revision, request.target.clone())
    }

    fn is_current(&self, revision: u64) -> bool {
        self.shared
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .revision
            == revision
    }

    /// Sleep until `interval` passes or a scan is requested; while paused,
    /// only a request ends the wait. Returns false once `stopped` is set.
    fn wait(&self, interval: Duration, stopped: &AtomicBool) -> bool {
        let (request, signal) = &*self.shared;
        let deadline = Instant::now() + interval;
        let mut request = request.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if stopped.load(Ordering::Acquire) {
                return false;
            }
            if request.wake {
                return true;
            }
            if request.paused {
                request = signal.wait(request).unwrap_or_else(PoisonError::into_inner);
                continue;
            }
            let now = Instant::now();
            if now >= deadline {
                return true;
            }
            request = signal
                .wait_timeout(request, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Wake every waiting poller so a stopped one can exit. Taking the lock
    /// orders this after a waiter's `stopped` check.
    pub(super) fn notify(&self) {
        let (request, signal) = &*self.shared;
        let _request = request.lock().unwrap_or_else(PoisonError::into_inner);
        signal.notify_all();
    }
}

#[derive(Debug)]
pub struct GitService {
    backend: CliGitBackend,
    generation: AtomicU64,
    write_locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

impl GitService {
    pub fn new(backend: CliGitBackend) -> Self {
        Self {
            backend,
            generation: AtomicU64::new(0),
            write_locks: Mutex::new(HashMap::new()),
        }
    }

    pub fn backend(&self) -> &CliGitBackend {
        &self.backend
    }

    pub fn refresh(
        &self,
        roots: &[PathBuf],
        active_path: Option<&Path>,
    ) -> GitResult<GitStoreSnapshot> {
        self.refresh_with_mode(roots, active_path, RefreshMode::Detailed)
    }

    fn refresh_fast(
        &self,
        roots: &[PathBuf],
        active_path: Option<&Path>,
    ) -> GitResult<GitStoreSnapshot> {
        self.refresh_with_mode(roots, active_path, RefreshMode::Fast)
    }

    fn refresh_with_mode(
        &self,
        roots: &[PathBuf],
        active_path: Option<&Path>,
        mode: RefreshMode,
    ) -> GitResult<GitStoreSnapshot> {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = super::remote::take_problem();
        let discovered = match mode {
            RefreshMode::Fast => super::discovery::discover_fast(self.backend.runner(), roots)?,
            RefreshMode::Detailed => self.backend.discover(roots)?,
        };
        let mut repositories = BTreeMap::new();
        for repository in discovered {
            let id = repository_id(&repository.worktree_root);
            let status = match mode {
                RefreshMode::Fast => self.backend.status_fast(&repository, id, generation),
                RefreshMode::Detailed => self.backend.status(&repository, id, generation),
            };
            let snapshot = match status {
                Ok(snapshot) => snapshot,
                // An environment repository that cannot be read leaves the others be.
                Err(error) if super::remote::is_remote(&repository.worktree_root) => {
                    super::remote::report_problem(error);
                    continue;
                }
                Err(error) => return Err(error),
            };
            repositories.insert(id, snapshot);
        }
        // Every unmerged file's conflicts, from one diff per repository.
        let mut conflicts = BTreeMap::new();
        if mode == RefreshMode::Detailed {
            for repository in repositories.values() {
                let unmerged: Vec<PathBuf> = repository
                    .files
                    .iter()
                    .filter(|(_, file)| file.conflict.is_some())
                    .map(|(path, _)| path.clone())
                    .collect();
                if unmerged.is_empty() {
                    continue;
                }
                for (path, regions) in self
                    .backend
                    .conflict_regions(&repository.worktree_root, &unmerged)
                    .unwrap_or_default()
                {
                    conflicts.insert(repository.worktree_root.join(path), Arc::from(regions));
                }
            }
        }
        let runtime = self.backend.git_runtime();
        let mut snapshot = GitStoreSnapshot {
            repositories: repositories.into(),
            active_repository: None,
            operation: None,
            // Why an environment repository was left out, shown when nothing else is.
            last_error: super::remote::take_problem(),
            runtime_label: Some(format!(
                "{:?} Git {}.{}.{}",
                runtime.source, runtime.version.major, runtime.version.minor, runtime.version.patch
            )),
            generation,
            line_changes: Arc::default(),
            conflicts: Arc::new(conflicts),
            diffs: Arc::default(),
            presentation: Arc::default(),
            initializing: false,
        };
        snapshot.choose_active_for_path(active_path);
        if mode == RefreshMode::Detailed
            && let Some(path) = active_path
            && let Some(repository) = snapshot.repository_for_path(path)
            && let Some(relative) = repository.relative_path(path)
            && !relative.as_os_str().is_empty()
        {
            let conflicted = repository
                .files
                .get(&relative)
                .is_some_and(|file| file.conflict.is_some());
            // The editor looks a file up by the path it opened it with.
            let regions = snapshot
                .conflicts
                .get(&repository.worktree_root.join(&relative))
                .cloned();
            let mut changes = BTreeMap::new();
            // An unmerged path has no single index side: `--cached` is empty
            // and the plain diff is a combined one, so compare with HEAD.
            let targets: &[DiffTarget] = if conflicted {
                &[DiffTarget::HeadToWorktree]
            } else {
                &[DiffTarget::HeadToIndex, DiffTarget::IndexToWorktree]
            };
            for &target in targets {
                if let Ok(diff) = self.backend.parsed_diff(
                    &repository.worktree_root,
                    target,
                    std::slice::from_ref(&relative),
                ) {
                    collect_line_changes(&diff, &mut changes);
                }
            }
            if let Some(regions) = regions {
                Arc::make_mut(&mut snapshot.conflicts).insert(path.to_path_buf(), regions);
            }
            if !changes.is_empty() {
                Arc::make_mut(&mut snapshot.line_changes).insert(path.to_path_buf(), changes);
            }
        }
        Ok(snapshot)
    }

    pub fn write<T>(
        &self,
        repository: &Path,
        operation: impl FnOnce(&CliGitBackend) -> GitResult<T>,
    ) -> GitResult<T> {
        let lock = {
            let mut locks = self
                .write_locks
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            Arc::clone(
                locks
                    .entry(repository.to_path_buf())
                    .or_insert_with(|| Arc::new(Mutex::new(()))),
            )
        };
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        operation(&self.backend)
    }

    pub fn clone_repository(
        &self,
        url: &str,
        target: &Path,
        cancellation: &CancellationToken,
    ) -> GitResult<()> {
        self.backend.clone_repository(url, target, cancellation)
    }

    /// Scan `roots` until stopped. The consumer compares each result with its
    /// actual state, since commands may change that state between scans.
    pub fn poll(
        &self,
        roots: &[PathBuf],
        control: &PollControl,
        stopped: &AtomicBool,
        interval: Duration,
        publish: &impl Fn(&PollTarget, GitResult<GitStoreSnapshot>),
    ) {
        let mut previous = None;
        // Publish a useful state as soon as possible. This avoids blocking
        // the drawer on nested-repository discovery, untracked-file
        // traversal, and two active-file diffs.
        let mut mode = RefreshMode::Fast;
        while !stopped.load(Ordering::Acquire) {
            let (revision, target) = control.begin_scan();
            let mut result = match mode {
                RefreshMode::Fast => self.refresh_fast(roots, target.active_path.as_deref()),
                RefreshMode::Detailed => self.refresh(roots, target.active_path.as_deref()),
            };
            if let Ok(snapshot) = &mut result {
                snapshot.pin_active(target.pinned_repository.as_deref());
            }
            if mode == RefreshMode::Detailed
                && let Ok(snapshot) = &mut result
            {
                for request in &target.diffs {
                    let content = self.refresh_diff(request, snapshot);
                    Arc::make_mut(&mut snapshot.diffs).insert(request.clone(), content);
                }
            }
            // A stopped poller's roots are stale; its result must not land.
            if stopped.load(Ordering::Acquire) {
                break;
            }
            if control.is_current(revision) {
                if let Ok(snapshot) = &mut result {
                    snapshot.prepare_for_ui(previous.as_ref());
                    previous = Some(snapshot.clone());
                }
                publish(&target, result);
            }
            if mode == RefreshMode::Fast {
                // Always publish the first detailed result. It carries nested
                // repositories, untracked files, and active-file line changes
                // that are intentionally absent from the fast snapshot.
                mode = RefreshMode::Detailed;
                continue;
            }
            if !control.wait(interval, stopped) {
                break;
            }
        }
    }

    pub(crate) fn refresh_diff(
        &self,
        request: &DiffRequest,
        snapshot: &GitStoreSnapshot,
    ) -> GitResult<DiffContent> {
        if request.target == DiffTarget::Changes {
            let repository = snapshot
                .repositories
                .values()
                .find(|repo| repo.worktree_root == request.repository_root);
            return match repository {
                Some(repository) => Ok(self.changes(repository)),
                None => {
                    let current =
                        self.refresh(std::slice::from_ref(&request.repository_root), None)?;
                    let repository = current
                        .repository_for_path(&request.repository_root)
                        .ok_or_else(|| {
                            GitError::new(
                                crate::features::git::error::GitErrorKind::Other,
                                "Repository is no longer available.",
                            )
                        })?;
                    Ok(self.changes(repository))
                }
            };
        }
        let diff = self.backend.parsed_full_diff(
            &request.repository_root,
            request.target,
            std::slice::from_ref(&request.path),
        )?;
        if diff.files.is_empty() && request.target != DiffTarget::HeadToIndex {
            let untracked = if let Some(repository) = snapshot
                .repositories
                .values()
                .find(|repository| repository.worktree_root == request.repository_root)
            {
                // Normal status can collapse an entire untracked directory.
                repository.files.iter().any(|(path, file)| {
                    request.path.starts_with(path) && file.worktree == ChangeKind::Untracked
                })
            } else {
                // Removing a workspace folder keeps its open tabs. Its diff
                // still needs to distinguish untracked content from an empty diff.
                !self
                    .backend
                    .runner()
                    .run(
                        super::command::GitCommand::new()
                            .cwd(&request.repository_root)
                            .args(["ls-files", "--others", "--exclude-standard", "-z", "--"])
                            .args([request.path.as_os_str()])
                            .read_only(),
                    )?
                    .stdout
                    .is_empty()
            };
            if untracked {
                return read_untracked(&request.repository_root.join(&request.path));
            }
        }
        Ok(DiffContent::Unified(diff))
    }

    /// Read tracked changes in two batches with normal diff context. Full-file
    /// context is reserved for the explicitly opened single-file view.
    pub fn changes(&self, repository: &RepositorySnapshot) -> DiffContent {
        let root = &repository.worktree_root;
        let entries = repository.change_entries();
        let mut files = Vec::new();
        for (group, target) in [
            (ChangeGroup::Conflicts, DiffTarget::HeadToWorktree),
            (ChangeGroup::Staged, DiffTarget::HeadToIndex),
            (ChangeGroup::Unstaged, DiffTarget::IndexToWorktree),
            (ChangeGroup::Untracked, DiffTarget::IndexToWorktree),
        ] {
            let paths: Vec<_> = entries
                .iter()
                .filter(|entry| entry.group == group)
                .map(|entry| entry.request.path.clone())
                .collect();
            if paths.is_empty() {
                continue;
            }
            let mut batch = if matches!(group, ChangeGroup::Staged | ChangeGroup::Unstaged) {
                Some(self.backend.parsed_diff(root, target, &[]).map(|diff| {
                    diff.files
                        .into_iter()
                        .filter_map(|file| {
                            let path = file.new_path.clone().or_else(|| file.old_path.clone())?;
                            Some((path, file))
                        })
                        .collect::<BTreeMap<_, _>>()
                }))
            } else {
                None
            };
            for path in paths {
                let content = match group {
                    ChangeGroup::Untracked => read_untracked(&root.join(&path)),
                    // The index holds no single side of an unmerged path.
                    ChangeGroup::Conflicts => self
                        .backend
                        .parsed_diff(root, target, std::slice::from_ref(&path))
                        .map(DiffContent::Unified),
                    _ => match batch.as_mut().unwrap() {
                        Ok(batch) => match batch.remove(&path) {
                            Some(file) => Ok(DiffContent::Unified(
                                crate::features::git::diff::UnifiedDiff { files: vec![file] },
                            )),
                            // Renames without hunks and binary patches may have no
                            // ---/+++ paths. Query these individually.
                            None => self
                                .backend
                                .parsed_diff(root, target, std::slice::from_ref(&path))
                                .map(DiffContent::Unified),
                        },
                        Err(error) => Err(error.clone()),
                    },
                };
                files.push(ChangeDiff {
                    path,
                    target,
                    group,
                    content,
                });
            }
        }
        DiffContent::Changes(files)
    }

    /// Every file `commit` changed, read in one batch with normal diff
    /// context, in `git show --name-status` order.
    pub fn commit_changes(&self, root: &Path, commit: &str) -> GitResult<DiffContent> {
        let paths = self.backend.commit_files(root, commit)?;
        let mut batch: BTreeMap<_, _> = self
            .backend
            .commit_diff(root, commit, &[], None)?
            .files
            .into_iter()
            .filter_map(|file| {
                let path = file.new_path.clone().or_else(|| file.old_path.clone())?;
                Some((path, file))
            })
            .collect();
        let files = paths
            .into_iter()
            .map(|(_, path)| {
                let content = match batch.remove(&path) {
                    Some(file) => Ok(crate::features::git::diff::UnifiedDiff { files: vec![file] }),
                    // Renames without hunks and binary patches may have no
                    // ---/+++ paths. Query these individually.
                    None => {
                        self.backend
                            .commit_diff(root, commit, std::slice::from_ref(&path), None)
                    }
                };
                ChangeDiff {
                    path,
                    target: DiffTarget::Commit,
                    group: ChangeGroup::Commit,
                    content: content.map(DiffContent::Unified),
                }
            })
            .collect();
        Ok(DiffContent::Changes(files))
    }
}

pub(crate) fn read_untracked(path: &Path) -> GitResult<DiffContent> {
    use crate::features::git::error::GitErrorKind;
    let limit = crate::infrastructure::document_filesystem::MAX_EDITABLE_FILE_BYTES as usize;
    let bytes = match super::repo_fs::read(path, limit) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            return Err(GitError::new(GitErrorKind::Io, "The file no longer exists."));
        }
        // Too large to show is shown as binary, as it always was.
        Err(error) if error.kind == GitErrorKind::Unsupported => return Ok(binary_content()),
        Err(error) => return Err(error),
    };
    Ok(
        match crate::features::encoding::decode(
            &bytes,
            None,
            &crate::infrastructure::document_filesystem::options(path),
            crate::infrastructure::document_filesystem::MAX_EDITABLE_FILE_BYTES as usize,
        ) {
            Ok(decoded) => DiffContent::Untracked(decoded.text),
            Err(_) => binary_content(),
        },
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RefreshMode {
    Fast,
    Detailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_the_active_path_wakes_the_poller() {
        let control = PollControl::default();
        let stopped = AtomicBool::new(false);
        let target = PollTarget {
            active_path: Some(PathBuf::from("/workspace/a")),
            ..Default::default()
        };
        control.set_target(target.clone());
        let started = Instant::now();
        assert!(control.wait(Duration::from_secs(60), &stopped));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(control.begin_scan().1, target);
        // The same path is not a new request.
        control.set_target(target);
        assert!(!control.shared.0.lock().unwrap().wake);
    }

    #[test]
    fn a_paused_poller_waits_until_resumed() {
        let control = PollControl::default();
        let stopped = Arc::new(AtomicBool::new(false));
        control.set_paused(true);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let waiter = {
            let control = control.clone();
            let stopped = Arc::clone(&stopped);
            std::thread::spawn(move || {
                ready_tx.send(()).unwrap();
                let scanned = control.wait(Duration::from_millis(10), &stopped);
                done_tx.send(scanned).unwrap();
            })
        };
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let while_paused = done_rx.recv_timeout(Duration::from_millis(50));
        control.set_paused(false);
        waiter.join().unwrap();
        assert_eq!(
            while_paused,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        );
        assert!(done_rx.recv_timeout(Duration::from_secs(5)).unwrap());
    }

    #[test]
    fn refresh_requests_invalidate_in_flight_scans_even_while_paused() {
        let control = PollControl::default();
        let (revision, _) = control.begin_scan();
        control.set_paused(true);
        control.request_refresh();
        assert!(!control.is_current(revision));
        assert!(control.wait(Duration::from_secs(60), &AtomicBool::new(false)));
        let (revision, _) = control.begin_scan();
        assert!(control.is_current(revision));
        control.set_target(PollTarget {
            active_path: Some(PathBuf::from("new.txt")),
            ..Default::default()
        });
        assert!(!control.is_current(revision));
    }

    #[test]
    fn pausing_preserves_an_in_flight_scan_but_removing_its_pin_invalidates_it() {
        let control = PollControl::default();
        let target = PollTarget {
            pinned_repository: Some("repo".into()),
            ..Default::default()
        };
        control.set_target(target.clone());
        let (revision, scanned) = control.begin_scan();
        assert_eq!(scanned, target);
        control.set_paused(true);
        // Existing pause policy stops interval scans; it does not cancel work
        // already running. Explicit refresh/retarget still wakes a paused poller.
        assert!(control.is_current(revision));
        control.set_target(PollTarget::default());
        assert!(!control.is_current(revision));
        assert!(control.wait(Duration::from_secs(60), &AtomicBool::new(false)));
        assert_eq!(control.begin_scan().1.pinned_repository, None);
    }

    #[test]
    fn stopping_ends_the_wait() {
        let control = PollControl::default();
        let stopped = Arc::new(AtomicBool::new(false));
        let waiter = {
            let control = control.clone();
            let stopped = Arc::clone(&stopped);
            std::thread::spawn(move || control.wait(Duration::from_secs(60), &stopped))
        };
        std::thread::sleep(Duration::from_millis(50));
        stopped.store(true, Ordering::Release);
        control.notify();
        assert!(!waiter.join().unwrap());
    }
}
