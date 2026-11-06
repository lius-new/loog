use super::error::GitResult;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RepositoryId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitRuntimeSource {
    Managed,
    System,
    Custom,
    RemoteEnvironment,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitCapabilities {
    pub porcelain_v2: bool,
    pub switch: bool,
    pub restore: bool,
    pub pathspec_from_file: bool,
    pub force_with_lease: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChangeKind {
    #[default]
    Unmodified,
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    Unmerged,
}

impl ChangeKind {
    pub fn from_porcelain(value: u8) -> Self {
        match value {
            b'A' => Self::Added,
            b'M' => Self::Modified,
            b'D' => Self::Deleted,
            b'R' => Self::Renamed,
            b'C' => Self::Copied,
            b'T' => Self::TypeChanged,
            b'U' => Self::Unmerged,
            _ => Self::Unmodified,
        }
    }

    pub fn indicator(self) -> &'static str {
        match self {
            Self::Unmodified => "",
            Self::Added => "A",
            Self::Modified => "M",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Copied => "C",
            Self::TypeChanged => "T",
            Self::Untracked => "?",
            Self::Unmerged => "U",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictKind {
    BothAdded,
    BothDeleted,
    BothModified,
    AddedByUs,
    AddedByThem,
    DeletedByUs,
    DeletedByThem,
    Other,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffStat {
    pub additions: u32,
    pub deletions: u32,
    pub binary: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileState {
    pub index: ChangeKind,
    pub worktree: ChangeKind,
    pub conflict: Option<ConflictKind>,
    pub original_path: Option<PathBuf>,
    pub index_stat: Option<DiffStat>,
    pub worktree_stat: Option<DiffStat>,
}

impl FileState {
    pub fn is_changed(&self) -> bool {
        self.index != ChangeKind::Unmodified
            || self.worktree != ChangeKind::Unmodified
            || self.conflict.is_some()
    }

    pub fn display_kind(&self) -> ChangeKind {
        self.conflict
            .map(|_| ChangeKind::Unmerged)
            .unwrap_or_else(|| {
                if self.worktree != ChangeKind::Unmodified {
                    self.worktree
                } else {
                    self.index
                }
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeadState {
    Branch(String),
    Detached(String),
    Unborn(String),
}

impl Default for HeadState {
    fn default() -> Self {
        Self::Unborn("HEAD".into())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpstreamState {
    pub name: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum RepositoryState {
    #[default]
    Normal,
    MergeInProgress,
    RebaseInProgress,
    CherryPickInProgress,
    RevertInProgress,
    Bisecting,
    DetachedHead,
    UnbornBranch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationKind {
    Scan,
    Clone,
    Stage,
    Unstage,
    Restore,
    Commit,
    Fetch,
    Pull,
    Push,
    Checkout,
    Merge,
    Rebase,
    Stash,
    Worktree,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OperationState {
    pub kind: OperationKind,
    pub started_at: SystemTime,
    pub progress: Option<f32>,
    pub cancellable: bool,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositorySnapshot {
    pub id: RepositoryId,
    pub worktree_root: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    pub head: HeadState,
    pub upstream: Option<UpstreamState>,
    pub ahead: u32,
    pub behind: u32,
    pub files: Arc<BTreeMap<PathBuf, FileState>>,
    /// Paths that match an ignore rule, relative to the worktree root. An
    /// ignored directory is listed once and covers everything below it.
    pub ignored: Arc<BTreeSet<PathBuf>>,
    pub repository_state: RepositoryState,
    /// The rebase step being applied and the total, during a rebase.
    pub operation_step: Option<(u32, u32)>,
    /// The commit HEAD points at; `None` before the first commit.
    pub head_oid: Option<String>,
    /// Stashes, newest first.
    pub stashes: Arc<Vec<StashInfo>>,
    /// Repository tags, newest first, including tags outside HEAD's history.
    pub tags: Arc<Vec<TagInfo>>,
    /// All worktrees sharing this repository, including the main worktree.
    pub worktrees: Arc<Vec<WorktreeInfo>>,
    pub features: RepositoryFeatures,
    pub generation: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepositoryFeatures {
    pub submodules: bool,
    pub lfs: bool,
    pub sparse_checkout: bool,
    pub shallow: bool,
}

impl RepositorySnapshot {
    /// File identities in the same group/path order used by the Changes view.
    /// This needs no patch reads and can seed a view before its bodies load.
    pub fn change_entries(&self) -> Vec<ChangeEntry> {
        let mut entries = Vec::new();
        for (group, target) in [
            (ChangeGroup::Conflicts, DiffTarget::HeadToWorktree),
            (ChangeGroup::Staged, DiffTarget::HeadToIndex),
            (ChangeGroup::Unstaged, DiffTarget::IndexToWorktree),
            (ChangeGroup::Untracked, DiffTarget::IndexToWorktree),
        ] {
            entries.extend(self.files.iter().filter_map(|(path, file)| {
                let included = match group {
                    ChangeGroup::Conflicts => file.conflict.is_some(),
                    ChangeGroup::Staged => {
                        file.conflict.is_none() && file.index != ChangeKind::Unmodified
                    }
                    ChangeGroup::Unstaged => {
                        file.conflict.is_none()
                            && !matches!(
                                file.worktree,
                                ChangeKind::Unmodified | ChangeKind::Untracked
                            )
                    }
                    ChangeGroup::Untracked => file.worktree == ChangeKind::Untracked,
                    ChangeGroup::Commit => false,
                };
                included.then(|| ChangeEntry {
                    request: DiffRequest {
                        repository_root: self.worktree_root.clone(),
                        path: path.clone(),
                        target,
                    },
                    group,
                })
            }));
        }
        entries
    }

    /// Equal apart from `generation`, which every scan bumps. Destructured so
    /// a new field cannot be left out of the comparison.
    pub fn same_state(&self, other: &Self) -> bool {
        let Self {
            id,
            worktree_root,
            git_dir,
            common_dir,
            head,
            upstream,
            ahead,
            behind,
            files,
            ignored,
            repository_state,
            operation_step,
            head_oid,
            stashes,
            tags,
            worktrees,
            features,
            generation: _,
        } = self;
        *id == other.id
            && *worktree_root == other.worktree_root
            && *git_dir == other.git_dir
            && *common_dir == other.common_dir
            && *head == other.head
            && *upstream == other.upstream
            && *ahead == other.ahead
            && *behind == other.behind
            && *files == other.files
            && *ignored == other.ignored
            && *repository_state == other.repository_state
            && *operation_step == other.operation_step
            && *head_oid == other.head_oid
            && *stashes == other.stashes
            && *tags == other.tags
            && *worktrees == other.worktrees
            && *features == other.features
    }

    pub fn branch_label(&self) -> String {
        match &self.head {
            HeadState::Branch(name) | HeadState::Unborn(name) => name.clone(),
            HeadState::Detached(oid) => oid.chars().take(8).collect(),
        }
    }

    pub fn dirty_count(&self) -> usize {
        self.files
            .values()
            .filter(|state| state.is_changed())
            .count()
    }

    /// `path` relative to the worktree root, or `None` outside it. Workspace
    /// folders may be verbatim (`\\?\D:\dir`, from `fs::canonicalize`) while
    /// Git reports `D:/dir`, so both sides drop that prefix first.
    pub fn relative_path(&self, path: &Path) -> Option<PathBuf> {
        comparable_path(path)
            .strip_prefix(comparable_path(&self.worktree_root))
            .ok()
            .map(Path::to_path_buf)
    }

    pub fn state_for_absolute_path(&self, path: &Path) -> Option<&FileState> {
        self.files.get(&self.relative_path(path)?)
    }

    /// How a file tree row colors its label, following Zed: the strongest
    /// change at or below `path` wins, then an untracked or ignored ancestor.
    /// Directories summarize their descendants.
    pub fn decoration_for_absolute_path(&self, path: &Path) -> Option<PathDecoration> {
        let relative = self.relative_path(path)?;
        let relative = relative.as_path();
        if relative.as_os_str().is_empty() {
            return None;
        }
        let strongest = self
            .files
            .range::<Path, _>((Bound::Included(relative), Bound::Unbounded))
            .take_while(|(file, _)| file.starts_with(relative))
            .filter_map(|(_, state)| PathDecoration::for_state(state))
            .min();
        if strongest.is_some() {
            return strongest;
        }
        // Status lists an untracked or ignored directory as one entry, so rows
        // below it inherit from the nearest listed ancestor.
        relative
            .ancestors()
            .filter(|ancestor| !ancestor.as_os_str().is_empty())
            .find_map(|ancestor| {
                if self
                    .files
                    .get(ancestor)
                    .is_some_and(|state| state.worktree == ChangeKind::Untracked)
                {
                    Some(PathDecoration::Created)
                } else if self.ignored.contains(ancestor) {
                    Some(PathDecoration::Ignored)
                } else {
                    None
                }
            })
    }
}

#[cfg(windows)]
pub(crate) fn comparable_path(path: &Path) -> Cow<'_, Path> {
    let Some(value) = path.to_str() else {
        return Cow::Borrowed(path);
    };
    if let Some(unc) = value.strip_prefix(r"\\?\UNC\") {
        Cow::Owned(PathBuf::from(format!(r"\\{unc}")))
    } else if let Some(local) = value.strip_prefix(r"\\?\")
        && local.as_bytes().get(1) == Some(&b':')
    {
        Cow::Borrowed(Path::new(local))
    } else {
        Cow::Borrowed(path)
    }
}

#[cfg(not(windows))]
pub(crate) fn comparable_path(path: &Path) -> Cow<'_, Path> {
    Cow::Borrowed(path)
}

/// Label color class for a file tree row, strongest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PathDecoration {
    Conflict,
    Deleted,
    Modified,
    Created,
    Ignored,
}

impl PathDecoration {
    pub(crate) fn for_state(state: &FileState) -> Option<Self> {
        if state.conflict.is_some() {
            return Some(Self::Conflict);
        }
        [state.index, state.worktree]
            .into_iter()
            .filter_map(|kind| match kind {
                ChangeKind::Unmerged => Some(Self::Conflict),
                ChangeKind::Deleted => Some(Self::Deleted),
                ChangeKind::Modified | ChangeKind::Renamed | ChangeKind::TypeChanged => {
                    Some(Self::Modified)
                }
                ChangeKind::Added | ChangeKind::Copied | ChangeKind::Untracked => {
                    Some(Self::Created)
                }
                ChangeKind::Unmodified => None,
            })
            .min()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiffTarget {
    /// All current change sections in one repository-wide tab.
    Changes,
    HeadToIndex,
    IndexToWorktree,
    HeadToWorktree,
    /// A commit against its first parent; the commit is named separately.
    Commit,
}

/// Identity of an open diff that needs to follow repository changes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DiffRequest {
    pub repository_root: PathBuf,
    pub path: PathBuf,
    pub target: DiffTarget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffContent {
    Unified(super::diff::UnifiedDiff),
    Untracked(String),
    Changes(Vec<ChangeDiff>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeGroup {
    Conflicts,
    Staged,
    Unstaged,
    Untracked,
    /// A file of one commit; commit tabs show no group header.
    Commit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeDiff {
    pub path: PathBuf,
    pub target: DiffTarget,
    pub group: ChangeGroup,
    pub content: GitResult<DiffContent>,
}

#[derive(Clone, Debug)]
pub struct ChangeEntry {
    pub request: DiffRequest,
    pub group: ChangeGroup,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitSummary {
    pub oid: String,
    pub parents: Vec<String>,
    pub author: String,
    pub author_email: String,
    pub timestamp: i64,
    pub subject: String,
    pub decorations: Vec<String>,
}

/// What a history commit's hover card shows beyond its summary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitDetails {
    pub author: String,
    pub author_email: String,
    /// The author date, formatted by git in local time.
    pub date: String,
    /// The full message, without trailing blank lines.
    pub message: String,
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
}

impl CommitDetails {
    /// Read the counts of a `--shortstat` line such as
    /// ` 3 files changed, 10 insertions(+), 2 deletions(-)`.
    pub fn set_shortstat(&mut self, line: &str) {
        for part in line.split(',') {
            let mut words = part.split_whitespace();
            let (Some(count), Some(kind)) = (words.next(), words.next()) else {
                continue;
            };
            let Ok(count) = count.parse() else { continue };
            if kind.starts_with("file") {
                self.files_changed = count;
            } else if kind.starts_with("insertion") {
                self.insertions = count;
            } else if kind.starts_with("deletion") {
                self.deletions = count;
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchInfo {
    pub name: String,
    pub remote: bool,
    pub current: bool,
    pub oid: String,
    pub upstream: Option<String>,
    pub worktree_path: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StashInfo {
    pub index: usize,
    pub oid: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagInfo {
    pub name: String,
    /// The commit a lightweight or annotated tag points at, when applicable.
    pub commit: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagAnnotation {
    pub tagger: String,
    pub email: String,
    pub date: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagDetails {
    pub target: String,
    pub target_kind: String,
    pub annotation: Option<TagAnnotation>,
    pub commit: Option<CommitDetails>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub oid: String,
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    pub locked: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteInfo {
    pub name: String,
    pub fetch_url: Option<String>,
    pub push_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameLine {
    pub oid: String,
    pub original_line: usize,
    pub final_line: usize,
    pub author: String,
    pub author_time: i64,
    pub summary: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConflictStages {
    pub path: PathBuf,
    pub base: Option<String>,
    pub ours: Option<String>,
    pub theirs: Option<String>,
}

/// Which version of a conflicted file to keep. During a merge `Ours` is the
/// checked-out branch; during a rebase it is the branch being rebased onto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictSide {
    Ours,
    Theirs,
}

pub use crate::domain::line_change::LineChange;

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(files: &[(&str, FileState)], ignored: &[&str]) -> RepositorySnapshot {
        RepositorySnapshot {
            id: RepositoryId(1),
            worktree_root: PathBuf::from("/repo"),
            git_dir: PathBuf::new(),
            common_dir: PathBuf::new(),
            head: HeadState::default(),
            upstream: None,
            ahead: 0,
            behind: 0,
            files: files
                .iter()
                .map(|(path, state)| (PathBuf::from(path), state.clone()))
                .collect::<BTreeMap<_, _>>()
                .into(),
            ignored: ignored
                .iter()
                .map(PathBuf::from)
                .collect::<BTreeSet<_>>()
                .into(),
            repository_state: RepositoryState::Normal,
            operation_step: None,
            head_oid: None,
            stashes: Default::default(),
            tags: Default::default(),
            worktrees: Default::default(),
            features: RepositoryFeatures::default(),
            generation: 1,
        }
    }

    fn worktree(kind: ChangeKind) -> FileState {
        FileState {
            worktree: kind,
            ..FileState::default()
        }
    }

    fn decoration(repository: &RepositorySnapshot, path: &str) -> Option<PathDecoration> {
        repository.decoration_for_absolute_path(&Path::new("/repo").join(path))
    }

    #[test]
    fn directories_take_the_strongest_descendant_change() {
        let repository = snapshot(
            &[
                ("src/new.rs", worktree(ChangeKind::Untracked)),
                ("src/ui/main.rs", worktree(ChangeKind::Modified)),
                ("src.txt", worktree(ChangeKind::Deleted)),
            ],
            &[],
        );
        assert_eq!(
            decoration(&repository, "src"),
            Some(PathDecoration::Modified)
        );
        assert_eq!(
            decoration(&repository, "src/ui"),
            Some(PathDecoration::Modified)
        );
        assert_eq!(
            decoration(&repository, "src/new.rs"),
            Some(PathDecoration::Created)
        );
        assert_eq!(decoration(&repository, "docs"), None);
        assert_eq!(decoration(&repository, ""), None);
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_workspace_paths_match_the_git_root() {
        let repository = RepositorySnapshot {
            worktree_root: PathBuf::from("D:/work/loom"),
            ..snapshot(
                &[("src/main.rs", worktree(ChangeKind::Modified))],
                &["target"],
            )
        };
        let tree = Path::new(r"\\?\D:\work\loom");
        assert_eq!(
            repository.decoration_for_absolute_path(&tree.join("target")),
            Some(PathDecoration::Ignored)
        );
        assert!(
            repository
                .state_for_absolute_path(&tree.join(r"src\main.rs"))
                .is_some()
        );
        assert_eq!(
            repository.relative_path(Path::new(r"\\?\D:\work\other")),
            None
        );
    }

    #[test]
    fn rows_inherit_from_untracked_or_ignored_ancestors() {
        let repository = snapshot(
            &[
                ("newdir", worktree(ChangeKind::Untracked)),
                ("target/keep.txt", worktree(ChangeKind::Modified)),
            ],
            &["target", "src/a.log"],
        );
        assert_eq!(
            decoration(&repository, "newdir/a/b.rs"),
            Some(PathDecoration::Created)
        );
        assert_eq!(
            decoration(&repository, "target/debug/x"),
            Some(PathDecoration::Ignored)
        );
        assert_eq!(
            decoration(&repository, "src/a.log"),
            Some(PathDecoration::Ignored)
        );
        assert_eq!(decoration(&repository, "src"), None);
        // A force-added change inside an ignored directory still wins.
        assert_eq!(
            decoration(&repository, "target"),
            Some(PathDecoration::Modified)
        );
    }
}
