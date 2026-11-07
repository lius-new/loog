//! Explorer rules against an in-memory directory port; no disk access.
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::edit::{validate_create_path, validate_rename_path};
use super::model::{ExplorerModel, Reveal};
use super::ports::DirectoryPort;
use super::roots::{self, MAX_RECENT_FOLDERS};
use super::tree::{self, TreeCache};
use super::{DirEntry, ExplorerCreateKind, ExplorerTargetKind};

/// Directories mapped to their child names; a trailing `/` marks a folder.
#[derive(Default)]
struct MemoryDirectories {
    directories: BTreeMap<PathBuf, Vec<&'static str>>,
    files: HashSet<PathBuf>,
    aliases: BTreeMap<PathBuf, PathBuf>,
}

impl MemoryDirectories {
    fn dir(mut self, path: &str, children: &[&'static str]) -> Self {
        self.directories
            .insert(PathBuf::from(path), children.to_vec());
        self
    }

    fn file(mut self, path: &str) -> Self {
        self.files.insert(PathBuf::from(path));
        self
    }
}

impl DirectoryPort for MemoryDirectories {
    fn read_directory(&self, directory: &Path) -> Arc<Vec<DirEntry>> {
        let parent: Arc<Path> = Arc::from(directory);
        let mut entries = self
            .directories
            .get(directory)
            .into_iter()
            .flatten()
            .map(|child| {
                let name = child.trim_end_matches('/');
                DirEntry::new(&parent, name.to_owned(), child.ends_with('/'))
            })
            .collect::<Vec<_>>();
        tree::sort_entries(&mut entries);
        Arc::new(entries)
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.directories.contains_key(path)
    }

    fn exists(&self, path: &Path) -> bool {
        self.is_dir(path) || self.files.contains(path)
    }

    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        self.aliases
            .get(path)
            .cloned()
            .or_else(|| self.exists(path).then(|| path.to_path_buf()))
    }
}

fn cache(port: &MemoryDirectories, loaded: &[&str], expanded: &[&str]) -> TreeCache {
    let mut tree = TreeCache::default();
    for path in loaded {
        tree.insert_listing(path.to_string(), port.read_directory(Path::new(path)));
    }
    for path in expanded {
        tree.expand(path.to_string());
    }
    tree
}

fn names(tree: &TreeCache, key: &str) -> Vec<String> {
    tree.listing(key)
        .unwrap()
        .iter()
        .map(|entry| entry.name.clone())
        .collect()
}

#[test]
fn entries_sort_folders_before_files_then_by_name() {
    let port = MemoryDirectories::default().dir("root", &["b.txt", "z-dir/", "a.txt", "c-dir/"]);
    let entries = port.read_directory(Path::new("root"));
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["c-dir", "z-dir", "a.txt", "b.txt"]
    );
}

#[test]
fn cached_directory_refresh_is_idempotent() {
    let mut port = MemoryDirectories::default().dir("root", &[]);
    let mut tree = cache(&port, &["root"], &[]);

    assert!(!tree.refresh_directory(&port, Path::new("root")));
    port = port.dir("root", &["new.txt"]);
    assert!(tree.refresh_directory(&port, Path::new("root")));
    assert!(!tree.refresh_directory(&port, Path::new("root")));
    assert_eq!(names(&tree, "root"), ["new.txt"]);
}

#[test]
fn refresh_never_loads_lazy_directories() {
    let port = MemoryDirectories::default()
        .dir("root", &["child/"])
        .dir("root/child", &["inner.txt"]);
    let mut tree = cache(&port, &["root"], &["root"]);

    assert!(!tree.refresh_directory(&port, Path::new("root/child")));
    assert!(!tree.refresh_affected(
        &port,
        &[PathBuf::from("root")],
        [PathBuf::from("root/child/inner.txt")]
    ));
    assert!(!tree.is_loaded("root/child"));
}

#[test]
fn refresh_prunes_removed_descendant_cache() {
    let mut port = MemoryDirectories::default()
        .dir("root", &["child/"])
        .dir("root/child", &[]);
    let mut tree = cache(&port, &["root", "root/child"], &["root", "root/child"]);

    port = MemoryDirectories::default().dir("root", &[]);
    assert!(tree.refresh_all(&port, &[PathBuf::from("root")]));
    assert!(!tree.is_loaded("root/child"));
    assert!(!tree.is_expanded("root/child"));
    assert!(tree.listing("root").unwrap().is_empty());
}

