//! Search task scheduling. Cleanup wakes debounce without joining disk I/O.
#[cfg(test)]
use crate::features::search::ports::SearchSourcePort;
use crate::features::search::{Request, Results};
use crate::infrastructure::ui_dispatch::LatestNotification;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

pub const DEBOUNCE: Duration = Duration::from_millis(200);
type Job = Box<dyn FnOnce() + Send>;
type Wake = Box<dyn FnOnce() + Send>;

pub(crate) trait SearchExecutor {
    fn schedule(&self, delay: Duration, cancel: Arc<AtomicBool>, job: Job) -> Result<Wake, String>;
}
struct ThreadExecutor;
impl SearchExecutor for ThreadExecutor {
    fn schedule(&self, delay: Duration, cancel: Arc<AtomicBool>, job: Job) -> Result<Wake, String> {
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("loog-workspace-search".into())
            .spawn(move || {
                if receiver.recv_timeout(delay).is_ok() || cancel.load(Ordering::Relaxed) {
                    return;
                }
                job();
            })
            .map_err(|error| format!("Could not start search: {error}"))?;
        Ok(Box::new(move || {
            let _ = sender.send(());
        }))
    }
}

pub(crate) struct SearchTicket {
    pub generation: u64,
    cancel: Arc<AtomicBool>,
    wake: Option<Wake>,
}
impl SearchTicket {
    pub(crate) fn stop(mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(wake) = self.wake.take() {
            wake();
        }
    }
}

