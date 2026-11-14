//! Source Control status polling: what the poller should scan, which scan
//! results may land, and the history load that follows the active HEAD.
use crate::app::context::AppContext;

use std::path::PathBuf;
use std::time::Duration;

use lgui::prelude::State;

use crate::app::state::AppState;
use crate::features::git::{GitResult, GitStoreSnapshot, PollTarget};
use crate::infrastructure::git::{PollControl, PollingHandle};

pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(2_000);

/// A scan result with the context it was taken in.
pub(crate) struct GitScanNotification {
    pub roots: Vec<PathBuf>,
    pub target: PollTarget,
    pub result: GitResult<GitStoreSnapshot>,
}

/// What the poller should scan now: the active file, open diffs, and the
/// repository chosen in Source Control while the active file is unchanged.
pub(crate) fn poll_target(app: &AppState) -> PollTarget {
    let mut target = app.workspace.git_poll_target();
    target.pinned_repository = app
        .git
        .model
        .pinned_for(target.active_path.as_deref())
        .cloned();
    target
}

/// Load the active repository's history while the bottom Commits view is shown.
pub(crate) fn history_key(
    app: &AppState,
    store: &GitStoreSnapshot,
) -> Option<(PathBuf, Option<String>)> {
    store
        .active()
        .filter(|_| {
            app.git_panel_visible()
                && !app
                    .git
                    .ui
                    .collapsed_sections
                    .contains(&crate::ui::git::panel::HISTORY)
        })
        .map(|repository| {
            let scoped = app
                .git
                .ui
                .history_scope
                .as_ref()
                .filter(|(root, _)| *root == repository.worktree_root)
                .and_then(|(_, name)| {
                    if let Some(tag) = name.strip_prefix("tag:") {
                        return repository
                            .tags
                            .iter()
                            .find(|info| info.name == tag)
                            .and_then(|info| info.commit.clone());
                    }
                    let (root, branches) = app.git.ui.history_branches.as_ref()?;
                    (*root == repository.worktree_root)
                        .then(|| branches.iter().find(|branch| branch.name == *name))
                        .flatten()
                        .map(|branch| branch.oid.clone())
                });
            (
                repository.worktree_root.clone(),
                scoped.or_else(|| repository.head_oid.clone()),
            )
        })
}

/// The active file the Commits view is restricted to, when that scope is chosen.
pub(crate) fn history_file(app: &AppState, store: &GitStoreSnapshot) -> Option<PathBuf> {
    if !app.git.ui.history_file_scope {
        return None;
    }
    let root = &store.active()?.worktree_root;
    app.workspace
        .active_path()
        .filter(|path| path.starts_with(root))
        .map(std::path::Path::to_path_buf)
}

/// Start polling `roots`; dropping the handle stops it.
pub(crate) fn start(
    roots: Vec<PathBuf>,
    control: PollControl,
    publish: impl Fn(GitScanNotification) + Send + 'static,
) -> PollingHandle {
    let scan_roots = roots.clone();
    crate::infrastructure::git::start_polling(
        roots,
        control,
        POLL_INTERVAL,
        move |target, result| {
            publish(GitScanNotification {
                roots: scan_roots.clone(),
                target: target.clone(),
                result,
            });
        },
    )
}

/// Deliver a scan on the UI thread, retiring whatever it replaces.
pub(crate) fn deliver(
    state: &AppContext,
    store: &State<GitStoreSnapshot>,
    notification: GitScanNotification,
) {
    let mut retired = None;
    state.try_update(|app| {
        let mut diffs_changed = false;
        store.try_update(|current| {
            let old = current.clone();
            let (store_changed, changed) = apply_scan(
                app,
                current,
                &notification.roots,
                &notification.target,
                notification.result,
            );
            retired = Some(old);
            diffs_changed = changed;
            store_changed
        });
        diffs_changed
    });
    crate::infrastructure::ui_dispatch::retire(retired);
}

