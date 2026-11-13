//! Shared request ownership, aggregation and cancellation for every provider.
//! Transports adapt into this module; the editor does not own a transport handle.
use super::{Feature, Merge, PositionEncoding, Stamp};
use loog_workspace_api::ResourceRef;
use serde_json::Value;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub(crate) const EXTERNAL_RANK: usize = 1000;
pub(crate) const EXTERNAL_SOURCE: u64 = 1 << 63;
pub(crate) type Cancellation = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Debug)]
pub(crate) struct RequestSpec {
    pub feature: Feature,
    pub resource: ResourceRef,
    pub stamp: Stamp,
    pub start: usize,
    pub end: usize,
    pub extra: Value,
    pub target: Option<u64>,
    pub server: Option<String>,
    pub timeout: Option<Duration>,
}
impl RequestSpec {
    pub fn new(
        feature: Feature,
        resource: impl Into<ResourceRef>,
        stamp: Stamp,
        offset: usize,
    ) -> Self {
        Self {
            feature,
            resource: resource.into(),
            stamp,
            start: offset,
            end: offset,
            extra: Value::Null,
            target: None,
            server: None,
            timeout: None,
        }
    }
    pub fn range(mut self, end: usize) -> Self {
        self.end = end;
        self
    }
    pub fn extra(mut self, extra: Value) -> Self {
        self.extra = extra;
        self
    }
    pub fn target(mut self, source: u64) -> Self {
        self.target = Some(source);
        self
    }
    pub fn server(mut self, name: impl Into<String>) -> Self {
        self.server = Some(name.into());
        self
    }
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Part {
    pub instance: u64,
    pub server: Arc<str>,
    pub rank: usize,
    pub encoding: PositionEncoding,
    pub value: Value,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    Stale,
    Unsupported,
    Busy,
    Failed(String),
}
#[derive(Clone, Debug, Default)]
pub(crate) struct Progress {
    pub parts: Vec<Arc<Part>>,
    pub finished: bool,
    pub failure: Option<Failure>,
}
impl Progress {
    pub fn usable(&self, feature: Feature) -> Vec<Arc<Part>> {
        match feature.merge() {
            Merge::First => self
                .parts
                .iter()
                .find(|p| !is_empty(&p.value))
                .cloned()
                .into_iter()
                .collect(),
            _ => self.parts.clone(),
        }
    }
}
fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        Value::String(s) => s.is_empty(),
        _ => false,
    }
}
#[derive(Default)]
struct Answers {
    parts: Vec<Arc<Part>>,
    pending: usize,
    deadline: Option<Instant>,
    failure: Option<String>,
}
#[derive(Clone)]
pub(crate) struct PartSink {
    answers: Arc<Mutex<Answers>>,
    closed: Arc<AtomicBool>,
}
impl PartSink {
    pub fn fail(&self, message: String) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let mut answers = self.answers.lock().unwrap();
        if answers.pending == 0 || answers.deadline.is_some_and(|d| Instant::now() >= d) {
            return;
        }
        answers.pending -= 1;
        answers.failure.get_or_insert(message);
    }
    pub fn is_live(&self) -> bool {
        !self.closed.load(Ordering::Acquire)
            && self
                .answers
                .lock()
                .unwrap()
                .deadline
                .is_none_or(|deadline| Instant::now() < deadline)
    }
    pub fn deliver(&self, part: Option<Part>) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let mut answers = self.answers.lock().unwrap();
        if answers.pending == 0 || answers.deadline.is_some_and(|d| Instant::now() >= d) {
            return;
        }
        answers.pending -= 1;
        if let Some(part) = part {
            answers.parts.push(Arc::new(part));
            answers.parts.sort_by_key(|p| p.rank);
        }
    }
}
pub(crate) struct RequestHandle {
    shared: Arc<Mutex<Progress>>,
    answers: Arc<Mutex<Answers>>,
    closed: Arc<AtomicBool>,
    cancel: Mutex<Vec<Cancellation>>,
}
impl RequestHandle {
    /// A request with no transport, suitable for extension-only operation.
    pub fn standalone() -> Self {
        Self::from_native(
            Arc::new(Mutex::new(Progress {
                finished: true,
                failure: Some(Failure::Unsupported),
                parts: vec![],
            })),
            || {},
        )
    }
    pub fn from_native(
        shared: Arc<Mutex<Progress>>,
        cancel: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self {
            shared,
            answers: Arc::default(),
            closed: Arc::default(),
            cancel: Mutex::new(vec![Arc::new(cancel)]),
        }
    }
    pub fn on_cancel(&self, cancel: Cancellation) {
        let mut callbacks = self.cancel.lock().unwrap();
        if !self.closed.load(Ordering::Acquire) {
            callbacks.push(cancel);
            return;
        }
        drop(callbacks);
        cancel();
    }
    pub fn poll(&self) -> Progress {
        if self.closed.load(Ordering::Acquire) {
            return Progress {
                finished: true,
                failure: Some(Failure::Failed("request cancelled".into())),
                parts: vec![],
            };
        }
        let mut progress = self.shared.lock().unwrap().clone();
        let answers = self.answers.lock().unwrap();
        progress.parts.extend(answers.parts.iter().cloned());
        progress.parts.sort_by_key(|p| p.rank);
        if answers.pending > 0 && answers.deadline.is_some_and(|d| Instant::now() < d) {
            progress.finished = false;
        }
        if !progress.parts.is_empty() {
            progress.failure = None;
        } else if let Some(message) = &answers.failure {
            progress.failure = Some(Failure::Failed(message.clone()));
        }
        progress
    }
    pub fn is_finished(&self) -> bool {
        self.poll().finished
    }
    pub fn expect_external(&self, count: usize, timeout: Duration) -> PartSink {
        let mut answers = self.answers.lock().unwrap();
        answers.pending += count;
        answers.deadline = Some(Instant::now() + timeout);
        PartSink {
            answers: self.answers.clone(),
            closed: self.closed.clone(),
        }
    }
    pub fn cancel(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let callbacks = std::mem::take(&mut *self.cancel.lock().unwrap());
        for cancel in callbacks {
            cancel();
        }
    }
}
impl Drop for RequestHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_external_failures_complete_without_fabricating_success_parts() {
        let handle = RequestHandle::standalone();
        let sink = handle.expect_external(1, Duration::from_secs(1));
        sink.fail("command rejected".into());
        let progress = handle.poll();
        assert!(progress.finished && progress.parts.is_empty());
        assert!(
            matches!(progress.failure, Some(Failure::Failed(ref message)) if message == "command rejected")
        );
        sink.deliver(Some(Part {
            instance: 1,
            server: "late".into(),
            rank: 0,
            encoding: PositionEncoding::Utf8,
            value: Value::Null,
        }));
        assert!(handle.poll().parts.is_empty());
    }
    #[test]
    fn extensions_answer_without_a_transport_and_dropped_requests_cancel_once() {
        let handle = RequestHandle::standalone();
        let sink = handle.expect_external(1, Duration::from_secs(1));
        assert!(!handle.poll().finished);
        sink.deliver(Some(Part {
            instance: EXTERNAL_SOURCE | 1,
            server: "plugin".into(),
            rank: 1,
            encoding: PositionEncoding::Utf8,
            value: Value::String("answer".into()),
        }));
        assert!(handle.poll().finished);
        assert!(handle.poll().failure.is_none());
        assert_eq!(handle.poll().parts.len(), 1);
        let cancelled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = cancelled.clone();
        handle.on_cancel(Arc::new(move || {
            observed.fetch_add(1, Ordering::Relaxed);
        }));
        handle.cancel();
        handle.cancel();
        drop(handle);
        assert_eq!(cancelled.load(Ordering::Relaxed), 1);
        assert!(!sink.is_live());
        sink.deliver(None);
    }
}
