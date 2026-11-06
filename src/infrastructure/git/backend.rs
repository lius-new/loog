use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::command::{CancellationToken, GitCommand, GitCommandRunner, nul_pathspec};
use super::discovery::{self, DiscoveredRepository};
use super::runtime::GitRuntime;
use crate::features::git::diff::{DiffLine, FileDiff, UnifiedDiff, parse_unified};
use crate::features::git::error::{GitError, GitErrorKind, GitResult};
use crate::features::git::parser::{parse_branches, parse_log, parse_numstat, parse_porcelain_v2};
use crate::features::git::types::*;

pub trait GitBackend: Send + Sync {
    fn git_runtime(&self) -> &GitRuntime;
    fn discover(&self, roots: &[PathBuf]) -> GitResult<Vec<DiscoveredRepository>>;
    fn status(
        &self,
        repository: &DiscoveredRepository,
        id: RepositoryId,
        generation: u64,
    ) -> GitResult<RepositorySnapshot>;
    fn status_fast(
        &self,
        repository: &DiscoveredRepository,
        id: RepositoryId,
        generation: u64,
    ) -> GitResult<RepositorySnapshot>;
    fn clone_repository(
        &self,
        url: &str,
        target: &Path,
        cancellation: &CancellationToken,
    ) -> GitResult<()>;
}

#[derive(Clone, Debug)]
pub struct CliGitBackend {
    runner: GitCommandRunner,
}

impl CliGitBackend {
    pub fn new(runtime: GitRuntime) -> Self {
        Self {
            runner: GitCommandRunner::new(runtime),
        }
    }

    pub fn runner(&self) -> &GitCommandRunner {
        &self.runner
    }

    pub fn diff(
        &self,
        repository: &Path,
        target: DiffTarget,
        paths: &[PathBuf],
    ) -> GitResult<String> {
        self.diff_with_context(repository, target, paths, None)
    }

    /// Produce a whole-file diff suitable for a document-style diff editor.
    /// A very large context keeps unchanged lines in the same parsed hunk
    /// without requiring separate `git show` calls for the index and HEAD.
    pub fn full_diff(
        &self,
        repository: &Path,
        target: DiffTarget,
        paths: &[PathBuf],
    ) -> GitResult<String> {
        self.diff_with_context(repository, target, paths, Some(1_000_000))
    }

    fn diff_with_context(
        &self,
        repository: &Path,
        target: DiffTarget,
        paths: &[PathBuf],
        context: Option<usize>,
    ) -> GitResult<String> {
        let mut args = vec![OsString::from("diff"), OsString::from("--no-ext-diff")];
        if let Some(context) = context {
            args.push(OsString::from(format!("--unified={context}")));
        }
        match target {
            DiffTarget::HeadToIndex => args.push("--cached".into()),
            DiffTarget::IndexToWorktree => {}
            DiffTarget::HeadToWorktree => args.push("HEAD".into()),
            DiffTarget::Commit | DiffTarget::Changes => {
                return Err(GitError::new(
                    GitErrorKind::Unsupported,
                    "This comparison requires a commit or a repository change list.",
                ));
            }
        }
        args.push("--".into());
        args.extend(paths.iter().map(|path| path.as_os_str().to_owned()));
        self.runner
            .run(GitCommand::new().cwd(repository).args(args).read_only())?
            .stdout_text()
    }

    /// The conflicts of each of the unmerged `paths`, read from one combined
    /// diff, keyed by path relative to `repository`.
    pub fn conflict_regions(
        &self,
        repository: &Path,
        paths: &[PathBuf],
    ) -> GitResult<BTreeMap<PathBuf, Vec<crate::features::git::conflict::ConflictRegion>>> {
        Ok(crate::features::git::conflict::regions_by_file(
            &self.diff_with_context(repository, DiffTarget::IndexToWorktree, paths, Some(0))?,
        ))
    }

