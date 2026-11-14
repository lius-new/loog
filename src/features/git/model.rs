//! Source Control business state kept between scans: the history list and
//! the expanded commit, cached commit details, the diff open in flight, the
//! repository chosen by the user, and a clone in progress.
//!
//! Fields are private; each transition keeps request sequences and caches
//! consistent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{ChangeKind, CommitDetails, CommitSummary, DiffRequest};

/// Recent commits of a repository's HEAD.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHistory {
    pub repository: PathBuf,
    /// The commit HEAD was at when these were listed.
    pub head: Option<String>,
    pub commits: Result<Vec<CommitSummary>, String>,
}

pub const HISTORY_PAGE_SIZE: usize = 50;

/// Identifies both the frozen history revision and one in-flight page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryRequest {
    sequence: u64,
    pub repository: PathBuf,
    pub head: Option<String>,
    /// Restricts the log to one file's history.
    pub path: Option<PathBuf>,
    pub skip: usize,
}

/// A history commit expanded to list the files it changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpandedCommit {
    pub oid: String,
    /// `None` while loading.
    pub files: Option<Result<Vec<(ChangeKind, PathBuf)>, String>>,
}

/// Commit details are immutable; the cache only bounds memory.
const COMMIT_DETAIL_CACHE: usize = 64;

#[derive(Clone, Debug, Default)]
pub struct GitModel {
    history: Option<GitHistory>,
    history_sequence: u64,
    history_pending: Option<HistoryRequest>,
    history_path: Option<PathBuf>,
    history_has_more: bool,
    history_commit: Option<ExpandedCommit>,
    commit_details: HashMap<String, Result<CommitDetails, String>>,
    open_diff_sequence: u64,
    diff_loading: Option<DiffRequest>,
    /// A repository chosen in Source Control, and the active file when it
    /// was chosen: the choice holds until that file changes.
    pinned_repository: Option<(PathBuf, Option<PathBuf>)>,
    cloning: bool,
    /// Worktrees of repositories in environments that Source Control scans. A local
    /// worktree is added to the workspace instead; an environment has no folder to add
    /// it to, so it is kept here.
    environment_worktrees: Vec<PathBuf>,
}

impl GitModel {
    pub fn environment_worktrees(&self) -> &[PathBuf] {
        &self.environment_worktrees
    }

    pub fn add_environment_worktree(&mut self, path: PathBuf) {
        if !self.environment_worktrees.contains(&path) {
            self.environment_worktrees.push(path);
        }
    }

    pub fn forget_environment_worktree(&mut self, path: &Path) {
        self.environment_worktrees.retain(|kept| kept != path);
    }

    // --- history ----------------------------------------------------------

    pub fn history(&self) -> Option<&GitHistory> {
        self.history.as_ref()
    }

    pub fn set_history(&mut self, history: GitHistory) {
        self.history_sequence += 1;
        self.history_pending = None;
        self.history_has_more = false;
        self.history = Some(history);
    }

    /// Reopening unchanged history preserves its loaded pages and expanded row.
    pub fn begin_history(
        &mut self,
        repository: PathBuf,
        head: Option<String>,
    ) -> Option<HistoryRequest> {
        self.begin_history_of(repository, head, None)
    }

    /// Like [`Self::begin_history`], optionally listing only the commits that touched `path`.
    pub fn begin_history_of(
        &mut self,
        repository: PathBuf,
        head: Option<String>,
        path: Option<PathBuf>,
    ) -> Option<HistoryRequest> {
        if self.history_pending.as_ref().is_some_and(|request| {
            request.repository == repository && request.head == head && request.path == path
        }) || (self.history_path == path
            && self.history.as_ref().is_some_and(|history| {
                history.repository == repository && history.head == head && history.commits.is_ok()
            }))
        {
            return None;
        }
        self.history = None;
        self.history_has_more = false;
        self.history_commit = None;
        self.history_path = path.clone();
        Some(self.history_request(repository, head, path, 0))
    }

    /// The file the loaded history is restricted to.
    pub fn history_path(&self) -> Option<&Path> {
        self.history_path.as_deref()
    }