/// Apply a scan unless the workspace roots or poll target changed since it
/// started, or a newer generation already landed. Returns whether the
/// snapshot changed and whether open diff tabs changed.
pub(crate) fn apply_scan(
    app: &mut AppState,
    current: &mut GitStoreSnapshot,
    roots: &[PathBuf],
    target: &PollTarget,
    result: GitResult<GitStoreSnapshot>,
) -> (bool, bool) {
    if app.explorer.model.roots() != roots || poll_target(app) != *target {
        crate::infrastructure::ui_dispatch::retire(result);
        return (false, false);
    }
    if result
        .as_ref()
        .is_ok_and(|next| next.generation < current.generation)
    {
        crate::infrastructure::ui_dispatch::retire(result);
        return (false, false);
    }
    let diffs_changed = if let Ok(next) = &result
        && next.generation >= current.generation
    {
        let retired = app
            .workspace
            .apply_prepared_git_diffs(&next.presentation.diffs, app.git.ui.split_diff);
        let changed = !retired.is_empty();
        crate::infrastructure::ui_dispatch::retire(retired);
        changed
    } else {
        false
    };
    (current.apply_scan(result), diffs_changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::git::{GitError, error::GitErrorKind};

    fn scan(generation: u64) -> GitResult<GitStoreSnapshot> {
        Ok(GitStoreSnapshot {
            generation,
            initializing: false,
            ..Default::default()
        })
    }

    #[test]
    fn rejects_success_and_failure_from_a_previous_workspace() {
        let old_roots = vec![PathBuf::from("old")];
        let mut app = AppState::new();
        app.explorer
            .model
            .restore(vec![PathBuf::from("current")], Vec::new());
        let mut store = GitStoreSnapshot::default();
        for result in [
            scan(100),
            Err(GitError::new(GitErrorKind::Other, "old scan failed")),
        ] {
            assert_eq!(
                apply_scan(
                    &mut app,
                    &mut store,
                    &old_roots,
                    &PollTarget::default(),
                    result
                ),
                (false, false)
            );
        }
        assert_eq!(store.generation, 0);
        assert!(store.last_error.is_none());
    }

    #[test]
    fn rejects_a_scan_for_a_tab_that_is_no_longer_active() {
        let mut app = AppState::new();
        app.workspace
            .open_path(PathBuf::from("a.txt"), String::new());
        let target = poll_target(&app);
        app.workspace
            .open_path(PathBuf::from("b.txt"), String::new());
        let mut store = GitStoreSnapshot::default();
        assert_eq!(
            apply_scan(&mut app, &mut store, &[], &target, scan(100)),
            (false, false)
        );
        assert_eq!(store.generation, 0);
        let current_target = poll_target(&app);
        assert_eq!(
            apply_scan(&mut app, &mut store, &[], &current_target, scan(101)),
            (true, false)
        );
        assert_eq!(store.generation, 101);
    }

    #[test]
    fn scans_keep_landing_while_a_repository_is_pinned() {
        let mut app = AppState::new();
        app.workspace
            .open_path(PathBuf::from("repo/a.rs"), String::new());
        app.git
            .model
            .pin_repository("repo".into(), Some("repo/a.rs".into()));
        let target = poll_target(&app);
        assert_eq!(target.pinned_repository, Some(PathBuf::from("repo")));
        let mut store = GitStoreSnapshot::default();

        assert_eq!(
            apply_scan(&mut app, &mut store, &[], &target, scan(5)),
            (true, false)
        );
        let mut changed = scan(6);
        if let Ok(snapshot) = &mut changed {
            snapshot.runtime_label = Some("git 2.47".into());
        }
        assert_eq!(
            apply_scan(&mut app, &mut store, &[], &target, changed),
            (true, false)
        );
        assert_eq!(store.generation, 6);
        assert_eq!(store.runtime_label.as_deref(), Some("git 2.47"));
    }

    #[test]
    fn a_scan_from_before_the_pin_is_rejected_and_the_pin_lapses_with_the_file() {
        let mut app = AppState::new();
        app.workspace
            .open_path(PathBuf::from("repo/a.rs"), String::new());
        let before = poll_target(&app);
        app.git
            .model
            .pin_repository("repo".into(), Some("repo/a.rs".into()));
        let mut store = GitStoreSnapshot::default();
        assert_eq!(
            apply_scan(&mut app, &mut store, &[], &before, scan(5)),
            (false, false)
        );

        app.workspace
            .open_path(PathBuf::from("repo/b.rs"), String::new());
        assert_eq!(poll_target(&app).pinned_repository, None);
    }

    #[test]
    fn an_older_generation_never_replaces_a_newer_one() {
        let mut app = AppState::new();
        let target = poll_target(&app);
        let mut store = GitStoreSnapshot::default();
        assert_eq!(
            apply_scan(&mut app, &mut store, &[], &target, scan(9)),
            (true, false)
        );
        assert_eq!(
            apply_scan(&mut app, &mut store, &[], &target, scan(8)),
            (false, false)
        );
        assert_eq!(store.generation, 9);
    }

    #[test]
    fn removing_the_pinned_repository_rejects_its_late_scan() {
        let mut app = AppState::new();
        app.workspace
            .open_path("repo/a.rs".into(), "preserve".into());
        app.git
            .model
            .pin_repository("repo".into(), Some("repo/a.rs".into()));
        let target = poll_target(&app);
        app.git.model.unpin(std::path::Path::new("repo"));
        let mut current = GitStoreSnapshot::default();
        assert_eq!(
            apply_scan(&mut app, &mut current, &[], &target, scan(99)),
            (false, false)
        );
        assert_eq!(current.generation, 0);
        assert_eq!(app.workspace.active_editor().unwrap().text(), "preserve");
        let next_target = poll_target(&app);
        assert_eq!(
            apply_scan(&mut app, &mut current, &[], &next_target, scan(100)),
            (true, false)
        );
        assert_eq!(current.generation, 100);
    }

    #[test]
    fn history_follows_the_active_repository_only_while_visible() {
        let mut app = AppState::new();
        let store = GitStoreSnapshot::default();
        assert_eq!(history_key(&app, &store), None);
        app.source_control.visible = true;
        assert_eq!(history_key(&app, &store), None, "no repository yet");
    }
}