    /// The conflict-marker lines `git diff --check` still finds in `paths`,
    /// counted per file; a file that is clean is left out.
    pub fn leftover_conflict_markers(
        &self,
        repository: &Path,
        paths: &[PathBuf],
    ) -> GitResult<BTreeMap<PathBuf, usize>> {
        let mut args = vec![
            OsString::from("diff"),
            OsString::from("--no-ext-diff"),
            OsString::from("--check"),
            OsString::from("--"),
        ];
        args.extend(paths.iter().map(|path| path.as_os_str().to_owned()));
        let output = self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(args)
                .read_only()
                .accept_exit_code(2),
        )?;
        Ok(crate::features::git::conflict::leftover_markers(
            &output.stdout_text()?,
        ))
    }

    /// `path`'s diff with no context lines: one hunk per change block, as
    /// the source of single-change patches. Raw text keeps CRLF intact.
    pub fn zero_context_diff(
        &self,
        repository: &Path,
        target: DiffTarget,
        path: &Path,
    ) -> GitResult<String> {
        self.diff_with_context(repository, target, &[path.to_path_buf()], Some(0))
    }

    /// Apply a zero-context patch to the index, or to the worktree when
    /// `to_index` is false, in `reverse` to undo it.
    pub fn apply_zero_context_patch(
        &self,
        repository: &Path,
        patch: &str,
        to_index: bool,
        reverse: bool,
    ) -> GitResult<()> {
        let mut args = vec!["apply", "--unidiff-zero", "--whitespace=nowarn"];
        if to_index {
            args.push("--cached");
        }
        if reverse {
            args.push("--reverse");
        }
        args.push("-");
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(args)
                .stdin(patch.as_bytes().to_vec()),
        )?;
        Ok(())
    }

    /// The files `commit` changed against its first parent.
    pub fn commit_files(
        &self,
        repository: &Path,
        commit: &str,
    ) -> GitResult<Vec<(ChangeKind, PathBuf)>> {
        validate_ref(commit)?;
        let output = self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args([
                    "show",
                    "--format=",
                    "--name-status",
                    "--diff-merges=first-parent",
                    "-z",
                    commit,
                ])
                .read_only(),
        )?;
        // `<status>\0<path>\0`, or `<status>\0<old>\0<new>\0` for renames
        // and copies, which list the new path.
        let mut fields = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|field| !field.is_empty());
        let mut files = Vec::new();
        while let Some(status) = fields.next() {
            let kind = ChangeKind::from_porcelain(status[0]);
            let mut path = fields.next();
            if matches!(kind, ChangeKind::Renamed | ChangeKind::Copied) {
                path = fields.next();
            }
            let Some(path) = path else {
                break;
            };
            files.push((kind, path_from_git_bytes(path)));
        }
        Ok(files)
    }

    /// `path`'s whole-file diff in `commit` against its first parent.
    pub fn commit_full_diff(
        &self,
        repository: &Path,
        commit: &str,
        path: &Path,
    ) -> GitResult<UnifiedDiff> {
        self.commit_diff(repository, commit, &[path.to_path_buf()], Some(1_000_000))
    }

    /// Read one overview body using the same plain-text path as a batch.
    /// Special encodings, binary files and header-only changes retain the
    /// object-aware comparison used by the full-file view.
    pub fn commit_file_diff(
        &self,
        repository: &Path,
        commit: &str,
        path: &Path,
    ) -> GitResult<UnifiedDiff> {
        validate_ref(commit)?;
        let paths = [path.to_path_buf()];
        if let Some(file) = self
            .commit_batch(repository, commit, None, &paths)?
            .remove(path)
        {
            return Ok(UnifiedDiff { files: vec![file] });
        }
        self.commit_diff(repository, commit, &paths, None)
    }

    /// `commit`'s diff against its first parent, limited to `paths` unless
    /// empty, with git's default context unless `context` is given.
    pub fn commit_diff(
        &self,
        repository: &Path,
        commit: &str,
        paths: &[PathBuf],
        context: Option<usize>,
    ) -> GitResult<UnifiedDiff> {
        validate_ref(commit)?;
        if paths.is_empty() {
            // One `git show` serves every file that needs no special decoding;
            // only binary, BOM-marked or non-UTF-8 files take the per-file
            // route, which costs several Git processes each.
            let mut batch = self.commit_batch(repository, commit, context, &[])?;
            let mut result = UnifiedDiff::default();
            for (_, path) in self.commit_files(repository, commit)? {
                if let Some(file) = batch.remove(&path) {
                    result.files.push(file);
                } else {
                    result.files.extend(
                        self.commit_diff(repository, commit, &[path], context)?
                            .files,
                    );
                }
            }
            return Ok(result);
        }
        if let Some(diff) = super::encoded_diff::compare(
            &self.runner,
            repository,
            DiffTarget::Commit,
            paths,
            Some(commit),
            context,
        )? {
            return Ok(diff);
        }
        let mut args = vec![
            OsString::from("show"),
            OsString::from("--format="),
            OsString::from("--no-ext-diff"),
        ];
        if let Some(context) = context {
            args.push(OsString::from(format!("--unified={context}")));
        }
        args.push(OsString::from("--diff-merges=first-parent"));
        args.push(OsString::from(commit));
        args.push(OsString::from("--"));
        args.extend(paths.iter().map(|path| path.as_os_str().to_owned()));
        let text = self
            .runner
            .run(GitCommand::new().cwd(repository).args(args).read_only())?
            .stdout_text()?;
        parse_unified(&text)
    }

    /// The text diffs of `commit`'s requested files from a single `git show`,
    /// keyed by path. Files whose patch may need encoding-aware handling are
    /// left out so the caller can read them individually.
    fn commit_batch(
        &self,
        repository: &Path,
        commit: &str,
        context: Option<usize>,
        paths: &[PathBuf],
    ) -> GitResult<std::collections::HashMap<PathBuf, FileDiff>> {
        let mut args = vec![
            OsString::from("show"),
            OsString::from("--format="),
            OsString::from("--no-ext-diff"),
        ];
        if let Some(context) = context {
            args.push(OsString::from(format!("--unified={context}")));
        }
        args.push(OsString::from("--diff-merges=first-parent"));
        args.push(OsString::from(commit));
        args.push(OsString::from("--"));
        args.extend(paths.iter().map(|path| path.as_os_str().to_owned()));
        let output = self
            .runner
            .run(GitCommand::new().cwd(repository).args(args).read_only())?;
        let text = String::from_utf8_lossy(&output.stdout);
        let plain = |file: &FileDiff| {
            !file.binary
                && file
                    .hunks
                    .iter()
                    .flat_map(|hunk| &hunk.lines)
                    .all(|line| match line {
                        DiffLine::Context(s) | DiffLine::Addition(s) | DiffLine::Deletion(s) => {
                            !s.contains('\u{feff}') && !s.contains('\u{fffd}')
                        }
                        DiffLine::NoNewline => true,
                    })
        };
        Ok(parse_unified(&text)?
            .files
            .into_iter()
            .filter(plain)
            .filter_map(|file| {
                let path = file.new_path.clone().or_else(|| file.old_path.clone())?;
                Some((path, file))
            })
            .collect())
    }

    pub fn parsed_diff(
        &self,
        repository: &Path,
        target: DiffTarget,
        paths: &[PathBuf],
    ) -> GitResult<UnifiedDiff> {
        self.parsed_bytes_diff(repository, target, paths, None)
    }

    fn parsed_bytes_diff(
        &self,
        repository: &Path,
        target: DiffTarget,
        paths: &[PathBuf],
        context: Option<usize>,
    ) -> GitResult<UnifiedDiff> {
        let raw = self.diff_with_context(repository, target, paths, context);
        if let Ok(text) = &raw {
            if !text.contains("Binary files ")
                && !text.contains("GIT binary patch")
                && !text.contains('\u{feff}')
            {
                return parse_unified(text);
            }
        }
        if paths.len() == 1 {
            if let Some(diff) = super::encoded_diff::compare(
                &self.runner,
                repository,
                target,
                paths,
                None,
                context,
            )? {
                return Ok(diff);
            }
            return parse_unified(&raw?);
        }
        if paths.is_empty() {
            let mut command = GitCommand::new()
                .cwd(repository)
                .args(["diff", "--name-only", "-z"]);
            if target == DiffTarget::HeadToIndex {
                command = command.args(["--cached"]);
            } else if target == DiffTarget::HeadToWorktree {
                command = command.args(["HEAD"]);
            }
            let output = self.runner.run(command.args(["--"]).read_only())?;
            let mut result = UnifiedDiff::default();
            for path in output.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
                let path = path_from_git_bytes(path);
                let value = if let Some(diff) = super::encoded_diff::compare(
                    &self.runner,
                    repository,
                    target,
                    std::slice::from_ref(&path),
                    None,
                    context,
                )? {
                    diff
                } else {
                    parse_unified(&self.diff_with_context(
                        repository,
                        target,
                        std::slice::from_ref(&path),
                        context,
                    )?)?
                };
                result.files.extend(value.files);
            }
            Ok(result)
        } else {
            parse_unified(&self.diff_with_context(repository, target, paths, context)?)
        }
    }

    pub fn parsed_full_diff(
        &self,
        repository: &Path,
        target: DiffTarget,
        paths: &[PathBuf],
    ) -> GitResult<UnifiedDiff> {
        self.parsed_bytes_diff(repository, target, paths, Some(1_000_000))
    }

    pub fn stage(&self, repository: &Path, paths: &[PathBuf]) -> GitResult<()> {
        self.pathspec_command(repository, &["add"], paths)?;
        Ok(())
    }

    pub fn unstage(&self, repository: &Path, paths: &[PathBuf]) -> GitResult<()> {
        if self.git_runtime().capabilities.restore {
            self.pathspec_command(repository, &["restore", "--staged"], paths)?;
        } else {
            self.pathspec_command(repository, &["reset", "HEAD"], paths)?;
        }
        Ok(())
    }

    pub fn restore_worktree(&self, repository: &Path, paths: &[PathBuf]) -> GitResult<()> {
        if !self.git_runtime().capabilities.restore {
            return Err(GitError::new(
                GitErrorKind::Unsupported,
                "This Git runtime does not support safe restore.",
            ));
        }
        self.pathspec_command(repository, &["restore"], paths)?;
        Ok(())
    }

    pub fn apply_index_patch(
        &self,
        repository: &Path,
        patch: &[u8],
        reverse: bool,
        expected_index_tree: Option<&str>,
    ) -> GitResult<()> {
        if let Some(expected) = expected_index_tree {
            let current = self.index_tree(repository)?;
            if current.trim() != expected.trim() {
                return Err(GitError::new(
                    GitErrorKind::Conflict,
                    "The index changed after this diff was generated. Refresh and try again.",
                ));
            }
        }
        let mut args = vec!["apply", "--cached", "--recount", "--whitespace=nowarn"];
        if reverse {
            args.push("--reverse");
        }
        args.push("-");
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(args)
                .stdin(patch.to_vec()),
        )?;
        Ok(())
    }

    pub fn apply_worktree_patch(
        &self,
        repository: &Path,
        patch: &[u8],
        reverse: bool,
    ) -> GitResult<()> {
        let mut args = vec!["apply", "--recount", "--whitespace=nowarn"];
        if reverse {
            args.push("--reverse");
        }
        args.push("-");
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(args)
                .stdin(patch.to_vec()),
        )?;
        Ok(())
    }

    pub fn index_tree(&self, repository: &Path) -> GitResult<String> {
        self.runner
            .run(
                GitCommand::new()
                    .cwd(repository)
                    .args(["write-tree"])
                    .read_only(),
            )?
            .stdout_text()
            .map(|value| value.trim().to_owned())
    }

    pub fn commit(
        &self,
        repository: &Path,
        message: &str,
        amend: bool,
        signoff: bool,
    ) -> GitResult<String> {
        // Amending with no new message keeps the commit's own.
        let keep_message = amend && message.trim().is_empty();
        let mut args = vec![
            "commit",
            if keep_message {
                "--no-edit"
            } else {
                "--file=-"
            },
        ];
        if amend {
            args.push("--amend");
        }
        if signoff {
            args.push("--signoff");
        }
        let mut command = GitCommand::new().cwd(repository).args(args);
        if !keep_message {
            command = command.stdin(message.as_bytes().to_vec());
        }
        self.runner.run(command)?.stdout_text()
    }

    /// Undo the last commit, keeping its changes staged. Returns the undone
    /// commit's message.
    pub fn undo_commit(&self, repository: &Path) -> GitResult<String> {
        let message = self
            .runner
            .run(
                GitCommand::new()
                    .cwd(repository)
                    .args(["log", "-1", "--format=%B"])
                    .read_only(),
            )?
            .stdout_text()?;
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(["reset", "--soft", "HEAD^"]),
        )?;
        Ok(message.trim_end().to_owned())
    }

    /// `commit`'s author, local date, full message and change counts
    /// against its first parent.
    pub fn commit_details(&self, repository: &Path, commit: &str) -> GitResult<CommitDetails> {
        validate_ref(commit)?;
        let text = self
            .runner
            .run(
                GitCommand::new()
                    .cwd(repository)
                    .args([
                        "show",
                        "--no-ext-diff",
                        "--shortstat",
                        "--diff-merges=first-parent",
                        "--date=format-local:%Y-%m-%d %H:%M",
                        "--format=%an%x00%ae%x00%ad%x00%B%x00",
                        commit,
                        "--",
                    ])
                    .read_only(),
            )?
            .stdout_text()?;
        let mut fields = text.splitn(5, '\0');
        let mut field = || fields.next().unwrap_or_default().to_owned();
        let mut details = CommitDetails {
            author: field(),
            author_email: field(),
            date: field(),
            message: field().trim_end().to_owned(),
            ..Default::default()
        };
        details.set_shortstat(&field());
        Ok(details)
    }

    pub fn branches(&self, repository: &Path) -> GitResult<Vec<BranchInfo>> {
        let output = self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args([
                    "for-each-ref",
                    "--sort=-committerdate",
                    "--format=%(refname)%00%(refname:short)%00%(HEAD)%00%(objectname)%00%(upstream:short)%00%(worktreepath)%00",
                    "refs/heads",
                    "refs/remotes",
                ])
                .read_only(),
        )?;
        let mut branches = parse_branches(&output.stdout)?;
        for branch in &mut branches {
            if let Some(path) = &mut branch.worktree_path {
                *path = std::fs::canonicalize(&*path).unwrap_or_else(|_| path.clone());
            }
        }
        Ok(branches)
    }

    pub fn tags(&self, repository: &Path) -> GitResult<Vec<TagInfo>> {
        let text = self
            .runner
            .run(
                GitCommand::new()
                    .cwd(repository)
                    .args([
                        "for-each-ref",
                        "--sort=-creatordate",
                        "--format=%(refname:strip=2)%00%(objecttype)%00%(objectname)%00%(*objecttype)%00%(*objectname)",
                        "refs/tags/",
                    ])
                    .read_only(),
            )?
            .stdout_text()?;
        text.lines()
            .map(|line| {
                let fields = line.split('\0').collect::<Vec<_>>();
                if fields.len() != 5 {
                    return Err(GitError::new(
                        GitErrorKind::InvalidOutput,
                        "Invalid tag output.",
                    ));
                }
                let commit = if fields[1] == "commit" {
                    Some(fields[2].to_owned())
                } else if fields[3] == "commit" {
                    Some(fields[4].to_owned())
                } else {
                    None
                };
                Ok(TagInfo {
                    name: fields[0].to_owned(),
                    commit,
                })
            })
            .collect()
    }

    pub fn tag_details(&self, repository: &Path, name: &str) -> GitResult<TagDetails> {
        validate_ref(name)?;
        let reference = format!("refs/tags/{name}");
        let output = self.runner.run(GitCommand::new().cwd(repository).args([
            "for-each-ref", "--count=1",
            "--format=%(refname)%00%(objecttype)%00%(objectname)%00%(*objecttype)%00%(*objectname)%00%(taggername)%00%(taggeremail:trim)%00%(taggerdate:format-local:%Y-%m-%d %H:%M)%00%(contents)",
            &reference,
        ]).read_only())?.stdout_text()?;
        let fields = output.splitn(9, '\0').collect::<Vec<_>>();
        if fields.len() != 9 || fields[0] != reference {
            return Err(GitError::new(
                GitErrorKind::Other,
                "The tag is no longer available.",
            ));
        }
        let annotated = fields[1] == "tag";
        let (target_kind, target) = if annotated {
            (fields[3], fields[4])
        } else {
            (fields[1], fields[2])
        };
        let commit = if target_kind == "commit" {
            Some(self.commit_details(repository, target)?)
        } else {
            None
        };
        Ok(TagDetails {
            target: target.to_owned(),
            target_kind: target_kind.to_owned(),
            annotation: annotated.then(|| TagAnnotation {
                tagger: fields[5].to_owned(),
                email: fields[6].to_owned(),
                date: fields[7].to_owned(),
                message: fields[8].trim_end().to_owned(),
            }),
            commit,
        })
    }

    fn validate_tag_name(&self, repository: &Path, name: &str) -> GitResult<()> {
        validate_ref(name)?;
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(["check-ref-format", &format!("refs/tags/{name}")])
                .read_only(),
        )?;
        Ok(())
    }

    pub fn switch(&self, repository: &Path, branch: &str) -> GitResult<()> {
        validate_ref(branch)?;
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(["switch", "--", branch]),
        )?;
        Ok(())
    }

    /// Create a local branch tracking `remote` (e.g. `origin/feature`) and
    /// switch to it. Git derives the local name from the remote's refspec.
    pub fn switch_tracking(&self, repository: &Path, remote: &str) -> GitResult<()> {
        validate_ref(remote)?;
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(["switch", "--track", "--", remote]),
        )?;
        Ok(())
    }

    pub fn create_branch(
        &self,
        repository: &Path,
        branch: &str,
        start: Option<&str>,
    ) -> GitResult<()> {
        validate_ref(branch)?;
        if let Some(start) = start {
            validate_ref(start)?;
        }
        let mut args = vec![
            OsString::from("switch"),
            OsString::from("-c"),
            branch.into(),
        ];
        if let Some(start) = start {
            args.push(start.into());
        }
        self.runner
            .run(GitCommand::new().cwd(repository).args(args))?;
        Ok(())
    }

    pub fn delete_branch(&self, repository: &Path, branch: &str, force: bool) -> GitResult<()> {
        validate_ref(branch)?;
        self.runner.run(GitCommand::new().cwd(repository).args([
            "branch",
            if force { "-D" } else { "-d" },
            "--",
            branch,
        ]))?;
        Ok(())
    }

    pub fn rename_branch(&self, repository: &Path, old: &str, new: &str) -> GitResult<()> {
        validate_ref(old)?;
        validate_ref(new)?;
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(["branch", "--move", "--", old, new]),
        )?;
        Ok(())
    }

    pub fn create_tag(&self, repository: &Path, tag: &str, target: Option<&str>) -> GitResult<()> {
        self.validate_tag_name(repository, tag)?;
        let mut args = vec![
            OsString::from("tag"),
            OsString::from("--no-sign"),
            OsString::from("--"),
            tag.into(),
        ];
        if let Some(target) = target {
            validate_ref(target)?;
            args.push(target.into());
        }
        self.runner
            .run(GitCommand::new().cwd(repository).args(args))?;
        Ok(())
    }

    pub fn delete_tag(&self, repository: &Path, tag: &str) -> GitResult<()> {
        self.validate_tag_name(repository, tag)?;
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(["tag", "--delete", "--", tag]),
        )?;
        Ok(())
    }

    pub fn history(
        &self,
        repository: &Path,
        revision: Option<&str>,
        path: Option<&Path>,
        skip: usize,
        limit: usize,
    ) -> GitResult<Vec<CommitSummary>> {
        let mut args = vec![
            OsString::from("log"),
            OsString::from("--date-order"),
            OsString::from(format!("--skip={skip}")),
            OsString::from(format!("--max-count={}", limit.clamp(1, 500))),
            OsString::from("--decorate=full"),
            OsString::from("--format=%x1e%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%s%x1f%D"),
        ];
        if let Some(revision) = revision {
            validate_ref(revision)?;
            args.push(revision.into());
        }
        if let Some(path) = path {
            args.push("--follow".into());
            args.push("--".into());
            args.push(path.as_os_str().to_owned());
        }
        let text = self
            .runner
            .run(GitCommand::new().cwd(repository).args(args).read_only())?
            .stdout_text()?;
        parse_log(&text)
    }

    pub fn show(&self, repository: &Path, revision: &str) -> GitResult<String> {
        validate_ref(revision)?;
        self.runner
            .run(
                GitCommand::new()
                    .cwd(repository)
                    .args(["show", "--no-ext-diff", "--format=fuller", "--", revision])
                    .read_only(),
            )?
            .stdout_text()
    }

    pub fn stash_list(&self, repository: &Path) -> GitResult<Vec<StashInfo>> {
        let output = self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(["stash", "list", "--format=%gd%x1f%H%x1f%gs%x00"])
                .read_only(),
        )?;
        output
            .stdout
            .split(|byte| *byte == 0)
            // Each record ends `\0\n`, leaving a newline after the last one.
            .filter(|record| !record.trim_ascii().is_empty())
            .map(|record| {
                let text = String::from_utf8_lossy(record);
                let fields = text.trim().split('\x1f').collect::<Vec<_>>();
                if fields.len() != 3 {
                    return Err(GitError::new(
                        GitErrorKind::InvalidOutput,
                        "Invalid stash output.",
                    ));
                }
                let index = fields[0]
                    .strip_prefix("stash@{")
                    .and_then(|value| value.strip_suffix('}'))
                    .and_then(|value| value.parse().ok())
                    .unwrap_or_default();
                Ok(StashInfo {
                    index,
                    oid: fields[1].to_owned(),
                    message: fields[2].to_owned(),
                })
            })
            .collect()
    }

    pub fn stash_push(
        &self,
        repository: &Path,
        message: Option<&str>,
        include_untracked: bool,
        keep_index: bool,
    ) -> GitResult<String> {
        let mut args = vec![OsString::from("stash"), OsString::from("push")];
        if include_untracked {
            args.push("--include-untracked".into());
        }
        if keep_index {
            args.push("--keep-index".into());
        }
        if let Some(message) = message {
            args.extend([OsString::from("--message"), message.into()]);
        }
        self.runner
            .run(GitCommand::new().cwd(repository).args(args))?
            .stdout_text()
    }

    /// Apply (or pop) a stash, restoring its staged changes as staged. When
    /// they no longer apply to the index, Git asks to retry without
    /// `--index`, which brings everything back unstaged.
    pub fn stash_apply(&self, repository: &Path, index: usize, pop: bool) -> GitResult<()> {
        let reference = format!("stash@{{{index}}}");
        let run = |restore_index: bool| {
            let mut args = vec!["stash", if pop { "pop" } else { "apply" }];
            if restore_index {
                args.push("--index");
            }
            args.push(&reference);
            self.runner
                .run(GitCommand::new().cwd(repository).args(args))
                .map(|_| ())
        };
        match run(true) {
            Err(error)
                if error
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("without --index")) =>
            {
                run(false)
            }
            result => result,
        }
    }

    /// Pop the newest stash onto the working tree without restoring its
    /// index, so it also applies on a branch other than the one it came from.
    pub fn stash_pop(&self, repository: &Path) -> GitResult<()> {
        self.runner
            .run(GitCommand::new().cwd(repository).args(["stash", "pop"]))?;
        Ok(())
    }

    pub fn stash_drop(&self, repository: &Path, index: usize) -> GitResult<()> {
        let reference = format!("stash@{{{index}}}");
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(["stash", "drop", &reference]),
        )?;
        Ok(())
    }

    pub fn worktrees(&self, repository: &Path) -> GitResult<Vec<WorktreeInfo>> {
        let output = self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(["worktree", "list", "--porcelain", "-z"])
                .read_only(),
        )?;
        parse_worktrees(&output.stdout)
    }

    pub fn add_worktree(
        &self,
        repository: &Path,
        path: &Path,
        branch: Option<&str>,
        create_branch: bool,
    ) -> GitResult<()> {
        let mut args = vec![OsString::from("worktree"), OsString::from("add")];
        if create_branch {
            let branch = branch
                .ok_or_else(|| GitError::new(GitErrorKind::Other, "A branch name is required."))?;
            validate_ref(branch)?;
            args.extend([OsString::from("-b"), branch.into()]);
        }
        args.push("--".into());
        args.push(comparable_path(path).as_os_str().to_owned());
        if let Some(branch) = branch.filter(|_| !create_branch) {
            validate_ref(branch)?;
            args.push(branch.into());
        }
        self.runner
            .run(GitCommand::new().cwd(repository).args(args))?;
        Ok(())
    }

    pub fn remove_worktree(&self, repository: &Path, path: &Path, force: bool) -> GitResult<()> {
        let mut args = vec![OsString::from("worktree"), OsString::from("remove")];
        if force {
            args.push("--force".into());
        }
        args.push("--".into());
        args.push(comparable_path(path).as_os_str().to_owned());
        self.runner
            .run(GitCommand::new().cwd(repository).args(args))?;
        Ok(())
    }

    /// Explicitly name and track the remote branch, independent of the
    /// repository's checkout.defaultRemote and branch.autoSetupMerge settings.
    pub fn add_tracking_worktree(
        &self,
        repository: &Path,
        path: &Path,
        remote: &str,
    ) -> GitResult<()> {
        validate_ref(remote)?;
        let name = remote
            .split_once('/')
            .map(|(_, name)| name)
            .ok_or_else(|| GitError::new(GitErrorKind::Other, "A remote branch is required."))?;
        validate_ref(name)?;
        self.runner.run(GitCommand::new().cwd(repository).args([
            OsString::from("worktree"),
            "add".into(),
            "--track".into(),
            "-b".into(),
            name.into(),
            "--".into(),
            comparable_path(path).as_os_str().to_owned(),
            remote.into(),
        ]))?;
        Ok(())
    }

    pub fn remotes(&self, repository: &Path) -> GitResult<Vec<RemoteInfo>> {
        let text = self
            .runner
            .run(
                GitCommand::new()
                    .cwd(repository)
                    .args(["remote", "-v"])
                    .read_only(),
            )?
            .stdout_text()?;
        let mut values = BTreeMap::<String, RemoteInfo>::new();
        for line in text.lines() {
            let mut fields = line.split_whitespace();
            let (Some(name), Some(url), Some(kind)) = (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let remote = values.entry(name.to_owned()).or_insert_with(|| RemoteInfo {
                name: name.to_owned(),
                fetch_url: None,
                push_url: None,
            });
            match kind {
                "(fetch)" => remote.fetch_url = Some(url.to_owned()),
                "(push)" => remote.push_url = Some(url.to_owned()),
                _ => {}
            }
        }
        Ok(values.into_values().collect())
    }

    pub fn add_remote(&self, repository: &Path, name: &str, url: &str) -> GitResult<()> {
        validate_ref(name)?;
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(["remote", "add", "--", name, url]),
        )?;
        Ok(())
    }

    pub fn remove_remote(&self, repository: &Path, name: &str) -> GitResult<()> {
        validate_ref(name)?;
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(["remote", "remove", "--", name]),
        )?;
        Ok(())
    }

    pub fn fetch(&self, repository: &Path, remote: Option<&str>, prune: bool) -> GitResult<()> {
        let mut args = vec!["fetch", "--progress"];
        if prune {
            args.push("--prune");
        }
        if let Some(remote) = remote {
            validate_ref(remote)?;
            args.push(remote);
        }
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(args)
                .allow_prompt()
                .timeout(Duration::from_secs(60 * 30)),
        )?;
        Ok(())
    }

    pub fn pull(&self, repository: &Path, rebase: bool) -> GitResult<()> {
        let mut args = vec!["pull", "--progress"];
        if rebase {
            args.push("--rebase");
        }
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(args)
                .allow_prompt()
                .timeout(Duration::from_secs(60 * 30)),
        )?;
        Ok(())
    }

    pub fn push(
        &self,
        repository: &Path,
        set_upstream: Option<(&str, &str)>,
        force_with_lease: bool,
    ) -> GitResult<()> {
        let mut args = vec![OsString::from("push"), OsString::from("--progress")];
        if force_with_lease {
            args.push("--force-with-lease".into());
        }
        if let Some((remote, branch)) = set_upstream {
            validate_ref(remote)?;
            validate_ref(branch)?;
            args.extend([
                OsString::from("--set-upstream"),
                remote.into(),
                branch.into(),
            ]);
        }
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(args)
                .allow_prompt()
                .timeout(Duration::from_secs(60 * 30)),
        )?;
        Ok(())
    }

    pub fn push_tag(&self, repository: &Path, remote: &str, name: &str) -> GitResult<()> {
        self.validate_tag_name(repository, name)?;
        validate_ref(remote)?;
        if !self
            .remotes(repository)?
            .iter()
            .any(|entry| entry.name == remote)
        {
            return Err(GitError::new(
                GitErrorKind::Other,
                "The remote is no longer configured.",
            ));
        }
        let reference = format!("refs/tags/{name}");
        let refspec = format!("{reference}:{reference}");
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args([
                    "-c",
                    &format!("remote.{remote}.mirror=false"),
                    "push",
                    "--progress",
                    "--no-follow-tags",
                    "--",
                    remote,
                    &refspec,
                ])
                .allow_prompt()
                .timeout(Duration::from_secs(60 * 30)),
        )?;
        Ok(())
    }

    pub fn continue_operation(&self, repository: &Path, state: RepositoryState) -> GitResult<()> {
        let args = match state {
            RepositoryState::MergeInProgress => vec!["merge", "--continue"],
            RepositoryState::RebaseInProgress => vec!["rebase", "--continue"],
            RepositoryState::CherryPickInProgress => vec!["cherry-pick", "--continue"],
            RepositoryState::RevertInProgress => vec!["revert", "--continue"],
            _ => {
                return Err(GitError::new(
                    GitErrorKind::Unsupported,
                    "No Git operation is active.",
                ));
            }
        };
        self.runner
            .run(GitCommand::new().cwd(repository).args(args))?;
        Ok(())
    }

    pub fn abort_operation(&self, repository: &Path, state: RepositoryState) -> GitResult<()> {
        let args = match state {
            RepositoryState::MergeInProgress => vec!["merge", "--abort"],
            RepositoryState::RebaseInProgress => vec!["rebase", "--abort"],
            RepositoryState::CherryPickInProgress => vec!["cherry-pick", "--abort"],
            RepositoryState::RevertInProgress => vec!["revert", "--abort"],
            RepositoryState::Bisecting => vec!["bisect", "reset"],
            _ => {
                return Err(GitError::new(
                    GitErrorKind::Unsupported,
                    "No Git operation is active.",
                ));
            }
        };
        self.runner
            .run(GitCommand::new().cwd(repository).args(args))?;
        Ok(())
    }

    /// Resolve `path`'s conflict with one side's version, then stage it. A
    /// side that deleted the file resolves by deleting it.
    pub fn resolve_conflict(
        &self,
        repository: &Path,
        path: &Path,
        side: ConflictSide,
    ) -> GitResult<()> {
        let stages = self.conflict_stages(repository)?;
        let Some(stages) = stages.iter().find(|stages| stages.path == path) else {
            return Err(GitError::new(
                GitErrorKind::Other,
                "The file has no conflict to resolve.",
            ));
        };
        let (flag, present) = match side {
            ConflictSide::Ours => ("--ours", stages.ours.is_some()),
            ConflictSide::Theirs => ("--theirs", stages.theirs.is_some()),
        };
        if present {
            self.runner.run(
                GitCommand::new()
                    .cwd(repository)
                    .args([OsString::from("checkout"), flag.into(), "--".into()])
                    .arg(path.as_os_str()),
            )?;
            self.stage(repository, &[path.to_path_buf()])
        } else {
            self.runner.run(
                GitCommand::new()
                    .cwd(repository)
                    .args(["rm", "--quiet", "--"])
                    .arg(path.as_os_str()),
            )?;
            Ok(())
        }
    }

    pub fn merge(&self, repository: &Path, revision: &str) -> GitResult<()> {
        validate_ref(revision)?;
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(["merge", "--", revision]),
        )?;
        Ok(())
    }

    pub fn rebase(&self, repository: &Path, revision: &str) -> GitResult<()> {
        validate_ref(revision)?;
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(["rebase", "--", revision]),
        )?;
        Ok(())
    }

    pub fn cherry_pick(&self, repository: &Path, revision: &str) -> GitResult<()> {
        validate_ref(revision)?;
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(["cherry-pick", "--", revision]),
        )?;
        Ok(())
    }

    pub fn revert(&self, repository: &Path, revision: &str) -> GitResult<()> {
        validate_ref(revision)?;
        self.runner.run(GitCommand::new().cwd(repository).args([
            "revert",
            "--no-edit",
            "--",
            revision,
        ]))?;
        Ok(())
    }

    pub fn conflict_stages(&self, repository: &Path) -> GitResult<Vec<ConflictStages>> {
        let output = self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(["ls-files", "--unmerged", "-z"])
                .read_only(),
        )?;
        parse_conflict_stages(&output.stdout)
    }

    pub fn read_blob(&self, repository: &Path, oid: &str) -> GitResult<Vec<u8>> {
        if oid.len() < 4 || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(GitError::new(GitErrorKind::Other, "Invalid Git object id."));
        }
        Ok(self
            .runner
            .run(
                GitCommand::new()
                    .cwd(repository)
                    .args(["cat-file", "blob", oid])
                    .read_only(),
            )?
            .stdout)
    }

    pub fn blame(&self, repository: &Path, path: &Path) -> GitResult<Vec<BlameLine>> {
        let output = self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args([
                    OsString::from("blame"),
                    OsString::from("--line-porcelain"),
                    OsString::from("--"),
                    path.as_os_str().to_owned(),
                ])
                .read_only(),
        )?;
        parse_blame(&String::from_utf8_lossy(&output.stdout))
    }

    pub fn submodule_update(&self, repository: &Path, recursive: bool) -> GitResult<()> {
        let mut args = vec!["submodule", "update", "--init"];
        if recursive {
            args.push("--recursive");
        }
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(args)
                .allow_prompt()
                .timeout(Duration::from_secs(60 * 30)),
        )?;
        Ok(())
    }

    pub fn lfs_pull(&self, repository: &Path) -> GitResult<()> {
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(["lfs", "pull"])
                .allow_prompt()
                .timeout(Duration::from_secs(60 * 30)),
        )?;
        Ok(())
    }

    pub fn sparse_checkout_set(&self, repository: &Path, patterns: &[String]) -> GitResult<()> {
        if patterns.is_empty() {
            return Err(GitError::new(
                GitErrorKind::Other,
                "At least one sparse path is required.",
            ));
        }
        let mut args = vec![
            OsString::from("sparse-checkout"),
            OsString::from("set"),
            OsString::from("--"),
        ];
        args.extend(patterns.iter().map(OsString::from));
        self.runner
            .run(GitCommand::new().cwd(repository).args(args))?;
        Ok(())
    }

    pub fn sparse_checkout_disable(&self, repository: &Path) -> GitResult<()> {
        self.runner.run(
            GitCommand::new()
                .cwd(repository)
                .args(["sparse-checkout", "disable"]),
        )?;
        Ok(())
    }

    fn pathspec_command(
        &self,
        repository: &Path,
        prefix: &[&str],
        paths: &[PathBuf],
    ) -> GitResult<()> {
        if paths.is_empty() {
            return Ok(());
        }
        if self.git_runtime().capabilities.pathspec_from_file {
            let mut args = prefix.iter().map(OsString::from).collect::<Vec<_>>();
            args.extend([
                OsString::from("--pathspec-from-file=-"),
                OsString::from("--pathspec-file-nul"),
            ]);
            self.runner.run(
                GitCommand::new()
                    .cwd(repository)
                    .args(args)
                    .stdin(nul_pathspec(paths)),
            )?;
        } else {
            let mut args = prefix.iter().map(OsString::from).collect::<Vec<_>>();
            args.push("--".into());
            args.extend(paths.iter().map(|path| path.as_os_str().to_owned()));
            self.runner
                .run(GitCommand::new().cwd(repository).args(args))?;
        }
        Ok(())
    }
}

