//! Background execution of Source Control write operations. One operation
//! runs at a time: it is marked in the snapshot, runs off the UI thread, and
//! its completion is applied on the UI thread before the poller rescans.
use crate::app::context::AppContext;

use std::sync::Arc;
use std::thread;

use lgui::ApplicationHandle;
use lgui::prelude::State;

use crate::app::state::AppState;
use crate::app::workspace::ReconcileResult;
use crate::features::git::snapshot::operation;
use crate::features::git::{GitResult, GitStoreSnapshot, OperationKind};
use crate::infrastructure::git::PollControl;

pub(crate) type Task = Box<dyn FnOnce() + Send>;

/// Where operation work runs and where its completion is delivered.
pub(crate) trait GitExecutor: Send + Sync {
    /// Start `work` in the background; false when it could not start.
    fn spawn(&self, name: String, work: Task) -> bool;
    /// Run `completion` on the UI thread.
    fn post(&self, completion: Task);
}

struct ThreadExecutor(ApplicationHandle);

impl GitExecutor for ThreadExecutor {
    fn spawn(&self, name: String, work: Task) -> bool {
        thread::Builder::new().name(name).spawn(work).is_ok()
    }

    fn post(&self, completion: Task) {
        self.0.post(completion);
    }
}

#[derive(Clone)]
pub(crate) struct GitTasks {
    state: AppContext,
    store: State<GitStoreSnapshot>,
    executor: Arc<dyn GitExecutor>,
    poll: PollControl,
    /// Rereads the disk when a completion could not apply every update.
    application: Option<ApplicationHandle>,
}

impl GitTasks {
    /// Tasks for the running application; `None` before it has started.
    pub(crate) fn for_state(state: &AppContext, store: &State<GitStoreSnapshot>) -> Option<Self> {
        let runtime = state.services()?;
        let application = runtime.application()?;
        Some(Self {
            state: state.clone(),
            store: store.clone(),
            executor: Arc::new(ThreadExecutor(application.clone())),
            poll: runtime.git_poll_control(),
            application: Some(application),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_executor(
        state: &AppContext,
        store: &State<GitStoreSnapshot>,
        executor: Arc<dyn GitExecutor>,
    ) -> Self {
        Self {
            state: state.clone(),
            store: store.clone(),
            executor,
            poll: PollControl::default(),
            application: None,
        }
    }

    /// Start operation `kind`, handing `work`'s result to `on_success` on the
    /// UI thread. Returns false when another operation is running or the
    /// worker could not start.
    pub(crate) fn run<T: Send + 'static>(
        &self,
        kind: OperationKind,
        message: impl Into<String>,
        work: impl FnOnce() -> GitResult<T> + Send + 'static,
        on_success: impl FnOnce(&mut AppState, T) + Send + 'static,
    ) -> bool {
        if self.store.get().operation.is_some() {
            return false;
        }
        let message = message.into();
        self.store.update(move |snapshot| {
            snapshot.operation = Some(operation(kind, message, false));
            snapshot.last_error = None;
        });
        let tasks = self.clone();
        let spawned = self.executor.spawn(
            format!("loog-git-{kind:?}").to_ascii_lowercase(),
            Box::new(move || {
                let result = work();
                let disk_updates = if result.is_ok() {
                    tasks.state.get().workspace.prepare_disk_updates(&[], true)
                } else {
                    Vec::new()
                };
                let executor = tasks.executor.clone();
                executor.post(Box::new(move || {
                    tasks.complete(kind, result, disk_updates, on_success)
                }));
            }),
        );
        if !spawned {
            self.store.update(|snapshot| snapshot.operation = None);
            self.poll.request_refresh();
        }
        spawned
    }

    fn complete<T>(
        &self,
        kind: OperationKind,
        result: GitResult<T>,
        disk_updates: Vec<crate::app::workspace::PreparedDiskUpdate>,
        on_success: impl FnOnce(&mut AppState, T),
    ) {
        match result {
            Ok(value) => {
                self.state.update(move |app| on_success(app, value));
                if kind == OperationKind::Commit {
                    self.state.update(|app| app.git.ui.commit_input.clear());
                }
                let application = self.application.clone();
                let state = self.state.clone();
                self.state.update(|app| {
                    let expected_updates = disk_updates.len();
                    let outcomes = app.workspace.apply_disk_updates(disk_updates);
                    if outcomes.len() < expected_updates
                        && let Some(application) = application
                    {
                        crate::app::file_watcher::refresh_async(state, application);
                    }
                    let conflicts = outcomes
                        .iter()
                        .filter(|outcome| matches!(outcome, ReconcileResult::Conflict(_)))
                        .count();
                    if conflicts > 0 {
                        app.show_toast(format!(
                            "{conflicts} open file(s) changed on disk; editor buffers were preserved."
                        ));
                    }
                });
                self.store.update(|current| current.operation = None);
            }
            Err(error) => {
                let message = error.user_message();
                self.state.update(move |app| app.show_error(message));
                self.store.update(move |snapshot| {
                    snapshot.operation = None;
                    snapshot.last_error = Some(error);
                });
            }
        }
        // The poller knows the current roots and open tabs. Never publish a
        // separate scan using the operation's starting context.
        self.poll.request_refresh();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use lgui::application::AppView;
    use lgui::core::UiScale;
    use lgui::prelude::{UiRect, panel};
    use lgui::session::UiSession;

    use super::*;
    use crate::features::git::{GitError, error::GitErrorKind};

    /// Holds spawned work and posted completions until a test runs them.
    #[derive(Default)]
    struct ManualExecutor {
        work: Mutex<Vec<Task>>,
        completions: Mutex<Vec<Task>>,
        refuse: bool,
    }

    impl GitExecutor for ManualExecutor {
        fn spawn(&self, _name: String, work: Task) -> bool {
            if self.refuse {
                return false;
            }
            self.work.lock().unwrap().push(work);
            true
        }

        fn post(&self, completion: Task) {
            self.completions.lock().unwrap().push(completion);
        }
    }

    impl ManualExecutor {
        fn run_work(&self) {
            let work = std::mem::take(&mut *self.work.lock().unwrap());
            work.into_iter().for_each(|task| task());
        }

        fn run_completions(&self) {
            let done = std::mem::take(&mut *self.completions.lock().unwrap());
            done.into_iter().for_each(|task| task());
        }
    }

    fn live() -> (AppContext, State<GitStoreSnapshot>, UiSession) {
        let exposed = Arc::new(Mutex::new(None));
        let output = exposed.clone();
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(AppState::new));
            let store = cx.state(GitStoreSnapshot::default());
            *output.lock().unwrap() = Some((state, store));
            panel(UiRect::new(0.0, 0.0, 10.0, 10.0), Default::default())
        });
        let mut session = UiSession::new();
        session.render_view(&view, UiRect::new(0.0, 0.0, 10.0, 10.0), UiScale::ONE);
        let (state, store) = exposed.lock().unwrap().take().unwrap();
        (state, store, session)
    }

