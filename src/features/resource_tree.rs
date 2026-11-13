//! Native tree state for provider-owned resources; no path parsing or IO.
use loog_workspace_api::{FileInfo, PresentedRoot, ProviderPath, RequestId, ScopeId};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub scope: ScopeId,
    pub path: ProviderPath,
    pub parent: Option<ProviderPath>,
    pub name: String,
    pub kind: loog_workspace_api::FileKind,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    NewFile,
    NewFolder,
    Rename,
    Delete,
}
#[derive(Clone)]
pub struct Edit {
    pub target: Target,
    pub kind: EditKind,
}

pub const MAX_ENTRIES: usize = 16_384;

#[derive(Clone, Default)]
pub struct Directory {
    pub generation: u64,
    pub expanded: bool,
    pub loaded: bool,
    pub entries: Vec<FileInfo>,
    pub cursor: Option<String>,
    pub pending: Option<RequestId>,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct Root {
    pub descriptor: PresentedRoot,
    pub directories: HashMap<ProviderPath, Directory>,
    pub watch: ResourceWatch,
}
#[derive(Clone, Default)]
pub struct ResourceWatch {
    pub handle: Option<u64>,
    pub pending: bool,
    pub error: Option<String>,
    pub sequence: u64,
    pub changed: HashSet<ProviderPath>,
    pub changed_bytes: usize,
    pub rescan: bool,
    pub queued: bool,
}

#[derive(Clone, Default)]
pub struct Tree {
    pub session: u64,
    pub trust_prompt: Option<loog_workspace_api::ResourceRef>,
    pub trust_review: Option<loog_workspace_api::ResourceRef>,
    pub bookmarks: Vec<loog_workspace_api::session::ResourceBookmark>,
    pub roots: Vec<Root>,
    pub open_generation: u64,
    pub directory_generation: u64,
    pub pending_open: Option<RequestId>,
    pub busy: HashSet<ScopeId>,
    pub edit: Option<Edit>,
}
impl Tree {
    pub fn sync(&mut self, roots: Vec<PresentedRoot>) {
        self.busy
            .retain(|scope| roots.iter().any(|root| root.scope == *scope));
        if self
            .edit
            .as_ref()
            .is_some_and(|edit| !roots.iter().any(|root| root.scope == edit.target.scope))
        {
            self.edit = None;
        }
        self.roots
            .retain(|root| roots.iter().any(|r| r.scope == root.descriptor.scope));
        for descriptor in roots {
            if !self
                .roots
                .iter()
                .any(|root| root.descriptor.scope == descriptor.scope)
            {
                self.roots.push(Root {
                    descriptor,
                    directories: HashMap::new(),
                    watch: ResourceWatch::default(),
                });
            }
        }
    }
    pub fn root(&self, scope: ScopeId) -> Option<&Root> {
        self.roots
            .iter()
            .find(|root| root.descriptor.scope == scope)
    }
    pub fn root_mut(&mut self, scope: ScopeId) -> Option<&mut Root> {
        self.roots
            .iter_mut()
            .find(|root| root.descriptor.scope == scope)
    }
    pub fn directory_mut(&mut self, scope: ScopeId, path: &ProviderPath) -> Option<&mut Directory> {
        let root = self.root_mut(scope)?;
        if !root.directories.contains_key(path) && root.directories.len() >= MAX_ENTRIES {
            return None;
        }
        Some(root.directories.entry(path.clone()).or_default())
    }
}
