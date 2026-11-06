//! Explorer business state: workspace roots, recent folders, loaded
//! directory listings and the pending inline create/rename request.
//!
//! Fields are private; every transition keeps roots, listings and edit
//! requests consistent. Disk access is injected per call.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::ports::DirectoryPort;
use super::tree::{self, TreeCache};
use super::{
    DirEntry, ExplorerCreateKind, ExplorerCreateRequest, ExplorerRenameRequest, ExplorerTargetKind,
    roots,
};

#[derive(Clone, Debug, Default)]
pub struct ExplorerModel {
    roots: Vec<PathBuf>,
    recents: Vec<PathBuf>,
    tree: TreeCache,
    create: Option<ExplorerCreateRequest>,
    rename: Option<ExplorerRenameRequest>,
    /// A refresh was skipped to protect an open create/rename row.
    pending_refresh: bool,
}

/// Outcome of revealing a path in the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reveal {
    OutsideWorkspace,
    /// Ancestors are loaded and expanded; `row` is the target's visible row.
    Revealed {
        row: Option<usize>,
    },
}

impl ExplorerModel {
    // --- roots and recent folders -------------------------------------

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn recents(&self) -> &[PathBuf] {
        &self.recents
    }

    pub fn is_root(&self, path: &Path) -> bool {
        self.roots.iter().any(|root| root == path)
    }

    /// The deepest root containing `path`.
    pub fn root_for_path(&self, path: &Path) -> Option<&PathBuf> {
        roots::root_for_path(&self.roots, path)
    }

    /// Install roots and recents from a saved session, unvalidated; call
    /// [`Self::hydrate`] to drop missing folders and load the roots.
    pub fn restore(&mut self, roots: Vec<PathBuf>, recents: Vec<PathBuf>) {
        self.roots = roots;
        self.recents = recents;
    }

    /// Validate restored folders and load every root expanded.
    pub fn hydrate(&mut self, port: &dyn DirectoryPort) {
        roots::hydrate(port, &mut self.roots, &mut self.recents);
        for root in &self.roots {
            self.tree.load_expanded(port, root);
        }
    }

    /// Replace the workspace with a single folder.
    pub fn open_root(&mut self, port: &dyn DirectoryPort, path: PathBuf) {
        self.roots.clear();
        self.roots.push(path.clone());
        self.tree.clear();
        self.tree.load_expanded(port, &path);
        roots::remember(&mut self.recents, path);
    }

    /// Add a folder to the workspace. Returns false (after remembering it)
    /// when it is already a root.
    pub fn add_root(&mut self, port: &dyn DirectoryPort, path: PathBuf) -> bool {
        if self.roots.contains(&path) {
            roots::remember(&mut self.recents, path);
            return false;
        }
        self.roots.push(path.clone());
        self.tree.load_expanded(port, &path);
        roots::remember(&mut self.recents, path);
        true
    }

    /// Remove a root without touching the disk. Returns `None` when `root`
    /// is not a root, otherwise whether an edit row below it was cancelled.
    pub fn remove_root(&mut self, root: &Path) -> Option<bool> {
        let original_len = self.roots.len();
        self.roots.retain(|folder| folder != root);
        if self.roots.len() == original_len {
            return None;
        }
        self.tree.remove_under(root);
        let mut cancelled = false;
        if self
            .create
            .as_ref()
            .is_some_and(|request| request.parent.starts_with(root))
        {
            self.create = None;
            cancelled = true;
        }
        if self
            .rename
            .as_ref()
            .is_some_and(|request| request.path.starts_with(root))
        {
            self.rename = None;
            cancelled = true;
        }
        Some(cancelled)
    }

    pub fn remember(&mut self, path: PathBuf) {
        roots::remember(&mut self.recents, path);
    }

    pub fn forget_recent(&mut self, path: &Path) {
        self.recents.retain(|recent| recent != path);
    }

