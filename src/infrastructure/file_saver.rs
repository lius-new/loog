//! Bounded FIFO disk worker. Callers supply work and post their own UI results.
use std::sync::mpsc::{self, SyncSender};

pub(crate) type Task = Box<dyn FnOnce() + Send + 'static>;

pub(crate) struct Worker {
    send: SyncSender<Task>,
}

impl Worker {
    pub(crate) fn new() -> std::io::Result<Self> {
        let (send, receive) = mpsc::sync_channel::<Task>(8);
        std::thread::Builder::new()
            .name("loog-file-saver".into())
            .spawn(move || {
                for task in receive {
                    task();
                }
            })?;
        Ok(Self { send })
    }

    pub(crate) fn submit(&self, task: Task) -> Result<(), String> {
        self.send
            .try_send(task)
            .map_err(|_| "File save queue is busy. Please try again.".into())
    }
}
