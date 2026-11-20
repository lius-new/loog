//! Lazily loaded directory listings and their expansion state.
//!
//! Watcher notifications are only hints. Refreshes always rebuild a cached
//! directory from the port so duplicate, reordered, or missing events
//! converge on the same state. Directories which were never loaded stay lazy.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::Listing;
use super::ports::DirectoryPort;

/// Keys are shared strings: copying the map (copy-on-write while a state snapshot is alive) then
/// costs a pointer per key instead of a heap block per key.
pub type Listings = HashMap<Arc<str>, Arc<Listing>>;

/// Cache key of a directory.
pub fn key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}


#[derive(Clone, Debug, Default)]
pub struct TreeCache {
    /// Shared and copy-on-write: reading the panel state clones this struct, which must not copy
    /// every cached directory key.
    listings: Arc<Listings>,
    expanded: Arc<HashSet<Arc<str>>>,
    /// Bumped on every change, so derived values can be cached per revision.
    revision: u64,
}

impl TreeCache {
    pub fn listing(&self, key: &str) -> Option<&Arc<Listing>> {
        self.listings.get(key)
    }

    pub fn listings(&self) -> impl Iterator<Item = (&str, &Arc<Listing>)> {
        self.listings.iter().map(|(key, listing)| (&**key, listing))
    }

    /// One shared allocation per directory key, whichever table asks first.
    fn shared_key(&self, key: String) -> Arc<str> {
        if let Some((shared, _)) = self.listings.get_key_value(key.as_str()) {
            return Arc::clone(shared);
        }
        if let Some(shared) = self.expanded.get(key.as_str()) {
            return Arc::clone(shared);
        }
        Arc::from(key)
    }

    pub fn is_loaded(&self, key: &str) -> bool {
        self.listings.contains_key(key)
    }

    pub fn is_expanded(&self, key: &str) -> bool {
        self.expanded.contains(key)
    }

    pub fn expand(&mut self, key: String) {
        let key = self.shared_key(key);
        self.expanded_mut().insert(key);
    }

    pub fn collapse(&mut self, key: &str) {
        self.expanded_mut().remove(key);
    }

    /// Replace one listing, returning the previous one for retirement.
    pub fn insert_listing(
        &mut self,
        key: String,
        mut entries: Arc<Listing>,
    ) -> Option<Arc<Listing>> {
        let key = self.shared_key(key);
        if let Some(listing) = Arc::get_mut(&mut entries) {
            listing.set_parent(Arc::clone(&key));
        }
        self.listings_mut().insert(key, entries)
    }

    /// Forget a directory which no longer exists, collapsing it.
    pub fn unload(&mut self, key: &str) -> Option<Arc<Listing>> {
        self.expanded_mut().remove(key);
        self.listings_mut().remove(key)
    }

    /// Read `directory` and show it expanded.
    pub fn load_expanded(&mut self, port: &dyn DirectoryPort, directory: &Path) {
        let key = self.shared_key(key(directory));
        let entries = read_shared(port, directory, &key);
        self.listings_mut().insert(Arc::clone(&key), entries);
        self.expanded_mut().insert(key);
    }

    /// Read `directory` without changing its expansion.
    pub fn load(&mut self, port: &dyn DirectoryPort, directory: &Path) {
        let key = self.shared_key(key(directory));
        let entries = read_shared(port, directory, &key);
        self.listings_mut().insert(key, entries);
    }

    /// Read `directory`, then every directory below it still marked expanded,
    /// so reopening a collapsed branch restores its nested expansion.
    pub fn load_with_expanded_below(&mut self, port: &dyn DirectoryPort, directory: &Path) {
        let shared = self.shared_key(key(directory));
        let entries = read_shared(port, directory, &shared);
        let nested = entries
            .iter()
            .filter(|entry| entry.is_dir && self.expanded.contains(key(&entry.path()).as_str()))
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        self.listings_mut().insert(shared, entries);
        for child in nested {
            self.load_with_expanded_below(port, &child);
        }
    }

    /// Drop the cached listings at or below `path`, keeping expansion state.
    pub fn unload_under(&mut self, path: &Path) {
        self.listings_mut()
            .retain(|cached, _| !Path::new(&**cached).starts_with(path));
    }

    /// Drop listings and expansion at or below `path`.
    pub fn remove_under(&mut self, path: &Path) {
        self.listings_mut()
            .retain(|cached, _| !Path::new(&**cached).starts_with(path));
        self.expanded_mut()
            .retain(|expanded| !Path::new(&**expanded).starts_with(path));
    }

    pub fn clear(&mut self) {
        self.listings_mut().clear();
        self.expanded_mut().clear();
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
            if self.expanded.contains(key.as_str())
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
            if self.expanded.contains(key.as_str()) {
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
                if self.expanded.contains(key.as_str()) {
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
                && self.expanded.contains(key.as_str())
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
        let Some(current) = self.listings.get(key.as_str()) else {
            return false;
        };
        let entries = port.read_directory(directory);
        if current == &entries {
            return false;
        }
        let shared = self.shared_key(key);
        let mut entries = entries;
        if let Some(listing) = Arc::get_mut(&mut entries) {
            listing.set_parent(Arc::clone(&shared));
        }
        self.listings_mut().insert(shared, entries);
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
        let directories = self.listings.keys().map(|key| PathBuf::from(&**key)).collect::<Vec<_>>();
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
        self.listings_mut().retain(|key, _| should_keep(Path::new(&**key)));
        let before_expanded = self.expanded.len();
        self.expanded_mut().retain(|key| should_keep(Path::new(&**key)));
        before_listings != self.listings.len() || before_expanded != self.expanded.len()
    }
}

impl PartialEq for TreeCache {
    fn eq(&self, other: &Self) -> bool {
        self.listings == other.listings && self.expanded == other.expanded
    }
}

impl TreeCache {
    fn listings_mut(&mut self) -> &mut Listings {
        self.revision = next_revision();
        Arc::make_mut(&mut self.listings)
    }

    fn expanded_mut(&mut self) -> &mut HashSet<Arc<str>> {
        self.revision = next_revision();
        Arc::make_mut(&mut self.expanded)
    }

    /// Changes whenever a listing or an expansion changes.
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

/// Revisions are drawn from one counter, so two caches never share a revision with different
/// contents.
fn next_revision() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Reads a directory and points its listing at the shared cache key, so the key text is held
/// once instead of once more inside every listing.
fn read_shared(port: &dyn DirectoryPort, directory: &Path, key: &Arc<str>) -> Arc<Listing> {
    let mut entries = port.read_directory(directory);
    if let Some(listing) = Arc::get_mut(&mut entries) {
        listing.set_parent(Arc::clone(key));
    }
    entries
}
