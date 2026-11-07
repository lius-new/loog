//! Generic file-work/timer resource; application callbacks own state coordination.
use std::sync::{
    Arc, Weak,
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::time::Duration;
pub trait Request: Clone + Send + 'static {
    fn generation(&self) -> u64;
    fn elapsed(&self) -> Duration;
}
#[derive(Clone)]
pub struct Lease {
    current: Weak<AtomicU64>,
}
impl Lease {
    pub fn is_current(&self, generation: u64) -> bool {
        self.current
            .upgrade()
            .is_some_and(|current| generation != 0 && current.load(Ordering::Acquire) == generation)
    }
}
#[derive(Clone)]
pub struct Worker<R: Request> {
    current: Arc<AtomicU64>,
    sequence: Arc<AtomicU64>,
    work: mpsc::Sender<Option<R>>,
    timer: mpsc::Sender<Option<R>>,
}
impl<R: Request> Worker<R> {
    pub fn new<P: Send + 'static>(
        prepare: impl Fn(&R) -> P + Send + 'static,
        completed: impl Fn(u64, P, Lease) + Send + 'static,
        indicator: impl Fn(u64, Lease) + Send + 'static,
        delay: Duration,
    ) -> Self {
        let current = Arc::new(AtomicU64::new(0));
        let (work, receiver) = mpsc::channel::<Option<R>>();
        let (timer, delays) = mpsc::channel::<Option<R>>();
        let worker_current = current.clone();
        std::thread::Builder::new()
            .name("loog-file-loader".into())
            .spawn(move || {
                while let Ok(Some(mut request)) = receiver.recv() {
                    // Coalesce a burst before doing any more disk/index work.
                    for next in receiver.try_iter() {
                        let Some(next) = next else {
                            return;
                        };
                        request = next;
                    }
                    if worker_current.load(Ordering::Acquire) != request.generation() {
                        continue;
                    }
                    let prepared = prepare(&request);
                    if worker_current.load(Ordering::Acquire) != request.generation() {
                        drop(prepared);
                        continue;
                    }
                    completed(
                        request.generation(),
                        prepared,
                        Lease {
                            current: Arc::downgrade(&worker_current),
                        },
                    );
                }
            })
            .expect("could not start file loader");
        let timer_current = current.clone();
        std::thread::Builder::new()
            .name("loog-file-loading-delay".into())
            .spawn(move || {
                while let Ok(Some(mut request)) = delays.recv() {
                    let mut wait = delay.saturating_sub(request.elapsed());
                    loop {
                        match delays.recv_timeout(wait) {
                            Ok(Some(next)) => {
                                request = next;
                                wait = delay.saturating_sub(request.elapsed());
                            }
                            Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                            Err(mpsc::RecvTimeoutError::Timeout) => {
                                if timer_current.load(Ordering::Acquire) != request.generation() {
                                    break;
                                }
                                let generation = request.generation();
                                indicator(
                                    generation,
                                    Lease {
                                        current: Arc::downgrade(&timer_current),
                                    },
                                );
                                break;
                            }
                        }
                    }
                }
            })
            .expect("could not start loading delay");
        Self {
            current,
            sequence: Arc::new(AtomicU64::new(0)),
            work,
            timer,
        }
    }
    pub fn shutdown(&self) {
        self.current.store(0, Ordering::Release);
        let _ = self.work.send(None);
        let _ = self.timer.send(None);
    }
    pub fn reserve_generation(&self) -> u64 {
        let generation = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        self.current.store(generation, Ordering::Release);
        generation
    }
    pub fn invalidate(&self) {
        self.current.store(0, Ordering::Release);
    }
    pub fn send_work(&self, request: R) -> bool {
        self.work.send(Some(request)).is_ok()
    }
    pub fn send_timer(&self, request: R) {
        let _ = self.timer.send(Some(request));
    }
}