impl GitBackend for CliGitBackend {
    fn git_runtime(&self) -> &GitRuntime {
        self.runner.git_runtime()
    }

    fn discover(&self, roots: &[PathBuf]) -> GitResult<Vec<DiscoveredRepository>> {
        discovery::discover(&self.runner, roots)
    }

    fn status(
        &self,
        repository: &DiscoveredRepository,
        id: RepositoryId,
        generation: u64,
    ) -> GitResult<RepositorySnapshot> {
        // Like VS Code and Zed, list every untracked file rather than one entry
        // per untracked directory, so each file can be opened, staged or
        // discarded on its own. Ignored directories still stay collapsed.
        self.status_with_untracked(repository, id, generation, "all")
    }

    fn status_fast(
        &self,
        repository: &DiscoveredRepository,
        id: RepositoryId,
        generation: u64,
    ) -> GitResult<RepositorySnapshot> {
        // The first paint should not wait for an untracked-file traversal.
        // A detailed refresh follows immediately on the same worker.
        self.status_with_untracked(repository, id, generation, "no")
    }

    fn clone_repository(
        &self,
        url: &str,
        target: &Path,
        cancellation: &CancellationToken,
    ) -> GitResult<()> {
        if url.trim().is_empty() {
            return Err(GitError::new(
                GitErrorKind::Other,
                "Repository URL is empty.",
            ));
        }
        self.runner.run_cancellable(
            GitCommand::new()
                .args([
                    OsString::from("clone"),
                    OsString::from("--progress"),
                    OsString::from("--"),
                    OsString::from(url),
                    target.as_os_str().to_owned(),
                ])
                .allow_prompt()
                .timeout(Duration::from_secs(60 * 60)),
            cancellation,
        )?;
        Ok(())
    }
}