    /// Follow a completed rename of `old_path` to `new_path`: retarget roots
    /// and recents, rebuild affected listings and close the rename row.
    /// Returns whether a root changed.
    pub fn apply_rename(
        &mut self,
        port: &dyn DirectoryPort,
        old_path: &Path,
        new_path: &Path,
    ) -> bool {
        let was_expanded = self.tree.is_expanded(&tree::key(old_path));
        let mut roots_changed = false;
        for root in &mut self.roots {
            if let Ok(relative) = root.strip_prefix(old_path) {
                *root = new_path.join(relative);
                roots_changed = true;
            }
        }
        for recent in &mut self.recents {
            if let Ok(relative) = recent.strip_prefix(old_path) {
                *recent = new_path.join(relative);
            }
        }
        self.tree.remove_under(old_path);
        if let Some(parent) = new_path.parent()
            && self.roots.iter().any(|root| parent.starts_with(root))
        {
            self.tree.load(port, parent);
        }
        if was_expanded || self.is_root(new_path) {
            self.tree.load_expanded(port, new_path);
        }
        self.rename = None;
        roots_changed
    }

    /// Follow a deleted `path`: drop roots, recents and listings below it,
    /// reload its parent and close any edit row. Returns whether roots or
    /// recents changed.
    pub fn apply_delete(&mut self, port: &dyn DirectoryPort, path: &Path) -> bool {
        let original_roots = self.roots.len();
        let original_recents = self.recents.len();
        self.roots.retain(|root| !root.starts_with(path));
        self.recents.retain(|recent| !recent.starts_with(path));
        self.tree.remove_under(path);
        if let Some(parent) = path.parent()
            && self.roots.iter().any(|root| parent.starts_with(root))
        {
            self.tree.load(port, parent);
        }
        self.create = None;
        self.rename = None;
        self.roots.len() != original_roots || self.recents.len() != original_recents
    }

    // --- directory listings --------------------------------------------

    pub fn listing(&self, key: &str) -> Option<&Arc<Vec<DirEntry>>> {
        self.tree.listing(key)
    }

    pub fn listings(&self) -> impl Iterator<Item = (&String, &Arc<Vec<DirEntry>>)> {
        self.tree.listings()
    }

    pub fn is_expanded(&self, key: &str) -> bool {
        self.tree.is_expanded(key)
    }

    /// Visible file paths in row order.
    pub fn visible_files(&self) -> Vec<PathBuf> {
        self.tree.visible_files(&self.roots)
    }

    /// Show or hide a loaded directory's children without reading it.
    pub fn set_expanded(&mut self, key: &str, expanded: bool) {
        if expanded {
            self.tree.expand(key.to_owned());
        } else {
            self.tree.collapse(key);
        }
    }

    /// Expand or collapse a directory row. Collapsing drops the cached
    /// listings below it, so only visible branches stay in memory and under
    /// refresh; expanding reads them again, restoring nested expansion.
    pub fn toggle_directory(&mut self, port: &dyn DirectoryPort, key: &str) {
        if self.tree.is_expanded(key) {
            self.tree.collapse(key);
            self.tree.unload_under(Path::new(key));
        } else {
            self.tree.expand(key.to_owned());
            self.tree.load_with_expanded_below(port, Path::new(key));
        }
    }

    /// Replace one listing, returning the previous one for retirement.
    pub fn insert_listing(
        &mut self,
        key: String,
        entries: Arc<Vec<DirEntry>>,
    ) -> Option<Arc<Vec<DirEntry>>> {
        self.tree.insert_listing(key, entries)
    }

    /// Forget a directory which no longer exists, collapsing it.
    pub fn unload(&mut self, key: &str) -> Option<Arc<Vec<DirEntry>>> {
        self.tree.unload(key)
    }

