use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use super::error::{GitError, GitErrorKind, GitResult};
use super::types::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedStatus<P: Ord = PathBuf> {
    pub head: HeadState,
    /// The commit HEAD points at; `None` before the first commit.
    pub head_oid: Option<String>,
    pub upstream: Option<UpstreamState>,
    pub ahead: u32,
    pub behind: u32,
    pub files: BTreeMap<P, FileState<P>>,
    pub ignored: BTreeSet<P>,
}

pub fn parse_porcelain_v2(input: &[u8]) -> GitResult<ParsedStatus> {
    parse_status_with(input, |bytes| Ok(bytes_to_path(bytes)))
}

pub fn parse_resource_status(input: &[u8]) -> GitResult<ParsedStatus<super::path::GitPath>> {
    resource_budget(input)?;
    parse_status_with(input, super::path::GitPath::from_git)
}

fn parse_status_with<P: Ord + Default>(
    input: &[u8],
    mut path: impl FnMut(&[u8]) -> GitResult<P>,
) -> GitResult<ParsedStatus<P>> {
    let records = input.split(|byte| *byte == 0).collect::<Vec<_>>();
    let mut status = ParsedStatus::default();
    let mut branch_oid = None;
    let mut branch_name = None;
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        index += 1;
        if record.is_empty() {
            continue;
        }
        if record.starts_with(b"# ") {
            parse_header(record, &mut status, &mut branch_oid, &mut branch_name)?;
            continue;
        }
        match record[0] {
            b'1' => {
                let fields = split_prefix(record, 8)?;
                let xy = fields[1];
                status.files.insert(
                    path(fields[8])?,
                    FileState {
                        index: change_at(xy, 0),
                        worktree: change_at(xy, 1),
                        ..FileState::default()
                    },
                );
            }
            b'2' => {
                let fields = split_prefix(record, 9)?;
                let original = records.get(index).copied().ok_or_else(|| invalid(record))?;
                index += 1;
                let xy = fields[1];
                status.files.insert(
                    path(fields[9])?,
                    FileState {
                        index: change_at(xy, 0),
                        worktree: change_at(xy, 1),
                        original_path: Some(path(original)?),
                        ..FileState::default()
                    },
                );
            }
            b'u' => {
                let fields = split_prefix(record, 10)?;
                let xy = fields[1];
                status.files.insert(
                    path(fields[10])?,
                    FileState {
                        index: change_at(xy, 0),
                        worktree: change_at(xy, 1),
                        conflict: Some(conflict_kind(xy)),
                        ..FileState::default()
                    },
                );
            }
            b'?' | b'!' => {
                if record.len() < 3 || record[1] != b' ' {
                    return Err(invalid(record));
                }
                let path = path(&record[2..])?;
                if record[0] == b'!' {
                    // Ignored paths are not changes. Keeping them out of
                    // `files` keeps dirty counts and Source Control unaware.
                    status.ignored.insert(path);
                    continue;
                }
                status
                    .files
                    .entry(path)
                    .or_insert_with(FileState::default)
                    .worktree = ChangeKind::Untracked;
            }
            _ => return Err(invalid(record)),
        }
    }

    status.head_oid = branch_oid
        .as_deref()
        .filter(|oid| *oid != "(initial)")
        .map(str::to_owned);
    status.head = match (branch_name.as_deref(), branch_oid.as_deref()) {
        (Some(name), Some("(initial)")) => HeadState::Unborn(name.to_owned()),
        (Some("(detached)"), Some(oid)) => HeadState::Detached(oid.to_owned()),
        (Some(name), _) => HeadState::Branch(name.to_owned()),
        (None, Some(oid)) => HeadState::Detached(oid.to_owned()),
        _ => HeadState::default(),
    };
    Ok(status)
}

/// Parse `git diff --numstat -z`. Rename/copy records have an empty path in
/// the first record followed by the old and new paths as separate NUL records;
/// the statistics belong to the new path shown by porcelain status.
pub fn parse_numstat(input: &[u8]) -> GitResult<BTreeMap<PathBuf, DiffStat>> {
    parse_numstat_with(input, |bytes| Ok(bytes_to_path(bytes)))
}