pub(crate) struct SearchTasks;
impl SearchTasks {
    pub(crate) fn launch(
        generation: u64,
        request: Option<Request>,
        resource: super::resource_search::Request,
        cancel: Arc<AtomicBool>,
        queue: LatestNotification<Result<Results, String>>,
    ) -> SearchTicket {
        let mut wake = None;
        if let Some(request) = request {
            let token = cancel.clone();
            let failure = queue.clone();
            match ThreadExecutor.schedule(
                DEBOUNCE,
                cancel.clone(),
                Box::new(move || {
                    let spec = request.spec.clone();
                    let mut local = Results::default();
                    let result = crate::infrastructure::search_filesystem::run(
                        request,
                        &token,
                        |mut value| {
                            value.complete = false;
                            local = value.clone();
                            queue.send(Ok(value));
                        },
                    )
                    .and_then(|_| {
                        super::resource_search::run(resource, &spec, &token, local, |value| {
                            queue.send(Ok(value))
                        })
                    });
                    if let Err(error) = result {
                        queue.send(Err(error));
                    }
                }),
            ) {
                Ok(signal) => wake = Some(signal),
                Err(error) => failure.send(Err(error)),
            }
        }
        SearchTicket {
            generation,
            cancel,
            wake,
        }
    }
    #[cfg(test)]
    pub(crate) fn launch_with(
        executor: &dyn SearchExecutor,
        source: Arc<dyn SearchSourcePort>,
        generation: u64,
        request: Option<Request>,
        cancel: Arc<AtomicBool>,
        queue: LatestNotification<Result<Results, String>>,
    ) -> SearchTicket {
        let mut wake = None;
        if let Some(request) = request {
            let token = cancel.clone();
            let failure = queue.clone();
            match executor.schedule(
                DEBOUNCE,
                cancel.clone(),
                Box::new(move || {
                    if let Err(error) = crate::features::search::service::run(
                        source.as_ref(),
                        request,
                        &token,
                        |results| queue.send(Ok(results)),
                    ) {
                        queue.send(Err(error));
                    }
                }),
            ) {
                Ok(signal) => wake = Some(signal),
                Err(error) => failure.send(Err(error)),
            }
        }
        SearchTicket {
            generation,
            cancel,
            wake,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::{ManualClock, UiTasks};
    use crate::features::search::{
        model::SearchModel,
        tests::{MemorySource, key, request, result},
    };
    use std::sync::Mutex;

    struct Pending {
        due: Duration,
        cancel: Arc<AtomicBool>,
        job: Job,
    }
    #[derive(Default)]
    struct Executor {
        clock: Mutex<ManualClock>,
        pending: Mutex<Vec<Pending>>,
    }
    impl SearchExecutor for Executor {
        fn schedule(
            &self,
            delay: Duration,
            cancel: Arc<AtomicBool>,
            job: Job,
        ) -> Result<Wake, String> {
            let due = self.clock.lock().unwrap().elapsed() + delay;
            self.pending
                .lock()
                .unwrap()
                .push(Pending { due, cancel, job });
            Ok(Box::new(|| {}))
        }
    }
    impl Executor {
        fn advance(&self, elapsed: Duration) {
            self.clock.lock().unwrap().advance(elapsed);
            let now = self.clock.lock().unwrap().elapsed();
            let jobs = std::mem::take(&mut *self.pending.lock().unwrap());
            for pending in jobs {
                if pending.due > now {
                    self.pending.lock().unwrap().push(pending);
                } else if !pending.cancel.load(Ordering::Relaxed) {
                    (pending.job)();
                }
            }
        }
    }

    #[test]
    fn debounce_uses_controlled_time_and_only_the_latest_ticket_runs() {
        let executor = Executor::default();
        let ui = UiTasks::default();
        let values = Arc::new(Mutex::new(Vec::new()));
        let source = Arc::new(MemorySource::default());
        let start = |generation| {
            let output = values.clone();
            let queue =
                LatestNotification::new(ui.application(), move |value: Result<Results, String>| {
                    output
                        .lock()
                        .unwrap()
                        .push((generation, value.unwrap().matches))
                });
            SearchTasks::launch_with(
                &executor,
                source.clone(),
                generation,
                Some(request()),
                Arc::new(AtomicBool::new(false)),
                queue,
            )
        };
        let old = start(1);
        assert_eq!(old.generation, 1);
        old.stop();
        let latest = start(2);
        assert_eq!(latest.generation, 2);
        executor.advance(Duration::from_millis(199));
        assert!(ui.pending_ids().is_empty());
        executor.advance(Duration::from_millis(1));
        ui.run_all();
        assert_eq!(&*values.lock().unwrap(), &[(2, 1)]);
        latest.stop();
    }

    #[test]
    fn a_late_old_mailbox_cannot_replace_a_new_generation_or_report_an_old_error() {
        let ui = UiTasks::default();
        let expected = key("needle");
        let model = Arc::new(Mutex::new(SearchModel::default()));
        let old = model.lock().unwrap().begin(expected.clone()).generation;
        let latest = model.lock().unwrap().begin(expected.clone()).generation;
        for (generation, value) in [
            (old, Err("old failure".into())),
            (latest, Ok(result("needle"))),
        ] {
            let output = model.clone();
            let expected = expected.clone();
            LatestNotification::new(ui.application(), move |value| {
                output
                    .lock()
                    .unwrap()
                    .apply(generation, &expected, &expected, false, value);
            })
            .send(value);
        }
        let ids = ui.pending_ids();
        ui.run(ids[1]);
        ui.run(ids[0]);
        let model = model.lock().unwrap();
        assert_eq!(model.status().results.matches, 1);
        assert!(model.status().error.is_none());
    }

    #[test]
    fn cancelling_during_debounce_never_scans_or_posts_results() {
        let executor = Executor::default();
        let ui = UiTasks::default();
        let token = Arc::new(AtomicBool::new(false));
        let ticket = SearchTasks::launch_with(
            &executor,
            Arc::new(MemorySource::default()),
            4,
            Some(request()),
            token.clone(),
            LatestNotification::new(ui.application(), |_: Result<Results, String>| {
                panic!("cancelled task published")
            }),
        );
        token.store(true, Ordering::Relaxed);
        executor.advance(DEBOUNCE);
        assert!(ui.pending_ids().is_empty());
        ticket.stop();
    }

    #[test]
    fn executor_failure_is_delivered_as_current_generation_failure() {
        struct Failed;
        impl SearchExecutor for Failed {
            fn schedule(&self, _: Duration, _: Arc<AtomicBool>, _: Job) -> Result<Wake, String> {
                Err("executor unavailable".into())
            }
        }
        let ui = UiTasks::default();
        let errors = Arc::new(Mutex::new(Vec::new()));
        let output = errors.clone();
        let ticket = SearchTasks::launch_with(
            &Failed,
            Arc::new(MemorySource::default()),
            5,
            Some(request()),
            Arc::new(AtomicBool::new(false)),
            LatestNotification::new(ui.application(), move |value: Result<Results, String>| {
                output.lock().unwrap().push(value.unwrap_err())
            }),
        );
        ui.run_all();
        assert_eq!(&*errors.lock().unwrap(), &["executor unavailable"]);
        ticket.stop();
    }
}
