//! Resource repository identity and asynchronous result guards.
use super::{RepositorySnapshot, path::GitPath};
use loog_workspace_api::{PresentedRoot, ResourceRef, ScopeId};
use std::{collections::BTreeMap, sync::Arc};

pub type Snapshot = RepositorySnapshot<ResourceRef, GitPath>;
pub type Groups = [Arc<Vec<GitPath>>; 4];
pub fn groups(repository: &Snapshot) -> Groups {
    use super::ChangeKind;
    let mut groups: [Vec<GitPath>; 4] = Default::default();
    for (path, file) in repository.files.iter() {
        if file.conflict.is_some() {
            groups[0].push(path.clone());
            continue;
        }
        if file.index != ChangeKind::Unmodified {
            groups[1].push(path.clone());
        }
        match file.worktree {
            ChangeKind::Unmodified => {}
            ChangeKind::Untracked => groups[3].push(path.clone()),
            _ => groups[2].push(path.clone()),
        }
    }
    groups.map(Arc::new)
}

#[derive(Clone)]
pub enum Intent {
    Select(Option<ScopeId>),
    Refresh(ScopeId),
    Stage(ScopeId, Vec<GitPath>, bool),
    Commit(ScopeId),
    Diff(ScopeId, GitPath, super::DiffTarget),
}

#[derive(Clone)]
pub struct Entry {
    pub root: PresentedRoot,
    pub target: ResourceRef,
    pub target_is_file: bool,
    pub sequence: u64,
    pub busy: bool,
    pub dirty: bool,
    pub repository: Option<Arc<Snapshot>>,
    pub groups: Groups,
    pub error: Option<String>,
}
#[derive(Clone, Default)]
pub struct Model {
    pub entries: BTreeMap<ScopeId, Entry>,
    pub selected: Option<ScopeId>,
    pub offline: Option<(ResourceRef, String)>,
    pub active: Option<ResourceRef>,
    pub opening: u64,
}
impl Model {
    pub fn accepts(&self, scope: ScopeId, sequence: u64, target: &ResourceRef) -> bool {
        self.entries
            .get(&scope)
            .is_some_and(|entry| entry.sequence == sequence && &entry.target == target)
    }
}