pub fn parse_resource_numstat(input: &[u8]) -> GitResult<BTreeMap<super::path::GitPath, DiffStat>> {
    resource_budget(input)?;
    parse_numstat_with(input, super::path::GitPath::from_git)
}

fn resource_budget(input: &[u8]) -> GitResult<()> {
    if input.len() > 16 * 1024 * 1024 || input.iter().filter(|byte| **byte == 0).count() > 100_000 {
        return Err(GitError::new(
            GitErrorKind::Unsupported,
            "Git status exceeds its 16 MiB / 100,000 record budget",
        ));
    }
    Ok(())
}

fn parse_numstat_with<P: Ord>(
    input: &[u8],
    mut path: impl FnMut(&[u8]) -> GitResult<P>,
) -> GitResult<BTreeMap<P, DiffStat>> {
    let records = input.split(|byte| *byte == 0).collect::<Vec<_>>();
    let mut stats = BTreeMap::new();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        index += 1;
        if record.is_empty() {
            continue;
        }
        let fields = record.splitn(3, |byte| *byte == b'\t').collect::<Vec<_>>();
        if fields.len() != 3 {
            return Err(invalid(record));
        }
        let binary = fields[0] == b"-" || fields[1] == b"-";
        let additions = if binary {
            0
        } else {
            String::from_utf8_lossy(fields[0])
                .parse()
                .map_err(|_| invalid(record))?
        };
        let deletions = if binary {
            0
        } else {
            String::from_utf8_lossy(fields[1])
                .parse()
                .map_err(|_| invalid(record))?
        };
        let path = if fields[2].is_empty() {
            // Skip the old path and associate the stat with the new path.
            index += 1;
            let new_path = records.get(index).copied().ok_or_else(|| invalid(record))?;
            index += 1;
            path(new_path)?
        } else {
            path(fields[2])?
        };
        stats.insert(
            path,
            DiffStat {
                additions,
                deletions,
                binary,
            },
        );
    }
    Ok(stats)
}

fn parse_header<P: Ord>(
    record: &[u8],
    status: &mut ParsedStatus<P>,
    branch_oid: &mut Option<String>,
    branch_name: &mut Option<String>,
) -> GitResult<()> {
    let text = std::str::from_utf8(record).map_err(|_| invalid(record))?;
    if let Some(value) = text.strip_prefix("# branch.oid ") {
        *branch_oid = Some(value.to_owned());
    } else if let Some(value) = text.strip_prefix("# branch.head ") {
        *branch_name = Some(value.to_owned());
    } else if let Some(value) = text.strip_prefix("# branch.upstream ") {
        status.upstream = Some(UpstreamState {
            name: value.to_owned(),
        });
    } else if let Some(value) = text.strip_prefix("# branch.ab ") {
        for item in value.split_whitespace() {
            if let Some(value) = item.strip_prefix('+') {
                status.ahead = value.parse().map_err(|_| invalid(record))?;
            } else if let Some(value) = item.strip_prefix('-') {
                status.behind = value.parse().map_err(|_| invalid(record))?;
            }
        }
    }
    Ok(())
}

fn split_prefix(record: &[u8], spaces: usize) -> GitResult<Vec<&[u8]>> {
    let mut fields = Vec::with_capacity(spaces + 1);
    let mut start = 0;
    for (index, byte) in record.iter().enumerate() {
        if *byte == b' ' && fields.len() < spaces {
            fields.push(&record[start..index]);
            start = index + 1;
        }
    }
    fields.push(&record[start..]);
    if fields.len() != spaces + 1 {
        return Err(invalid(record));
    }
    Ok(fields)
}

fn change_at(xy: &[u8], index: usize) -> ChangeKind {
    xy.get(index)
        .copied()
        .map(ChangeKind::from_porcelain)
        .unwrap_or_default()
}