impl CliGitBackend {
    fn status_with_untracked(
        &self,
        repository: &DiscoveredRepository,
        id: RepositoryId,
        generation: u64,
        untracked: &str,
    ) -> GitResult<RepositorySnapshot> {
        let mut args = vec![
            OsString::from("status"),
            OsString::from("--porcelain=v2"),
            OsString::from("--branch"),
            OsString::from("-z"),
            OsString::from(format!("--untracked-files={untracked}")),
        ];
        // `matching` lists an ignored directory once without walking it, which
        // keeps `target/` or `node_modules/` cheap. Git rejects it with `no`.
        if untracked != "no" {
            args.push(OsString::from("--ignored=matching"));
        }
        let output = self.runner.run(
            GitCommand::new()
                .cwd(&repository.worktree_root)
                .args(args)
                .read_only(),
        )?;
        let mut parsed = parse_porcelain_v2(&output.stdout)?;
        // Keep the fast first paint process-free beyond status itself. The
        // detailed refresh enriches visible rows with real line statistics.
        if untracked != "no" {
            if let Ok(index_stats) = self.diff_stats(&repository.worktree_root, true) {
                for (path, stat) in index_stats {
                    if let Some(file) = parsed.files.get_mut(&path) {
                        file.index_stat = Some(stat);
                    }
                }
            }
            if let Ok(worktree_stats) = self.diff_stats(&repository.worktree_root, false) {
                for (path, stat) in worktree_stats {
                    if let Some(file) = parsed.files.get_mut(&path) {
                        file.worktree_stat = Some(stat);
                    }
                }
            }
        }
        let repository_state = repository_state(repository, &parsed.head);
        let operation_step = (repository_state == RepositoryState::RebaseInProgress)
            .then(|| rebase_step(&repository.git_dir))
            .flatten();
        let features = repository_features(repository);
        Ok(RepositorySnapshot {
            id,
            worktree_root: repository.worktree_root.clone(),
            git_dir: repository.git_dir.clone(),
            common_dir: repository.common_dir.clone(),
            head: parsed.head,
            upstream: parsed.upstream,
            ahead: parsed.ahead,
            behind: parsed.behind,
            files: parsed.files.into(),
            ignored: parsed.ignored.into(),
            repository_state,
            operation_step,
            head_oid: parsed.head_oid,
            stashes: read_stashes(&repository.common_dir).into(),
            tags: if untracked == "no" {
                Vec::new()
            } else {
                self.tags(&repository.worktree_root)?
            }
            .into(),
            worktrees: if untracked == "no" {
                Vec::new()
            } else {
                self.worktrees(&repository.worktree_root)?
            }
            .into(),
            features,
            generation,
        })
    }

