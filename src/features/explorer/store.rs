//! Compact storage for the Explorer's directory listings.
//!
//! A tree with thousands of expanded folders holds tens of thousands of entries. Keeping each
//! folder as its own few heap blocks plus a key string cost several times the size of the names
//! themselves, so everything lives in three flat vectors instead:
//!
//! * `dirs`: one small fixed-size node per directory ever seen;
//! * `names`: the bytes of every loaded listing's names, and of every directory's key suffix;
//! * `ends`: for each entry, where its name ends, with a flag for directories.
//!
//! A directory's key is not stored. It is its parent's key plus a suffix, so it is rebuilt by
//! walking up the chain when needed. Lookups go through a 64-bit hash of the key, computed
//! incrementally, so finding a child never builds a string. A collision between two keys of one
//! tree has a probability around 2^-40 even for a million directories.
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use super::Listing;

const NONE: u32 = u32::MAX;
const DIRECTORY: u32 = 1 << 31;

const LOADED: u8 = 1;
const EXPANDED: u8 = 2;
/// The key ends with a path separator, so a child's name follows it directly.
const ENDS_WITH_SEPARATOR: u8 = 4;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Bytes of name text the arenas grow by at a time. Doubling would leave up to half of a large
/// arena unused; a fixed step bounds the waste, and appending is still cheap.
const NAMES_STEP: usize = 64 * 1024;

/// Makes room for dditional more items, growing by at least step and never by doubling.
fn grow<T>(vec: &mut Vec<T>, additional: usize, step: usize) {
    if vec.capacity() - vec.len() < additional {
        vec.reserve_exact(additional.max(step));
    }
}