#[test]
fn refresh_keeps_missing_roots_but_prunes_directories_outside_roots() {
    let port = MemoryDirectories::default().dir("other", &[]);
    let mut tree = cache(&port, &["gone-root", "other"], &["gone-root", "other"]);

    assert!(tree.refresh_all(&port, &[PathBuf::from("gone-root")]));
    assert!(tree.is_loaded("gone-root"));
    assert!(tree.is_expanded("gone-root"));
    assert!(!tree.is_loaded("other"));
}

#[test]
fn refresh_affected_reloads_parents_and_loaded_paths() {
    let mut port = MemoryDirectories::default()
        .dir("root", &["src/"])
        .dir("root/src", &[]);
    let mut tree = cache(&port, &["root", "root/src"], &["root"]);

    port = MemoryDirectories::default()
        .dir("root", &["src/", "added.txt"])
        .dir("root/src", &["main.rs"]);
    assert!(tree.refresh_affected(
        &port,
        &[PathBuf::from("root")],
        [PathBuf::from("root/added.txt"), PathBuf::from("root/src")]
    ));
    assert_eq!(names(&tree, "root"), ["src", "added.txt"]);
    assert_eq!(names(&tree, "root/src"), ["main.rs"]);
}

#[test]
fn unchanged_refresh_keeps_listing_identity() {
    let port = MemoryDirectories::default().dir("root", &["a.txt"]);
    let mut tree = cache(&port, &["root"], &[]);
    let before = tree.listing("root").unwrap().clone();

    assert!(!tree.refresh_all(&port, &[PathBuf::from("root")]));
    assert!(Arc::ptr_eq(&before, tree.listing("root").unwrap()));
}

#[test]
fn create_path_validation_accepts_nested_relative_names() {
    let port = MemoryDirectories::default();
    let parent = PathBuf::from("workspace");

    assert_eq!(
        validate_create_path(&port, &parent, "src/components/button.rs").unwrap(),
        PathBuf::from("src").join("components").join("button.rs")
    );
    assert_eq!(
        validate_create_path(&port, &parent, "assets\\icons\\").unwrap(),
        PathBuf::from("assets").join("icons")
    );
    assert_eq!(
        validate_create_path(&port, &parent, "笔记/日志 🙂.md").unwrap(),
        PathBuf::from("笔记").join("日志 🙂.md")
    );
}

#[test]
fn create_path_validation_rejects_empty_absolute_and_parent_names() {
    let port = MemoryDirectories::default();
    let parent = PathBuf::from("workspace");

    assert!(validate_create_path(&port, &parent, "   ").is_err());
    assert!(validate_create_path(&port, &parent, "/absolute").is_err());
    assert!(validate_create_path(&port, &parent, "../outside").is_err());
    assert!(validate_create_path(&port, &parent, "src/../outside").is_err());
    assert!(validate_create_path(&port, &parent, "src/ padded").is_err());
}

#[test]
fn create_and_rename_reject_existing_targets() {
    let port = MemoryDirectories::default()
        .dir("workspace", &["taken.rs", "old.rs"])
        .file("workspace/taken.rs")
        .file("workspace/old.rs");
    let parent = PathBuf::from("workspace");

    assert!(validate_create_path(&port, &parent, "taken.rs").is_err());
    assert!(validate_rename_path(&port, &parent.join("old.rs"), "taken.rs").is_err());
    assert_eq!(
        validate_rename_path(&port, &parent.join("old.rs"), "old.rs").unwrap(),
        parent.join("old.rs")
    );
}

#[test]
fn rename_validation_accepts_one_folder_name_and_rejects_paths() {
    let port = MemoryDirectories::default();
    let path = PathBuf::from("__loog_missing__/old-name");

    assert_eq!(
        validate_rename_path(&port, &path, "new-name").unwrap(),
        PathBuf::from("__loog_missing__/new-name")
    );
    assert!(validate_rename_path(&port, &path, "nested/name").is_err());
    assert!(validate_rename_path(&port, &path, "..").is_err());
    assert!(validate_rename_path(&port, &path, "   ").is_err());
}