fn conflict_kind(xy: &[u8]) -> ConflictKind {
    match xy {
        b"AA" => ConflictKind::BothAdded,
        b"DD" => ConflictKind::BothDeleted,
        b"UU" => ConflictKind::BothModified,
        b"AU" => ConflictKind::AddedByUs,
        b"UA" => ConflictKind::AddedByThem,
        b"DU" => ConflictKind::DeletedByUs,
        b"UD" => ConflictKind::DeletedByThem,
        _ => ConflictKind::Other,
    }
}

fn invalid(record: &[u8]) -> GitError {
    GitError::new(GitErrorKind::InvalidOutput, "Invalid porcelain v2 output.")
        .with_detail(String::from_utf8_lossy(record))
}

#[cfg(unix)]
pub(super) fn bytes_to_path(value: &[u8]) -> PathBuf {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(OsString::from_vec(value.to_vec()))
}

#[cfg(not(unix))]
pub(super) fn bytes_to_path(value: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(value).into_owned())
}

pub fn parse_log(input: &str) -> GitResult<Vec<CommitSummary>> {
    input
        .split('\x1e')
        .filter(|record| !record.trim().is_empty())
        .map(|record| {
            let fields = record
                .trim_start_matches(['\r', '\n'])
                .split('\x1f')
                .collect::<Vec<_>>();
            if fields.len() < 7 {
                return Err(GitError::new(
                    GitErrorKind::InvalidOutput,
                    "Invalid Git log output.",
                ));
            }
            Ok(CommitSummary {
                oid: fields[0].to_owned(),
                parents: fields[1].split_whitespace().map(str::to_owned).collect(),
                author: fields[2].to_owned(),
                author_email: fields[3].to_owned(),
                timestamp: fields[4].parse().unwrap_or_default(),
                subject: fields[5].to_owned(),
                decorations: fields[6]
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .collect(),
            })
        })
        .collect()
}

