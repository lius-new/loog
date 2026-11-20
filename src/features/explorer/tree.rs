//! Lazily loaded directory listings and their expansion state.
//!
//! Watcher notifications are only hints. Refreshes always rebuild a cached
//! directory from the port so duplicate, reordered, or missing events
//! converge on the same state. Directories which were never loaded stay lazy.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::Listing;
use super::ports::DirectoryPort;
use super::store::Store;
use super::types::EntryRef;

/// Cache key of a directory.
pub fn key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A loaded directory's entries, borrowed from the cache.
#[derive(Clone)]
pub struct ListingRef<'a> {
    store: &'a Store,
    index: u32,
    /// The directory's key, which the entries' paths are built on.
    parent: Cow<'a, str>,
}

impl ListingRef<'_> {
    pub fn len(&self) -> usize {
        self.store.entry_count(self.index)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, position: usize) -> Option<EntryRef<'_>> {
        self.store
            .entry(self.index, position)
            .map(|(name, is_dir)| EntryRef::new(name, is_dir, &self.parent))
    }

    pub fn iter(&self) -> impl Iterator<Item = EntryRef<'_>> + '_ {
        (0..self.len()).filter_map(|position| self.get(position))
    }

    /// Whether `other` lists the same entries in the same order.
    pub fn same_as(&self, other: &Listing) -> bool {
        self.len() == other.len()
            && self
                .iter()
                .zip(other.iter())
                .all(|(a, b)| a.name == b.name && a.is_dir == b.is_dir)
    }

    /// Whether both list the same entries in the same order.
    pub fn same_entries(&self, other: &ListingRef<'_>) -> bool {
        self.len() == other.len()
            && self
                .iter()
                .zip(other.iter())
                .all(|(a, b)| a.name == b.name && a.is_dir == b.is_dir)
    }

    /// An independent copy, for callers that keep a listing while the cache changes.
    pub fn to_listing(&self) -> Arc<Listing> {
        Arc::new(Listing::ordered(
            Arc::from(&*self.parent),
            self.iter()
                .map(|entry| (entry.name.to_owned(), entry.is_dir)),
        ))
    }
}

#[derive(Clone, Default)]
pub struct TreeCache {
    /// Shared and copy-on-write: reading the panel state clones this struct, which must not copy
    /// every cached directory.
    store: Arc<Store>,
    /// Bumped on every change, so derived values can be cached per revision.
    revision: u64,
}

impl std::fmt::Debug for TreeCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TreeCache")
            .field("loaded", &self.store.len_loaded())
            .field("revision", &self.revision)
            .finish()
    }
}

impl TreeCache {
    pub fn listing<'a>(&'a self, key: &'a str) -> Option<ListingRef<'a>> {
        let index = self
            .store
            .lookup(key)
            .filter(|&index| self.store.is_loaded(index))?;
        Some(ListingRef {
            store: &self.store,
            index,
            parent: Cow::Borrowed(key),
        })
    }

    /// Every loaded directory with its key.
    pub fn listings(&self) -> impl Iterator<Item = (String, ListingRef<'_>)> {
        self.store.loaded().map(|index| {
            let key = self.store.key_of(index);
            let listing = ListingRef {
                store: &self.store,
                index,
                parent: Cow::Owned(key.clone()),
            };
            (key, listing)
        })
    }

    pub fn is_loaded(&self, key: &str) -> bool {
        self.store
            .lookup(key)
            .is_some_and(|index| self.store.is_loaded(index))
    }

    pub fn is_expanded(&self, key: &str) -> bool {
        self.store
            .lookup(key)
            .is_some_and(|index| self.store.is_expanded(index))
    }

    /// Rows below an expanded directory: its entries and, for each expanded directory among
    /// them, theirs. Remembered per revision.
    pub fn rows_below(&self, key: &str) -> usize {
        self.store
            .lookup(key)
            .map_or(0, |index| self.store.rows_below(index, self.revision))
    }