#[cfg(target_os = "windows")]
#[test]
fn create_path_validation_rejects_windows_device_and_invalid_names() {
    let port = MemoryDirectories::default();
    let parent = PathBuf::from("workspace");

    assert!(validate_create_path(&port, &parent, "CON.txt").is_err());
    assert!(validate_create_path(&port, &parent, "bad?.txt").is_err());
    assert!(validate_create_path(&port, &parent, "trailing.").is_err());
}

#[test]
fn recent_folders_are_mru_ordered_and_bounded() {
    let mut recents = Vec::new();

    for index in 0..MAX_RECENT_FOLDERS + 2 {
        roots::remember(&mut recents, PathBuf::from(format!("workspace-{index}")));
    }
    roots::remember(&mut recents, PathBuf::from("workspace-4"));

    assert_eq!(recents.len(), MAX_RECENT_FOLDERS);
    assert_eq!(recents[0], PathBuf::from("workspace-4"));
    assert_eq!(recents[1], PathBuf::from("workspace-9"));
}

#[test]
fn hydrate_drops_missing_folders_and_lists_roots_among_recents() {
    let mut port = MemoryDirectories::default()
        .dir("a", &[])
        .dir("b", &[])
        .dir("canonical", &[]);
    port.aliases
        .insert(PathBuf::from("alias"), PathBuf::from("canonical"));
    let mut workspace_roots = vec![
        PathBuf::from("a"),
        PathBuf::from("gone"),
        PathBuf::from("alias"),
        PathBuf::from("canonical"),
    ];
    let mut recents = vec![PathBuf::from("b"), PathBuf::from("missing")];

    roots::hydrate(&port, &mut workspace_roots, &mut recents);

    assert_eq!(
        workspace_roots,
        [PathBuf::from("a"), PathBuf::from("canonical")]
    );
    assert_eq!(
        recents,
        [
            PathBuf::from("b"),
            PathBuf::from("a"),
            PathBuf::from("canonical")
        ]
    );
}

#[test]
fn restored_roots_do_not_reorder_existing_recent_history() {
    let port = MemoryDirectories::default()
        .dir("first-workspace", &[])
        .dir("second-workspace", &[])
        .dir("older-workspace", &[]);
    let first = PathBuf::from("first-workspace");
    let second = PathBuf::from("second-workspace");
    let older = PathBuf::from("older-workspace");
    let mut workspace_roots = vec![first.clone(), second.clone()];
    let mut recents = vec![second.clone(), older.clone(), first.clone()];

    roots::hydrate(&port, &mut workspace_roots, &mut recents);

    assert_eq!(recents, vec![second, older, first]);
}

#[test]
fn root_for_path_prefers_the_deepest_containing_root() {
    let workspace_roots = [
        PathBuf::from("workspace"),
        PathBuf::from("workspace/nested"),
        PathBuf::from("other"),
    ];

    assert_eq!(
        roots::root_for_path(&workspace_roots, Path::new("workspace/nested/src/lib.rs")),
        Some(&PathBuf::from("workspace/nested"))
    );
    assert_eq!(
        roots::root_for_path(&workspace_roots, Path::new("workspace/src")),
        Some(&PathBuf::from("workspace"))
    );
    assert_eq!(
        roots::root_for_path(&workspace_roots, Path::new("elsewhere")),
        None
    );
}

/// A path built from components, so keys match the platform separator.
fn p(path: &str) -> PathBuf {
    path.split('/').collect()
}

fn k(path: &str) -> String {
    tree::key(&p(path))
}

fn model(port: &MemoryDirectories, roots: &[&str]) -> ExplorerModel {
    let mut model = ExplorerModel::default();
    model.restore(roots.iter().map(|root| p(root)).collect(), Vec::new());
    model.hydrate(port);
    model
}

#[test]
fn refresh_is_deferred_during_inline_edit() {
    let mut port = MemoryDirectories::default().dir("root", &[]);
    let mut model = model(&port, &["root"]);
    model.begin_create(PathBuf::from("root"), ExplorerCreateKind::File);

    assert!(!model.refresh_all(&port));
    assert!(!model.refresh_affected(&port, [PathBuf::from("root/x.txt")]));
    assert!(model.pending_refresh());
    assert!(!model.apply_pending_refresh(&port));

    model.cancel_edit();
    port = port.dir("root", &["later.txt"]);
    assert!(model.apply_pending_refresh(&port));
    assert!(!model.pending_refresh());
    assert_eq!(model.listing("root").unwrap()[0].name, "later.txt");
}

