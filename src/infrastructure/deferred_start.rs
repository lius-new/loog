//! Starts an owned background loop on its first request instead of while idle.
use std::sync::Mutex;

pub(crate) struct DeferredStart {
    pending: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl DeferredStart {
    pub(crate) fn new(start: impl FnOnce() + Send + 'static) -> Self {
        Self {
            pending: Mutex::new(Some(Box::new(start))),
        }
    }

    pub(crate) fn start(&self) {
        let pending = self.pending.lock().unwrap().take();
        if let Some(start) = pending {
            start();
        }
    }

    pub(crate) fn cancel(&self) {
        // Dropping an unstarted loop also drops its receiver. Pending callers
        // then observe a disconnected channel exactly as after worker shutdown.
        let pending = self.pending.lock().unwrap().take();
        drop(pending);
    }
}