    pub fn history_can_load_more(&self) -> bool {
        self.history_has_more && self.history_pending.is_none()
    }

    pub fn begin_history_page(&mut self, repository: &Path) -> Option<HistoryRequest> {
        if !self.history_can_load_more() {
            return None;
        }
        let history = self
            .history
            .as_ref()
            .filter(|history| history.repository == repository && history.head.is_some())?;
        let skip = history.commits.as_ref().ok()?.len();
        Some(self.history_request(
            history.repository.clone(),
            history.head.clone(),
            self.history_path.clone(),
            skip,
        ))
    }

    fn history_request(
        &mut self,
        repository: PathBuf,
        head: Option<String>,
        path: Option<PathBuf>,
        skip: usize,
    ) -> HistoryRequest {
        self.history_sequence += 1;
        let request = HistoryRequest {
            sequence: self.history_sequence,
            repository,
            head,
            path,
            skip,
        };
        self.history_pending = Some(request.clone());
        request
    }

    /// The extra lookahead commit determines exhaustion without an empty fetch.
    /// Failed later pages retain the list and can be retried by scrolling again.
    pub fn finish_history_page(
        &mut self,
        request: &HistoryRequest,
        result: Result<Vec<CommitSummary>, String>,
    ) -> bool {
        if self.history_pending.as_ref() != Some(request) {
            return false;
        }
        self.history_pending = None;
        if request.skip == 0 {
            let has_more = result
                .as_ref()
                .is_ok_and(|commits| commits.len() > HISTORY_PAGE_SIZE);
            let commits = result.map(|mut commits| {
                commits.truncate(HISTORY_PAGE_SIZE);
                commits
            });
            self.set_history(GitHistory {
                repository: request.repository.clone(),
                head: request.head.clone(),
                commits,
            });
            self.history_has_more = has_more;
        } else if let Ok(mut page) = result {
            let Some(history) = self.history.as_mut().filter(|history| {
                history.repository == request.repository && history.head == request.head
            }) else {
                return false;
            };
            let Ok(commits) = &mut history.commits else {
                return false;
            };
            if commits.len() != request.skip {
                return false;
            }
            self.history_has_more = page.len() > HISTORY_PAGE_SIZE;
            page.truncate(HISTORY_PAGE_SIZE);
            commits.extend(page);
        }
        true
    }

    pub fn history_commit(&self) -> Option<&ExpandedCommit> {
        self.history_commit.as_ref()
    }

    /// Keep tag badges current without discarding loaded pages or expanded files.
    pub fn update_history_tag(&mut self, repository: &Path, name: &str, target: Option<&str>) {
        let Some(history) = self
            .history
            .as_mut()
            .filter(|history| history.repository == repository)
        else {
            return;
        };
        let Ok(commits) = &mut history.commits else {
            return;
        };
        let reference = format!("tag: refs/tags/{name}");
        let short_reference = format!("tag: {name}");
        for commit in commits {
            commit
                .decorations
                .retain(|value| value != &reference && value != &short_reference);
            if target == Some(commit.oid.as_str()) {
                commit.decorations.push(reference.clone());
            }
        }
    }

    /// Expand `oid` with its files still loading, or collapse it when it is
    /// already expanded. Returns whether it is expanded now.
    pub fn toggle_commit(&mut self, oid: String) -> bool {
        if self
            .history_commit
            .as_ref()
            .is_some_and(|commit| commit.oid == oid)
        {
            self.history_commit = None;
            false
        } else {
            self.history_commit = Some(ExpandedCommit { oid, files: None });
            true
        }
    }

    /// Fill in the files of `oid` when it is still the expanded commit.
    pub fn set_commit_files(
        &mut self,
        oid: &str,
        files: Result<Vec<(ChangeKind, PathBuf)>, String>,
    ) {
        if let Some(commit) = self
            .history_commit
            .as_mut()
            .filter(|commit| commit.oid == oid)
        {
            commit.files = Some(files);
        }
    }

    pub fn collapse_commit(&mut self) {
        self.history_commit = None;
    }

    pub fn commit_detail(&self, oid: &str) -> Option<&Result<CommitDetails, String>> {
        self.commit_details.get(oid)
    }