pub fn parse_branches(input: &[u8]) -> GitResult<Vec<BranchInfo>> {
    // Git adds a newline after each formatted record. NUL fields keep paths
    // containing tabs or newlines intact.
    let mut fields = input.split(|byte| *byte == 0);
    let mut branches = Vec::new();
    while let Some(reference) = fields.next() {
        let reference = reference.strip_prefix(b"\n").unwrap_or(reference);
        if reference.is_empty() {
            continue;
        }
        let mut next = || fields.next().ok_or_else(|| invalid(reference));
        let name = String::from_utf8_lossy(next()?).into_owned();
        let current = next()? == b"*";
        let oid = String::from_utf8_lossy(next()?).into_owned();
        let upstream = next()?;
        let worktree = next()?;
        let remote = reference.starts_with(b"refs/remotes/");
        if remote && reference.ends_with(b"/HEAD") {
            continue;
        }
        branches.push(BranchInfo {
            name,
            remote,
            current,
            oid,
            upstream: (!upstream.is_empty())
                .then(|| String::from_utf8_lossy(upstream).into_owned()),
            worktree_path: (!worktree.is_empty()).then(|| bytes_to_path(worktree)),
        });
    }
    Ok(branches)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn resource_status_preserves_git_names_and_rejects_escaping_entries() {
        use super::super::path::GitPath;
        let input = "# branch.head main\0? src/中文\\name.txt\0! generated/\0";
        let status = parse_resource_status(input.as_bytes()).unwrap();
        let path = GitPath::from_git("src/中文\\name.txt".as_bytes()).unwrap();
        assert_eq!(status.files[&path].worktree, ChangeKind::Untracked);
        assert_eq!(path.components().count(), 2);
        assert!(
            status
                .ignored
                .contains(&GitPath::from_git(b"generated").unwrap())
        );
        assert!(parse_resource_status(b"? ../escape\0").is_err());
        let stats = parse_resource_numstat("3\t2\tsrc/中文\\name.txt\0".as_bytes()).unwrap();
        assert_eq!(stats[&path].additions, 3);
        let removed =
            parse_resource_status(b"1 D. N... 100644 000000 000000 a b keep.txt\0? keep.txt\0")
                .unwrap();
        let state = &removed.files[&GitPath::from_git(b"keep.txt").unwrap()];
        assert_eq!(
            (state.index, state.worktree),
            (ChangeKind::Deleted, ChangeKind::Untracked)
        );
        assert!(parse_resource_status(&vec![0; 100_001]).is_err());
    }

    #[test]
    fn branches_skip_remote_head_symrefs() {
        let input = b"refs/heads/main\0main\0*\0aaa\0origin/main\0/repo\0\n\
refs/remotes/origin/HEAD\0origin\0 \0aaa\0\0\0\n\
refs/remotes/origin/main\0origin/main\0 \0aaa\0\0\0\n";
        let branches = parse_branches(input).unwrap();
        assert_eq!(
            branches
                .iter()
                .map(|branch| (branch.name.as_str(), branch.remote, branch.current))
                .collect::<Vec<_>>(),
            [("main", false, true), ("origin/main", true, false)]
        );
        assert_eq!(branches[0].upstream.as_deref(), Some("origin/main"));
        assert_eq!(branches[0].worktree_path, Some(PathBuf::from("/repo")));
        assert_eq!(branches[1].worktree_path, None);
    }

    #[test]
    fn branch_worktree_paths_preserve_tabs_newlines_and_unicode() {
        let path = "/repo/中文\tworktree\nnext";
        let input = format!(
            "refs/heads/topic\0topic\0 \0abc\0\0{path}\0\nrefs/heads/main\0main\0*\0abc\0\0/repo\0\n"
        );
        let branches = parse_branches(input.as_bytes()).unwrap();
        assert_eq!(branches.len(), 2);
        assert_eq!(branches[0].worktree_path, Some(PathBuf::from(path)));
        assert!(branches[1].current);
        assert!(parse_branches(b"refs/heads/topic\0topic\0").is_err());
    }

    #[test]
    fn parses_headers_xy_rename_conflict_and_special_paths() {
        let input = b"# branch.oid abcdef\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -3\0\
1 .M N... 100644 100644 100644 a b src/main.rs\0\
2 R. N... 100644 100644 100644 a b R100 new name.rs\0old name.rs\0\
u UU N... 100644 100644 100644 100644 a b c file conflict.rs\0\
? \xe4\xb8\xad\xe6\x96\x87.txt\0";
        let parsed = parse_porcelain_v2(input).unwrap();
        assert_eq!(parsed.head, HeadState::Branch("main".into()));
        assert_eq!(parsed.ahead, 2);
        assert_eq!(parsed.behind, 3);
        assert_eq!(
            parsed.files[&PathBuf::from("src/main.rs")].worktree,
            ChangeKind::Modified
        );
        assert_eq!(
            parsed.files[&PathBuf::from("new name.rs")].original_path,
            Some(PathBuf::from("old name.rs"))
        );
        assert_eq!(
            parsed.files[&PathBuf::from("file conflict.rs")].conflict,
            Some(ConflictKind::BothModified)
        );
        assert!(parsed.files.contains_key(&PathBuf::from("中文.txt")));
    }

    #[test]
    fn parses_unborn_and_detached_heads() {
        assert_eq!(
            parse_porcelain_v2(b"# branch.oid (initial)\0# branch.head topic\0")
                .unwrap()
                .head,
            HeadState::Unborn("topic".into())
        );
        assert_eq!(
            parse_porcelain_v2(b"# branch.oid abc123\0# branch.head (detached)\0")
                .unwrap()
                .head,
            HeadState::Detached("abc123".into())
        );
    }

    #[test]
    fn ignored_paths_stay_out_of_changed_files() {
        let parsed = parse_porcelain_v2(b"? newdir/\0! target/\0! src/a.log\0").unwrap();
        assert_eq!(parsed.files.len(), 1);
        assert!(parsed.ignored.contains(Path::new("target")));
        assert!(parsed.ignored.contains(Path::new("src/a.log")));
    }
}