#[test]
fn opening_a_root_replaces_roots_and_listings() {
    let port = MemoryDirectories::default()
        .dir("first", &["sub/"])
        .dir("first/sub", &[])
        .dir("second", &[]);
    let mut model = model(&port, &["first"]);
    model.toggle_directory(&port, "first/sub");

    model.open_root(&port, PathBuf::from("second"));

    assert_eq!(model.roots(), [PathBuf::from("second")]);
    assert!(model.listing("first").is_none());
    assert!(model.listing("first/sub").is_none());
    assert!(model.is_expanded("second"));
    assert_eq!(model.recents()[0], PathBuf::from("second"));
}

#[test]
fn adding_a_duplicate_root_only_refreshes_its_recency() {
    let port = MemoryDirectories::default().dir("a", &[]).dir("b", &[]);
    let mut model = ExplorerModel::default();

    assert!(model.add_root(&port, PathBuf::from("a")));
    assert!(model.add_root(&port, PathBuf::from("b")));
    assert!(!model.add_root(&port, PathBuf::from("a")));

    assert_eq!(model.roots(), [PathBuf::from("a"), PathBuf::from("b")]);
    assert_eq!(model.recents(), [PathBuf::from("a"), PathBuf::from("b")]);
}

#[test]
fn removing_a_root_cancels_edit_rows_below_it_only() {
    let port = MemoryDirectories::default()
        .dir("a", &["src/"])
        .dir("a/src", &[])
        .dir("b", &[]);
    let mut model = model(&port, &["a", "b"]);

    model.begin_create(PathBuf::from("b"), ExplorerCreateKind::Folder);
    assert_eq!(model.remove_root(Path::new("a")), Some(false));
    assert!(model.create_request().is_some());
    assert_eq!(model.remove_root(Path::new("a")), None);

    model.begin_rename(PathBuf::from("b/file.rs"), ExplorerTargetKind::File);
    assert_eq!(model.remove_root(Path::new("b")), Some(true));
    assert!(!model.editing());
    assert!(model.roots().is_empty());
    assert_eq!(model.listings().count(), 0);
}

#[test]
fn rename_retargets_nested_roots_recents_and_expansion() {
    let mut port = MemoryDirectories::default()
        .dir("ws", &["old/"])
        .dir("ws/old", &["inner/"])
        .dir("ws/old/inner", &[]);
    let mut model = model(&port, &["ws", "ws/old/inner"]);
    model.toggle_directory(&port, &k("ws/old"));
    model.remember(p("ws/old/inner"));
    model.begin_rename(p("ws/old"), ExplorerTargetKind::Directory);

    port = MemoryDirectories::default()
        .dir("ws", &["new/"])
        .dir("ws/new", &["inner/"])
        .dir("ws/new/inner", &[]);
    assert!(model.apply_rename(&port, &p("ws/old"), &p("ws/new")));

    assert_eq!(model.roots(), [PathBuf::from("ws"), p("ws/new/inner")]);
    assert_eq!(model.recents()[0], p("ws/new/inner"));
    assert!(
        model
            .listings()
            .all(|(key, _)| !Path::new(key).starts_with(p("ws/old")))
    );
    assert!(model.is_expanded(&k("ws/new")));
    // Existing behavior: a nested root below the renamed folder keeps its
    // place but loses its loaded listing until expanded again.
    assert!(!model.is_expanded(&k("ws/new/inner")));
    assert_eq!(model.listing(&k("ws")).unwrap()[0].name, "new");
    assert!(model.rename_request().is_none());
}

#[test]
fn renaming_a_collapsed_file_does_not_expand_it() {
    let mut port = MemoryDirectories::default().dir("ws", &["a.rs"]);
    let mut model = model(&port, &["ws"]);

    port = MemoryDirectories::default().dir("ws", &["b.rs"]);
    assert!(!model.apply_rename(&port, Path::new("ws/a.rs"), Path::new("ws/b.rs")));

    assert!(!model.is_expanded("ws/b.rs"));
    assert!(model.listing("ws/b.rs").is_none());
    assert_eq!(model.listing("ws").unwrap()[0].name, "b.rs");
}

