//! Cold compilation must not delay running guests, process IO or their timers.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};

const MAX_LOADS: usize = 8;
const MAX_BYTES: usize = 128 * 1024 * 1024;

pub(super) struct Request {
    pub id: String,
    pub generation: u64,
    pub component: Vec<u8>,
    pub package_dir: PathBuf,
    pub version: String,
    pub cache: PathBuf,
    pub limits: Limits,
    pub storage: Option<PathBuf>,
    pub files: bool,
    pub domains: Vec<String>,
    pub cancelled: Arc<AtomicBool>,
}

pub struct Completed {
    pub(super) request: Request,
    pub(super) code: Result<loog_extension_host::aot::LoadedCode, ExtensionError>,
    _permit: Permit,
}
impl std::fmt::Debug for Completed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Completed")
            .field("id", &self.request.id)
            .field("generation", &self.request.generation)
            .finish_non_exhaustive()
    }
}

struct Permit {
    budget: Arc<Mutex<(usize, usize)>>,
    bytes: usize,
}
impl Drop for Permit {
    fn drop(&mut self) {
        let mut budget = self.budget.lock().unwrap();
        budget.0 -= 1;
        budget.1 -= self.bytes;
    }
}

pub(super) struct Loader {
    queue: Option<SyncSender<(Request, Permit)>>,
    budget: Arc<Mutex<(usize, usize)>>,
    thread: Option<JoinHandle<()>>,
}
impl Loader {
    pub fn new(
        runtime: Arc<Runtime>,
        replies: Sender<Job>,
        stopped: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        Self::with_hook(runtime, replies, stopped, |_| {})
    }

    fn with_hook(
        runtime: Arc<Runtime>,
        replies: Sender<Job>,
        stopped: Arc<AtomicBool>,
        before: impl Fn(&Request) + Send + 'static,
    ) -> Result<Self, String> {
        let (queue, work) = sync_channel::<(Request, Permit)>(MAX_LOADS);
        let thread = std::thread::Builder::new()
            .name("loog-component-loader".into())
            .spawn(move || {
                while let Ok((mut request, permit)) = work.recv() {
                    if stopped.load(Ordering::Acquire) || request.cancelled.load(Ordering::Acquire)
                    {
                        continue;
                    }
                    before(&request);
                    let bytes = std::mem::take(&mut request.component);
                    let code = runtime.compile_package(
                        &bytes,
                        &request.cache,
                        &request.package_dir,
                        &request.id,
                        &request.version,
                    );
                    drop(bytes);
                    if stopped.load(Ordering::Acquire) || request.cancelled.load(Ordering::Acquire)
                    {
                        continue;
                    }
                    if replies
                        .send(Job::ComponentLoaded(Box::new(Completed {
                            request,
                            code,
                            _permit: permit,
                        })))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|error| format!("could not start component loader: {error}"))?;
        Ok(Self {
            queue: Some(queue),
            budget: Arc::default(),
            thread: Some(thread),
        })
    }

    pub fn submit(&self, request: Request) -> Result<(), ExtensionError> {
        let bytes = request.component.len();
        {
            let mut budget = self.budget.lock().unwrap();
            if budget.0 >= MAX_LOADS || bytes > MAX_BYTES.saturating_sub(budget.1) {
                return Err(ExtensionError::new(
                    ErrorKind::LimitExceeded,
                    "component loading budget exceeded; retry after pending loads finish",
                ));
            }
            budget.0 += 1;
            budget.1 += bytes;
        }
        let permit = Permit {
            budget: self.budget.clone(),
            bytes,
        };
        self.queue
            .as_ref()
            .unwrap()
            .try_send((request, permit))
            .map_err(|_| {
                ExtensionError::new(ErrorKind::LimitExceeded, "component loader unavailable")
            })
    }
}
impl Drop for Loader {
    fn drop(&mut self) {
        self.queue.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn request(generation: u64) -> Request {
        Request {
            id: "example.slow".into(),
            generation,
            component: vec![],
            package_dir: PathBuf::new(),
            version: "1.0.0".into(),
            cache: PathBuf::new(),
            limits: Limits::default(),
            storage: None,
            files: false,
            domains: vec![],
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn slow_compilation_keeps_dispatch_alive_and_stop_rejects_its_completion() {
        let runtime = Arc::new(Runtime::new().unwrap());
        let stopped = Arc::new(AtomicBool::new(false));
        let (jobs, inbox) = channel();
        let (events, observed) = channel();
        let (entered, waiting) = channel();
        let (release, gate) = channel();
        let loader = Loader::with_hook(
            runtime.clone(),
            jobs.clone(),
            stopped.clone(),
            move |request| {
                if request.generation == 1 {
                    entered.send(()).unwrap();
                    gate.recv_timeout(Duration::from_secs(5)).unwrap();
                }
            },
        )
        .unwrap();
        let mut worker = Worker {
            workspace_wake: None,
            stopped: stopped.clone(),
            runtime,
            replies: jobs.clone(),
            loader: Some(loader),
            loading: HashMap::new(),
            sink: Box::new(move |event| {
                let _ = events.send(event);
            }),
            instances: HashMap::new(),
            inbox,
        };
        worker.load(request(1));
        let thread = std::thread::spawn(move || worker.run());
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        jobs.send(Job::Stop {
            id: "example.slow".into(),
        })
        .unwrap();
        jobs.send(Job::WorkspaceWakeAfter { milliseconds: 0 })
            .unwrap();
        let responsive = matches!(
            observed.recv_timeout(Duration::from_secs(2)),
            Ok(Event::WorkspaceWake)
        );
        release.send(()).unwrap();
        let request = request(2);
        jobs.send(Job::Load {
            id: request.id,
            generation: 2,
            component: vec![],
            package_dir: request.package_dir,
            version: request.version,
            cache: request.cache,
            limits: request.limits,
            storage: None,
            files: false,
            domains: vec![],
        })
        .unwrap();
        let next = observed.recv_timeout(Duration::from_secs(5));
        stopped.store(true, Ordering::Release);
        jobs.send(Job::Shutdown).unwrap();
        thread.join().unwrap();
        assert!(
            responsive,
            "executor must service timers while compilation is blocked"
        );
        assert!(
            matches!(
                next,
                Ok(Event::Loaded {
                    generation: 2,
                    result: Err(_),
                    ..
                })
            ),
            "a stopped generation must never publish its late load"
        );
        assert!(observed.try_recv().is_err());
    }

    #[test]
    fn pending_compilation_is_bounded_and_shutdown_releases_queued_work() {
        let runtime = Arc::new(Runtime::new().unwrap());
        let stopped = Arc::new(AtomicBool::new(false));
        let (jobs, inbox) = channel();
        let (release, gate) = channel();
        let (entered, waiting) = channel();
        let loader = Loader::with_hook(runtime, jobs, stopped.clone(), move |_| {
            entered.send(()).unwrap();
            gate.recv_timeout(Duration::from_secs(5)).unwrap();
        })
        .unwrap();
        loader.submit(request(1)).unwrap();
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        for generation in 2..=MAX_LOADS as u64 {
            loader.submit(request(generation)).unwrap();
        }
        assert_eq!(
            loader.submit(request(9)).unwrap_err().kind,
            ErrorKind::LimitExceeded
        );
        let budget = loader.budget.clone();
        stopped.store(true, Ordering::Release);
        release.send(()).unwrap();
        drop(loader);
        assert_eq!(*budget.lock().unwrap(), (0, 0));
        assert!(inbox.try_recv().is_err());
    }
}