    pub fn cache_commit_detail(&mut self, oid: String, details: Result<CommitDetails, String>) {
        if self.commit_details.len() >= COMMIT_DETAIL_CACHE {
            self.commit_details.clear();
        }
        self.commit_details.insert(oid, details);
    }

    // --- diff opening -----------------------------------------------------

    /// Start opening a diff tab; any earlier open still in flight becomes
    /// stale. Returns the new open's sequence.
    pub fn begin_diff_open(&mut self, loading: DiffRequest) -> u64 {
        self.open_diff_sequence += 1;
        self.diff_loading = Some(loading);
        self.open_diff_sequence
    }

    /// Invalidate any open in flight, e.g. because its tab already exists.
    pub fn cancel_diff_open(&mut self) {
        self.open_diff_sequence += 1;
        self.diff_loading = None;
    }

    /// Finish open `sequence`. Returns false, changing nothing, when a newer
    /// open superseded it.
    pub fn finish_diff_open(&mut self, sequence: u64) -> bool {
        if self.open_diff_sequence != sequence {
            return false;
        }
        self.diff_loading = None;
        true
    }

    /// The worker could not start; nothing is loading.
    pub fn abandon_diff_open(&mut self) {
        self.diff_loading = None;
    }

    pub fn diff_loading(&self) -> Option<&DiffRequest> {
        self.diff_loading.as_ref()
    }

    // --- repository choice ------------------------------------------------

    /// Make `root` the Source Control repository while `active_path` stays
    /// the active file, collapsing any expanded commit.
    pub fn pin_repository(&mut self, root: PathBuf, active_path: Option<PathBuf>) {
        self.pinned_repository = Some((root, active_path));
        self.history_commit = None;
    }

    /// The pinned repository, while the active file is still the one it
    /// was chosen with.
    pub fn pinned_for(&self, active_path: Option<&Path>) -> Option<&PathBuf> {
        self.pinned_repository
            .as_ref()
            .filter(|(_, pinned_path)| pinned_path.as_deref() == active_path)
            .map(|(root, _)| root)
    }

    /// Forget the choice when it named `root`.
    pub fn unpin(&mut self, root: &Path) {
        if self
            .pinned_repository
            .as_ref()
            .is_some_and(|(pinned, _)| pinned == root)
        {
            self.pinned_repository = None;
        }
    }

    pub fn has_pinned_repository(&self) -> bool {
        self.pinned_repository.is_some()
    }

    // --- clone --------------------------------------------------------------

    pub fn cloning(&self) -> bool {
        self.cloning
    }

    /// Returns false when a clone is already running.
    pub fn begin_clone(&mut self) -> bool {
        !std::mem::replace(&mut self.cloning, true)
    }