    fn diff_stats(
        &self,
        repository: &Path,
        staged: bool,
    ) -> GitResult<std::collections::BTreeMap<PathBuf, DiffStat>> {
        let mut args = vec![
            OsString::from("diff"),
            OsString::from("--numstat"),
            OsString::from("-z"),
            OsString::from("--no-ext-diff"),
        ];
        if staged {
            args.push(OsString::from("--cached"));
        }
        args.push(OsString::from("--"));
        let output = self
            .runner
            .run(GitCommand::new().cwd(repository).args(args).read_only())?;
        parse_numstat(&output.stdout)
    }
}

/// The rebase step being applied and the total, from the rebase state
/// directory: `rebase-merge` for the default backend, `rebase-apply` for
/// the apply backend.
fn rebase_step(git_dir: &Path) -> Option<(u32, u32)> {
    let read =
        |path: PathBuf| -> Option<u32> { std::fs::read_to_string(path).ok()?.trim().parse().ok() };
    [
        ("rebase-merge", "msgnum", "end"),
        ("rebase-apply", "next", "last"),
    ]
    .into_iter()
    .find_map(|(dir, step, total)| {
        let dir = git_dir.join(dir);
        Some((read(dir.join(step))?, read(dir.join(total))?))
    })
}

/// Stashes, newest first, read from the stash reflog without starting Git.
/// Each line is `<old> <new> <ident> <time> <zone>\t<message>`.
fn read_stashes(common_dir: &Path) -> Vec<StashInfo> {
    let Ok(log) = std::fs::read_to_string(common_dir.join("logs").join("refs").join("stash"))
    else {
        return Vec::new();
    };
    let mut stashes = log
        .lines()
        .filter_map(|line| {
            let (header, message) = line.split_once('\t')?;
            let oid = header.split(' ').nth(1)?;
            Some((oid.to_owned(), message.to_owned()))
        })
        .collect::<Vec<_>>();
    stashes.reverse();
    stashes
        .into_iter()
        .enumerate()
        .map(|(index, (oid, message))| StashInfo {
            index,
            oid,
            message,
        })
        .collect()
}

fn repository_state(repository: &DiscoveredRepository, head: &HeadState) -> RepositoryState {
    let git = &repository.git_dir;
    let common = &repository.common_dir;
    if git.join("MERGE_HEAD").exists() {
        RepositoryState::MergeInProgress
    } else if git.join("rebase-merge").exists() || git.join("rebase-apply").exists() {
        RepositoryState::RebaseInProgress
    } else if git.join("CHERRY_PICK_HEAD").exists() {
        RepositoryState::CherryPickInProgress
    } else if git.join("REVERT_HEAD").exists() {
        RepositoryState::RevertInProgress
    } else if common.join("BISECT_LOG").exists() {
        RepositoryState::Bisecting
    } else {
        match head {
            HeadState::Detached(_) => RepositoryState::DetachedHead,
            HeadState::Unborn(_) => RepositoryState::UnbornBranch,
            HeadState::Branch(_) => RepositoryState::Normal,
        }
    }
}

fn validate_ref(value: &str) -> GitResult<()> {
    if value.is_empty()
        || value.starts_with('-')
        || value.contains(['\0', '\n', '\r'])
        || value == ".."
    {
        Err(GitError::new(GitErrorKind::Other, "Invalid Git reference."))
    } else {
        Ok(())
    }
}

fn parse_worktrees(input: &[u8]) -> GitResult<Vec<WorktreeInfo>> {
    let mut values = Vec::new();
    let mut current = None::<WorktreeInfo>;
    for record in input
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let text = String::from_utf8_lossy(record);
        if let Some(path) = text.strip_prefix("worktree ") {
            if let Some(previous) = current.take() {
                values.push(previous);
            }
            current = Some(WorktreeInfo {
                path: {
                    let path = PathBuf::from(path);
                    std::fs::canonicalize(&path).unwrap_or(path)
                },
                oid: String::new(),
                branch: None,
                bare: false,
                detached: false,
                locked: None,
            });
        } else if let Some(current) = current.as_mut() {
            if let Some(value) = text.strip_prefix("HEAD ") {
                current.oid = value.to_owned();
            } else if let Some(value) = text.strip_prefix("branch ") {
                current.branch = Some(value.trim_start_matches("refs/heads/").to_owned());
            } else if text == "bare" {
                current.bare = true;
            } else if text == "detached" {
                current.detached = true;
            } else if let Some(value) = text.strip_prefix("locked") {
                current.locked = Some(value.trim().to_owned());
            }
        }
    }
    if let Some(current) = current {
        values.push(current);
    }
    Ok(values)
}

fn repository_features(repository: &DiscoveredRepository) -> RepositoryFeatures {
    let attributes = std::fs::read_to_string(repository.worktree_root.join(".gitattributes"))
        .unwrap_or_default();
    RepositoryFeatures {
        submodules: repository.worktree_root.join(".gitmodules").is_file(),
        lfs: attributes.lines().any(|line| line.contains("filter=lfs")),
        sparse_checkout: repository
            .common_dir
            .join("info")
            .join("sparse-checkout")
            .is_file(),
        shallow: repository.common_dir.join("shallow").is_file(),
    }
}

fn parse_conflict_stages(input: &[u8]) -> GitResult<Vec<ConflictStages>> {
    let mut values = BTreeMap::<PathBuf, ConflictStages>::new();
    for record in input
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
            return Err(GitError::new(
                GitErrorKind::InvalidOutput,
                "Invalid conflict stage output.",
            ));
        };
        let header = String::from_utf8_lossy(&record[..tab]);
        let mut fields = header.split_whitespace();
        let _mode = fields.next();
        let oid = fields.next().ok_or_else(|| {
            GitError::new(GitErrorKind::InvalidOutput, "Missing conflict object id.")
        })?;
        let stage: u8 = fields
            .next()
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| GitError::new(GitErrorKind::InvalidOutput, "Missing conflict stage."))?;
        let path = path_from_git_bytes(&record[tab + 1..]);
        let value = values
            .entry(path.clone())
            .or_insert_with(|| ConflictStages {
                path,
                ..ConflictStages::default()
            });
        match stage {
            1 => value.base = Some(oid.to_owned()),
            2 => value.ours = Some(oid.to_owned()),
            3 => value.theirs = Some(oid.to_owned()),
            _ => {}
        }
    }
    Ok(values.into_values().collect())
}

fn parse_blame(input: &str) -> GitResult<Vec<BlameLine>> {
    let mut result = Vec::new();
    let mut lines = input.lines().peekable();
    while let Some(header) = lines.next() {
        let mut fields = header.split_whitespace();
        let Some(oid) = fields.next() else { continue };
        if oid.len() < 4 || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        let original_line = fields
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let final_line = fields
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let count = fields
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1);
        let mut author = String::new();
        let mut author_time = 0;
        let mut summary = String::new();
        for line in lines.by_ref() {
            if line.starts_with('\t') {
                break;
            }
            if let Some(value) = line.strip_prefix("author ") {
                author = value.to_owned();
            } else if let Some(value) = line.strip_prefix("author-time ") {
                author_time = value.parse().unwrap_or_default();
            } else if let Some(value) = line.strip_prefix("summary ") {
                summary = value.to_owned();
            }
        }
        for offset in 0..count {
            result.push(BlameLine {
                oid: oid.to_owned(),
                original_line: original_line + offset,
                final_line: final_line + offset,
                author: author.clone(),
                author_time,
                summary: summary.clone(),
            });
        }
    }
    Ok(result)
}

#[cfg(unix)]
fn path_from_git_bytes(value: &[u8]) -> PathBuf {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(OsString::from_vec(value.to_vec()))
}