fn fnv(mut hash: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

pub(super) fn hash_key(key: &str) -> u64 {
    fnv(FNV_OFFSET, key.as_bytes())
}

/// The key of `parent` joined with `name`, hashed without building it. `Path::join` adds a
/// separator unless the parent is empty or already ends with one.
fn child_hash(parent_hash: u64, parent_flags: u8, parent_is_empty: bool, name: &str) -> u64 {
    let mut hash = parent_hash;
    if parent_flags & ENDS_WITH_SEPARATOR == 0 && !parent_is_empty {
        hash = fnv(hash, std::path::MAIN_SEPARATOR_STR.as_bytes());
    }
    fnv(hash, name.as_bytes())
}

/// An open-addressing table of directory indices. Every node already holds its own hash, so a
/// slot is just the 4-byte index of a node, where a HashMap<u64, u32> would keep the hash and
/// the index in each of at least a third more slots.
#[derive(Clone, Default)]
struct Index {
    slots: Vec<u32>,
    len: usize,
}

impl Index {
    fn find(&self, dirs: &[Dir], hash: u64) -> Option<u32> {
        if self.slots.is_empty() {
            return None;
        }
        let mask = self.slots.len() - 1;
        let mut at = hash as usize & mask;
        loop {
            let slot = self.slots[at];
            if slot == NONE {
                return None;
            }
            if dirs[slot as usize].hash == hash {
                return Some(slot);
            }
            at = (at + 1) & mask;
        }
    }

    /// Adds node index, whose hash is already stored in dirs.
    fn insert(&mut self, dirs: &[Dir], index: u32) {
        // Keep it at most 70% full, so probes stay short.
        if (self.len + 1) * 10 > self.slots.len() * 7 {
            let size = (self.slots.len() * 2).max(64);
            self.slots = vec![NONE; size];
            for (other, dir) in dirs.iter().enumerate() {
                if other as u32 != index {
                    self.place(dirs, other as u32, dir.hash);
                }
            }
        }
        self.place(dirs, index, dirs[index as usize].hash);
        self.len += 1;
    }

    fn place(&mut self, _dirs: &[Dir], index: u32, hash: u64) {
        let mask = self.slots.len() - 1;
        let mut at = hash as usize & mask;
        while self.slots[at] != NONE {
            at = (at + 1) & mask;
        }
        self.slots[at] = index;
    }
}

#[derive(Default)]
struct Dir {
    hash: u64,
    /// The directory that holds this one, or `NONE` when `suffix` is the whole key.
    parent: u32,
    suffix_start: u32,
    suffix_len: u32,
    names_start: u32,
    names_len: u32,
    ends_start: u32,
    count: u32,
    flags: u8,
    /// `(revision << 32) | rows`: the rows below this directory, valid for one revision.
    rows: AtomicU64,
    /// The widest directory name and the widest file name, as two 32s, valid while
    /// width_epoch matches the caller's (it identifies the font).
    widths: AtomicU64,
    width_epoch: AtomicU32,
}

impl Clone for Dir {
    fn clone(&self) -> Self {
        Self {
            hash: self.hash,
            parent: self.parent,
            suffix_start: self.suffix_start,
            suffix_len: self.suffix_len,
            names_start: self.names_start,
            names_len: self.names_len,
            ends_start: self.ends_start,
            count: self.count,
            flags: self.flags,
            rows: AtomicU64::new(self.rows.load(Ordering::Relaxed)),
            widths: AtomicU64::new(self.widths.load(Ordering::Relaxed)),
            width_epoch: AtomicU32::new(self.width_epoch.load(Ordering::Relaxed)),
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct Store {
    dirs: Vec<Dir>,
    index: Index,
    names: Vec<u8>,
    ends: Vec<u32>,
    /// Bytes of `names` and entries of `ends` that no directory refers to any more.
    garbage: usize,
}

impl Store {
    pub fn lookup(&self, key: &str) -> Option<u32> {
        self.index.find(&self.dirs, hash_key(key))
    }

    fn dir(&self, index: u32) -> &Dir {
        &self.dirs[index as usize]
    }

    fn dir_mut(&mut self, index: u32) -> &mut Dir {
        &mut self.dirs[index as usize]
    }

    fn text(&self, start: u32, len: u32) -> &str {
        let bytes = &self.names[start as usize..(start + len) as usize];
        // SAFETY: only whole `&str` values are appended to `names`, and every range handed out
        // here was recorded from one of them.
        unsafe { std::str::from_utf8_unchecked(bytes) }
    }

    /// The node for `key`, created when it is new.
    pub fn ensure(&mut self, key: &str) -> u32 {
        let hash = hash_key(key);
        if let Some(index) = self.index.find(&self.dirs, hash) {
            return index;
        }
        let parent_key = Path::new(key)
            .parent()
            .and_then(Path::to_str)
            .filter(|parent| !parent.is_empty() && key.starts_with(parent));
        let (parent, suffix_start) = match parent_key.and_then(|parent| self.lookup(parent).map(|index| (index, parent))) {
            Some((index, parent)) => (index, parent.len()),
            None => (NONE, 0),
        };
        let suffix = &key[suffix_start..];
        grow(&mut self.names, suffix.len(), NAMES_STEP);
        let start = self.names.len() as u32;
        self.names.extend_from_slice(suffix.as_bytes());
        let mut flags = 0;
        if key.is_empty() || key.chars().next_back().is_some_and(std::path::is_separator) {
            flags |= ENDS_WITH_SEPARATOR;
        }
        let index = self.dirs.len() as u32;
        grow(&mut self.dirs, 1, 1024);
        self.dirs.push(Dir {
            hash,
            parent,
            suffix_start: start,
            suffix_len: suffix.len() as u32,
            flags,
            ..Dir::default()
        });
        self.index.insert(&self.dirs, index);
        index
    }

    /// The key of a directory, rebuilt from its chain of suffixes.
    pub fn key_of(&self, index: u32) -> String {
        let mut parts = Vec::new();
        let mut current = index;
        loop {
            let dir = self.dir(current);
            parts.push(self.text(dir.suffix_start, dir.suffix_len));
            if dir.parent == NONE {
                break;
            }
            current = dir.parent;
        }
        let mut key = String::with_capacity(parts.iter().map(|part| part.len()).sum());
        for part in parts.iter().rev() {
            key.push_str(part);
        }
        key
    }

    pub fn is_loaded(&self, index: u32) -> bool {
        self.dir(index).flags & LOADED != 0
    }

    pub fn is_expanded(&self, index: u32) -> bool {
        self.dir(index).flags & EXPANDED != 0
    }

    pub fn set_expanded(&mut self, index: u32, expanded: bool) {
        let dir = self.dir_mut(index);
        if expanded {
            dir.flags |= EXPANDED;
        } else {
            dir.flags &= !EXPANDED;
        }
    }

    pub fn set_listing(&mut self, index: u32, listing: &Listing) {
        self.release_listing(index);
        grow(
            &mut self.names,
            listing.iter().map(|entry| entry.name.len()).sum(),
            NAMES_STEP,
        );
        grow(&mut self.ends, listing.len(), NAMES_STEP / 4);
        let names_start = self.names.len() as u32;
        let ends_start = self.ends.len() as u32;
        for entry in listing.iter() {
            self.names.extend_from_slice(entry.name.as_bytes());
            self.ends
                .push((self.names.len() as u32 - names_start) | if entry.is_dir { DIRECTORY } else { 0 });
        }
        let names_len = self.names.len() as u32 - names_start;
        let count = listing.len() as u32;
        let dir = self.dir_mut(index);
        dir.names_start = names_start;
        dir.names_len = names_len;
        dir.ends_start = ends_start;
        dir.count = count;
        dir.flags |= LOADED;
        dir.width_epoch.store(0, Ordering::Relaxed);
        self.compact_if_wasteful();
    }

    pub fn clear_listing(&mut self, index: u32) {
        self.release_listing(index);
        let dir = self.dir_mut(index);
        dir.flags &= !LOADED;
        dir.width_epoch.store(0, Ordering::Relaxed);
    }

    /// The remembered widest directory and file names, if they were stored for this epoch.
    pub fn name_widths(&self, index: u32, epoch: u32) -> Option<(f32, f32)> {
        let dir = self.dir(index);
        (epoch != 0 && dir.width_epoch.load(Ordering::Relaxed) == epoch).then(|| {
            let widths = dir.widths.load(Ordering::Relaxed);
            (f32::from_bits((widths >> 32) as u32), f32::from_bits(widths as u32))
        })
    }

    pub fn remember_name_widths(&self, index: u32, epoch: u32, widths: (f32, f32)) {
        let dir = self.dir(index);
        dir.widths.store(
            (u64::from(widths.0.to_bits()) << 32) | u64::from(widths.1.to_bits()),
            Ordering::Relaxed,
        );
        dir.width_epoch.store(epoch, Ordering::Relaxed);
    }

    fn release_listing(&mut self, index: u32) {
        let dir = self.dir(index);
        if dir.flags & LOADED != 0 {
            self.garbage += dir.names_len as usize + dir.count as usize * 4;
        }
    }

    /// Rebuilds the arenas without the space that replaced or cleared listings left behind.
    fn compact_if_wasteful(&mut self) {
        let used = self.names.len() + self.ends.len() * 4;
        if self.garbage < 64 * 1024 || self.garbage * 2 < used {
            return;
        }
        let mut names = Vec::with_capacity(used - self.garbage);
        let mut ends = Vec::with_capacity(self.ends.len());
        for dir in &mut self.dirs {
            let suffix = &self.names[dir.suffix_start as usize..(dir.suffix_start + dir.suffix_len) as usize];
            dir.suffix_start = names.len() as u32;
            names.extend_from_slice(suffix);
            if dir.flags & LOADED != 0 {
                let listing = &self.names[dir.names_start as usize..(dir.names_start + dir.names_len) as usize];
                dir.names_start = names.len() as u32;
                names.extend_from_slice(listing);
                let range = dir.ends_start as usize..(dir.ends_start + dir.count) as usize;
                dir.ends_start = ends.len() as u32;
                ends.extend_from_slice(&self.ends[range]);
            }
        }
        self.names = names;
        self.ends = ends;
        self.garbage = 0;
    }

    pub fn len_loaded(&self) -> usize {
        self.dirs.iter().filter(|dir| dir.flags & LOADED != 0).count()
    }

    /// Every directory that has a listing or an expansion mark.
    pub fn marked(&self) -> impl Iterator<Item = u32> + '_ {
        self.dirs
            .iter()
            .enumerate()
            .filter(|(_, dir)| dir.flags & (LOADED | EXPANDED) != 0)
            .map(|(index, _)| index as u32)
    }

    pub fn loaded(&self) -> impl Iterator<Item = u32> + '_ {
        self.dirs
            .iter()
            .enumerate()
            .filter(|(_, dir)| dir.flags & LOADED != 0)
            .map(|(index, _)| index as u32)
    }

    pub fn entry_count(&self, index: u32) -> usize {
        self.dir(index).count as usize
    }

    /// Entry `position` of a directory's listing: its name and whether it is a directory.
    pub fn entry(&self, index: u32, position: usize) -> Option<(&str, bool)> {
        let dir = self.dir(index);
        if dir.flags & LOADED == 0 || position >= dir.count as usize {
            return None;
        }
        let end = self.ends[dir.ends_start as usize + position];
        let start = if position == 0 {
            0
        } else {
            self.ends[dir.ends_start as usize + position - 1] & !DIRECTORY
        };
        let name = self.text(dir.names_start + start, (end & !DIRECTORY) - start);
        Some((name, end & DIRECTORY != 0))
    }

    /// The node of `name` inside directory `parent`, if there is one.
    pub fn child(&self, parent: u32, name: &str) -> Option<u32> {
        let dir = self.dir(parent);
        let hash = child_hash(dir.hash, dir.flags, dir.suffix_len == 0 && dir.parent == NONE, name);
        self.index.find(&self.dirs, hash)
    }

    /// Rows below an expanded directory: its entries and, for each expanded directory among them,
    /// theirs. Remembered per revision, because every scroll step asks again.
    pub fn rows_below(&self, index: u32, revision: u64) -> usize {
        let dir = self.dir(index);
        if dir.flags & EXPANDED == 0 || dir.flags & LOADED == 0 {
            return 0;
        }
        let stamp = revision as u32;
        let remembered = dir.rows.load(Ordering::Relaxed);
        if remembered != 0 && (remembered >> 32) as u32 == stamp {
            return remembered as u32 as usize;
        }
        let mut rows = 0usize;
        for position in 0..dir.count as usize {
            rows += 1;
            let Some((name, is_dir)) = self.entry(index, position) else {
                continue;
            };
            if is_dir && let Some(child) = self.child(index, name) {
                rows += self.rows_below(child, revision);
            }
        }
        dir.rows
            .store((u64::from(stamp) << 32) | rows as u32 as u64, Ordering::Relaxed);
        rows
    }

    pub fn heap_bytes(&self) -> usize {
        self.dirs.capacity() * std::mem::size_of::<Dir>()
            + self.names.capacity()
            + self.ends.capacity() * 4
            + self.index.slots.capacity() * 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn listing(parent: &str, entries: &[(&str, bool)]) -> Listing {
        Listing::ordered(Arc::from(parent), entries.iter().map(|(name, dir)| (*name, *dir)))
    }

    fn join(parent: &str, name: &str) -> String {
        Path::new(parent).join(name).to_string_lossy().into_owned()
    }

    #[test]
    fn keys_are_rebuilt_from_the_chain_of_suffixes() {
        let root = join("C:\\", "proj");
        let sub = join(&root, "src");
        let deep = join(&sub, "ui");
        let mut store = Store::default();
        let root_index = store.ensure(&root);
        let sub_index = store.ensure(&sub);
        let deep_index = store.ensure(&deep);
        assert_eq!(store.key_of(root_index), root);
        assert_eq!(store.key_of(sub_index), sub);
        assert_eq!(store.key_of(deep_index), deep);
        // Asking again finds the same node instead of making another.
        assert_eq!(store.ensure(&sub), sub_index);
        assert_eq!(store.lookup(&deep), Some(deep_index));
        assert_eq!(store.lookup("elsewhere"), None);
    }

    #[test]
    fn a_child_is_found_from_its_parent_without_building_its_key() {
        let root = join("C:\\", "proj");
        let sub = join(&root, "src");
        let mut store = Store::default();
        let root_index = store.ensure(&root);
        let sub_index = store.ensure(&sub);
        assert_eq!(store.child(root_index, "src"), Some(sub_index));
        assert_eq!(store.child(root_index, "missing"), None);
        // A drive root already ends with a separator, so no second one is inserted.
        let drive = store.ensure("C:\\");
        let proj = store.ensure(&root);
        assert_eq!(store.child(drive, "proj"), Some(proj));
    }

    #[test]
    fn entries_come_back_in_order_with_their_kind() {
        let mut store = Store::default();
        let index = store.ensure("root");
        store.set_listing(index, &listing("root", &[("a", true), ("bb", false), ("ccc", true)]));
        assert_eq!(store.entry_count(index), 3);
        assert_eq!(store.entry(index, 0), Some(("a", true)));
        assert_eq!(store.entry(index, 1), Some(("bb", false)));
        assert_eq!(store.entry(index, 2), Some(("ccc", true)));
        assert_eq!(store.entry(index, 3), None);
        store.clear_listing(index);
        assert!(!store.is_loaded(index));
        assert_eq!(store.entry(index, 0), None);
    }

    #[test]
    fn replacing_listings_over_and_over_keeps_the_arenas_bounded() {
        let mut store = Store::default();
        let index = store.ensure("root");
        let names: Vec<String> = (0..400).map(|n| format!("file-number-{n:04}.rs")).collect();
        let entries: Vec<(&str, bool)> = names.iter().map(|name| (name.as_str(), false)).collect();
        for _ in 0..200 {
            store.set_listing(index, &listing("root", &entries));
        }
        // One listing is about 10 KiB; without compaction 200 copies would be 2 MiB.
        assert!(store.names.len() < 400 * 1024, "names grew to {}", store.names.len());
        assert_eq!(store.entry(index, 399), Some(("file-number-0399.rs", false)));
        assert_eq!(store.key_of(index), "root");
    }

    #[test]
    fn compaction_keeps_every_other_listing_and_key_intact() {
        let mut store = Store::default();
        let a = store.ensure("a");
        let b = store.ensure("b");
        store.set_listing(b, &listing("b", &[("keep", false)]));
        let names: Vec<String> = (0..300).map(|n| format!("entry-{n:05}")).collect();
        let entries: Vec<(&str, bool)> = names.iter().map(|name| (name.as_str(), true)).collect();
        for _ in 0..100 {
            store.set_listing(a, &listing("a", &entries));
        }
        assert_eq!(store.entry(b, 0), Some(("keep", false)));
        assert_eq!(store.key_of(b), "b");
        assert_eq!(store.entry(a, 299), Some(("entry-00299", true)));
    }

    #[test]
    fn rows_below_counts_expanded_subtrees_and_is_remembered_per_revision() {
        let root = join("C:\\", "proj");
        let a = join(&root, "a");
        let d = join(&a, "d");
        let hidden = join(&root, "hidden");
        let mut store = Store::default();
        let root_index = store.ensure(&root);
        let a_index = store.ensure(&a);
        let d_index = store.ensure(&d);
        let hidden_index = store.ensure(&hidden);
        store.set_listing(root_index, &listing(&root, &[("a", true), ("b", false), ("hidden", true)]));
        store.set_listing(a_index, &listing(&a, &[("c", false), ("d", true)]));
        store.set_listing(d_index, &listing(&d, &[("e", false)]));
        store.set_listing(hidden_index, &listing(&hidden, &[("x", false), ("y", false)]));
        for index in [root_index, a_index, d_index] {
            store.set_expanded(index, true);
        }
        // a, c, d, e, b and the collapsed `hidden` folder itself.
        assert_eq!(store.rows_below(root_index, 1), 6);
        assert_eq!(store.rows_below(a_index, 1), 3);
        assert_eq!(store.rows_below(hidden_index, 1), 0);
        // Expanding another folder changes the count on the next revision.
        store.set_expanded(hidden_index, true);
        assert_eq!(store.rows_below(root_index, 2), 8);
    }

    #[test]
    fn marked_lists_directories_with_a_listing_or_an_expansion_only() {
        let mut store = Store::default();
        let loaded = store.ensure("loaded");
        let expanded = store.ensure("expanded");
        let _plain = store.ensure("plain");
        store.set_listing(loaded, &listing("loaded", &[]));
        store.set_expanded(expanded, true);
        let mut marked: Vec<u32> = store.marked().collect();
        marked.sort_unstable();
        assert_eq!(marked, vec![loaded, expanded]);
        assert_eq!(store.loaded().collect::<Vec<_>>(), vec![loaded]);
    }
}