    /// Rows below one directory inside another, given the parent's key and the child's name.
    pub fn child_rows_below(&self, parent: &str, name: &str) -> usize {
        self.store
            .lookup(parent)
            .and_then(|index| self.store.child(index, name))
            .map_or(0, |index| self.store.rows_below(index, self.revision))
    }

    /// The widest directory name and widest file name in a loaded directory, as measured by
    /// measure the first time and remembered until the listing changes or epoch (which
    /// identifies the font) does.
    pub fn name_widths(
        &self,
        key: &str,
        epoch: u32,
        measure: impl FnOnce(&ListingRef<'_>) -> (f32, f32),
    ) -> Option<(f32, f32)> {
        let index = self
            .store
            .lookup(key)
            .filter(|&index| self.store.is_loaded(index))?;
        if let Some(widths) = self.store.name_widths(index, epoch) {
            return Some(widths);
        }
        let listing = ListingRef {
            store: &self.store,
            index,
            parent: Cow::Borrowed(key),
        };
        let widths = measure(&listing);
        self.store.remember_name_widths(index, epoch, widths);
        Some(widths)
    }

    /// Keys of the expanded directories directly inside a loaded directory.
    pub fn expanded_children(&self, key: &str) -> Vec<String> {
        let Some(listing) = self.listing(key) else {
            return Vec::new();
        };
        listing
            .iter()
            .filter(|entry| entry.is_dir)
            .filter(|entry| {
                self.store
                    .lookup(key)
                    .and_then(|parent| self.store.child(parent, entry.name))
                    .is_some_and(|child| self.store.is_expanded(child))
            })
            .map(|entry| entry.path().to_string_lossy().into_owned())
            .collect()
    }

    pub fn expand(&mut self, key: String) {
        let store = self.store_mut();
        let index = store.ensure(&key);
        store.set_expanded(index, true);
    }

    pub fn collapse(&mut self, key: &str) {
        if let Some(index) = self
            .store
            .lookup(key)
            .filter(|&index| self.store.is_expanded(index))
        {
            self.store_mut().set_expanded(index, false);
        }
    }

    /// Replace one listing.
    pub fn insert_listing(&mut self, key: String, entries: Arc<Listing>) {
        let store = self.store_mut();
        let index = store.ensure(&key);
        store.set_listing(index, &entries);
    }

    /// Forget a directory which no longer exists, collapsing it. Returns whether it had a
    /// listing.
    pub fn unload(&mut self, key: &str) -> bool {
        let Some(index) = self.store.lookup(key) else {
            return false;
        };
        let existed = self.store.is_loaded(index);
        let store = self.store_mut();
        store.clear_listing(index);
        store.set_expanded(index, false);
        existed
    }

    /// Read `directory` and show it expanded.
    pub fn load_expanded(&mut self, port: &dyn DirectoryPort, directory: &Path) {
        let entries = port.read_directory(directory);
        let store = self.store_mut();
        let index = store.ensure(&key(directory));
        store.set_listing(index, &entries);
        store.set_expanded(index, true);
    }

    /// Read `directory` without changing its expansion.
    pub fn load(&mut self, port: &dyn DirectoryPort, directory: &Path) {
        let entries = port.read_directory(directory);
        let store = self.store_mut();
        let index = store.ensure(&key(directory));
        store.set_listing(index, &entries);
    }

    /// Read `directory`, then every directory below it still marked expanded,
    /// so reopening a collapsed branch restores its nested expansion.
    pub fn load_with_expanded_below(&mut self, port: &dyn DirectoryPort, directory: &Path) {
        let entries = port.read_directory(directory);
        let nested = entries
            .iter()
            .filter(|entry| entry.is_dir && self.is_expanded(&key(&entry.path())))
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        let store = self.store_mut();
        let index = store.ensure(&key(directory));
        store.set_listing(index, &entries);
        for child in nested {
            self.load_with_expanded_below(port, &child);
        }
    }

    /// Drop the cached listings at or below `path`, keeping expansion state.
    pub fn unload_under(&mut self, path: &Path) {
        let stale: Vec<u32> = self
            .store
            .loaded()
            .filter(|&index| Path::new(&self.store.key_of(index)).starts_with(path))
            .collect();
        if stale.is_empty() {
            return;
        }
        let store = self.store_mut();
        for index in stale {
            store.clear_listing(index);
        }
    }

    /// Drop listings and expansion at or below `path`.
    pub fn remove_under(&mut self, path: &Path) {
        let stale: Vec<u32> = self
            .store
            .marked()
            .filter(|&index| Path::new(&self.store.key_of(index)).starts_with(path))
            .collect();
        if stale.is_empty() {
            return;
        }
        let store = self.store_mut();
        for index in stale {
            store.clear_listing(index);
            store.set_expanded(index, false);
        }
    }

    pub fn clear(&mut self) {
        self.revision = next_revision();
        self.store = Arc::new(Store::default());
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
            if self.is_expanded(&key)
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
            if self.is_expanded(&key) {
                self.collect_visible_files(&key, &mut out);
            }
        }
        out
    }