#[cfg(not(unix))]
fn path_from_git_bytes(value: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(value).into_owned())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::infrastructure::git::runtime::GitRuntimeManager;

    fn temporary_repository_root() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        loop {
            let root = std::env::temp_dir().join(format!(
                "loom-git-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            // Exclusive creation also handles directories left by an older process
            // that happened to have the same PID. Wall-clock resolution is irrelevant.
            match std::fs::create_dir(&root) {
                Ok(()) => return root,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create Git fixture directory {}: {error}", root.display()),
            }
        }
    }

    pub(crate) fn repository() -> Option<(PathBuf, CliGitBackend)> {
        let runtime = GitRuntimeManager::default().resolve().ok()?;
        Some(repository_with_runtime(runtime))
    }

    pub(crate) fn required_repository() -> (PathBuf, CliGitBackend) {
        let runtime = GitRuntimeManager::default()
            .resolve()
            .expect("resolve Git runtime for required repository fixture");
        repository_with_runtime(runtime)
    }

    fn repository_with_runtime(runtime: GitRuntime) -> (PathBuf, CliGitBackend) {
        let root = temporary_repository_root();
        let backend = CliGitBackend::new(runtime);
        // These synthetic fixtures assert exact LF bytes after checkout.
        // Pin their local policy independently of system/user defaults.
        let commands: &[&[&str]] = &[
            &["init", "-b", "main"],
            &["config", "--local", "core.autocrlf", "false"],
            &["config", "--local", "core.eol", "lf"],
            &["config", "--local", "user.name", "Loom Test"],
            &["config", "--local", "user.email", "loom@example.invalid"],
        ];
        for args in commands {
            backend
                .runner()
                .run(GitCommand::new().cwd(&root).args(args.iter().copied()))
                .unwrap_or_else(|error| {
                    panic!(
                        "Git fixture setup failed: executable={} cwd={} args={args:?}; {error:?}",
                        backend.runner().git_runtime().executable.display(),
                        root.display()
                    )
                });
        }
        (root, backend)
    }

    #[test]
    fn temporary_repository_directories_are_exclusive_under_parallel_creation() {
        let threads: Vec<_> = (0..32)
            .map(|_| std::thread::spawn(temporary_repository_root))
            .collect();
        let roots: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        let unique: std::collections::HashSet<_> = roots.iter().collect();
        assert_eq!(unique.len(), roots.len());
        for root in roots {
            assert!(root.is_dir());
            std::fs::remove_dir(root).unwrap();
        }
    }

    #[test]
    fn real_repository_status_stage_commit_history_diff_and_blame() {
        let Some((root, backend)) = repository() else {
            return;
        };
        std::fs::write(root.join("tracked.txt"), "one\n").unwrap();
        backend
            .stage(&root, &[PathBuf::from("tracked.txt")])
            .unwrap();
        backend.commit(&root, "initial", false, false).unwrap();
        std::fs::write(root.join("tracked.txt"), "one\ntwo\n").unwrap();
        std::fs::write(root.join("untracked.txt"), "new\n").unwrap();
        std::fs::write(root.join(".gitignore"), "build/\n").unwrap();
        std::fs::create_dir_all(root.join("build/out")).unwrap();
        std::fs::write(root.join("build/out/app"), "bin\n").unwrap();
        std::fs::create_dir_all(root.join("drafts/nested")).unwrap();
        std::fs::write(root.join("drafts/nested/page.html"), "draft\n").unwrap();

        let discovered = backend.discover(std::slice::from_ref(&root)).unwrap();
        let status = backend.status(&discovered[0], RepositoryId(1), 7).unwrap();
        assert_eq!(status.head, HeadState::Branch("main".into()));
        assert_eq!(
            status.files[&PathBuf::from("tracked.txt")].worktree,
            ChangeKind::Modified
        );
        assert_eq!(
            status.files[&PathBuf::from("untracked.txt")].worktree,
            ChangeKind::Untracked
        );
        assert_eq!(
            status.files[&PathBuf::from("drafts/nested/page.html")].worktree,
            ChangeKind::Untracked
        );
        assert!(!status.files.contains_key(&PathBuf::from("drafts")));
        assert!(status.ignored.contains(&PathBuf::from("build")));
        assert!(!status.files.contains_key(&PathBuf::from("build")));
        assert!(
            backend
                .diff(&root, DiffTarget::IndexToWorktree, &[])
                .unwrap()
                .contains("+two")
        );
        for args in [
            &["branch", "feature/ui"][..],
            &["tag", "v1.0"][..],
            &["update-ref", "refs/remotes/upstream/main", "HEAD"][..],
        ] {
            backend
                .runner()
                .run(GitCommand::new().cwd(&root).args(args.iter().copied()))
                .unwrap();
        }
        let history = backend.history(&root, None, None, 0, 20).unwrap();
        assert_eq!(history[0].subject, "initial");
        for reference in [
            "HEAD -> refs/heads/main",
            "refs/heads/feature/ui",
            "refs/remotes/upstream/main",
            "tag: refs/tags/v1.0",
        ] {
            assert!(
                history[0]
                    .decorations
                    .iter()
                    .any(|value| value == reference)
            );
        }
        assert!(
            backend
                .branches(&root)
                .unwrap()
                .iter()
                .any(|branch| branch.current && branch.name == "main")
        );
        assert_eq!(
            backend.blame(&root, Path::new("tracked.txt")).unwrap()[0].author,
            "Loom Test"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn git_reports_the_conflicts_and_leftover_markers_of_every_unmerged_file() {
        let Some((root, backend)) = repository() else {
            return;
        };
        let git = |args: &[&str]| {
            backend
                .runner()
                .run(GitCommand::new().cwd(&root).args(args))
        };
        let commit = |name: &str, contents: &str, message: &str| {
            std::fs::write(root.join(name), contents).unwrap();
            backend.stage(&root, &[PathBuf::from(name)]).unwrap();
            backend.commit(&root, message, false, false).unwrap();
        };
        commit("a.txt", "a\n", "base a");
        commit("b.txt", "b\n", "base b");
        backend.create_branch(&root, "topic", None).unwrap();
        commit("a.txt", "topic\n", "topic a");
        commit("b.txt", "topic\n", "topic b");
        backend.switch(&root, "main").unwrap();
        commit("a.txt", "main\n", "main a");
        commit("b.txt", "main\n", "main b");
        backend.switch(&root, "topic").unwrap();
        assert!(git(&["rebase", "main"]).is_err());

        let both = [PathBuf::from("a.txt"), PathBuf::from("b.txt")];
        let regions = backend.conflict_regions(&root, &both).unwrap();
        assert_eq!(regions.len(), 1, "only a.txt is unmerged: {regions:?}");
        assert_eq!(regions[&PathBuf::from("a.txt")].len(), 1);
        let left = backend
            .leftover_conflict_markers(&root, &[PathBuf::from("a.txt")])
            .unwrap();
        assert_eq!(left[&PathBuf::from("a.txt")], 3);

        // Taking one side by hand removes the markers; Git still lists the
        // file as unmerged until it is staged.
        std::fs::write(root.join("a.txt"), "main\n").unwrap();
        assert!(
            backend
                .leftover_conflict_markers(&root, &[PathBuf::from("a.txt")])
                .unwrap()
                .is_empty()
        );
        assert!(backend.conflict_regions(&root, &both).unwrap().is_empty());
    }

    #[test]
    fn real_conflict_regions_come_from_git_and_end_with_the_markers_it_wrote() {
        let Some((root, backend)) = repository() else {
            return;
        };
        let git = |args: &[&str]| {
            backend
                .runner()
                .run(GitCommand::new().cwd(&root).args(args))
        };
        let commit = |contents: &str, message: &str| {
            std::fs::write(root.join("f.txt"), contents).unwrap();
            backend.stage(&root, &[PathBuf::from("f.txt")]).unwrap();
            backend.commit(&root, message, false, false).unwrap();
        };
        let regions = || {
            let diff = backend
                .zero_context_diff(&root, DiffTarget::IndexToWorktree, Path::new("f.txt"))
                .unwrap();
            crate::features::git::conflict::parse_regions(&diff)
        };
        commit(
            "1
a
2
b
3
",
            "base",
        );
        backend.create_branch(&root, "topic", None).unwrap();
        commit(
            "1
X
2
X2
3
",
            "topic",
        );
        backend.switch(&root, "main").unwrap();
        commit(
            "1
Y
2
Y2
3
",
            "main",
        );
        backend.switch(&root, "topic").unwrap();
        assert!(git(&["rebase", "main"]).is_err());

        let text = std::fs::read_to_string(root.join("f.txt")).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let found = regions();
        assert_eq!(found.len(), 1, "{text}");
        let region = &found[0];
        assert!(lines[region.start - 1].starts_with("<<<<<<<"), "{text}");
        assert!(lines[region.separator - 1].starts_with("======="), "{text}");
        assert!(lines[region.end - 1].starts_with(">>>>>>>"), "{text}");
        assert_eq!(lines[region.current.start - 1], "Y");
        assert_eq!(lines[region.incoming.start - 1], "X");

        // Deleting the markers does not resolve it for Git, but nothing is
        // left to show as a conflict.
        std::fs::write(
            root.join("f.txt"),
            "1
Y
2
X2
3
",
        )
        .unwrap();
        assert!(regions().is_empty());
    }

    #[test]
    fn real_repository_resolves_conflicts_and_continues_a_rebase() {
        let Some((root, backend)) = repository() else {
            return;
        };
        let git = |args: &[&str]| {
            backend
                .runner()
                .run(GitCommand::new().cwd(&root).args(args))
        };
        let commit = |name: &str, contents: &str, message: &str| {
            std::fs::write(root.join(name), contents).unwrap();
            backend.stage(&root, &[PathBuf::from(name)]).unwrap();
            backend.commit(&root, message, false, false).unwrap();
        };
        let scan = |backend: &CliGitBackend| {
            let discovered = backend.discover(std::slice::from_ref(&root)).unwrap();
            backend.status(&discovered[0], RepositoryId(1), 1).unwrap()
        };
        commit("a.txt", "base\n", "base");
        commit("b.txt", "base\n", "base b");
        backend.create_branch(&root, "topic", None).unwrap();
        commit("a.txt", "topic\n", "topic a");
        commit("b.txt", "topic\n", "topic b");
        backend.switch(&root, "main").unwrap();
        commit("a.txt", "main\n", "main a");
        commit("b.txt", "main\n", "main b");
        backend.switch(&root, "topic").unwrap();

        // Both topic commits conflict; the rebase stops on the first.
        assert!(git(&["rebase", "main"]).is_err());
        let status = scan(&backend);
        assert_eq!(status.repository_state, RepositoryState::RebaseInProgress);
        assert_eq!(status.operation_step, Some((1, 2)));
        assert!(status.files[&PathBuf::from("a.txt")].conflict.is_some());

        // "Ours" is the branch being rebased onto.
        backend
            .resolve_conflict(&root, Path::new("a.txt"), ConflictSide::Ours)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "main\n"
        );
        // Continuing must not wait for an editor.
        assert!(
            backend
                .continue_operation(&root, RepositoryState::RebaseInProgress)
                .is_err()
        );
        assert_eq!(scan(&backend).operation_step, Some((2, 2)));
        backend
            .resolve_conflict(&root, Path::new("b.txt"), ConflictSide::Theirs)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("b.txt")).unwrap(),
            "topic\n"
        );
        backend
            .continue_operation(&root, RepositoryState::RebaseInProgress)
            .unwrap();
        let status = scan(&backend);
        assert_eq!(status.repository_state, RepositoryState::Normal);
        assert_eq!(status.operation_step, None);

        // The stash reflog read during scans matches `git stash list`.
        std::fs::write(root.join("a.txt"), "first\n").unwrap();
        backend
            .stash_push(&root, Some("first"), false, false)
            .unwrap();
        std::fs::write(root.join("a.txt"), "second\n").unwrap();
        backend
            .stash_push(&root, Some("second"), false, false)
            .unwrap();
        let scanned = scan(&backend).stashes;
        assert_eq!(*scanned, backend.stash_list(&root).unwrap());
        assert_eq!(scanned[0].message, "On topic: second");
        backend.stash_apply(&root, 1, true).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "first\n"
        );
        assert_eq!(scan(&backend).stashes.len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn tag_snapshots_include_lightweight_annotated_and_packed_refs_outside_head() {
        let Some((root, backend)) = repository() else {
            return;
        };
        let git = |args: &[&str]| {
            backend
                .runner()
                .run(GitCommand::new().cwd(&root).args(args))
                .unwrap()
                .stdout_text()
                .unwrap()
        };
        git(&["commit", "--allow-empty", "-m", "initial"]);
        let initial = git(&["rev-parse", "HEAD"]).trim().to_owned();
        git(&["config", "tag.gpgSign", "true"]);
        backend.create_tag(&root, "v1.0", Some(&initial)).unwrap();
        git(&["config", "tag.gpgSign", "false"]);
        assert!(backend.create_tag(&root, "v1.0", Some(&initial)).is_err());
        for invalid in [
            "",
            "-oops",
            "bad name",
            "releases/.hidden",
            "releases/old.lock/x",
        ] {
            assert!(backend.create_tag(&root, invalid, Some(&initial)).is_err());
        }
        git(&["branch", "v1.0"]);
        git(&["tag", "-a", "releases/v2.0", "-m", "Release two"]);
        git(&["switch", "--orphan", "other"]);
        git(&["commit", "--allow-empty", "-m", "other root"]);
        git(&["tag", "other-root"]);
        let other = git(&["rev-parse", "HEAD"]).trim().to_owned();
        git(&["switch", "main"]);
        // A tag can also point at a tree; it must be listed without a commit action.
        git(&["tag", "tree", "HEAD^{tree}"]);
        git(&["pack-refs", "--all"]);
        let discovered = backend.discover(std::slice::from_ref(&root)).unwrap();
        let scan = || backend.status(&discovered[0], RepositoryId(1), 1).unwrap();
        let before = scan();
        for (name, commit) in [
            ("v1.0", Some(&initial)),
            ("releases/v2.0", Some(&initial)),
            ("other-root", Some(&other)),
            ("tree", None),
        ] {
            let tag = before.tags.iter().find(|tag| tag.name == name).unwrap();
            assert_eq!(tag.commit.as_ref(), commit);
        }
        assert_eq!(before.tags.len(), 4);
        let lightweight = backend.tag_details(&root, "v1.0").unwrap();
        assert_eq!(lightweight.target, initial);
        assert!(lightweight.annotation.is_none());
        assert_eq!(lightweight.commit.unwrap().message, "initial");
        let annotated = backend.tag_details(&root, "releases/v2.0").unwrap();
        assert_eq!(annotated.target, initial);
        let annotation = annotated.annotation.unwrap();
        assert_eq!(annotation.tagger, "Loom Test");
        assert!(!annotation.date.is_empty());
        assert_eq!(annotation.message, "Release two");
        assert_eq!(
            backend
                .tag_details(&root, "other-root")
                .unwrap()
                .commit
                .unwrap()
                .message,
            "other root"
        );
        let tree = backend.tag_details(&root, "tree").unwrap();
        assert_eq!(tree.target_kind, "tree");
        assert!(tree.commit.is_none());
        assert!(
            backend.tag_details(&root, "releases").is_err(),
            "A tag prefix must not display another tag's metadata"
        );
        backend.delete_tag(&root, "v1.0").unwrap();
        assert!(backend.tag_details(&root, "v1.0").is_err());
        assert!(backend.delete_tag(&root, "missing").is_err());
        assert_eq!(git(&["rev-parse", "refs/heads/v1.0"]).trim(), initial);
        let after = scan();
        assert_eq!(after.tags.len(), 3);
        assert!(
            !before.same_state(&after),
            "Tag changes must refresh the drawer even with unchanged HEAD"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn pushing_a_tag_sends_only_that_ref_and_preserves_remote_conflicts() {
        let Some((root, backend)) = repository() else {
            return;
        };
        let git = |args: &[&str]| {
            backend
                .runner()
                .run(GitCommand::new().cwd(&root).args(args))
                .unwrap()
                .stdout_text()
                .unwrap()
        };
        let remote = root.join("remote.git");
        git(&["init", "--bare", remote.to_str().unwrap()]);
        backend
            .add_remote(&root, "origin", remote.to_str().unwrap())
            .unwrap();
        git(&["commit", "--allow-empty", "-m", "initial"]);
        let initial = git(&["rev-parse", "HEAD"]).trim().to_owned();
        backend
            .create_tag(&root, "releases/v1", Some(&initial))
            .unwrap();
        git(&[
            "tag",
            "-a",
            "--no-sign",
            "-m",
            "Other release",
            "other/annotation",
        ]);
        git(&["branch", "releases/v1"]);
        git(&["config", "push.followTags", "true"]);
        git(&["config", "remote.origin.mirror", "true"]);
        git(&["config", "remote.origin.push", "+refs/heads/*:refs/heads/*"]);
        let remote_git = |args: &[&str]| {
            backend
                .runner()
                .run(GitCommand::new().cwd(&remote).args(args))
                .unwrap()
                .stdout_text()
                .unwrap()
        };
        backend.push_tag(&root, "origin", "releases/v1").unwrap();
        assert_eq!(
            remote_git(&["for-each-ref", "--format=%(refname)"])
                .lines()
                .collect::<Vec<_>>(),
            ["refs/tags/releases/v1"]
        );
        assert_eq!(
            remote_git(&["rev-parse", "refs/tags/releases/v1"]).trim(),
            initial
        );
        git(&["commit", "--allow-empty", "-m", "next"]);
        git(&["tag", "--force", "--no-sign", "releases/v1", "HEAD"]);
        assert!(backend.push_tag(&root, "origin", "releases/v1").is_err());
        assert_eq!(
            remote_git(&["rev-parse", "refs/tags/releases/v1"]).trim(),
            initial
        );
        backend
            .push_tag(&root, "origin", "other/annotation")
            .unwrap();
        assert_eq!(
            remote_git(&["rev-parse", "refs/tags/other/annotation"]),
            git(&["rev-parse", "refs/tags/other/annotation"])
        );
        assert_eq!(
            remote_git(&["cat-file", "-t", "refs/tags/other/annotation"]).trim(),
            "tag"
        );
        assert!(remote_git(&["for-each-ref", "refs/heads"]).is_empty());
        assert!(backend.push_tag(&root, "missing", "releases/v1").is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn history_pages_keep_their_snapshot_when_new_commits_arrive() {
        let Some((root, backend)) = repository() else {
            return;
        };
        let commit = |index| {
            backend
                .runner()
                .run(GitCommand::new().cwd(&root).args([
                    "commit",
                    "--allow-empty",
                    "-m",
                    &format!("Commit {index}"),
                ]))
                .unwrap();
        };
        for index in 0..56 {
            commit(index);
        }
        let head = backend.history(&root, None, None, 0, 1).unwrap()[0]
            .oid
            .clone();
        let mut first = backend.history(&root, Some(&head), None, 0, 51).unwrap();
        assert_eq!(first.len(), 51);
        first.truncate(50);
        commit(56);
        let second = backend.history(&root, Some(&head), None, 50, 51).unwrap();
        assert_eq!(second.len(), 6);
        first.extend(second);
        let expected = backend.history(&root, Some(&head), None, 0, 500).unwrap();
        // Ref badges follow live refs; the commit order stays anchored to head.
        assert_eq!(
            first.iter().map(|commit| &commit.oid).collect::<Vec<_>>(),
            expected
                .iter()
                .map(|commit| &commit.oid)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            first
                .iter()
                .map(|commit| &commit.oid)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            56
        );
        assert!(first.iter().all(|commit| commit.subject != "Commit 56"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn real_repository_amends_without_a_message_and_undoes_commits() {
        let Some((root, backend)) = repository() else {
            return;
        };
        let subjects = |backend: &CliGitBackend| {
            backend
                .history(&root, None, None, 0, 10)
                .unwrap()
                .into_iter()
                .map(|commit| commit.subject)
                .collect::<Vec<_>>()
        };
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        backend.stage(&root, &[PathBuf::from("a.txt")]).unwrap();
        backend.commit(&root, "first", false, false).unwrap();
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        backend.stage(&root, &[PathBuf::from("b.txt")]).unwrap();
        backend
            .commit(&root, "second\n\nwith a body", false, false)
            .unwrap();

        // An empty amend message keeps the commit's own.
        std::fs::write(root.join("c.txt"), "c\n").unwrap();
        backend.stage(&root, &[PathBuf::from("c.txt")]).unwrap();
        backend.commit(&root, "", true, false).unwrap();
        assert_eq!(subjects(&backend), ["second", "first"]);

        // Undo returns the full message and keeps the changes staged.
        assert_eq!(backend.undo_commit(&root).unwrap(), "second\n\nwith a body");
        assert_eq!(subjects(&backend), ["first"]);
        let discovered = backend.discover(std::slice::from_ref(&root)).unwrap();
        let status = backend.status(&discovered[0], RepositoryId(1), 1).unwrap();
        assert_eq!(
            status.files[&PathBuf::from("b.txt")].index,
            ChangeKind::Added
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn real_repository_creates_switches_and_tracks_branches() {
        let Some((root, backend)) = repository() else {
            return;
        };
        std::fs::write(root.join("tracked.txt"), "one\n").unwrap();
        backend
            .stage(&root, &[PathBuf::from("tracked.txt")])
            .unwrap();
        backend.commit(&root, "initial", false, false).unwrap();
        // A remote-tracking ref without a reachable remote.
        backend
            .add_remote(&root, "origin", "https://example.invalid/loom.git")
            .unwrap();
        backend
            .runner()
            .run(GitCommand::new().cwd(&root).args([
                "update-ref",
                "refs/remotes/origin/feature/remote",
                "HEAD",
            ]))
            .unwrap();
        let current = |backend: &CliGitBackend| {
            backend
                .branches(&root)
                .unwrap()
                .into_iter()
                .find(|branch| branch.current)
                .map(|branch| branch.name)
        };

        backend.create_branch(&root, "topic", None).unwrap();
        assert_eq!(current(&backend).as_deref(), Some("topic"));
        backend.switch(&root, "main").unwrap();
        assert_eq!(current(&backend).as_deref(), Some("main"));
        backend
            .switch_tracking(&root, "origin/feature/remote")
            .unwrap();
        let branches = backend.branches(&root).unwrap();
        let tracking = branches
            .iter()
            .find(|branch| branch.current)
            .expect("a branch is checked out");
        assert_eq!(tracking.name, "feature/remote");
        assert_eq!(tracking.upstream.as_deref(), Some("origin/feature/remote"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn groups_conflict_index_stages_by_path() {
        let parsed = parse_conflict_stages(
            b"100644 aaaa 1\tfile.txt\x00100644 bbbb 2\tfile.txt\x00100644 cccc 3\tfile.txt\0",
        )
        .unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].base.as_deref(), Some("aaaa"));
        assert_eq!(parsed[0].ours.as_deref(), Some("bbbb"));
        assert_eq!(parsed[0].theirs.as_deref(), Some("cccc"));
    }

    #[test]
    fn repository_changes_keep_both_sides_and_refresh_new_and_removed_files() {
        use crate::features::git::diff_document::DiffDocument;
        use crate::infrastructure::git::GitService;
        let Some((root, backend)) = repository() else {
            return;
        };
        std::fs::write(root.join("both.txt"), "original\n").unwrap();
        std::fs::write(root.join("binary.dat"), [0, 1, 2]).unwrap();
        std::fs::write(root.join("old.txt"), "rename only\n").unwrap();
        backend
            .stage(
                &root,
                &["both.txt".into(), "binary.dat".into(), "old.txt".into()],
            )
            .unwrap();
        backend.commit(&root, "initial", false, false).unwrap();
        std::fs::write(root.join("both.txt"), "staged\n").unwrap();
        backend.stage(&root, &["both.txt".into()]).unwrap();
        std::fs::write(root.join("both.txt"), "working\n").unwrap();
        std::fs::write(root.join("binary.dat"), [0, 3, 4]).unwrap();
        std::fs::rename(root.join("old.txt"), root.join("renamed.txt")).unwrap();
        backend
            .stage(&root, &["old.txt".into(), "renamed.txt".into()])
            .unwrap();
        std::fs::create_dir_all(root.join("drafts")).unwrap();
        std::fs::write(root.join("drafts/中文.txt"), "draft\n").unwrap();
        std::fs::write(root.join("untracked.bin"), [0, 9, 8]).unwrap();
        let service = GitService::new(backend);
        let snapshot = service.refresh(std::slice::from_ref(&root), None).unwrap();
        let repository = snapshot.active().unwrap();
        let request = DiffRequest {
            repository_root: repository.worktree_root.clone(),
            path: PathBuf::new(),
            target: DiffTarget::Changes,
        };
        let document = DiffDocument::from_content(
            request.clone(),
            service.refresh_diff(&request, &snapshot).unwrap(),
        );
        let sections = document.sections.as_ref().unwrap();
        assert!(
            sections
                .windows(2)
                .all(|pair| pair[0].group <= pair[1].group)
        );
        let file = |path: &str, group| {
            sections
                .iter()
                .find(|section| section.document.path == Path::new(path) && section.group == group)
                .unwrap()
        };
        assert!(
            file("both.txt", ChangeGroup::Staged)
                .document
                .rows
                .iter()
                .any(|row| row.text == "staged")
        );
        assert!(
            file("both.txt", ChangeGroup::Unstaged)
                .document
                .rows
                .iter()
                .any(|row| row.text == "working")
        );
        assert_eq!(
            file("drafts/中文.txt", ChangeGroup::Untracked)
                .document
                .additions(),
            1
        );
        assert!(file("binary.dat", ChangeGroup::Unstaged).document.binary);
        assert!(
            file("untracked.bin", ChangeGroup::Untracked)
                .document
                .binary
        );
        assert!(file("renamed.txt", ChangeGroup::Staged).error.is_none());
        std::fs::remove_file(root.join("drafts/中文.txt")).unwrap();
        std::fs::write(root.join("later.txt"), "added later\n").unwrap();
        let next = service.refresh(std::slice::from_ref(&root), None).unwrap();
        let refreshed = DiffDocument::from_content(
            request.clone(),
            service.refresh_diff(&request, &next).unwrap(),
        );
        let sections = refreshed.sections.unwrap();
        assert!(
            sections
                .iter()
                .any(|section| section.document.path == Path::new("later.txt"))
        );
        assert!(
            !sections
                .iter()
                .any(|section| section.document.path == Path::new("drafts/中文.txt"))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn commit_changes_list_every_file_of_the_commit() {
        use crate::features::git::diff_document::DiffDocument;
        use crate::infrastructure::git::GitService;
        let Some((root, backend)) = repository() else {
            return;
        };
        std::fs::write(root.join("kept.txt"), "one\n").unwrap();
        std::fs::write(root.join("gone.txt"), "bye\n").unwrap();
        std::fs::write(root.join("old.txt"), "rename only\n").unwrap();
        backend
            .stage(
                &root,
                &["kept.txt".into(), "gone.txt".into(), "old.txt".into()],
            )
            .unwrap();
        backend.commit(&root, "initial", false, false).unwrap();
        std::fs::write(root.join("kept.txt"), "two\n").unwrap();
        std::fs::remove_file(root.join("gone.txt")).unwrap();
        std::fs::rename(root.join("old.txt"), root.join("renamed.txt")).unwrap();
        std::fs::write(root.join("binary.dat"), [0, 1, 2]).unwrap();
        backend
            .stage(
                &root,
                &[
                    "kept.txt".into(),
                    "gone.txt".into(),
                    "old.txt".into(),
                    "renamed.txt".into(),
                    "binary.dat".into(),
                ],
            )
            .unwrap();
        backend
            .commit(&root, "second\n\nwith a body", false, false)
            .unwrap();
        let head = backend.history(&root, None, None, 0, 1).unwrap()[0]
            .oid
            .clone();
        let details = backend.commit_details(&root, &head).unwrap();
        assert_eq!(details.message, "second\n\nwith a body");
        assert!(!details.author.is_empty() && !details.date.is_empty());
        assert_eq!(
            (details.files_changed, details.insertions, details.deletions),
            (4, 1, 2)
        );
        let service = GitService::new(backend);
        let content = service.commit_changes(&root, &head).unwrap();
        let document = DiffDocument::commit_changes(root.clone(), head.clone(), content);
        assert_eq!(document.target, DiffTarget::Commit);
        assert!(document.title().starts_with("Commit "));
        let sections = document.sections.as_ref().unwrap();
        let file = |path: &str| {
            sections
                .iter()
                .find(|section| section.document.path == Path::new(path))
                .unwrap()
        };
        assert_eq!(sections.len(), 4);
        assert!(
            sections
                .iter()
                .all(|section| section.group == ChangeGroup::Commit
                    && section.error.is_none()
                    && section.document.revision.as_deref() == Some(head.as_str())
                    && section.document.blocks.is_empty())
        );
        assert!(
            file("kept.txt")
                .document
                .rows
                .iter()
                .any(|row| row.text == "two")
        );
        assert_eq!(file("gone.txt").document.deletions(), 1);
        assert!(file("binary.dat").document.binary);
        let preview = service
            .backend()
            .commit_file_diff(&root, &head, Path::new("kept.txt"))
            .unwrap();
        assert!(
            preview.files[0]
                .hunks
                .iter()
                .flat_map(|h| &h.lines)
                .any(|line| matches!(line, DiffLine::Addition(text) if text == "two"))
        );
        let binary = service
            .backend()
            .commit_file_diff(&root, &head, Path::new("binary.dat"))
            .unwrap();
        assert!(binary.files[0].binary);
        file("renamed.txt");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn polling_reloads_open_diffs_after_edits_stage_commit_and_deletion() {
        use crate::features::git::PollTarget;
        use crate::features::git::diff::DiffLine;
        use crate::infrastructure::git::{GitService, PollControl};
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let Some((root, backend)) = repository() else {
            return;
        };
        let tracked = PathBuf::from("tracked.txt");
        let untracked = PathBuf::from("new.txt");
        std::fs::write(root.join(&tracked), "one\n").unwrap();
        backend
            .stage(&root, std::slice::from_ref(&tracked))
            .unwrap();
        backend.commit(&root, "initial", false, false).unwrap();
        std::fs::write(root.join(&tracked), "one\ntwo\n").unwrap();
        std::fs::write(root.join(&untracked), "new\n").unwrap();
        let request = |path: &PathBuf, target| DiffRequest {
            repository_root: root.clone(),
            path: path.clone(),
            target,
        };
        let worktree = request(&tracked, DiffTarget::IndexToWorktree);
        let index = request(&tracked, DiffTarget::HeadToIndex);
        let added = request(&untracked, DiffTarget::IndexToWorktree);
        let control = PollControl::default();
        control.set_target(PollTarget {
            active_path: Some(root.join(&tracked)),
            diffs: vec![worktree.clone(), index.clone(), added.clone()],
            pinned_repository: None,
        });
        let service = GitService::new(backend);
        let phase = AtomicUsize::new(0);
        let stopped = AtomicBool::new(false);
        let has_addition = |content: &GitResult<DiffContent>, text: &str| {
            let DiffContent::Unified(diff) = content.as_ref().unwrap() else {
                panic!("expected a tracked diff")
            };
            diff.files
                .iter()
                .flat_map(|file| &file.hunks)
                .flat_map(|hunk| &hunk.lines)
                .any(|line| matches!(line, DiffLine::Addition(value) if value == text))
        };
        let empty = |content: &GitResult<DiffContent>| {
            assert_eq!(
                content.as_ref().unwrap(),
                &DiffContent::Unified(UnifiedDiff::default())
            );
        };
        service.poll(
            std::slice::from_ref(&root),
            &control,
            &stopped,
            Duration::from_millis(1),
            &|_, result| {
                let snapshot = result.unwrap();
                if snapshot.diffs.is_empty() {
                    return;
                } // Fast startup scan.
                match phase.fetch_add(1, Ordering::AcqRel) {
                    0 => {
                        assert!(has_addition(&snapshot.diffs[&worktree], "two"));
                        empty(&snapshot.diffs[&index]);
                        assert_eq!(
                            snapshot.diffs[&added],
                            Ok(DiffContent::Untracked("new\n".into()))
                        );
                        // Same porcelain status and numstat; diff content must still change.
                        std::fs::write(root.join(&tracked), "one\nthree\n").unwrap();
                        std::fs::write(root.join(&untracked), "changed\n").unwrap();
                    }
                    1 => {
                        assert!(has_addition(&snapshot.diffs[&worktree], "three"));
                        assert_eq!(
                            snapshot.diffs[&added],
                            Ok(DiffContent::Untracked("changed\n".into()))
                        );
                        service
                            .backend()
                            .stage(&root, std::slice::from_ref(&tracked))
                            .unwrap();
                        std::fs::remove_file(root.join(&untracked)).unwrap();
                    }
                    2 => {
                        empty(&snapshot.diffs[&worktree]);
                        empty(&snapshot.diffs[&added]);
                        assert!(has_addition(&snapshot.diffs[&index], "three"));
                        service
                            .backend()
                            .commit(&root, "update", false, false)
                            .unwrap();
                    }
                    3 => {
                        empty(&snapshot.diffs[&worktree]);
                        empty(&snapshot.diffs[&index]);
                        stopped.store(true, Ordering::Release);
                    }
                    _ => panic!("unexpected scan"),
                }
                control.request_refresh();
            },
        );
        assert_eq!(phase.load(Ordering::Acquire), 4);
        // Open diff tabs outlive removal of their workspace folder.
        std::fs::write(root.join(&untracked), "outside workspace\n").unwrap();
        let stopped = AtomicBool::new(false);
        service.poll(
            &[],
            &control,
            &stopped,
            Duration::from_millis(1),
            &|_, result| {
                let snapshot = result.unwrap();
                if snapshot.diffs.is_empty() {
                    return;
                }
                assert!(snapshot.repositories.is_empty());
                assert_eq!(
                    snapshot.diffs[&added],
                    Ok(DiffContent::Untracked("outside workspace\n".into()))
                );
                stopped.store(true, Ordering::Release);
            },
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fast_and_detailed_discovery_agree_on_repository_paths() {
        let Some((root, backend)) = repository() else {
            return;
        };
        let roots = [root.clone()];
        let fast = discovery::discover_fast(backend.runner(), &roots).unwrap();
        let detailed = backend.discover(&roots).unwrap();
        assert_eq!(fast, detailed);
        assert_eq!(fast[0].worktree_root, std::fs::canonicalize(&root).unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn read_only_commands_time_out_by_default() {
        assert!(GitCommand::new().timeout.is_none());
        assert!(GitCommand::new().read_only().timeout.is_some());
        let explicit = Duration::from_secs(5);
        assert_eq!(
            GitCommand::new().timeout(explicit).read_only().timeout,
            Some(explicit)
        );
    }
}
