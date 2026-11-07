//! One-shot timers that run on the UI thread.
//!
//! lgui has no timer API, so a short-lived thread sleeps and then posts the work
//! back to the application. Dropping the returned [`Timer`] cancels it: the
//! thread wakes at once and exits without running anything.
use lgui::ApplicationHandle;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::Duration;

/// A pending timer; dropping it cancels the work.
pub(crate) struct Timer {
    cancel: Sender<()>,
}

impl Drop for Timer {
    fn drop(&mut self) {
        let _ = self.cancel.send(());
    }
}

/// Runs `work` on the UI thread after `delay`, unless the timer is dropped first.
pub(crate) fn after(
    application: ApplicationHandle,
    delay: Duration,
    work: impl FnOnce() + Send + 'static,
) -> Timer {
    let (cancel, cancelled) = mpsc::channel::<()>();
    let spawned = std::thread::Builder::new()
        .name("loog-ui-timer".to_owned())
        .spawn(move || {
            // A message or a closed channel both mean "cancelled".
            if let Err(RecvTimeoutError::Timeout) = cancelled.recv_timeout(delay) {
                application.post(work);
            }
        });
    if spawned.is_err() {
        // No thread, no timer: the work is simply not scheduled.
    }
    Timer { cancel }
}

/// Debounced background work. Cancellation is checked during work and before posting.
pub(crate) fn background_after<T: Send + 'static>(
    application: ApplicationHandle,
    delay: Duration,
    work: impl FnOnce(&dyn Fn() -> bool) -> Option<T> + Send + 'static,
    deliver: impl FnOnce(T) + Send + 'static,
) -> Timer {
    let (cancel, cancelled) = mpsc::channel::<()>();
    let _ = std::thread::Builder::new()
        .name("loog-completion".to_owned())
        .spawn(move || {
            if let Err(RecvTimeoutError::Timeout) = cancelled.recv_timeout(delay) {
                let is_cancelled =
                    || !matches!(cancelled.try_recv(), Err(mpsc::TryRecvError::Empty));
                if let Some(result) = work(&is_cancelled)
                    && !is_cancelled()
                {
                    application.post(move || deliver(result));
                }
            }
        });
    Timer { cancel }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    fn application() -> ApplicationHandle {
        crate::app::test_support::UiTasks::default().application()
    }

    #[test]
    fn a_timer_runs_its_work_after_the_delay() {
        let tasks = crate::app::test_support::UiTasks::default();
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        let _timer = after(tasks.application(), Duration::from_millis(30), move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let limit = Instant::now() + Duration::from_secs(5);
        while count.load(Ordering::SeqCst) == 0 {
            tasks.run_all();
            assert!(Instant::now() < limit, "the timer never fired");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_dropped_timer_never_runs_and_leaves_no_thread_waiting() {
        let tasks = crate::app::test_support::UiTasks::default();
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        let timer = after(tasks.application(), Duration::from_millis(60), move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        drop(timer);
        std::thread::sleep(Duration::from_millis(200));
        tasks.run_all();
        assert_eq!(count.load(Ordering::SeqCst), 0);
        let _ = application();
    }
}
