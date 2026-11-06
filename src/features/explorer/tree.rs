//! Lazily loaded directory listings and their expansion state.
//!
//! Watcher notifications are only hints. Refreshes always rebuild a cached
//! directory from the port so duplicate, reordered, or missing events
//! converge on the same state. Directories which were never loaded stay lazy.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::DirEntry;
use super::ports::DirectoryPort;

pub type Listings = HashMap<String, Arc<Vec<DirEntry>>>;

/// Cache key of a directory.
pub fn key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Directories first, then by name.
pub fn sort_entries(entries: &mut [DirEntry]) {
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TreeCache {
    listings: Listings,
    expanded: HashSet<String>,
}

impl TreeCache {
    pub fn listing(&self, key: &str) -> Option<&Arc<Vec<DirEntry>>> {
        self.listings.get(key)
    }

    pub fn listings(&self) -> impl Iterator<Item = (&String, &Arc<Vec<DirEntry>>)> {
        self.listings.iter()
    }

    pub fn is_loaded(&self, key: &str) -> bool {
        self.listings.contains_key(key)
    }

    pub fn is_expanded(&self, key: &str) -> bool {
        self.expanded.contains(key)
    }

    pub fn expand(&mut self, key: String) {
        self.expanded.insert(key);
    }

    pub fn collapse(&mut self, key: &str) {
        self.expanded.remove(key);
    }

    /// Replace one listing, returning the previous one for retirement.
    pub fn insert_listing(
        &mut self,
        key: String,
        entries: Arc<Vec<DirEntry>>,
    ) -> Option<Arc<Vec<DirEntry>>> {
        self.listings.insert(key, entries)
    }

    /// Forget a directory which no longer exists, collapsing it.
    pub fn unload(&mut self, key: &str) -> Option<Arc<Vec<DirEntry>>> {
        self.expanded.remove(key);
        self.listings.remove(key)
    }

    /// Read `directory` and show it expanded.
    pub fn load_expanded(&mut self, port: &dyn DirectoryPort, directory: &Path) {
        let key = key(directory);
        self.listings
            .insert(key.clone(), port.read_directory(directory));
        self.expanded.insert(key);
    }

    /// Read `directory` without changing its expansion.
    pub fn load(&mut self, port: &dyn DirectoryPort, directory: &Path) {
        self.listings
            .insert(key(directory), port.read_directory(directory));
    }

    /// Read `directory`, then every directory below it still marked expanded,
    /// so reopening a collapsed branch restores its nested expansion.
    pub fn load_with_expanded_below(&mut self, port: &dyn DirectoryPort, directory: &Path) {
        let entries = port.read_directory(directory);
        let nested = entries
            .iter()
            .filter(|entry| entry.is_dir && self.expanded.contains(&key(&entry.path())))
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        self.listings.insert(key(directory), entries);
        for child in nested {
            self.load_with_expanded_below(port, &child);
        }
    }

    /// Drop the cached listings at or below `path`, keeping expansion state.
    pub fn unload_under(&mut self, path: &Path) {
        self.listings
            .retain(|cached, _| !Path::new(cached).starts_with(path));
    }

    /// Drop listings and expansion at or below `path`.
    pub fn remove_under(&mut self, path: &Path) {
        self.listings
            .retain(|cached, _| !Path::new(cached).starts_with(path));
        self.expanded
            .retain(|expanded| !Path::new(expanded).starts_with(path));
    }

    pub fn clear(&mut self) {
        self.listings.clear();
        self.expanded.clear();
    }

    /// Row index of `target` among the visible rows below `roots`, counting
    /// each root row first.
    pub fn visible_row_index(&self, roots: &[PathBuf], target: &Path) -> Option<usize> {
        let mut index = 0usize;
        for root in roots {
            if root == target {
                return Some(index);
            }
            index += 1;
            let key = key(root);
            if self.expanded.contains(&key)
                && let Some(found) = self.visible_descendant_index(&key, target, &mut index)
            {
                return Some(found);
            }
        }
        None
    }

    /// Visible file paths in row order, for range selection.
    pub fn visible_files(&self, roots: &[PathBuf]) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for root in roots {
            let key = key(root);
            if self.expanded.contains(&key) {
                self.collect_visible_files(&key, &mut out);
            }
        }
        out
    }

    fn collect_visible_files(&self, directory_key: &str, out: &mut Vec<PathBuf>) {
        let Some(entries) = self.listings.get(directory_key) else {
            return;
        };
        for entry in entries.iter() {
            if entry.is_dir {
                let key = key(&entry.path());
                if self.expanded.contains(&key) {
                    self.collect_visible_files(&key, out);
                }
            } else {
                out.push(entry.path());
            }
        }
    }

    fn visible_descendant_index(
        &self,
        directory_key: &str,
        target: &Path,
        index: &mut usize,
    ) -> Option<usize> {
        let entries = self.listings.get(directory_key)?;
        for entry in entries.iter() {
            if entry.path() == target {
                return Some(*index);
            }
            *index += 1;
            let key = key(&entry.path());
            if entry.is_dir
                && self.expanded.contains(&key)
                && let Some(found) = self.visible_descendant_index(&key, target, index)
            {
                return Some(found);
            }
        }
        None
    }

    /// Refresh one directory only when it is already loaded. Returns whether
    /// the cache changed.
    pub fn refresh_directory(&mut self, port: &dyn DirectoryPort, directory: &Path) -> bool {
        let key = key(directory);
        let Some(current) = self.listings.get(&key) else {
            return false;
        };
        let entries = port.read_directory(directory);
        if current == &entries {
            return false;
        }
        self.listings.insert(key, entries);
        true
    }

    /// Refresh selected directories and discard cached descendants that no
    /// longer exist.
    pub fn refresh_directories(
        &mut self,
        port: &dyn DirectoryPort,
        roots: &[PathBuf],
        directories: impl IntoIterator<Item = PathBuf>,
    ) -> bool {
        let mut changed = false;
        for directory in directories.into_iter().collect::<BTreeSet<_>>() {
            changed |= self.refresh_directory(port, &directory);
        }
        changed | self.prune_unavailable(port, roots)
    }

    /// Refresh every loaded directory. Bounded by the cache rather than a
    /// recursive scan of the roots.
    pub fn refresh_all(&mut self, port: &dyn DirectoryPort, roots: &[PathBuf]) -> bool {
        let directories = self.listings.keys().map(PathBuf::from).collect::<Vec<_>>();
        self.refresh_directories(port, roots, directories)
    }

    /// Refresh the parent listings of changed paths, and a changed path itself
    /// when it is loaded.
    pub fn refresh_affected(
        &mut self,
        port: &dyn DirectoryPort,
        roots: &[PathBuf],
        paths: impl IntoIterator<Item = PathBuf>,
    ) -> bool {
        let mut directories = BTreeSet::new();
        for path in paths {
            if self.listings.contains_key(key(&path).as_str()) {
                directories.insert(path.clone());
            }
            if let Some(parent) = path.parent() {
                directories.insert(parent.to_path_buf());
            }
        }
        self.refresh_directories(port, roots, directories)
    }

    fn prune_unavailable(&mut self, port: &dyn DirectoryPort, roots: &[PathBuf]) -> bool {
        let should_keep = |path: &Path| {
            roots.iter().any(|root| root == path)
                || (port.is_dir(path) && roots.iter().any(|root| path.starts_with(root)))
        };

        let before_listings = self.listings.len();
        self.listings.retain(|key, _| should_keep(Path::new(key)));
        let before_expanded = self.expanded.len();
        self.expanded.retain(|key| should_keep(Path::new(key)));
        before_listings != self.listings.len() || before_expanded != self.expanded.len()
    }
}
