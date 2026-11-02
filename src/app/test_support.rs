//! Deterministic UI delivery and elapsed time for lifecycle tests.
//! Tasks run outside the queue lock, so a task can post another task.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(crate) mod language;

type Task = Box<dyn FnOnce() + Send + 'static>;

#[derive(Clone, Default)]
pub(crate) struct UiTasks(Arc<Mutex<Queue>>);

#[derive(Default)]
struct Queue {
    sequence: u64,
    tasks: BTreeMap<u64, Task>,
}

impl UiTasks {
    pub(crate) fn application(&self) -> lgui::ApplicationHandle {
        let queue = self.clone();
        lgui::ApplicationHandle::new(
            move |task| {
                let mut queue = queue.0.lock().unwrap();
                queue.sequence += 1;
                let id = queue.sequence;
                queue.tasks.insert(id, task);
            },
            || {},
        )
    }

    pub(crate) fn pending_ids(&self) -> Vec<u64> {
        self.0.lock().unwrap().tasks.keys().copied().collect()
    }

    pub(crate) fn run(&self, id: u64) -> bool {
        let task = self.0.lock().unwrap().tasks.remove(&id);
        if let Some(task) = task {
            task();
            true
        } else {
            false
        }
    }

    /// Deliver this batch only. Newly posted tasks belong to the next batch.
    pub(crate) fn run_all(&self) {
        for id in self.pending_ids() {
            self.run(id);
        }
    }
}

#[derive(Default)]
pub(crate) struct ManualClock(Duration);

impl ManualClock {
    pub(crate) fn elapsed(&self) -> Duration {
        self.0
    }

    pub(crate) fn advance(&mut self, elapsed: Duration) {
        self.0 = self.0.saturating_add(elapsed);
    }
}

pub(crate) fn live(initial: crate::app::state::AppState) -> crate::app::context::AppContext {
    use lgui::{
        application::AppView,
        core::UiScale,
        prelude::{UiRect, VisualStyle, panel},
        session::UiSession,
    };
    let initial = Arc::new(Mutex::new(Some(initial)));
    let output = Arc::new(Mutex::new(None));
    let exposed = output.clone();
    let view: AppView = Arc::new(move |cx| {
        let state = crate::app::context::AppContext::detached(
            cx.state_with(|| initial.lock().unwrap().take().unwrap()),
        );
        *exposed.lock().unwrap() = Some(state);
        panel(UiRect::new(0.0, 0.0, 10.0, 10.0), VisualStyle::default())
    });
    UiSession::new().render_view(&view, UiRect::new(0.0, 0.0, 10.0, 10.0), UiScale::ONE);
    let state = output.lock().unwrap().take().unwrap();
    state
}
/// A mounted application runtime with deterministic delivery of background save results.
pub(crate) struct SavingUi {
    pub state: crate::app::context::AppContext,
    pub runtime: Arc<crate::app::runtime::AppRuntime>,
    pub tasks: UiTasks,
}
impl SavingUi {
    pub fn new(app: crate::app::state::AppState) -> Self {
        let tasks = UiTasks::default();
        let runtime = Arc::new(crate::app::runtime::AppRuntime::default());
        runtime.set_application(tasks.application());
        let state = live(app).with_runtime(&runtime);
        Self {
            state,
            runtime,
            tasks,
        }
    }
    pub fn wait(&self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            self.tasks.run_all();
            if self.state.get().editor.saving.pending == 0 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "save did not complete"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::AppState;

    #[test]
    fn out_of_order_delivery_ignores_a_stale_toast_timer() {
        let queue = UiTasks::default();
        let app = queue.application();
        let state = Arc::new(Mutex::new(AppState::new()));
        let mut clock = ManualClock::default();
        let old = {
            let mut state = state.lock().unwrap();
            state.show_toast("first");
            state.chrome.toast.clone().unwrap()
        };
        let output = state.clone();
        app.post(move || {
            assert!(!output.lock().unwrap().dismiss_toast(old.id));
        });
        state.lock().unwrap().show_toast("replacement");
        let output = state.clone();
        app.post(move || {
            assert_eq!(
                output
                    .lock()
                    .unwrap()
                    .chrome
                    .toast
                    .as_ref()
                    .unwrap()
                    .message,
                "replacement"
            );
        });
        clock.advance(Duration::from_millis(2_999));
        assert!(clock.elapsed() < old.kind.duration());
        clock.advance(Duration::from_millis(1));
        assert_eq!(clock.elapsed(), old.kind.duration());
        let ids = queue.pending_ids();
        assert!(queue.run(ids[1]));
        assert!(queue.run(ids[0]));
        assert!(!queue.run(ids[0]));
        assert_eq!(
            state.lock().unwrap().chrome.toast.as_ref().unwrap().message,
            "replacement"
        );
    }

    #[test]
    fn tasks_can_post_without_locking_the_delivery_queue() {
        let queue = UiTasks::default();
        let app = queue.application();
        let inner = app.clone();
        app.post(move || inner.post(|| {}));
        queue.run_all();
        assert_eq!(queue.pending_ids().len(), 1);
        queue.run_all();
        assert!(queue.pending_ids().is_empty());
    }
}