    fn tasks(
        executor: &Arc<ManualExecutor>,
    ) -> (GitTasks, AppContext, State<GitStoreSnapshot>, UiSession) {
        let (state, store, session) = live();
        let tasks = GitTasks::with_executor(&state, &store, executor.clone());
        (tasks, state, store, session)
    }

    #[test]
    fn a_second_operation_waits_for_the_first_to_finish() {
        let executor = Arc::new(ManualExecutor::default());
        let (tasks, _state, store, _session) = tasks(&executor);

        assert!(tasks.run(OperationKind::Stage, "Staging", || Ok(()), |_, ()| {}));
        assert!(store.get().operation.is_some());
        assert!(!tasks.run(OperationKind::Commit, "Committing", || Ok(()), |_, ()| {}));

        executor.run_work();
        executor.run_completions();
        assert!(store.get().operation.is_none());
        assert!(tasks.run(OperationKind::Commit, "Committing", || Ok(()), |_, ()| {}));
    }

    #[test]
    fn a_completion_applies_only_on_the_ui_turn() {
        let executor = Arc::new(ManualExecutor::default());
        let (tasks, state, store, _session) = tasks(&executor);
        state.update(|app| app.git.ui.commit_input.set_text("message".to_owned()));

        assert!(tasks.run(
            OperationKind::Commit,
            "Committing",
            || Ok(7),
            |app, value: i32| {
                app.show_success(format!("value {value}"));
            }
        ));
        executor.run_work();
        assert!(store.get().operation.is_some());
        assert_eq!(state.get().git.ui.commit_input.text(), "message");

        executor.run_completions();
        let app = state.get();
        assert!(store.get().operation.is_none());
        assert!(app.git.ui.commit_input.text().is_empty());
        assert!(
            app.chrome
                .toast
                .as_ref()
                .unwrap()
                .message
                .contains("value 7")
        );
    }

    #[test]
    fn a_failed_operation_keeps_the_commit_message_and_records_the_error() {
        let executor = Arc::new(ManualExecutor::default());
        let (tasks, state, store, _session) = tasks(&executor);
        state.update(|app| app.git.ui.commit_input.set_text("keep me".to_owned()));

        assert!(tasks.run(
            OperationKind::Commit,
            "Committing",
            || Err::<(), _>(GitError::new(GitErrorKind::Other, "hook rejected")),
            |_, ()| panic!("must not succeed"),
        ));
        executor.run_work();
        executor.run_completions();

        let snapshot = store.get();
        assert!(snapshot.operation.is_none());
        assert!(snapshot.last_error.is_some());
        assert_eq!(state.get().git.ui.commit_input.text(), "keep me");
        assert!(state.get().chrome.toast.is_some());
    }

    #[test]
    fn a_worker_that_cannot_start_clears_the_operation() {
        let executor = Arc::new(ManualExecutor {
            refuse: true,
            ..Default::default()
        });
        let (tasks, _state, store, _session) = tasks(&executor);

        assert!(!tasks.run(OperationKind::Push, "Pushing", || Ok(()), |_, ()| {}));
        assert!(store.get().operation.is_none());
    }

    #[test]
    fn a_new_operation_clears_the_previous_error() {
        let executor = Arc::new(ManualExecutor::default());
        let (tasks, _state, store, _session) = tasks(&executor);
        store.update(|snapshot| {
            snapshot.last_error = Some(GitError::new(GitErrorKind::Other, "old"));
        });

        assert!(tasks.run(OperationKind::Fetch, "Fetching", || Ok(()), |_, ()| {}));
        assert!(store.get().last_error.is_none());
    }
}
