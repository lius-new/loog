//! Application disk reconciliation and watcher delivery coordination.
use crate::app::context::AppContext;
use crate::app::state::AppState;
use crate::app::workspace::ReconcileResult;
use crate::infrastructure::explorer_filesystem::ExplorerFilesystem;
pub(crate) use crate::infrastructure::file_watcher::{
    FileChangeBatch, FileChangeKind, Lease, start_async_scoped,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
/// Apply one watcher batch to the two disk-backed models. Directory changes
/// are resolved by rereading cached listings, while open documents use their
/// clean/dirty reconciliation policy.
pub(crate) fn apply_batch(app: &mut AppState, batch: &FileChangeBatch) -> bool {
    let paths = batch.paths().map(Path::to_path_buf).collect::<Vec<_>>();
    let tree_changed = if batch.requires_full_rescan {
        app.explorer.model.refresh_all(&ExplorerFilesystem)
    } else {
        app.explorer.model.refresh_affected(
            &ExplorerFilesystem,
            batch
                .changes
                .iter()
                .filter(|change| change.kind != FileChangeKind::Modify)
                .map(|change| change.path.clone()),
        )
    };
    let document_changed = app
        .workspace
        .reconcile_changed_paths(&paths, batch.requires_full_rescan)
        .into_iter()
        .any(|result| {
            matches!(
                result,
                ReconcileResult::Reloaded(_)
                    | ReconcileResult::Conflict(_)
                    | ReconcileResult::Missing(_)
            )
        });
    tree_changed || document_changed
}

type PreparedDirectory = (
    String,
    Arc<Vec<crate::features::explorer::DirEntry>>,
    Option<Arc<Vec<crate::features::explorer::DirEntry>>>,
);

pub(crate) struct PreparedFileBatch {
    roots: Vec<PathBuf>,
    directories: Vec<PreparedDirectory>,
    disk_updates: Vec<crate::app::workspace::PreparedDiskUpdate>,
    editing: Vec<(
        crate::domain::identity::FileId,
        u64,
        PathBuf,
        loog_editor_core::text::FileOptions,
    )>,
    defer_tree: bool,
    invalidate_search: bool,
}

pub(crate) fn prepare_batch(app: &AppState, batch: &FileChangeBatch) -> PreparedFileBatch {
    let paths = batch.paths().map(Path::to_path_buf).collect::<Vec<_>>();
    let disk_updates = app
        .workspace
        .prepare_disk_updates(&paths, batch.requires_full_rescan);
    let mut working = app.explorer.model.clone();
    working.clear_pending_refresh();
    if batch.requires_full_rescan {
        working.refresh_all(&ExplorerFilesystem);
    } else {
        working.refresh_affected(
            &ExplorerFilesystem,
            batch
                .changes
                .iter()
                .filter(|change| change.kind != FileChangeKind::Modify)
                .map(|change| change.path.clone()),
        );
    }
    let directories = app
        .explorer
        .model
        .listings()
        .filter_map(|(key, previous)| {
            let next = working.listing(key);
            (next != Some(previous)).then(|| (key.clone(), previous.clone(), next.cloned()))
        })
        .collect();
    let editing = if batch.requires_full_rescan
        || paths
            .iter()
            .any(|p| p.file_name().is_some_and(|n| n == ".editorconfig"))
    {
        app.workspace
            .documents
            .iter()
            .filter(|(_, d)| d.is_text())
            .filter_map(|(&id, d)| {
                let path = d.meta.local_path()?;
                Some((
                    id,
                    d.incarnation,
                    path.to_path_buf(),
                    crate::infrastructure::editorconfig::load(path),
                ))
            })
            .collect()
    } else {
        Vec::new()
    };
    PreparedFileBatch {
        editing,
        roots: app.explorer.model.roots().to_vec(),
        directories,
        disk_updates,
        defer_tree: working.pending_refresh(),
        invalidate_search: !app.search.ui.query.text().is_empty()
            && (batch.requires_full_rescan
                || crate::infrastructure::search_filesystem::paths_affect_search(
                    &app.search.spec(),
                    app.explorer.model.roots(),
                    &batch
                        .changes
                        .iter()
                        // Directory timestamp changes do not change contents.
                        .filter(|change| {
                            change.kind != FileChangeKind::Modify || !change.path.is_dir()
                        })
                        .map(|change| change.path.clone())
                        .collect::<Vec<_>>(),
                )),
    }
}

pub(crate) fn apply_prepared_batch(app: &mut AppState, batch: PreparedFileBatch) -> (bool, bool) {
    let mut changed = false;
    let mut retry = false;
    if app.explorer.model.roots() == batch.roots {
        if batch.defer_tree || app.explorer.model.editing() {
            app.explorer.model.defer_refresh();
        } else {
            app.explorer.model.clear_pending_refresh();
            for (key, previous, next) in &batch.directories {
                if app
                    .explorer
                    .model
                    .listing(key)
                    .is_some_and(|current| Arc::ptr_eq(current, previous))
                {
                    let old = if let Some(next) = next {
                        app.explorer.model.insert_listing(key.clone(), next.clone())
                    } else {
                        app.explorer.model.unload(key)
                    };
                    crate::infrastructure::ui_dispatch::retire(old);
                    changed = true;
                } else if app.explorer.model.listing(key).is_some() {
                    retry = true;
                }
            }
        }
    }
    crate::infrastructure::ui_dispatch::retire(batch.directories);
    for (id, incarnation, path, options) in batch.editing {
        if let Some(doc) =
            app.workspace.documents.get_mut(&id).filter(|d| {
                d.incarnation == incarnation && d.meta.local_path() == Some(path.as_path())
            })
        {
            doc.buffer.initialize_file_options(options);
            changed = true;
        }
    }
    let expected_updates = batch.disk_updates.len();
    let outcomes = app.workspace.apply_disk_updates(batch.disk_updates);
    retry |= outcomes.len() < expected_updates;
    changed |= outcomes.iter().any(|result| {
        matches!(
            result,
            ReconcileResult::Reloaded(_)
                | ReconcileResult::Conflict(_)
                | ReconcileResult::Missing(_)
        )
    });
    let content_reloaded = outcomes.iter().any(|result| {
        matches!(result, ReconcileResult::Reloaded(path)
            if app.explorer.model.roots().iter().any(|root| path.starts_with(root)))
    });
    if app.explorer.model.roots() == batch.roots
        && (batch.invalidate_search || content_reloaded)
        && !app.search.ui.query.text().is_empty()
        && !app.search.model.status().cancelled
    {
        app.search.ui.refresh += 1;
        changed = true;
    }
    changed |= super::editing::configure_all(app);
    (changed, retry)
}

pub(crate) fn notify_prepared_impl(
    state: AppContext,
    application: lgui::ApplicationHandle,
    prepared: PreparedFileBatch,
    lease: Option<Lease>,
) {
    let next_application = application.clone();
    application.post(move || {
        if lease.as_ref().is_some_and(Lease::cancelled) {
            crate::infrastructure::ui_dispatch::retire(prepared);
            return;
        }
        let mut retry = false;
        state.try_update(|app| {
            let (changed, stale) = apply_prepared_batch(app, prepared);
            retry = stale;
            changed
        });
        if retry {
            refresh_async_impl(state, next_application, lease);
        }
    });
}
pub(crate) fn notify_prepared(
    state: AppContext,
    application: lgui::ApplicationHandle,
    prepared: PreparedFileBatch,
) {
    notify_prepared_impl(state, application, prepared, None)
}
pub(crate) fn notify_prepared_scoped(
    state: AppContext,
    application: lgui::ApplicationHandle,
    prepared: PreparedFileBatch,
    lease: Lease,
) {
    notify_prepared_impl(state, application, prepared, Some(lease))
}

pub(crate) fn refresh_async_impl(
    state: AppContext,
    application: lgui::ApplicationHandle,
    lease: Option<Lease>,
) {
    if lease.as_ref().is_some_and(Lease::cancelled) {
        return;
    }
    let _ = thread::Builder::new()
        .name("loog-disk-refresh".into())
        .spawn(move || {
            let prepared = prepare_reconciliation(&state.get());
            notify_prepared_impl(state, application, prepared, lease);
        });
}
pub(crate) fn refresh_async(state: AppContext, application: lgui::ApplicationHandle) {
    refresh_async_impl(state, application, None)
}

fn prepare_reconciliation(app: &AppState) -> PreparedFileBatch {
    let mut prepared = prepare_batch(
        app,
        &FileChangeBatch {
            changes: Vec::new(),
            requires_full_rescan: true,
        },
    );
    // Focus changes and directory-cache retries only reconcile disk models.
    // Actual watcher events and reloaded documents still invalidate results.
    prepared.invalidate_search = false;
    prepared
}

#[cfg(test)]
mod tests {
    use crate::infrastructure::file_watcher::{coalesce_events, normalized_roots};
    use notify::{Event, EventKind};
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use crate::infrastructure::file_watcher::{FileChange, MAX_BATCH_PATHS, start};
    use notify::event::{CreateKind, ModifyKind, RemoveKind, RenameMode};

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    fn temp_directory(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "loog-file-watcher-{label}-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        std::fs::canonicalize(path).unwrap()
    }

    fn event(kind: EventKind, paths: impl IntoIterator<Item = PathBuf>) -> notify::Result<Event> {
        let mut event = Event::new(kind);
        event.paths.extend(paths);
        Ok(event)
    }

    #[test]
    fn roots_are_deduplicated_and_nested_roots_are_removed() {
        let root = std::env::temp_dir();
        assert_eq!(
            normalized_roots(vec![root.join("missing"), root.clone(), root.clone()]),
            [std::fs::canonicalize(root).unwrap()]
        );
    }

    #[test]
    fn batches_deduplicate_paths_and_keep_strongest_change() {
        let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = root.join("loog-watch-test.txt");
        let batch = coalesce_events(
            vec![
                event(EventKind::Modify(ModifyKind::Any), [path.clone()]),
                event(EventKind::Create(CreateKind::Any), [path.clone()]),
                event(EventKind::Remove(RemoveKind::Any), [path.clone()]),
            ],
            std::slice::from_ref(&root),
        );
        assert_eq!(
            batch.changes,
            [FileChange {
                path,
                kind: FileChangeKind::Remove
            }]
        );
    }

    #[test]
    fn errors_request_a_full_rescan() {
        let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let batch = coalesce_events(
            vec![Err(notify::Error::generic("watch failed"))],
            std::slice::from_ref(&root),
        );
        assert!(batch.requires_full_rescan);
    }

    #[test]
    fn rename_paths_are_structural_changes() {
        let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let old = root.join("old.txt");
        let new = root.join("new.txt");
        let batch = coalesce_events(
            vec![event(
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                [old.clone(), new.clone()],
            )],
            std::slice::from_ref(&root),
        );
        assert_eq!(
            batch.changes,
            [
                FileChange {
                    path: new,
                    kind: FileChangeKind::Create,
                },
                FileChange {
                    path: old,
                    kind: FileChangeKind::Create,
                }
            ]
        );
    }

    #[test]
    fn oversized_batches_degrade_to_a_full_rescan() {
        let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let events = (0..=MAX_BATCH_PATHS)
            .map(|index| {
                event(
                    EventKind::Create(CreateKind::File),
                    [root.join(format!("generated-{index}.txt"))],
                )
            })
            .collect();
        let batch = coalesce_events(events, std::slice::from_ref(&root));
        assert!(batch.requires_full_rescan);
    }

    #[test]
    fn events_outside_roots_are_filtered() {
        let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let outside = PathBuf::from(if cfg!(windows) {
            r"Z:\outside.txt"
        } else {
            "/loog-outside.txt"
        });
        let batch = coalesce_events(
            vec![event(EventKind::Modify(ModifyKind::Any), [outside])],
            std::slice::from_ref(&root),
        );
        assert!(batch.changes.is_empty());
    }

    #[test]
    fn git_metadata_events_do_not_trigger_explorer_reload_batches() {
        let root = temp_directory("git-metadata");
        let batch = coalesce_events(
            vec![
                event(
                    EventKind::Modify(ModifyKind::Any),
                    [root.join(".git/index")],
                ),
                event(
                    EventKind::Create(CreateKind::File),
                    [root.join("nested/.git/objects/object")],
                ),
                event(EventKind::Modify(ModifyKind::Any), [root.join("source.rs")]),
            ],
            std::slice::from_ref(&root),
        );
        assert_eq!(batch.changes.len(), 1);
        assert_eq!(batch.changes[0].path, root.join("source.rs"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn outdated_directory_notifications_request_a_fresh_read() {
        let root = temp_directory("delayed-notification");
        let mut app = AppState::new();
        app.explorer.model.restore(vec![root.clone()], Vec::new());
        let key = root.to_string_lossy().into_owned();
        app.explorer.model.insert_listing(
            key.clone(),
            crate::infrastructure::explorer_filesystem::read_directory(&root),
        );
        fs::write(root.join("first.txt"), "first").unwrap();
        let full = FileChangeBatch {
            changes: Vec::new(),
            requires_full_rescan: true,
        };
        let first = prepare_batch(&app, &full);
        fs::write(root.join("second.txt"), "second").unwrap();
        let second = prepare_batch(&app, &full);
        assert_eq!(apply_prepared_batch(&mut app, first), (true, false));
        assert_eq!(apply_prepared_batch(&mut app, second), (false, true));
        let retry = prepare_batch(&app, &full);
        assert_eq!(apply_prepared_batch(&mut app, retry), (true, false));
        assert_eq!(app.explorer.model.listing(&key).unwrap().len(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn search_invalidation_ignores_unrelated_events_and_focus_reconciliation() {
        let root = temp_directory("search-refresh");
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join(".gitignore"), "target/\n").unwrap();
        fs::write(root.join("target/generated.txt"), "generated").unwrap();
        fs::write(root.join("a.txt"), "needle").unwrap();
        let mut app = AppState::new();
        app.explorer.model.restore(vec![root.clone()], Vec::new());
        app.search.ui.query.set_text("needle");
        // Search is cached even when another drawer is visible.
        for (name, expected) in [("target/generated.txt", 0), ("a.txt", 1)] {
            let prepared = prepare_batch(
                &app,
                &FileChangeBatch {
                    changes: vec![FileChange {
                        path: root.join(name),
                        kind: FileChangeKind::Modify,
                    }],
                    requires_full_rescan: false,
                },
            );
            apply_prepared_batch(&mut app, prepared);
            assert_eq!(app.search.ui.refresh, expected);
        }
        let full = FileChangeBatch {
            changes: Vec::new(),
            requires_full_rescan: true,
        };
        let prepared = prepare_reconciliation(&app);
        apply_prepared_batch(&mut app, prepared);
        assert_eq!(
            app.search.ui.refresh, 1,
            "Focus alone must not restart a search"
        );
        app.workspace.open_path(root.join("a.txt"), "needle".into());
        fs::write(root.join("a.txt"), "changed needle").unwrap();
        let prepared = prepare_reconciliation(&app);
        apply_prepared_batch(&mut app, prepared);
        assert_eq!(
            app.search.ui.refresh, 2,
            "An actual clean-buffer reload must invalidate results"
        );
        app.search.model.fixture_status().cancelled = true;
        let prepared = prepare_batch(&app, &full);
        apply_prepared_batch(&mut app, prepared);
        assert_eq!(
            app.search.ui.refresh, 2,
            "Watcher events must respect an explicit stop"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn applying_batch_refreshes_tree_and_clean_open_document() {
        let root = temp_directory("apply");
        let open_path = root.join("open.txt");
        let added_path = root.join("added.txt");
        fs::write(&open_path, "before").unwrap();

        let mut app = AppState::new();
        app.explorer.model.restore(vec![root.clone()], Vec::new());
        let root_key = root.to_string_lossy().into_owned();
        app.explorer.model.insert_listing(
            root_key.clone(),
            crate::infrastructure::explorer_filesystem::read_directory(&root),
        );
        app.workspace.open_path(open_path.clone(), "before".into());

        fs::write(&open_path, "after and longer").unwrap();
        fs::write(&added_path, "added").unwrap();
        let batch = FileChangeBatch {
            changes: vec![
                FileChange {
                    path: open_path,
                    kind: FileChangeKind::Modify,
                },
                FileChange {
                    path: added_path,
                    kind: FileChangeKind::Create,
                },
            ],
            requires_full_rescan: false,
        };

        assert!(apply_batch(&mut app, &batch));
        assert_eq!(
            app.workspace.active_editor().unwrap().text(),
            "after and longer"
        );
        assert!(
            app.explorer
                .model
                .listing(&root_key)
                .unwrap()
                .iter()
                .any(|entry| entry.name == "added.txt")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn watcher_reports_external_creation_and_stops_cleanly() {
        let root = temp_directory("service");
        let created = root.join("created.txt");
        let (sender, receiver) = mpsc::channel();
        let watcher = start(vec![root.clone()], move |batch| {
            let _ = sender.send(batch);
        })
        .unwrap();

        fs::write(&created, "created").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut observed = false;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Ok(batch) = receiver.recv_timeout(remaining) else {
                break;
            };
            if batch.paths().any(|path| path == created) {
                observed = true;
                break;
            }
        }
        assert!(observed, "watcher did not report the created file");

        drop(watcher);
        fs::remove_dir_all(root).unwrap();
    }

    // --- Explorer state changed between preparing and applying a batch ---

    fn full_rescan() -> FileChangeBatch {
        FileChangeBatch {
            changes: Vec::new(),
            requires_full_rescan: true,
        }
    }

    fn explorer_at(root: &Path) -> AppState {
        let mut app = AppState::new();
        app.explorer
            .model
            .restore(vec![root.to_path_buf()], Vec::new());
        app.explorer.model.hydrate(&ExplorerFilesystem);
        app
    }

    fn key(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    fn names(app: &AppState, directory: &Path) -> Vec<String> {
        app.explorer
            .model
            .listing(&key(directory))
            .unwrap()
            .iter()
            .map(|entry| entry.name.clone())
            .collect()
    }

    #[test]
    fn collapsing_during_preparation_keeps_the_collapse_and_stays_unloaded() {
        let root = temp_directory("race-collapse");
        let sub = root.join("sub");
        fs::create_dir(&sub).unwrap();
        let mut app = explorer_at(&root);
        app.explorer
            .model
            .toggle_directory(&ExplorerFilesystem, &key(&sub));
        fs::write(sub.join("new.txt"), "new").unwrap();

        let prepared = prepare_batch(&app, &full_rescan());
        app.explorer
            .model
            .toggle_directory(&ExplorerFilesystem, &key(&sub));

        assert_eq!(apply_prepared_batch(&mut app, prepared), (false, false));
        assert!(!app.explorer.model.is_expanded(&key(&sub)));
        assert!(app.explorer.model.listing(&key(&sub)).is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directory_expanded_during_preparation_stays_loaded() {
        let root = temp_directory("race-expand");
        let later = root.join("later");
        fs::create_dir(&later).unwrap();
        fs::write(later.join("inner.txt"), "inner").unwrap();
        let mut app = explorer_at(&root);
        fs::write(root.join("added.txt"), "added").unwrap();

        let prepared = prepare_batch(&app, &full_rescan());
        app.explorer
            .model
            .toggle_directory(&ExplorerFilesystem, &key(&later));

        assert_eq!(apply_prepared_batch(&mut app, prepared), (true, false));
        assert!(app.explorer.model.is_expanded(&key(&later)));
        assert_eq!(names(&app, &later), ["inner.txt"]);
        assert_eq!(names(&app, &root), ["later", "added.txt"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn edit_row_opened_during_preparation_defers_the_tree() {
        let root = temp_directory("race-edit");
        let mut app = explorer_at(&root);
        fs::write(root.join("added.txt"), "added").unwrap();

        let prepared = prepare_batch(&app, &full_rescan());
        app.explorer.model.begin_create(
            root.clone(),
            crate::features::explorer::ExplorerCreateKind::File,
        );

        assert_eq!(apply_prepared_batch(&mut app, prepared), (false, false));
        assert!(names(&app, &root).is_empty());
        assert!(app.explorer.model.pending_refresh());

        app.explorer.model.cancel_edit();
        assert!(
            app.explorer
                .model
                .apply_pending_refresh(&ExplorerFilesystem)
        );
        assert_eq!(names(&app, &root), ["added.txt"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn root_replaced_during_preparation_discards_the_tree_batch() {
        let root = temp_directory("race-old-root");
        let other = temp_directory("race-new-root");
        fs::write(other.join("kept.txt"), "kept").unwrap();
        let mut app = explorer_at(&root);
        fs::write(root.join("added.txt"), "added").unwrap();

        let prepared = prepare_batch(&app, &full_rescan());
        app.explorer
            .model
            .open_root(&ExplorerFilesystem, other.clone());

        assert_eq!(apply_prepared_batch(&mut app, prepared), (false, false));
        assert!(app.explorer.model.listing(&key(&root)).is_none());
        assert_eq!(names(&app, &other), ["kept.txt"]);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(other).unwrap();
    }

    #[test]
    fn rename_during_preparation_rejects_the_late_listing_and_retries() {
        let root = temp_directory("race-rename");
        let old = root.join("old.txt");
        let new = root.join("new.txt");
        fs::write(&old, "old").unwrap();
        let mut app = explorer_at(&root);
        fs::write(root.join("added.txt"), "added").unwrap();

        let prepared = prepare_batch(&app, &full_rescan());
        fs::rename(&old, &new).unwrap();
        app.explorer
            .model
            .apply_rename(&ExplorerFilesystem, &old, &new);
        let after_rename = names(&app, &root);

        assert_eq!(apply_prepared_batch(&mut app, prepared), (false, true));
        assert_eq!(names(&app, &root), after_rename);
        assert!(after_rename.contains(&"new.txt".to_owned()));
        assert!(!after_rename.contains(&"old.txt".to_owned()));

        let retry = prepare_batch(&app, &full_rescan());
        assert_eq!(apply_prepared_batch(&mut app, retry), (false, false));
        assert_eq!(names(&app, &root), ["added.txt", "new.txt"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn removed_directory_is_unloaded_and_collapsed() {
        let root = temp_directory("race-removed");
        let sub = root.join("sub");
        fs::create_dir(&sub).unwrap();
        let mut app = explorer_at(&root);
        app.explorer
            .model
            .toggle_directory(&ExplorerFilesystem, &key(&sub));
        fs::remove_dir(&sub).unwrap();

        let prepared = prepare_batch(&app, &full_rescan());
        assert_eq!(apply_prepared_batch(&mut app, prepared), (true, false));
        assert!(app.explorer.model.listing(&key(&sub)).is_none());
        assert!(!app.explorer.model.is_expanded(&key(&sub)));
        assert!(names(&app, &root).is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    /// `cargo test --release --bin Loog prepare_batch_cost -- --ignored --nocapture`
    /// Times watcher preparation with a loaded tree and large open documents.
    #[test]
    #[ignore = "benchmark"]
    fn prepare_batch_cost_benchmark() {
        let root = temp_directory("bench-prepare");
        let mut directories = Vec::new();
        for d in 0..100 {
            let directory = root.join(format!("dir-{d:03}"));
            fs::create_dir(&directory).unwrap();
            for f in 0..100 {
                fs::write(directory.join(format!("file-{f:03}.txt")), "").unwrap();
            }
            directories.push(directory);
        }
        let mut app = AppState::new();
        app.explorer.model.restore(vec![root.clone()], Vec::new());
        app.explorer.model.hydrate(&ExplorerFilesystem);
        for directory in &directories {
            app.explorer
                .model
                .toggle_directory(&ExplorerFilesystem, &key(directory));
        }
        bench_prepare(app, &root);
    }

    fn bench_prepare(mut app: AppState, root: &Path) {
        // 20 open documents of 512 KiB (10 MiB) that the batch does not touch.
        for index in 0..20 {
            let text = format!("{index:02} 中文 emoji 🙂 line\n").repeat(512 * 1024 / 32);
            app.workspace
                .open_path(root.join(format!("open-{index:02}.rs")), text);
        }
        let single = FileChangeBatch {
            changes: vec![FileChange {
                path: root.join("dir-050").join("created.txt"),
                kind: FileChangeKind::Create,
            }],
            requires_full_rescan: false,
        };
        let full = FileChangeBatch {
            changes: Vec::new(),
            requires_full_rescan: true,
        };
        let median_us = |clone: &dyn Fn()| {
            let mut samples = (0..30)
                .map(|_| {
                    let start = Instant::now();
                    clone();
                    start.elapsed().as_secs_f64() * 1_000_000.0
                })
                .collect::<Vec<_>>();
            samples.sort_by(f64::total_cmp);
            samples[15]
        };
        println!(
            "BENCH clone app_state_median_us={:.2} explorer_model_median_us={:.2}",
            median_us(&|| drop(std::hint::black_box(app.clone()))),
            median_us(&|| drop(std::hint::black_box(app.explorer.model.clone())))
        );
        for (label, batch) in [("single_change", &single), ("full_rescan", &full)] {
            for _ in 0..3 {
                std::hint::black_box(prepare_batch(&app, batch));
            }
            let mut samples = (0..30)
                .map(|_| {
                    let start = Instant::now();
                    std::hint::black_box(prepare_batch(&app, batch));
                    start.elapsed().as_secs_f64() * 1000.0
                })
                .collect::<Vec<_>>();
            samples.sort_by(f64::total_cmp);
            println!(
                "BENCH prepare_batch {label} median_ms={:.4} p90_ms={:.4}",
                samples[15], samples[27]
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn queued_reconciliation_cannot_publish_after_its_native_watcher_is_dropped() {
        use crate::app::test_support::UiTasks;
        use lgui::{
            application::AppView,
            core::UiScale,
            prelude::{UiRect, VisualStyle, panel},
            session::UiSession,
        };
        use std::sync::Mutex;
        let root = temp_directory("queued-owner-cleanup");
        let path = root.join("document.txt");
        fs::write(&path, "before").unwrap();
        let mut initial = AppState::new();
        initial
            .explorer
            .model
            .restore(vec![root.clone()], Vec::new());
        initial.workspace.open_path(path.clone(), "before".into());
        let exposed = Arc::new(Mutex::new(None));
        let output = exposed.clone();
        let rect = UiRect::new(0.0, 0.0, 10.0, 10.0);
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| initial.clone()));
            *output.lock().unwrap() = Some(state);
            panel(rect, VisualStyle::default())
        });
        let mut session = UiSession::new();
        session.render_view(&view, rect, UiScale::ONE);
        let state = exposed.lock().unwrap().clone().unwrap();
        let handle = start(vec![root.clone()], |_| {}).unwrap();
        let lease = handle.lease();
        assert!(!lease.cancelled());
        fs::write(&path, "after").unwrap();
        let prepared = prepare_batch(
            &state.get(),
            &FileChangeBatch {
                changes: vec![FileChange {
                    path: path.clone(),
                    kind: FileChangeKind::Modify,
                }],
                requires_full_rescan: false,
            },
        );
        let tasks = UiTasks::default();
        notify_prepared_scoped(state.clone(), tasks.application(), prepared, lease.clone());
        assert!(!tasks.pending_ids().is_empty());
        drop(handle);
        assert!(lease.cancelled());
        tasks.run_all();
        assert_eq!(
            state
                .get()
                .workspace
                .active_editor()
                .unwrap()
                .buffer()
                .text()
                .to_string(),
            "before"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