#[test]
fn delete_drops_folders_below_and_reloads_the_parent() {
    let mut port = MemoryDirectories::default()
        .dir("ws", &["gone/", "kept.rs"])
        .dir("ws/gone", &[])
        .dir("other", &[]);
    let mut model = model(&port, &["ws", "ws/gone"]);
    model.remember(PathBuf::from("other"));
    model.begin_create(PathBuf::from("ws"), ExplorerCreateKind::File);

    port = MemoryDirectories::default()
        .dir("ws", &["kept.rs"])
        .dir("other", &[]);
    assert!(model.apply_delete(&port, Path::new("ws/gone")));

    assert_eq!(model.roots(), [PathBuf::from("ws")]);
    assert!(!model.recents().contains(&PathBuf::from("ws/gone")));
    assert!(model.listing("ws/gone").is_none());
    assert_eq!(names_of(&model, "ws"), ["kept.rs"]);
    assert!(!model.editing());
    assert!(!model.apply_delete(&port, Path::new("ws/kept.rs")));
}

#[test]
fn reveal_loads_ancestors_and_reports_the_visible_row() {
    let port = MemoryDirectories::default()
        .dir("ws", &["a/", "z.rs"])
        .dir("ws/a", &["b/", "x.rs"])
        .dir("ws/a/b", &["target.rs"])
        .dir("other", &[]);
    let mut model = model(&port, &["ws", "other"]);

    assert_eq!(
        model.reveal(&port, &p("ws/a/b/target.rs")),
        Reveal::Revealed { row: Some(3) }
    );
    assert!(model.is_expanded(&k("ws/a")));
    assert!(model.is_expanded(&k("ws/a/b")));
    assert_eq!(
        model.reveal(&port, &p("elsewhere/file.rs")),
        Reveal::OutsideWorkspace
    );
}

#[test]
fn completed_create_expands_every_new_folder() {
    let port = MemoryDirectories::default()
        .dir("ws", &["a/"])
        .dir("ws/a", &["b/"])
        .dir("ws/a/b", &["c.rs"]);
    let mut model = model(&port, &["ws"]);
    model.begin_create(PathBuf::from("ws"), ExplorerCreateKind::File);

    model.complete_create(
        &port,
        Path::new("ws"),
        &p("ws/a/b/c.rs"),
        ExplorerCreateKind::File,
    );

    assert!(model.is_expanded(&k("ws/a")));
    assert!(model.is_expanded(&k("ws/a/b")));
    assert!(!model.is_expanded(&k("ws/a/b/c.rs")));
    assert_eq!(names_of(&model, &k("ws/a/b")), ["c.rs"]);
    assert!(model.create_request().is_none());
}

#[test]
fn collapsing_a_directory_drops_its_listing_and_reopening_rereads_it() {
    let mut port = MemoryDirectories::default()
        .dir("ws", &["src/"])
        .dir("ws/src", &["main.rs"]);
    let mut model = model(&port, &["ws"]);

    model.toggle_directory(&port, "ws/src");
    assert!(model.is_expanded("ws/src"));
    port = port.dir("ws/src", &["changed.rs"]);
    model.toggle_directory(&port, "ws/src");
    assert!(!model.is_expanded("ws/src"));
    assert!(model.listing("ws/src").is_none());
    model.toggle_directory(&port, "ws/src");
    assert_eq!(names_of(&model, "ws/src"), ["changed.rs"]);
}

#[test]
fn collapsing_drops_nested_listings_and_reopening_restores_nested_expansion() {
    let port = MemoryDirectories::default()
        .dir("ws", &["src/"])
        .dir("ws/src", &["inner/", "a.rs"])
        .dir("ws/src/inner", &["deep.rs"]);
    let mut model = model(&port, &["ws"]);
    // Keys are built the way listings build them, with the platform separator.
    let src = Path::new("ws").join("src").to_string_lossy().into_owned();
    let inner = Path::new("ws")
        .join("src")
        .join("inner")
        .to_string_lossy()
        .into_owned();
    model.toggle_directory(&port, &src);
    model.toggle_directory(&port, &inner);

    model.toggle_directory(&port, &src);
    assert!(model.listing(&src).is_none());
    assert!(model.listing(&inner).is_none());

    model.toggle_directory(&port, &src);
    assert!(model.is_expanded(&inner));
    assert_eq!(names_of(&model, &inner), ["deep.rs"]);
}

fn names_of(model: &ExplorerModel, key: &str) -> Vec<String> {
    model
        .listing(key)
        .unwrap()
        .iter()
        .map(|entry| entry.name.clone())
        .collect()
}
