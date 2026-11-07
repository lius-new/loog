//! One coalescing parser worker for document and card consumers. No UI state is
//! locked while parsing. Shared leases account for results retained by frames.
use crate::domain::document::DocumentStamp;
use crate::domain::identity::{FileId, ViewId};
use crate::features::markdown::model::{
    ContentFormat, MarkdownDocument, ParseFailure, ParseLimits, ParseProgress, ParseStop,
    SyntaxOptions,
};
use crate::features::markdown::parser::parse_document;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContentOrigin {
    Document(DocumentStamp),
    Markup { owner: u64, revision: u64 },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum SourceFamily {
    Document(FileId, u64),
    Markup(u64),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ParseKey {
    pub origin: ContentOrigin,
    pub generation: u64,
    pub format: ContentFormat,
    pub syntax: SyntaxOptions,
    pub limits: ParseLimits,
}
impl ParseKey {
    pub fn document_stamp(self) -> Option<DocumentStamp> {
        match self.origin {
            ContentOrigin::Document(stamp) => Some(stamp),
            ContentOrigin::Markup { .. } => None,
        }
    }
    fn family(self) -> SourceFamily {
        match self.origin {
            ContentOrigin::Document(stamp) => {
                SourceFamily::Document(stamp.document, stamp.incarnation)
            }
            ContentOrigin::Markup { owner, .. } => SourceFamily::Markup(owner),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Consumer {
    Document(ViewId),
    Card(u64),
}
#[derive(Clone)]
pub struct Demand {
    pub consumer: Consumer,
    pub key: ParseKey,
    pub source: Arc<String>,
    pub active: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheKind {
    Parse,
    Layout,
    Image,
}
impl CacheKind {
    fn index(self) -> usize {
        match self {
            Self::Parse => 0,
            Self::Layout => 1,
            Self::Image => 2,
        }
    }
}
#[derive(Default)]
struct Accounting {
    used: [usize; 3],
}
#[derive(Clone)]
pub struct MemoryBudget {
    state: Arc<Mutex<Accounting>>,
    total: usize,
    children: [usize; 3],
}
impl Default for MemoryBudget {
    fn default() -> Self {
        Self {
            state: Arc::default(),
            total: 128 * 1024 * 1024,
            children: [32 * 1024 * 1024, 32 * 1024 * 1024, 64 * 1024 * 1024],
        }
    }
}
impl MemoryBudget {
    pub fn reserve(&self, kind: CacheKind, bytes: usize) -> Option<BudgetLease> {
        let mut state = self.state.lock().unwrap();
        let index = kind.index();
        if state.used[index].saturating_add(bytes) > self.children[index]
            || state.used.iter().sum::<usize>().saturating_add(bytes) > self.total
        {
            return None;
        }
        state.used[index] += bytes;
        Some(BudgetLease {
            budget: self.clone(),
            kind,
            bytes,
        })
    }
    pub fn used(&self) -> [usize; 3] {
        self.state.lock().unwrap().used
    }
}
pub struct BudgetLease {
    budget: MemoryBudget,
    kind: CacheKind,
    bytes: usize,
}
impl Drop for BudgetLease {
    fn drop(&mut self) {
        let mut state = self.budget.state.lock().unwrap();
        state.used[self.kind.index()] = state.used[self.kind.index()].saturating_sub(self.bytes);
    }
}
pub struct Publication {
    pub key: ParseKey,
    pub revision: u64,
    pub source: Arc<String>,
    pub outcome: Result<Arc<MarkdownDocument>, ParseFailure>,
    _lease: BudgetLease,
}
#[derive(Clone)]
pub enum ParseStatus {
    Pending,
    Ready(Arc<Publication>),
    Declined(ParseFailure),
    Stopped,
}
struct Cached {
    value: Arc<Publication>,
    used: u64,
}
struct Job {
    key: ParseKey,
    source: Arc<String>,
    active: bool,
    first_waiting: Instant,
    not_before: Instant,
}
#[derive(Default)]
struct Queue {
    targets: HashMap<Consumer, ParseKey>,
    pending: VecDeque<Job>,
    pending_bytes: usize,
    running: Option<(ParseKey, Arc<AtomicBool>)>,
    ready: HashMap<ParseKey, Cached>,
    failures: HashMap<ParseKey, ParseFailure>,
    last_started: HashMap<SourceFamily, Instant>,
    clock: u64,
    revision: u64,
}
struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    alive: AtomicBool,
    budget: MemoryBudget,
    completed: Arc<dyn Fn(ParseKey) + Send + Sync>,
}
struct Owner {
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}
impl Owner {
    fn stop(&self) {
        self.shared.alive.store(false, Ordering::Release);
        let mut queue = self.shared.queue.lock().unwrap();
        if let Some((_, cancel)) = &queue.running {
            cancel.store(true, Ordering::Release);
        }
        let retired = std::mem::take(&mut queue.ready);
        let pending = std::mem::take(&mut queue.pending);
        queue.targets.clear();
        queue.failures.clear();
        queue.pending_bytes = 0;
        drop(queue);
        self.shared.wake.notify_all();
        // The parser constructor cannot be interrupted. Never join from a UI
        // callback; its thread exits after construction observes cancellation.
        self.thread.lock().unwrap().take();
        crate::infrastructure::ui_dispatch::retire((retired, pending));
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.stop();
    }
}
#[derive(Clone)]
pub struct Worker {
    owner: Arc<Owner>,
}
fn failed(reason: ParseStop) -> ParseFailure {
    ParseFailure {
        reason,
        progress: ParseProgress::default(),
    }
}
impl Worker {
    pub fn new(budget: MemoryBudget, completed: impl Fn(ParseKey) + Send + Sync + 'static) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
            alive: AtomicBool::new(true),
            budget,
            completed: Arc::new(completed),
        });
        let task = shared.clone();
        let thread = thread::Builder::new()
            .name("loog-markdown-parse".into())
            .spawn(move || run(task))
            .expect("could not start Markdown parser");
        Self {
            owner: Arc::new(Owner {
                shared,
                thread: Mutex::new(Some(thread)),
            }),
        }
    }
    pub fn shutdown(&self) {
        self.owner.stop();
    }
    pub fn budget(&self) -> MemoryBudget {
        self.owner.shared.budget.clone()
    }
    /// Atomically replace demand, so two views of the same edited document never
    /// expose a partially updated set of versions to the parser thread.
    pub fn set_demands(&self, mut demands: Vec<Demand>) {
        let shared = &self.owner.shared;
        if !shared.alive.load(Ordering::Acquire) {
            return;
        }
        demands.sort_by_key(|d| !d.active);
        demands.truncate(128);
        let mut queue = shared.queue.lock().unwrap();
        if !shared.alive.load(Ordering::Acquire) {
            return;
        }
        queue.targets = demands.iter().map(|d| (d.consumer, d.key)).collect();
        let wanted = queue.targets.values().copied().collect::<HashSet<_>>();
        if let Some((key, cancel)) = &queue.running {
            if !wanted.contains(key) {
                cancel.store(true, Ordering::Release);
            }
        }
        let mut retired = Vec::new();
        let obsolete = queue
            .ready
            .keys()
            .filter(|key| !wanted.contains(*key))
            .copied()
            .collect::<Vec<_>>();
        for key in obsolete {
            if let Some(value) = queue.ready.remove(&key) {
                retired.push(value);
            }
        }
        queue.failures.retain(|key, _| wanted.contains(key));
        let active_keys = demands
            .iter()
            .filter(|d| d.active)
            .map(|d| d.key)
            .collect::<HashSet<_>>();
        let old = std::mem::take(&mut queue.pending);
        let mut first_waiting = HashMap::new();
        let mut discarded = Vec::new();
        queue.pending_bytes = 0;
        for mut job in old {
            job.active = active_keys.contains(&job.key);
            first_waiting
                .entry(job.key.family())
                .or_insert(job.first_waiting);
            if wanted.contains(&job.key) {
                queue.pending_bytes = queue.pending_bytes.saturating_add(job.source.capacity());
                queue.pending.push_back(job);
            } else {
                discarded.push(job);
            }
        }
        let live_documents = wanted
            .iter()
            .map(|key| key.family())
            .collect::<HashSet<_>>();
        queue
            .last_started
            .retain(|id, _| live_documents.contains(id));
        let now = Instant::now();
        for demand in demands {
            if demand.source.len() > demand.key.limits.source_bytes {
                queue
                    .failures
                    .insert(demand.key, failed(ParseStop::SourceBudget));
                continue;
            }
            if queue.ready.contains_key(&demand.key)
                || queue.failures.contains_key(&demand.key)
                || queue
                    .running
                    .as_ref()
                    .is_some_and(|(key, _)| *key == demand.key)
                || queue.pending.iter().any(|job| job.key == demand.key)
            {
                continue;
            }
            if queue.pending.len() >= 64
                || queue.pending_bytes.saturating_add(demand.source.capacity()) > 8 * 1024 * 1024
            {
                queue
                    .failures
                    .insert(demand.key, failed(ParseStop::RetainedBudget));
                continue;
            }
            let first = first_waiting
                .get(&demand.key.family())
                .copied()
                .unwrap_or(now);
            let not_before = if queue.last_started.contains_key(&demand.key.family()) {
                (now + Duration::from_millis(80)).min(first + Duration::from_millis(250))
            } else {
                now
            };
            queue.pending_bytes += demand.source.capacity();
            queue.pending.push_back(Job {
                key: demand.key,
                source: demand.source,
                active: demand.active,
                first_waiting: first,
                not_before,
            });
        }
        queue
            .pending
            .make_contiguous()
            .sort_by_key(|job| !job.active);
        drop(queue);
        shared.wake.notify_one();
        if !retired.is_empty() || !discarded.is_empty() {
            crate::infrastructure::ui_dispatch::retire((retired, discarded));
        }
    }
    pub fn retry(&self, consumer: Consumer) {
        let mut queue = self.owner.shared.queue.lock().unwrap();
        if let Some(key) = queue.targets.get(&consumer).copied() {
            queue.failures.remove(&key);
        }
    }
    pub fn query(&self, consumer: Consumer, key: ParseKey) -> ParseStatus {
        let shared = &self.owner.shared;
        if !shared.alive.load(Ordering::Acquire) {
            return ParseStatus::Stopped;
        }
        let mut queue = shared.queue.lock().unwrap();
        if queue.targets.get(&consumer) != Some(&key) {
            return ParseStatus::Declined(failed(ParseStop::RetainedBudget));
        }
        queue.clock = queue.clock.wrapping_add(1);
        let clock = queue.clock;
        if let Some(cache) = queue.ready.get_mut(&key) {
            cache.used = clock;
            return ParseStatus::Ready(cache.value.clone());
        }
        if let Some(failure) = queue.failures.get(&key) {
            return ParseStatus::Declined(failure.clone());
        }
        ParseStatus::Pending
    }
}

fn run(shared: Arc<Shared>) {
    loop {
        let (job, cancel) = {
            let mut queue = shared.queue.lock().unwrap();
            loop {
                if !shared.alive.load(Ordering::Acquire) {
                    return;
                }
                let now = Instant::now();
                if let Some(index) = queue.pending.iter().position(|job| job.not_before <= now) {
                    let job = queue.pending.remove(index).unwrap();
                    queue.pending_bytes = queue.pending_bytes.saturating_sub(job.source.capacity());
                    let cancel = Arc::new(AtomicBool::new(false));
                    queue.running = Some((job.key, cancel.clone()));
                    queue.last_started.insert(job.key.family(), now);
                    break (job, cancel);
                }
                queue = if let Some(next) = queue.pending.iter().map(|job| job.not_before).min() {
                    shared
                        .wake
                        .wait_timeout(queue, next.saturating_duration_since(now))
                        .unwrap()
                        .0
                } else {
                    shared.wake.wait(queue).unwrap()
                };
            }
        };
        let parsed = parse_document(
            job.source.clone(),
            job.key.format,
            job.key.syntax,
            job.key.limits,
            || cancel.load(Ordering::Acquire) || !shared.alive.load(Ordering::Acquire),
        )
        .map(Arc::new);
        if cancel.load(Ordering::Acquire) || !shared.alive.load(Ordering::Acquire) {
            shared.queue.lock().unwrap().running = None;
            continue;
        }
        let bytes = parsed
            .as_ref()
            .map_or(job.source.capacity(), |document| document.retained_bytes())
            .saturating_add(std::mem::size_of::<Publication>());
        let mut lease = shared.budget.reserve(CacheKind::Parse, bytes);
        // Cache evictions only free the budget when the last frame releases its
        // immutable result. A retained old frame cannot hide memory from the ledger.
        while lease.is_none() {
            let retired = {
                let mut queue = shared.queue.lock().unwrap();
                let oldest = queue
                    .ready
                    .iter()
                    .min_by_key(|(_, entry)| entry.used)
                    .map(|(key, _)| *key);
                oldest.and_then(|key| {
                    let value = queue.ready.remove(&key);
                    if queue.targets.values().any(|wanted| *wanted == key) {
                        queue
                            .failures
                            .insert(key, failed(ParseStop::RetainedBudget));
                    }
                    value
                })
            };
            if retired.is_none() {
                break;
            }
            if let Some(value) = retired {
                let key = value.value.key;
                drop(value);
                if shared.alive.load(Ordering::Acquire) {
                    (shared.completed)(key);
                }
            }
            lease = shared.budget.reserve(CacheKind::Parse, bytes);
        }
        let mut queue = shared.queue.lock().unwrap();
        queue.running = None;
        let current = shared.alive.load(Ordering::Acquire)
            && !cancel.load(Ordering::Acquire)
            && queue.targets.values().any(|key| *key == job.key);
        if !current {
            drop(queue);
            continue;
        }
        if let Some(lease) = lease {
            queue.revision = queue.revision.wrapping_add(1);
            let revision = queue.revision;
            queue.clock = queue.clock.wrapping_add(1);
            let used = queue.clock;
            queue.ready.insert(
                job.key,
                Cached {
                    value: Arc::new(Publication {
                        key: job.key,
                        revision,
                        source: job.source,
                        outcome: parsed,
                        _lease: lease,
                    }),
                    used,
                },
            );
        } else {
            queue
                .failures
                .insert(job.key, failed(ParseStop::RetainedBudget));
        }
        drop(queue);
        if shared.alive.load(Ordering::Acquire) {
            (shared.completed)(job.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    fn key(owner: u64, revision: u64) -> ParseKey {
        ParseKey {
            origin: ContentOrigin::Markup { owner, revision },
            generation: 0,
            format: ContentFormat::Markdown,
            syntax: SyntaxOptions::default(),
            limits: ParseLimits::automatic(),
        }
    }
    fn demand(consumer: Consumer, key: ParseKey, source: &str) -> Demand {
        Demand {
            consumer,
            key,
            source: Arc::new(source.to_owned()),
            active: true,
        }
    }
    fn wait(worker: &Worker, consumer: Consumer, key: ParseKey) -> Arc<Publication> {
        let limit = Instant::now() + Duration::from_secs(5);
        loop {
            if let ParseStatus::Ready(value) = worker.query(consumer, key) {
                return value;
            }
            assert!(Instant::now() < limit, "parse did not publish");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    #[test]
    fn consumers_share_one_immutable_publication_and_held_results_keep_the_lease() {
        let budget = MemoryBudget::default();
        let worker = Worker::new(budget.clone(), |_| {});
        let key = key(2, 1);
        worker.set_demands(vec![
            demand(Consumer::Card(1), key, "# shared"),
            demand(Consumer::Card(2), key, "# shared"),
        ]);
        let a = wait(&worker, Consumer::Card(1), key);
        let b = wait(&worker, Consumer::Card(2), key);
        assert!(Arc::ptr_eq(&a, &b));
        worker.set_demands(vec![demand(Consumer::Card(2), key, "# shared")]);
        assert!(matches!(
            worker.query(Consumer::Card(1), key),
            ParseStatus::Declined(_)
        ));
        assert!(Arc::ptr_eq(&b, &wait(&worker, Consumer::Card(2), key)));
        worker.shutdown();
        assert!(budget.used()[0] > 0);
        drop(a);
        drop(b);
        let limit = Instant::now() + Duration::from_secs(2);
        while budget.used()[0] > 0 {
            assert!(Instant::now() < limit, "publication lease leaked");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    #[test]
    fn superseded_work_never_publishes_over_the_latest_key() {
        let worker = Worker::new(MemoryBudget::default(), |_| {});
        let old = key(4, 1);
        let latest = key(4, 2);
        worker.set_demands(vec![demand(
            Consumer::Card(4),
            old,
            &"old **content**\n\n".repeat(15_000),
        )]);
        worker.set_demands(vec![demand(Consumer::Card(4), latest, "latest 中文")]);
        let published = wait(&worker, Consumer::Card(4), latest);
        assert_eq!(published.source.as_str(), "latest 中文");
        assert!(matches!(
            worker.query(Consumer::Card(4), old),
            ParseStatus::Declined(_)
        ));
        worker.shutdown();
        assert!(matches!(
            worker.query(Consumer::Card(4), latest),
            ParseStatus::Stopped
        ));
    }
    #[test]
    fn parent_and_child_limits_count_outstanding_leases() {
        let budget = MemoryBudget {
            state: Arc::default(),
            total: 16,
            children: [10, 10, 10],
        };
        let parse = budget.reserve(CacheKind::Parse, 10).unwrap();
        assert!(budget.reserve(CacheKind::Layout, 7).is_none());
        let layout = budget.reserve(CacheKind::Layout, 6).unwrap();
        assert!(budget.reserve(CacheKind::Image, 1).is_none());
        drop(parse);
        let image = budget.reserve(CacheKind::Image, 10).unwrap();
        assert_eq!(budget.used(), [0, 6, 10]);
        drop((layout, image));
        assert_eq!(budget.used(), [0, 0, 0]);
    }
}