    fn collect_visible_files(&self, directory_key: &str, out: &mut Vec<PathBuf>) {
        let Some(entries) = self.listing(directory_key) else {
            return;
        };
        for entry in entries.iter() {
            if entry.is_dir {
                let key = key(&entry.path());
                if self.is_expanded(&key) {
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
        let entries = self.listing(directory_key)?;
        for entry in entries.iter() {
            if entry.path() == target {
                return Some(*index);
            }
            *index += 1;
            let key = key(&entry.path());
            if entry.is_dir
                && self.is_expanded(&key)
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
        let Some(index) = self
            .store
            .lookup(&key)
            .filter(|&index| self.store.is_loaded(index))
        else {
            return false;
        };
        let entries = port.read_directory(directory);
        let current = ListingRef {
            store: &self.store,
            index,
            parent: Cow::Borrowed(&key),
        };
        if current.same_as(&entries) {
            return false;
        }
        self.store_mut().set_listing(index, &entries);
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
        let directories = self
            .store
            .loaded()
            .map(|index| PathBuf::from(self.store.key_of(index)))
            .collect::<Vec<_>>();
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
            if self.is_loaded(&key(&path)) {
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
        let dropped: Vec<u32> = self
            .store
            .marked()
            .filter(|&index| !should_keep(Path::new(&self.store.key_of(index))))
            .collect();
        if dropped.is_empty() {
            return false;
        }
        let store = self.store_mut();
        for index in dropped {
            store.clear_listing(index);
            store.set_expanded(index, false);
        }
        true
    }

    fn store_mut(&mut self) -> &mut Store {
        self.revision = next_revision();
        Arc::make_mut(&mut self.store)
    }

    /// Changes whenever a listing or an expansion changes.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Bytes this cache holds on the heap.
    pub fn heap_bytes(&self) -> usize {
        self.store.heap_bytes()
    }

    /// Everything a comparison needs, by directory key.
    fn content(&self) -> BTreeMap<String, (bool, Option<Vec<(String, bool)>>)> {
        self.store
            .marked()
            .map(|index| {
                let key = self.store.key_of(index);
                let entries = self.store.is_loaded(index).then(|| {
                    (0..self.store.entry_count(index))
                        .filter_map(|position| self.store.entry(index, position))
                        .map(|(name, is_dir)| (name.to_owned(), is_dir))
                        .collect()
                });
                (key, (self.store.is_expanded(index), entries))
            })
            .collect()
    }
}

impl PartialEq for TreeCache {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.store, &other.store) || self.content() == other.content()
    }
}

/// Revisions are drawn from one counter, so two caches never share a revision with different
/// contents.
fn next_revision() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}
