//! What the next commit takes: whole files and chosen change blocks.
//!
//! The choice lives only in the interface. Nothing is staged when a box is
//! ticked; the commit button adds and commits exactly these targets.
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use super::diff_document::ChangeBlock;

/// How a changed file takes part in the next commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitChoice {
    Excluded,
    /// The whole file, as it is on disk.
    Included,
    /// Only these change blocks of the file, against HEAD.
    Blocks(BTreeSet<ChangeBlock>),
}

/// The look of a file's checkbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckState {
    Unchecked,
    Checked,
    Partial,
}

impl CommitChoice {
    /// Tracked changes are taken by default and new files are not.
    pub fn default_for(untracked: bool) -> Self {
        if untracked {
            Self::Excluded
        } else {
            Self::Included
        }
    }

    pub fn check_state(&self) -> CheckState {
        match self {
            Self::Excluded => CheckState::Unchecked,
            Self::Included => CheckState::Checked,
            Self::Blocks(blocks) if blocks.is_empty() => CheckState::Unchecked,
            Self::Blocks(_) => CheckState::Partial,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CommitSelection {
    choices: HashMap<PathBuf, CommitChoice>,
}

impl CommitSelection {
    pub fn choice(&self, path: &Path, untracked: bool) -> CommitChoice {
        self.choices
            .get(path)
            .cloned()
            .unwrap_or_else(|| CommitChoice::default_for(untracked))
    }

    fn set(&mut self, path: PathBuf, choice: CommitChoice, untracked: bool) {
        if choice == CommitChoice::default_for(untracked) {
            self.choices.remove(&path);
        } else {
            self.choices.insert(path, choice);
        }
    }

    /// Ticking a file takes all of it; unticking drops it, partial or not.
    pub fn toggle_file(&mut self, path: PathBuf, untracked: bool) {
        let next = match self.choice(&path, untracked) {
            CommitChoice::Included => CommitChoice::Excluded,
            _ => CommitChoice::Included,
        };
        self.set(path, next, untracked);
    }

    /// Flip one block of `path`, whose blocks are `all`. Taking every block
    /// takes the whole file, and none leaves it out.
    pub fn toggle_block(
        &mut self,
        path: PathBuf,
        untracked: bool,
        block: ChangeBlock,
        all: &[ChangeBlock],
    ) {
        let mut chosen: BTreeSet<ChangeBlock> = match self.choice(&path, untracked) {
            CommitChoice::Excluded => BTreeSet::new(),
            CommitChoice::Included => all.iter().copied().collect(),
            CommitChoice::Blocks(blocks) => blocks,
        };
        if !chosen.remove(&block) {
            chosen.insert(block);
        }
        let next = if chosen.is_empty() {
            CommitChoice::Excluded
        } else if all.iter().all(|block| chosen.contains(block)) {
            CommitChoice::Included
        } else {
            CommitChoice::Blocks(chosen)
        };
        self.set(path, next, untracked);
    }

    /// Whether `block` of `path` is taken.
    pub fn takes_block(&self, path: &Path, untracked: bool, block: &ChangeBlock) -> bool {
        match self.choice(path, untracked) {
            CommitChoice::Excluded => false,
            CommitChoice::Included => true,
            CommitChoice::Blocks(blocks) => blocks.contains(block),
        }
    }

    /// Take every one of `files` (path, untracked), or none of them.
    pub fn set_all<'a>(&mut self, files: impl Iterator<Item = (&'a Path, bool)>, included: bool) {
        for (path, untracked) in files {
            let choice = if included {
                CommitChoice::Included
            } else {
                CommitChoice::Excluded
            };
            self.set(path.to_path_buf(), choice, untracked);
        }
    }

    pub fn clear(&mut self) {
        self.choices.clear();
    }

    /// What a commit takes out of `files` (path, untracked).
    pub fn targets<'a>(&self, files: impl Iterator<Item = (&'a Path, bool)>) -> CommitTargets {
        let mut targets = CommitTargets::default();
        for (path, untracked) in files {
            match self.choice(path, untracked) {
                CommitChoice::Excluded => {}
                CommitChoice::Included => targets.whole.push(path.to_path_buf()),
                CommitChoice::Blocks(blocks) if blocks.is_empty() => {}
                CommitChoice::Blocks(blocks) => targets
                    .blocks
                    .push((path.to_path_buf(), blocks.into_iter().collect())),
            }
        }
        targets
    }
}

/// The files and blocks a commit takes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitTargets {
    pub whole: Vec<PathBuf>,
    pub blocks: Vec<(PathBuf, Vec<ChangeBlock>)>,
}

impl CommitTargets {
    pub fn is_empty(&self) -> bool {
        self.whole.is_empty() && self.blocks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(start: usize) -> ChangeBlock {
        ChangeBlock {
            old_start: start,
            old_lines: 1,
            new_start: start,
            new_lines: 1,
        }
    }

    #[test]
    fn tracked_files_default_in_and_new_files_default_out() {
        let selection = CommitSelection::default();
        assert_eq!(
            selection.choice(Path::new("a"), false),
            CommitChoice::Included
        );
        assert_eq!(
            selection.choice(Path::new("b"), true),
            CommitChoice::Excluded
        );
    }

    #[test]
    fn toggling_a_file_flips_it_and_a_partial_file_becomes_whole() {
        let mut selection = CommitSelection::default();
        selection.toggle_file("a".into(), false);
        assert_eq!(
            selection.choice(Path::new("a"), false),
            CommitChoice::Excluded
        );
        selection.toggle_file("a".into(), false);
        assert_eq!(
            selection.choice(Path::new("a"), false),
            CommitChoice::Included
        );
        let all = [block(1), block(5)];
        selection.toggle_block("a".into(), false, all[0], &all);
        assert_eq!(
            selection.choice(Path::new("a"), false).check_state(),
            CheckState::Partial
        );
        selection.toggle_file("a".into(), false);
        assert_eq!(
            selection.choice(Path::new("a"), false),
            CommitChoice::Included
        );
    }

    #[test]
    fn blocks_collapse_to_the_whole_file_or_nothing() {
        let mut selection = CommitSelection::default();
        let all = [block(1), block(5)];
        selection.toggle_block("a".into(), false, all[0], &all);
        assert!(!selection.takes_block(Path::new("a"), false, &all[0]));
        assert!(selection.takes_block(Path::new("a"), false, &all[1]));
        selection.toggle_block("a".into(), false, all[1], &all);
        assert_eq!(
            selection.choice(Path::new("a"), false),
            CommitChoice::Excluded
        );
        selection.toggle_block("a".into(), false, all[0], &all);
        selection.toggle_block("a".into(), false, all[1], &all);
        assert_eq!(
            selection.choice(Path::new("a"), false),
            CommitChoice::Included
        );
    }

    #[test]
    fn targets_list_whole_files_and_chosen_blocks() {
        let mut selection = CommitSelection::default();
        let all = [block(1), block(5)];
        selection.toggle_block("b".into(), false, all[0], &all);
        selection.toggle_file("c".into(), true);
        let files = [
            (Path::new("a"), false),
            (Path::new("b"), false),
            (Path::new("c"), true),
            (Path::new("d"), true),
        ];
        let targets = selection.targets(files.into_iter());
        assert_eq!(targets.whole, [PathBuf::from("a"), PathBuf::from("c")]);
        assert_eq!(targets.blocks, [(PathBuf::from("b"), vec![all[1]])]);
    }
}
