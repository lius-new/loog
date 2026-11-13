//! Read-only, line-oriented documents shown by the diff editor.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::diff::{DiffLine, UnifiedDiff};
use super::{ChangeGroup, DiffContent, DiffRequest, DiffTarget, LineChange};
use loog_editor_core::text::display_width;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffRowKind {
    Context,
    Addition,
    Deletion,
    Hunk,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffRow {
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub text: String,
    pub kind: DiffRowKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitDiffRow {
    pub old_line: Option<usize>,
    pub old_text: Option<String>,
    pub new_line: Option<usize>,
    pub new_text: Option<String>,
    pub hunk: Option<String>,
    pub changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffDocument {
    /// COMPAT(local-git-diff-location): Local history and patch commands retain
    /// their native path fields until they use resource identities. Provided
    /// comparisons leave those fields empty and use this identity exclusively;
    /// remove the dual representation when local consumers migrate. No removal
    /// version is scheduled.
    pub provided: Option<ResourceDiffIdentity>,
    pub repository_root: PathBuf,
    pub path: PathBuf,
    pub target: DiffTarget,
    pub rows: Vec<DiffRow>,
    pub split_rows: Vec<SplitDiffRow>,
    pub binary: bool,
    /// The commit of a `DiffTarget::Commit` diff.
    pub revision: Option<String>,
    /// Change blocks that can be staged, unstaged or discarded on their own,
    /// in display order; empty when the diff offers no such actions.
    pub blocks: Vec<ChangeBlock>,
    /// Present only for the repository-wide Changes tab and a commit's
    /// all-files tab.
    pub sections: Option<Vec<DiffSection>>,
    /// Identity of a progressive overview read. Completed documents have none.
    pub loading: Option<u64>,
    metrics: DiffMetrics,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceDiffIdentity {
    pub repository: loog_workspace_api::ResourceRef,
    pub path: super::path::GitPath,
    pub file: loog_workspace_api::ResourceRef,
}

/// Diff row height at the default 12px code size.
const MIN_ROW_HEIGHT: f32 = 20.0;

pub fn row_height_for(font_size: f32) -> f32 {
    (font_size * MIN_ROW_HEIGHT / 12.0)
        .round()
        .max(MIN_ROW_HEIGHT)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffSection {
    pub document: Arc<DiffDocument>,
    pub group: ChangeGroup,
    pub error: Option<String>,
    pub loading: bool,
}

impl DiffSection {
    pub fn request(&self) -> DiffRequest {
        self.document.request()
    }
}

pub struct SectionLayout {
    pub index: usize,
    pub top: usize,
    pub body_top: usize,
    pub body_rows: usize,
}

pub struct ChangesLayout {
    pub sections: Vec<SectionLayout>,
    pub row_count: usize,
    pub starts: Arc<Vec<usize>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiffMetrics {
    split_projection: super::split_layout::Projection,
    additions: usize,
    deletions: usize,
    longest_inline: usize,
    longest_split: usize,
    inline_starts: Arc<Vec<usize>>,
    split_starts: Arc<Vec<usize>>,
    /// Runs in inline display-row coordinates, including deleted rows and hunk headers.
    inline_marks: Arc<Vec<(usize, usize, LineChange)>>,
}

impl DiffMetrics {
    fn new(rows: &[DiffRow], split_rows: &[SplitDiffRow]) -> Self {
        let mut inline_marks: Vec<(usize, usize, LineChange)> = Vec::new();
        let (mut longest_inline, mut longest_split) = (0, 0);
        for (index, row) in rows.iter().enumerate() {
            let width = display_width(&row.text);
            longest_inline = longest_inline.max(width);
            // Split reuses these same code strings; its hunk headings do not
            // occupy either code column. Measure the text only once.
            if row.kind != DiffRowKind::Hunk {
                longest_split = longest_split.max(width);
            }
            let change = match row.kind {
                DiffRowKind::Addition => LineChange::Added,
                DiffRowKind::Deletion => LineChange::Deleted,
                _ => continue,
            };
            match inline_marks.last_mut() {
                Some((first, lines, previous))
                    if *previous == change && *first + *lines == index =>
                {
                    *lines += 1;
                }
                _ => inline_marks.push((index, 1, change)),
            }
        }
        Self {
            split_projection: super::split_layout::Projection::new(split_rows),
            inline_marks: inline_marks.into(),
            additions: rows
                .iter()
                .filter(|row| row.kind == DiffRowKind::Addition)
                .count(),
            deletions: rows
                .iter()
                .filter(|row| row.kind == DiffRowKind::Deletion)
                .count(),
            longest_inline,
            longest_split,
            inline_starts: rows
                .iter()
                .enumerate()
                .filter_map(|(index, row)| {
                    let changed = matches!(row.kind, DiffRowKind::Addition | DiffRowKind::Deletion);
                    let previous = index > 0
                        && matches!(
                            rows[index - 1].kind,
                            DiffRowKind::Addition | DiffRowKind::Deletion
                        );
                    (changed && !previous).then_some(index)
                })
                .collect::<Vec<_>>()
                .into(),
            split_starts: split_rows
                .iter()
                .enumerate()
                .filter_map(|(index, row)| {
                    (row.changed && (index == 0 || !split_rows[index - 1].changed)).then_some(index)
                })
                .collect::<Vec<_>>()
                .into(),
        }
    }
}

impl DiffDocument {
    pub fn split_projection(&self) -> &super::split_layout::Projection {
        &self.metrics.split_projection
    }

    pub fn from_content(request: DiffRequest, content: DiffContent) -> Self {
        match content {
            DiffContent::Unified(diff) => {
                Self::from_unified(request.repository_root, request.path, request.target, diff)
            }
            DiffContent::Untracked(contents) => Self::added(
                request.repository_root,
                request.path,
                request.target,
                &contents,
            ),
            DiffContent::Changes(files) => {
                let sections = files
                    .into_iter()
                    .map(|file| {
                        let request = DiffRequest {
                            repository_root: request.repository_root.clone(),
                            path: file.path,
                            target: file.target,
                        };
                        let (content, error) = match file.content {
                            Ok(content) => (content, None),
                            Err(error) => (
                                DiffContent::Unified(UnifiedDiff::default()),
                                Some(error.user_message()),
                            ),
                        };
                        let document = Self::from_content(request, content);
                        DiffSection {
                            document: Arc::new(document),
                            group: file.group,
                            error,
                            loading: false,
                        }
                    })
                    .collect();
                let mut document = Self::from_unified(
                    request.repository_root,
                    PathBuf::new(),
                    request.target,
                    UnifiedDiff::default(),
                );
                document.sections = Some(sections);
                document
            }
        }
    }

    /// Every file of commit `revision`, one collapsible section each.
    pub fn commit_changes(
        repository_root: PathBuf,
        revision: String,
        content: DiffContent,
    ) -> Self {
        let mut document = Self::from_content(
            DiffRequest {
                repository_root,
                path: PathBuf::new(),
                target: DiffTarget::Commit,
            },
            content,
        );
        for section in document.sections.iter_mut().flatten() {
            Arc::make_mut(&mut section.document).revision = Some(revision.clone());
        }
        document.revision = Some(revision);
        document
    }

    pub fn request(&self) -> DiffRequest {
        DiffRequest {
            repository_root: self.repository_root.clone(),
            path: self.path.clone(),
            target: self.target,
        }
    }

    pub fn changes_layout(&self, split: bool, collapsed: &BTreeSet<DiffRequest>) -> ChangesLayout {
        let mut layout = ChangesLayout {
            sections: Vec::new(),
            row_count: 0,
            starts: Arc::default(),
        };
        let mut starts = Vec::new();
        for (index, section) in self.sections.iter().flatten().enumerate() {
            let top = layout.row_count;
            layout.row_count += 2;
            let body_top = layout.row_count;
            let body_rows = if collapsed.contains(&section.request()) {
                0
            } else {
                let rows = if split {
                    section.document.split_rows.len()
                } else {
                    section.document.rows.len()
                };
                starts.extend(
                    section
                        .document
                        .change_starts(split)
                        .iter()
                        .map(|row| body_top + row),
                );
                rows.max(1)
            };
            layout.row_count += body_rows;
            layout.sections.push(SectionLayout {
                index,
                top,
                body_top,
                body_rows,
            });
        }
        layout.starts = starts.into();
        layout
    }

    pub fn from_unified(
        repository_root: PathBuf,
        path: PathBuf,
        target: DiffTarget,
        diff: UnifiedDiff,
    ) -> Self {
        Self::from_payload(repository_root, path, target, diff)
    }

    pub fn from_resource(
        identity: ResourceDiffIdentity,
        target: DiffTarget,
        diff: UnifiedDiff<super::path::GitPath>,
    ) -> Self {
        let mut document = Self::from_payload(PathBuf::new(), PathBuf::new(), target, diff);
        document.provided = Some(identity);
        // Decoded text cannot be applied as a raw Git patch.
        document.blocks.clear();
        document
    }

    fn from_payload<P>(
        repository_root: PathBuf,
        path: PathBuf,
        target: DiffTarget,
        diff: UnifiedDiff<P>,
    ) -> Self {
        let mut rows = Vec::new();
        let mut binary = false;

        let mut encoded = false;
        if let Some(file) = diff.files.into_iter().next() {
            encoded = file.encoded;
            if let Some(change) = file.format_change {
                rows.push(DiffRow {
                    old_line: None,
                    new_line: None,
                    text: change,
                    kind: DiffRowKind::Hunk,
                });
            }
            binary = file.binary;
            for (hunk_index, hunk) in file.hunks.into_iter().enumerate() {
                if hunk_index > 0 || hunk.old_start > 1 || hunk.new_start > 1 {
                    rows.push(DiffRow {
                        old_line: None,
                        new_line: None,
                        text: format!(
                            "@@ -{},{} +{},{} @@{}",
                            hunk.old_start,
                            hunk.old_lines,
                            hunk.new_start,
                            hunk.new_lines,
                            if hunk.header.is_empty() {
                                String::new()
                            } else {
                                format!(" {}", hunk.header)
                            }
                        ),
                        kind: DiffRowKind::Hunk,
                    });
                }

                let mut old_line = hunk.old_start;
                let mut new_line = hunk.new_start;
                for line in hunk.lines {
                    match line {
                        DiffLine::Context(text) => {
                            rows.push(DiffRow {
                                old_line: line_number(old_line),
                                new_line: line_number(new_line),
                                text,
                                kind: DiffRowKind::Context,
                            });
                            old_line += 1;
                            new_line += 1;
                        }
                        DiffLine::Addition(text) => {
                            rows.push(DiffRow {
                                old_line: None,
                                new_line: line_number(new_line),
                                text,
                                kind: DiffRowKind::Addition,
                            });
                            new_line += 1;
                        }
                        DiffLine::Deletion(text) => {
                            rows.push(DiffRow {
                                old_line: line_number(old_line),
                                new_line: None,
                                text,
                                kind: DiffRowKind::Deletion,
                            });
                            old_line += 1;
                        }
                        DiffLine::NoNewline => {}
                    }
                }
            }
        }

        let split_rows = build_split_rows(&rows);
        let metrics = DiffMetrics::new(&rows, &split_rows);
        // Only the index and worktree can take a single change back.
        let blocks = if !binary
            && !encoded
            && matches!(
                target,
                DiffTarget::HeadToIndex | DiffTarget::IndexToWorktree
            ) {
            change_blocks(&rows)
        } else {
            Vec::new()
        };
        Self {
            provided: None,
            repository_root,
            path,
            target,
            rows,
            split_rows,
            binary,
            revision: None,
            blocks,
            sections: None,
            loading: None,
            metrics,
        }
    }

    /// The diff of `path` in commit `revision`.
    pub fn commit(
        repository_root: PathBuf,
        path: PathBuf,
        revision: String,
        diff: UnifiedDiff,
    ) -> Self {
        Self {
            revision: Some(revision),
            ..Self::from_unified(repository_root, path, DiffTarget::Commit, diff)
        }
    }

    /// Whether `other` shows the same file, side and commit.
    pub fn same_view(&self, other: &Self) -> bool {
        (match (&self.provided, &other.provided) {
            (Some(a), Some(b)) => a == b && self.target == other.target,
            (None, None) => self.matches(&other.repository_root, &other.path, other.target),
            _ => false,
        }) && self.revision == other.revision
    }

    pub fn added(
        repository_root: PathBuf,
        path: PathBuf,
        target: DiffTarget,
        contents: &str,
    ) -> Self {
        let rows: Vec<DiffRow> = contents
            .lines()
            .enumerate()
            .map(|(index, text)| DiffRow {
                old_line: None,
                new_line: Some(index + 1),
                text: text.to_owned(),
                kind: DiffRowKind::Addition,
            })
            .collect();
        let split_rows = build_split_rows(&rows);
        let metrics = DiffMetrics::new(&rows, &split_rows);
        Self {
            provided: None,
            repository_root,
            path,
            target,
            rows,
            split_rows,
            binary: false,
            revision: None,
            blocks: Vec::new(),
            sections: None,
            loading: None,
            metrics,
        }
    }

    pub fn resource(&self) -> loog_workspace_api::ResourceRef {
        self.provided.as_ref().map_or_else(
            || loog_workspace_api::ResourceRef::local(self.repository_root.join(&self.path)),
            |identity| identity.file.clone(),
        )
    }

    pub fn path_label(&self) -> String {
        self.provided.as_ref().map_or_else(
            || self.path.display().to_string(),
            |identity| identity.path.display().to_string(),
        )
    }

    pub fn title(&self) -> String {
        if let Some(identity) = &self.provided {
            return format!(
                "{} ({})",
                identity
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
                self.source_label()
            );
        }
        if self.sections.is_some() {
            let repository = self
                .repository_root
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            return match &self.revision {
                Some(revision) => format!("Commit {} ({repository})", short_oid(revision)),
                None => format!("Changes ({repository})"),
            };
        }
        let name = self
            .path
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.to_string_lossy().into_owned());
        match &self.revision {
            Some(revision) => format!("{name} ({})", short_oid(revision)),
            None => format!("{name} ({})", self.source_label()),
        }
    }

    pub fn source_label(&self) -> &'static str {
        match self.target {
            DiffTarget::Changes => "All Changes",
            DiffTarget::HeadToIndex => "Index",
            DiffTarget::IndexToWorktree | DiffTarget::HeadToWorktree => "Working Tree",
            DiffTarget::Commit => "Commit",
        }
    }

    pub fn matches(&self, repository_root: &Path, path: &Path, target: DiffTarget) -> bool {
        self.provided.is_none()
            && self.repository_root == repository_root
            && self.path == path
            && self.target == target
    }

    pub fn additions(&self) -> usize {
        if let Some(sections) = &self.sections {
            return sections
                .iter()
                .map(|section| section.document.additions())
                .sum();
        }
        self.metrics.additions
    }

    pub fn deletions(&self) -> usize {
        if let Some(sections) = &self.sections {
            return sections
                .iter()
                .map(|section| section.document.deletions())
                .sum();
        }
        self.metrics.deletions
    }

    pub fn changed_file_count(&self) -> usize {
        self.sections
            .as_ref()
            .map(|sections| {
                sections
                    .iter()
                    .map(|section| section.document.path.as_path())
                    .collect::<BTreeSet<_>>()
                    .len()
            })
            .unwrap_or(1)
    }

    pub fn change_starts(&self, split: bool) -> Arc<Vec<usize>> {
        if split {
            self.metrics.split_starts.clone()
        } else {
            self.metrics.inline_starts.clone()
        }
    }

    pub fn inline_change_marks(&self) -> &[(usize, usize, LineChange)] {
        &self.metrics.inline_marks
    }

    pub fn longest_line(&self, split: bool) -> usize {
        if let Some(sections) = &self.sections {
            return sections
                .iter()
                .map(|section| section.document.longest_line(split))
                .max()
                .unwrap_or_default();
        }
        if split {
            self.metrics.longest_split
        } else {
            self.metrics.longest_inline
        }
    }
}

/// A run of changed lines, numbered as a zero-context hunk header numbers
/// it: an empty side starts at the line before the change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChangeBlock {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
}

/// The change blocks of `rows`, in the order `change_starts` lists them.
fn change_blocks(rows: &[DiffRow]) -> Vec<ChangeBlock> {
    let mut blocks = Vec::new();
    let (mut last_old, mut last_new) = (0, 0);
    let mut index = 0;
    while index < rows.len() {
        let row = &rows[index];
        if !matches!(row.kind, DiffRowKind::Addition | DiffRowKind::Deletion) {
            last_old = row.old_line.unwrap_or(last_old);
            last_new = row.new_line.unwrap_or(last_new);
            index += 1;
            continue;
        }
        let mut block = ChangeBlock {
            old_start: last_old,
            old_lines: 0,
            new_start: last_new,
            new_lines: 0,
        };
        while let Some(row) = rows
            .get(index)
            .filter(|row| matches!(row.kind, DiffRowKind::Addition | DiffRowKind::Deletion))
        {
            if let Some(line) = row.old_line {
                if block.old_lines == 0 {
                    block.old_start = line;
                }
                block.old_lines += 1;
                last_old = line;
            }
            if let Some(line) = row.new_line {
                if block.new_lines == 0 {
                    block.new_start = line;
                }
                block.new_lines += 1;
                last_new = line;
            }
            index += 1;
        }
        blocks.push(block);
    }
    blocks
}

/// The abbreviated commit id shown in titles.
pub fn short_oid(oid: &str) -> &str {
    &oid[..oid.len().min(8)]
}

fn build_split_rows(rows: &[DiffRow]) -> Vec<SplitDiffRow> {
    let mut result = Vec::new();
    let mut index = 0;
    while index < rows.len() {
        match rows[index].kind {
            DiffRowKind::Context => {
                let row = &rows[index];
                result.push(SplitDiffRow {
                    old_line: row.old_line,
                    old_text: Some(row.text.clone()),
                    new_line: row.new_line,
                    new_text: Some(row.text.clone()),
                    hunk: None,
                    changed: false,
                });
                index += 1;
            }
            DiffRowKind::Hunk => {
                result.push(SplitDiffRow {
                    old_line: None,
                    old_text: None,
                    new_line: None,
                    new_text: None,
                    hunk: Some(rows[index].text.clone()),
                    changed: false,
                });
                index += 1;
            }
            DiffRowKind::Addition | DiffRowKind::Deletion => {
                let block_start = index;
                while index < rows.len()
                    && matches!(
                        rows[index].kind,
                        DiffRowKind::Addition | DiffRowKind::Deletion
                    )
                {
                    index += 1;
                }
                let block = &rows[block_start..index];
                let deletions = block
                    .iter()
                    .filter(|row| row.kind == DiffRowKind::Deletion)
                    .collect::<Vec<_>>();
                let additions = block
                    .iter()
                    .filter(|row| row.kind == DiffRowKind::Addition)
                    .collect::<Vec<_>>();
                for offset in 0..deletions.len().max(additions.len()) {
                    let old = deletions.get(offset).copied();
                    let new = additions.get(offset).copied();
                    result.push(SplitDiffRow {
                        old_line: old.and_then(|row| row.old_line),
                        old_text: old.map(|row| row.text.clone()),
                        new_line: new.and_then(|row| row.new_line),
                        new_text: new.map(|row| row.text.clone()),
                        hunk: None,
                        changed: true,
                    });
                }
            }
        }
    }
    result
}

fn line_number(value: usize) -> Option<usize> {
    (value > 0).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::git::diff::parse_unified;

    #[test]
    fn resource_diff_identity_does_not_alias_other_environments_or_native_paths() {
        use loog_workspace_api::{EnvironmentKey, ResourceRef};
        let document = |instance| {
            let environment = EnvironmentKey::new("fixture", instance).unwrap();
            DiffDocument::from_resource(
                ResourceDiffIdentity {
                    repository: ResourceRef::provided(environment.clone(), "/repo").unwrap(),
                    path: super::super::path::GitPath::from_git("中文\\file[1].txt".as_bytes())
                        .unwrap(),
                    file: ResourceRef::provided(environment, "/repo/中文\\file[1].txt").unwrap(),
                },
                DiffTarget::IndexToWorktree,
                UnifiedDiff::default(),
            )
        };
        let first = document("first");
        assert!(first.same_view(&document("first")));
        assert!(!first.same_view(&document("second")));
        assert!(!first.same_view(&DiffDocument::added(
            PathBuf::new(),
            PathBuf::new(),
            DiffTarget::IndexToWorktree,
            ""
        )));
        assert!(!first.matches(Path::new(""), Path::new(""), DiffTarget::IndexToWorktree));
        assert_eq!(first.path_label(), "中文\\file[1].txt");
        assert!(first.resource().local_path().is_none());
        assert!(first.blocks.is_empty());
    }

    #[test]
    fn change_blocks_are_numbered_like_zero_context_hunks() {
        // Replace line 2, insert after line 4, delete line 6.
        let diff = parse_unified(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,6 +1,6 @@\n 1\n-2\n+two\n 3\n 4\n+4b\n 5\n-6\n",
        )
        .unwrap();
        let document =
            DiffDocument::from_unified("r".into(), "f".into(), DiffTarget::IndexToWorktree, diff);
        let block = |old_start, old_lines, new_start, new_lines| ChangeBlock {
            old_start,
            old_lines,
            new_start,
            new_lines,
        };
        assert_eq!(
            document.blocks,
            [block(2, 1, 2, 1), block(4, 0, 5, 1), block(6, 1, 6, 0)]
        );
        assert_eq!(document.change_starts(false).len(), document.blocks.len());
        assert_eq!(document.change_starts(true).len(), document.blocks.len());
        // Commits and new files offer no block actions.
        let added = DiffDocument::added("r".into(), "f".into(), DiffTarget::IndexToWorktree, "x\n");
        assert!(added.blocks.is_empty());
    }

    #[test]
    fn diff_widths_use_tab_stops_and_unicode_display_columns() {
        let diff = parse_unified(
            "diff --git a/a.vue b/a.vue\n--- a/a.vue\n+++ b/a.vue\n@@ -1,1 +1,1 @@\n-\t\t<div>\n+a\t中\tb\n",
        )
        .unwrap();
        let document = DiffDocument::from_unified(
            "repo".into(),
            "a.vue".into(),
            DiffTarget::IndexToWorktree,
            diff,
        );
        assert_eq!(document.longest_line(false), 13);
        assert_eq!(document.longest_line(true), 13);
        assert_eq!(document.rows[0].text, "\t\t<div>");
        assert_eq!(document.split_rows[0].new_text.as_deref(), Some("a\t中\tb"));

        let added = DiffDocument::added(
            "repo".into(),
            "a.vue".into(),
            DiffTarget::HeadToWorktree,
            "a\t中\tb\r\ne\u{301}\t👩‍💻\tx\r\n",
        );
        assert_eq!(added.longest_line(false), 9);
        assert_eq!(added.longest_line(true), 9);
        assert_eq!(added.rows[0].text, "a\t中\tb");
    }

    #[test]
    fn split_width_excludes_hunk_headings() {
        let document = DiffDocument::from_unified(
            "repo".into(), "a.txt".into(), DiffTarget::IndexToWorktree,
            parse_unified("diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -10,1 +10,1 @@ region\n-x\n+y\n").unwrap(),
        );
        assert_eq!(document.longest_line(false), 24);
        assert_eq!(document.longest_line(true), 1);
    }

    #[test]
    fn converts_a_unified_diff_to_numbered_editor_rows() {
        let diff = parse_unified(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,3 @@\n same\n-old\n+new\n+more\n",
        )
        .unwrap();
        let document = DiffDocument::from_unified(
            PathBuf::from("repo"),
            PathBuf::from("a.txt"),
            DiffTarget::IndexToWorktree,
            diff,
        );

        assert_eq!(document.rows.len(), 4);
        assert_eq!(document.rows[0].old_line, Some(1));
        assert_eq!(document.rows[0].new_line, Some(1));
        assert_eq!(document.rows[1].kind, DiffRowKind::Deletion);
        assert_eq!(document.rows[1].new_line, None);
        assert_eq!(document.rows[2].kind, DiffRowKind::Addition);
        assert_eq!(document.rows[2].new_line, Some(2));
        assert_eq!(document.split_rows.len(), 3);
        assert_eq!(document.split_rows[1].old_text.as_deref(), Some("old"));
        assert_eq!(document.split_rows[1].new_text.as_deref(), Some("new"));
        assert_eq!(document.split_rows[2].old_text, None);
        assert_eq!(document.split_rows[2].new_text.as_deref(), Some("more"));
        assert_eq!(document.change_starts(false).as_slice(), &[1]);
        assert_eq!(document.change_starts(true).as_slice(), &[1]);
        assert_eq!(document.title(), "a.txt (Working Tree)");
    }
}
