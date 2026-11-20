//! Explorer data shared by the model, application commands and views.
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One entry in a loaded directory listing. The full path is not stored per
/// entry: every entry of a listing shares its parent, and a path is built only
/// for the rows that need one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    parent: Arc<Path>,
}

impl DirEntry {
    pub fn new(parent: &Arc<Path>, name: String, is_dir: bool) -> Self {
        Self {
            name,
            is_dir,
            parent: Arc::clone(parent),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.parent.join(&self.name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExplorerCreateKind {
    File,
    Folder,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExplorerCreateRequest {
    pub kind: ExplorerCreateKind,
    pub parent: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExplorerRenameRequest {
    pub path: PathBuf,
    pub kind: ExplorerTargetKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExplorerTargetKind {
    File,
    Directory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExplorerContextTarget {
    pub path: PathBuf,
    pub kind: ExplorerTargetKind,
}

const DIRECTORY: u32 = 1 << 31;

/// The entries of one directory, kept compact: every name lives in one buffer and each entry
/// costs 8 bytes of offsets, instead of a `String` and its own heap block per entry. A
/// large expanded tree holds tens of thousands of entries, so the saving is most of its memory.
#[derive(Clone, Debug)]
pub struct Listing {
    /// The directory's cache key, shared with the cache's own tables.
    parent: Arc<str>,
    names: Box<str>,
    /// `(start, end | DIRECTORY)` of each entry's name in `names`, in display order.
    spans: Box<[(u32, u32)]>,
}

/// One entry of a [`Listing`], borrowed from it.
#[derive(Clone, Copy, Debug)]
pub struct EntryRef<'a> {
    pub name: &'a str,
    pub is_dir: bool,
    parent: &'a str,
}

impl EntryRef<'_> {
    pub fn path(&self) -> PathBuf {
        Path::new(self.parent).join(self.name)
    }
}

impl Listing {
    /// Directories first, then by name, whatever order the entries arrive in.
    pub fn sorted<N: AsRef<str>>(
        parent: Arc<str>,
        entries: impl IntoIterator<Item = (N, bool)>,
    ) -> Self {
        let mut names = String::new();
        let mut spans: Vec<(u32, u32)> = Vec::new();
        for (name, is_dir) in entries {
            let start = names.len() as u32;
            names.push_str(name.as_ref());
            spans.push((start, names.len() as u32 | if is_dir { DIRECTORY } else { 0 }));
        }
        let text = names.as_str();
        spans.sort_by(|a, b| {
            (b.1 & DIRECTORY)
                .cmp(&(a.1 & DIRECTORY))
                .then_with(|| text[a.0 as usize..(a.1 & !DIRECTORY) as usize]
                    .cmp(&text[b.0 as usize..(b.1 & !DIRECTORY) as usize]))
        });
        Self {
            parent,
            names: names.into_boxed_str(),
            spans: spans.into_boxed_slice(),
        }
    }

    /// Keeps the given order.
    pub fn in_order(parent: Arc<Path>, entries: &[DirEntry]) -> Self {
        let parent: Arc<str> = Arc::from(parent.to_string_lossy().as_ref());
        let mut names = String::new();
        let mut spans = Vec::with_capacity(entries.len());
        for entry in entries {
            let start = names.len() as u32;
            names.push_str(&entry.name);
            spans.push((start, names.len() as u32 | if entry.is_dir { DIRECTORY } else { 0 }));
        }
        Self {
            parent,
            names: names.into_boxed_str(),
            spans: spans.into_boxed_slice(),
        }
    }

    /// Points the listing at the cache's shared copy of its directory key.
    pub fn set_parent(&mut self, parent: Arc<str>) {
        self.parent = parent;
    }

    pub fn len(&self) -> usize {
        self.spans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<EntryRef<'_>> {
        self.spans.get(index).map(|&(start, end)| EntryRef {
            name: &self.names[start as usize..(end & !DIRECTORY) as usize],
            is_dir: end & DIRECTORY != 0,
            parent: &*self.parent,
        })
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = EntryRef<'_>> + '_ {
        (0..self.spans.len()).map(|index| self.get(index).expect("index within the listing"))
    }

    /// Bytes this listing holds on the heap.
    pub fn heap_bytes(&self) -> usize {
        self.names.len() + self.spans.len() * std::mem::size_of::<(u32, u32)>()
    }
}

impl PartialEq for Listing {
    fn eq(&self, other: &Self) -> bool {
        self.parent == other.parent
            && self.len() == other.len()
            && self
                .iter()
                .zip(other.iter())
                .all(|(a, b)| a.name == b.name && a.is_dir == b.is_dir)
    }
}

impl Eq for Listing {}

impl Default for Listing {
    fn default() -> Self {
        Self {
            parent: Arc::from(""),
            names: Box::default(),
            spans: Box::default(),
        }
    }
}

/// Collects entries that share one parent, keeping their order.
impl FromIterator<DirEntry> for Listing {
    fn from_iter<I: IntoIterator<Item = DirEntry>>(entries: I) -> Self {
        let entries: Vec<DirEntry> = entries.into_iter().collect();
        match entries.first() {
            Some(first) => Listing::in_order(Arc::clone(&first.parent), &entries),
            None => Listing::default(),
        }
    }
}