    pub fn finish_clone(&mut self) {
        self.cloning = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::git::DiffTarget;

    fn commits(start: usize, end: usize) -> Vec<CommitSummary> {
        (start..end)
            .map(|index| CommitSummary {
                oid: format!("commit-{index}"),
                parents: Vec::new(),
                author: "author".into(),
                author_email: "author@example.com".into(),
                timestamp: 0,
                subject: format!("Commit {index}"),
                decorations: Vec::new(),
            })
            .collect()
    }

    #[test]
    fn tag_updates_preserve_loaded_history_and_only_touch_the_target_repository() {
        let mut model = GitModel::default();
        let first = model
            .begin_history("repo".into(), Some("head".into()))
            .unwrap();
        model.finish_history_page(&first, Ok(commits(0, 51)));
        model.toggle_commit("commit-2".into());
        model.update_history_tag(Path::new("repo"), "v1.0", Some("commit-2"));
        model.update_history_tag(Path::new("other"), "v1.0", None);
        assert_eq!(
            model.history().unwrap().commits.as_ref().unwrap()[2].decorations,
            ["tag: refs/tags/v1.0"]
        );
        model.update_history_tag(Path::new("repo"), "v1.0", None);
        assert!(
            model.history().unwrap().commits.as_ref().unwrap()[2]
                .decorations
                .is_empty()
        );
        assert_eq!(model.history().unwrap().commits.as_ref().unwrap().len(), 50);
        assert!(model.history_can_load_more());
        assert_eq!(model.history_commit().unwrap().oid, "commit-2");
    }

    #[test]
    fn history_pages_append_once_preserve_expansion_and_stop_at_an_exact_page_boundary() {
        let mut model = GitModel::default();
        let root = PathBuf::from("repo");
        let first = model
            .begin_history(root.clone(), Some("head".into()))
            .unwrap();
        assert!(
            model
                .begin_history(root.clone(), first.head.clone())
                .is_none()
        );
        assert!(model.finish_history_page(&first, Ok(commits(0, 51))));
        assert_eq!(
            model.history().unwrap().commits.as_ref().unwrap(),
            &commits(0, 50)
        );
        model.toggle_commit("commit-2".into());
        model.set_commit_files("commit-2", Ok(Vec::new()));
        let second = model.begin_history_page(&root).unwrap();
        assert_eq!(second.skip, 50);
        assert_eq!(second.head, first.head);
        assert!(model.begin_history_page(&root).is_none());
        assert!(!model.history_can_load_more());
        assert_eq!(model.history().unwrap().commits.as_ref().unwrap().len(), 50);
        assert!(model.finish_history_page(&second, Ok(commits(50, 100))));
        assert!(!model.finish_history_page(&second, Ok(commits(50, 100))));
        assert_eq!(
            model.history().unwrap().commits.as_ref().unwrap(),
            &commits(0, 100)
        );
        assert_eq!(model.history_commit().unwrap().oid, "commit-2");
        assert!(model.history_commit().unwrap().files.is_some());
        assert!(model.begin_history_page(&root).is_none());
        assert!(model.begin_history(root, first.head).is_none());
    }

    #[test]
    fn history_rejects_late_pages_after_repository_or_head_changes() {
        let mut model = GitModel::default();
        let first = model
            .begin_history("a".into(), Some("head".into()))
            .unwrap();
        let other = model
            .begin_history("b".into(), Some("head".into()))
            .unwrap();
        let returned = model
            .begin_history("a".into(), Some("head".into()))
            .unwrap();
        assert!(!model.finish_history_page(&first, Ok(commits(0, 51))));
        assert!(!model.finish_history_page(&other, Ok(commits(0, 51))));
        assert!(model.finish_history_page(&returned, Ok(commits(0, 51))));
        let page = model.begin_history_page(Path::new("a")).unwrap();
        let newer = model
            .begin_history("a".into(), Some("new-head".into()))
            .unwrap();
        assert!(!model.finish_history_page(&page, Ok(commits(50, 101))));
        assert!(model.finish_history_page(&newer, Ok(commits(100, 103))));
        assert_eq!(
            model.history().unwrap().commits.as_ref().unwrap(),
            &commits(100, 103)
        );
        assert!(!model.history_can_load_more());
    }

    #[test]
    fn a_file_scope_reloads_the_history_and_rejects_the_branch_listing() {
        let mut model = GitModel::default();
        let branch = model
            .begin_history("repo".into(), Some("head".into()))
            .unwrap();
        assert!(model.finish_history_page(&branch, Ok(commits(0, 3))));
        assert!(model.history_path().is_none());
        // The same listing is kept, but another file needs its own log.
        assert!(
            model
                .begin_history_of("repo".into(), Some("head".into()), None)
                .is_none()
        );
        let file = model
            .begin_history_of("repo".into(), Some("head".into()), Some("f.rs".into()))
            .unwrap();
        assert_eq!(model.history_path(), Some(Path::new("f.rs")));
        assert!(model.history().is_none());
        assert!(!model.finish_history_page(&branch, Ok(commits(0, 3))));
        assert!(model.finish_history_page(&file, Ok(commits(0, 2))));
        assert!(
            model
                .begin_history_of("repo".into(), Some("head".into()), Some("f.rs".into()))
                .is_none()
        );
        let back = model
            .begin_history("repo".into(), Some("head".into()))
            .unwrap();
        assert!(model.history_path().is_none());
        assert!(model.finish_history_page(&back, Ok(commits(0, 3))));
    }

    #[test]
    fn failed_history_pages_keep_the_list_and_allow_a_retry() {
        let mut model = GitModel::default();
        let first = model
            .begin_history("repo".into(), Some("head".into()))
            .unwrap();
        model.finish_history_page(&first, Ok(commits(0, 51)));
        assert!(model.begin_history_page(Path::new("other")).is_none());
        let page = model.begin_history_page(Path::new("repo")).unwrap();
        assert!(model.finish_history_page(&page, Err("read failed".into())));
        assert_eq!(
            model.history().unwrap().commits.as_ref().unwrap(),
            &commits(0, 50)
        );
        let retry = model.begin_history_page(Path::new("repo")).unwrap();
        assert_eq!(retry.skip, page.skip);
        assert!(!model.finish_history_page(&page, Ok(commits(50, 101))));
        model.finish_history_page(&retry, Ok(commits(50, 53)));
        assert_eq!(
            model.history().unwrap().commits.as_ref().unwrap(),
            &commits(0, 53)
        );
        assert!(!model.history_can_load_more());
    }

    fn request(path: &str) -> DiffRequest {
        DiffRequest {
            repository_root: PathBuf::from("repo"),
            path: PathBuf::from(path),
            target: DiffTarget::IndexToWorktree,
        }
    }

    #[test]
    fn a_newer_diff_open_makes_the_earlier_one_stale() {
        let mut model = GitModel::default();
        let first = model.begin_diff_open(request("a.rs"));
        let second = model.begin_diff_open(request("b.rs"));

        assert!(!model.finish_diff_open(first));
        assert_eq!(model.diff_loading(), Some(&request("b.rs")));
        assert!(model.finish_diff_open(second));
        assert!(model.diff_loading().is_none());
    }

    #[test]
    fn cancelling_an_open_rejects_its_late_result() {
        let mut model = GitModel::default();
        let open = model.begin_diff_open(request("a.rs"));
        model.cancel_diff_open();

        assert!(model.diff_loading().is_none());
        assert!(!model.finish_diff_open(open));
    }

    #[test]
    fn toggling_a_commit_expands_then_collapses_and_ignores_late_files() {
        let mut model = GitModel::default();
        assert!(model.toggle_commit("a".into()));
        model.set_commit_files("b", Ok(Vec::new()));
        assert_eq!(model.history_commit().unwrap().files, None);
        model.set_commit_files("a", Ok(vec![(ChangeKind::Added, "x.rs".into())]));
        assert!(model.history_commit().unwrap().files.is_some());

        assert!(!model.toggle_commit("a".into()));
        assert!(model.history_commit().is_none());
        model.set_commit_files("a", Ok(Vec::new()));
        assert!(model.history_commit().is_none());
    }

    #[test]
    fn a_pinned_repository_holds_until_the_active_file_changes() {
        let mut model = GitModel::default();
        model.toggle_commit("a".into());
        model.pin_repository("repo".into(), Some("repo/a.rs".into()));

        assert!(model.history_commit().is_none());
        assert_eq!(
            model.pinned_for(Some(Path::new("repo/a.rs"))),
            Some(&PathBuf::from("repo"))
        );
        assert_eq!(model.pinned_for(Some(Path::new("repo/b.rs"))), None);
        model.unpin(Path::new("other"));
        assert!(model.has_pinned_repository());
        model.unpin(Path::new("repo"));
        assert!(!model.has_pinned_repository());
    }

    #[test]
    fn the_commit_detail_cache_is_bounded() {
        let mut model = GitModel::default();
        for index in 0..COMMIT_DETAIL_CACHE {
            model.cache_commit_detail(format!("{index}"), Err("x".into()));
        }
        assert!(model.commit_detail("0").is_some());
        model.cache_commit_detail("new".into(), Err("x".into()));
        assert!(model.commit_detail("0").is_none());
        assert!(model.commit_detail("new").is_some());
    }

    #[test]
    fn only_one_clone_runs_at_a_time() {
        let mut model = GitModel::default();
        assert!(model.begin_clone());
        assert!(!model.begin_clone());
        model.finish_clone();
        assert!(!model.cloning());
    }
}