    /// Load and expand every directory from the containing root down to
    /// `path`'s parent. `row` is `path`'s visible row afterwards.
    pub fn reveal(&mut self, port: &dyn DirectoryPort, path: &Path) -> Reveal {
        let Some(root) = self.root_for_path(path).cloned() else {
            return Reveal::OutsideWorkspace;
        };
        let mut directories = Vec::new();
        let mut current = path.parent();
        while let Some(directory) = current {
            if !directory.starts_with(&root) {
                break;
            }
            directories.push(directory.to_path_buf());
            if directory == root {
                break;
            }
            current = directory.parent();
        }
        for directory in directories.iter().rev() {
            // Directories already shown keep their listing, so revealing on every
            // tab switch does not reread the disk.
            let key = tree::key(directory);
            if self.tree.is_expanded(&key) && self.tree.listing(&key).is_some() {
                continue;
            }
            self.tree.load_expanded(port, directory);
        }
        Reveal::Revealed {
            row: self.tree.visible_row_index(&self.roots, path),
        }
    }

    // --- refresh ----------------------------------------------------------

    pub fn pending_refresh(&self) -> bool {
        self.pending_refresh
    }

    /// Remember that a refresh must run once the edit row closes.
    pub fn defer_refresh(&mut self) {
        self.pending_refresh = true;
    }

    pub fn clear_pending_refresh(&mut self) {
        self.pending_refresh = false;
    }

    /// Refresh every loaded listing, or defer while an edit row is open.
    pub fn refresh_all(&mut self, port: &dyn DirectoryPort) -> bool {
        if self.editing() {
            self.pending_refresh = true;
            return false;
        }
        self.tree.refresh_all(port, &self.roots)
    }

    /// Refresh the listings affected by changed paths, or defer while an
    /// edit row is open.
    pub fn refresh_affected(
        &mut self,
        port: &dyn DirectoryPort,
        paths: impl IntoIterator<Item = PathBuf>,
    ) -> bool {
        if self.editing() {
            self.pending_refresh = true;
            return false;
        }
        self.tree.refresh_affected(port, &self.roots, paths)
    }

    /// Run a refresh deferred by an edit row which has since closed.
    pub fn apply_pending_refresh(&mut self, port: &dyn DirectoryPort) -> bool {
        if !self.pending_refresh || self.editing() {
            return false;
        }
        self.pending_refresh = false;
        self.refresh_all(port)
    }

    // --- inline create / rename -------------------------------------------

    pub fn create_request(&self) -> Option<&ExplorerCreateRequest> {
        self.create.as_ref()
    }

    pub fn rename_request(&self) -> Option<&ExplorerRenameRequest> {
        self.rename.as_ref()
    }

    pub fn editing(&self) -> bool {
        self.create.is_some() || self.rename.is_some()
    }

    /// Open a create row below `parent`, expanding it.
    pub fn begin_create(&mut self, parent: PathBuf, kind: ExplorerCreateKind) {
        self.tree.expand(tree::key(&parent));
        self.rename = None;
        self.create = Some(ExplorerCreateRequest { kind, parent });
    }

    pub fn begin_rename(&mut self, path: PathBuf, kind: ExplorerTargetKind) {
        self.create = None;
        self.rename = Some(ExplorerRenameRequest { path, kind });
    }

    pub fn cancel_edit(&mut self) {
        self.create = None;
        self.rename = None;
    }

    /// Show a created item: reload and expand every directory from the
    /// create row's parent down to the new folder (or the new file's
    /// parent), then close the create row.
    pub fn complete_create(
        &mut self,
        port: &dyn DirectoryPort,
        parent: &Path,
        target: &Path,
        kind: ExplorerCreateKind,
    ) {
        let refresh_until = if kind == ExplorerCreateKind::Folder {
            target
        } else {
            target.parent().unwrap_or(parent)
        };
        let mut directory = parent.to_path_buf();
        self.tree.load_expanded(port, &directory);
        if let Ok(relative) = refresh_until.strip_prefix(parent) {
            for component in relative.components() {
                directory.push(component.as_os_str());
                self.tree.load_expanded(port, &directory);
            }
        }
        self.create = None;
    }
}
