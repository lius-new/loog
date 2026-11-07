//! Worker-to-UI notifications. Workers never acquire reactive UI state locks
//! while processing Git output, and a slow UI keeps only the latest scan.

use lgui::ApplicationHandle;
use std::sync::{Arc, Mutex, OnceLock, mpsc};

struct Pending<T> {
    latest: Option<T>,
    scheduled: bool,
}

pub struct LatestNotification<T> {
    pending: Arc<Mutex<Pending<T>>>,
    application: ApplicationHandle,
    apply: Arc<dyn Fn(T) + Send + Sync>,
}

impl<T> Clone for LatestNotification<T> {
    fn clone(&self) -> Self {
        Self {
            pending: self.pending.clone(),
            application: self.application.clone(),
            apply: self.apply.clone(),
        }
    }
}

impl<T: Send + 'static> LatestNotification<T> {
    pub fn new(application: ApplicationHandle, apply: impl Fn(T) + Send + Sync + 'static) -> Self {
        Self {
            pending: Arc::new(Mutex::new(Pending {
                latest: None,
                scheduled: false,
            })),
            application,
            apply: Arc::new(apply),
        }
    }

    pub fn send(&self, next: T) {
        let (schedule, displaced) = {
            let mut pending = self.pending.lock().unwrap();
            let displaced = pending.latest.replace(next);
            let schedule = !pending.scheduled;
            pending.scheduled = true;
            (schedule, displaced)
        };
        // Free superseded large snapshots on the producer, outside the lock.
        drop(displaced);
        if schedule {
            let pending = self.pending.clone();
            let apply = self.apply.clone();
            self.application.post(move || {
                let next = {
                    let mut pending = pending.lock().unwrap();
                    pending.scheduled = false;
                    pending.latest.take()
                };
                if let Some(next) = next {
                    apply(next);
                }
            });
        }
    }
}

/// Large old snapshots and diff rows must also be freed off the event loop.
pub fn retire(value: impl Send + 'static) {
    static SENDER: OnceLock<mpsc::Sender<Box<dyn Send>>> = OnceLock::new();
    let sender = SENDER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<Box<dyn Send>>();
        std::thread::Builder::new()
            .name("loog-state-reclaimer".into())
            .spawn(move || {
                for value in receiver {
                    drop(value);
                }
            })
            .expect("could not start state reclaimer");
        sender
    });
    let _ = sender.send(Box::new(value));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    type ApplicationTask = Box<dyn FnOnce() + Send + 'static>;

    #[test]
    fn burst_keeps_one_task_and_delivers_only_the_latest_value() {
        let tasks = Arc::new(Mutex::new(Vec::<ApplicationTask>::new()));
        let posted = tasks.clone();
        let application =
            ApplicationHandle::new(move |task| posted.lock().unwrap().push(task), || {});
        let value = Arc::new(AtomicUsize::new(0));
        let applied = value.clone();
        let queue = LatestNotification::new(application, move |next| {
            applied.store(next, Ordering::Release);
        });
        for next in 1..=10_000 {
            queue.send(next);
        }
        assert_eq!(value.load(Ordering::Acquire), 0); // No synchronous UI callback.
        assert_eq!(tasks.lock().unwrap().len(), 1);
        let task = tasks.lock().unwrap().pop().unwrap();
        task();
        assert_eq!(value.load(Ordering::Acquire), 10_000);
        queue.send(10_001);
        let task = tasks.lock().unwrap().pop().unwrap();
        task();
        assert_eq!(value.load(Ordering::Acquire), 10_001);
    }
}
