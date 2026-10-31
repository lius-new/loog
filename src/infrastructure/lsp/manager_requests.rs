//! Requests the application makes of the servers while the user works: hover,
//! go to definition, completion and the rest.
//!
//! The application asks with a [`RequestSpec`] and gets a [`RequestHandle`] it
//! can poll for the answers of every server that can reply. Only the document's
//! text at the stamp the request names is ever asked about; if the coordinator
//! no longer holds that state, the request is stale and nothing is sent. A newer
//! request of the same kind for the same document replaces an older one, and
//! dropping the handle cancels the request.
use super::{Coordinator, Msg, Phase, Running};
use crate::features::language_service::message::Id;
use crate::features::language_service::reply::{Span, WorkspaceEdit};
use crate::features::language_service::{
    capabilities::PositionEncoding,
    message::{CONTENT_MODIFIED, REQUEST_CANCELLED, ResponseError},
    position::LineIndex,
    request::{self, Feature, Merge},
    sync::Stamp,
    uri,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc::Sender},
    time::{Duration, Instant},
};

/// The way to answer a request the server made of the application.
#[derive(Clone)]
pub(crate) struct Reply {
    tx: Sender<Msg>,
    instance: u64,
    run: u64,
    id: Id,
}

impl Reply {
    pub fn send(&self, outcome: Result<Value, ResponseError>) {
        let _ = self.tx.send(Msg::Respond {
            instance: self.instance,
            run: self.run,
            id: self.id.clone(),
            outcome,
        });
    }
}

/// Something a server asked the application to do.
pub(crate) enum ServerAction {
    /// `workspace/applyEdit`; answer with `{ "applied": bool }`.
    ApplyEdit {
        label: Option<String>,
        edit: WorkspaceEdit,
        reply: Reply,
        expected: Vec<(PathBuf, Stamp)>,
    },
    /// `window/showDocument`; answer with `{ "success": bool }`.
    ShowDocument {
        uri: String,
        selection: Option<Span>,
        reply: Reply,
    },
}

/// Tags at or above this belong to requests made through this channel; lower
/// ones are the coordinator's own (semantic tokens).
pub(super) const TAG_BASE: u64 = 1 << 40;
/// Requests in flight at once, over all documents.
const MAX_LIVE: usize = 32;
/// How long past the servers' own limit a request may linger before it is
/// finished by force.
const GRACE: Duration = Duration::from_millis(500);

#[derive(Clone, Debug)]
pub(crate) struct RequestSpec {
    pub feature: Feature,
    /// The document the request is about; ignored by requests about the workspace.
    pub path: PathBuf,
    /// The state of the document the offsets below are in.
    pub stamp: Stamp,
    /// Byte offsets into the document at `stamp`.
    pub start: usize,
    pub end: usize,
    /// See [`request::params`].
    pub extra: Value,
    /// For a request about an earlier answer: the instance that gave it.
    pub target: Option<u64>,
    /// Only the servers of this name may answer (the person's chosen formatter).
    pub server: Option<String>,
    /// How long to wait, if not the usual.
    pub timeout: Option<Duration>,
}

impl RequestSpec {
    pub fn new(feature: Feature, path: PathBuf, stamp: Stamp, offset: usize) -> Self {
        Self {
            feature,
            path,
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

    pub fn server(mut self, name: impl Into<String>) -> Self {
        self.server = Some(name.into());
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn target(mut self, instance: u64) -> Self {
        self.target = Some(instance);
        self
    }
}

/// One server's answer.
#[derive(Clone, Debug)]
pub(crate) struct Part {
    pub instance: u64,
    pub server: Arc<str>,
    /// The server's place in the listing; lower is preferred.
    pub rank: usize,
    pub encoding: PositionEncoding,
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The document is no longer in the state the request asked about.
    Stale,
    /// No running server can answer.
    Unsupported,
    /// Too many requests are waiting.
    Busy,
    Failed(String),
}

/// What has come back so far.
#[derive(Clone, Debug, Default)]
pub(crate) struct Progress {
    /// In the servers' listing order.
    pub parts: Vec<Arc<Part>>,
    /// Every server has answered, failed or been given up on.
    pub finished: bool,
    /// Why there is nothing, when the request finished without any part.
    pub failure: Option<Failure>,
}

impl Progress {
    /// The answers to use for `feature`: all of them, or the first non-empty one.
    pub fn usable(&self, feature: Feature) -> Vec<Arc<Part>> {
        match feature.merge() {
            Merge::First => self
                .parts
                .iter()
                .find(|part| !request::is_empty(&part.value))
                .cloned()
                .into_iter()
                .collect(),
            _ => self.parts.clone(),
        }
    }
}

/// The place of answers from extensions in the listing of parts: after every server.
pub(crate) const EXTERNAL_RANK: usize = 1000;

/// Answers that do not come from a language server (extension providers). They
/// are kept apart from the servers' parts, which the coordinator replaces as a
/// whole, and joined with them when the application polls.
#[derive(Default)]
struct External {
    parts: Vec<Arc<Part>>,
    /// Answers promised and not yet delivered.
    pending: usize,
    /// After this, a promised answer that never came is given up on.
    give_up_at: Option<Instant>,
}

/// Where an extension's answer goes. Each promised answer is delivered once,
/// with `None` when there is nothing to show; a late or lost one is covered by
/// the handle's own deadline.
#[derive(Clone)]
pub(crate) struct PartSink {
    external: Arc<Mutex<External>>,
}

impl PartSink {
    pub fn deliver(&self, part: Option<Part>) {
        let mut external = self.external.lock().unwrap();
        external.pending = external.pending.saturating_sub(1);
        if let Some(part) = part {
            external.parts.push(Arc::new(part));
            external.parts.sort_by_key(|part| part.rank);
        }
    }
}

pub(crate) struct RequestHandle {
    id: u64,
    shared: Arc<Mutex<Progress>>,
    external: Arc<Mutex<External>>,
    tx: Sender<Msg>,
}

impl RequestHandle {
    pub fn poll(&self) -> Progress {
        let mut progress = self.shared.lock().unwrap().clone();
        let external = self.external.lock().unwrap();
        progress.parts.extend(external.parts.iter().cloned());
        progress.parts.sort_by_key(|part| part.rank);
        if external.pending > 0 && external.give_up_at.is_some_and(|at| Instant::now() < at) {
            progress.finished = false;
        }
        if !progress.parts.is_empty() {
            progress.failure = None;
        }
        progress
    }

    pub fn is_finished(&self) -> bool {
        let external = self.external.lock().unwrap();
        let waiting =
            external.pending > 0 && external.give_up_at.is_some_and(|at| Instant::now() < at);
        !waiting && self.shared.lock().unwrap().finished
    }

    /// Promises `count` answers from outside the servers, to be delivered through
    /// the returned sink within `timeout`.
    pub fn expect_external(&self, count: usize, timeout: Duration) -> PartSink {
        let mut external = self.external.lock().unwrap();
        external.pending += count;
        external.give_up_at = Some(Instant::now() + timeout);
        PartSink {
            external: self.external.clone(),
        }
    }

    pub fn cancel(&self) {
        let _ = self.tx.send(Msg::Cancel(self.id));
    }
}

impl Drop for RequestHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl super::Manager {
    /// What servers have asked of the application since the last call.
    pub fn take_actions(&self) -> Vec<ServerAction> {
        std::mem::take(&mut *self.actions.lock().unwrap())
    }

    /// Starts a request. `on_change` runs when the first and each later answer
    /// arrives.
    pub fn request(&self, spec: RequestSpec) -> RequestHandle {
        let id = self
            .next_request
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let shared = Arc::new(Mutex::new(Progress::default()));
        let _ = self.tx.send(Msg::Request {
            id,
            spec: Box::new(spec),
            shared: shared.clone(),
        });
        RequestHandle {
            id,
            shared,
            external: Arc::default(),
            tx: self.tx.clone(),
        }
    }
}

struct Live {
    spec: RequestSpec,
    shared: Arc<Mutex<Progress>>,
    /// Requests of the same kind for the same document replace each other.
    replaces: Option<(Feature, PathBuf)>,
    /// Tag to the instance it was sent to.
    waiting: HashMap<u64, u64>,
    attempts: HashMap<u64, Attempt>,
    parts: Vec<Arc<Part>>,
    stale: bool,
    errors: Vec<String>,
    deadline: Instant,
}

struct Attempt {
    params: Value,
    run: u64,
    retried: bool,
}

#[derive(Default)]
pub(super) struct Book {
    live: HashMap<u64, Live>,
    /// Tag to the request it belongs to.
    tags: HashMap<u64, u64>,
    next_tag: u64,
}

impl Book {
    pub fn deadline(&self) -> Option<Instant> {
        self.live.values().map(|live| live.deadline).min()
    }
}

fn is_stale(error: &ResponseError) -> bool {
    // -32802 is ServerCancelled: the server dropped the work because the text moved.
    error.code == CONTENT_MODIFIED || error.code == -32802
}

impl Coordinator {
    /// A capability transaction invalidates outstanding interactive answers from
    /// this instance. Static providers remain available for subsequent requests.
    pub(super) fn registration_changed(&mut self, at: usize) {
        self.instances[at].registration_revision =
            self.instances[at].registration_revision.wrapping_add(1);
        let instance = &self.instances[at];
        let mut affected = Vec::new();
        for (id, live) in &mut self.book.live {
            let tags = live
                .waiting
                .iter()
                .filter(|(_, server)| **server == instance.id)
                .map(|(tag, _)| *tag)
                .collect::<Vec<_>>();
            let before = live.parts.len();
            live.parts.retain(|part| part.instance != instance.id);
            if !tags.is_empty() || before != live.parts.len() {
                for tag in tags {
                    live.waiting.remove(&tag);
                    live.attempts.remove(&tag);
                    self.book.tags.remove(&tag);
                    if let Phase::Running(running) = &instance.phase {
                        running.client.cancel(tag);
                    }
                }
                live.stale = true;
                affected.push(*id);
            }
        }
        for id in affected {
            self.settle(id);
        }
        self.results_changed = true;
    }

    pub(super) fn request(&mut self, id: u64, spec: RequestSpec, shared: Arc<Mutex<Progress>>) {
        let key = spec
            .feature
            .replaces_older()
            .then(|| (spec.feature, spec.path.clone()));
        if let Some(key) = &key {
            let older: Vec<u64> = self
                .book
                .live
                .iter()
                .filter(|(_, live)| live.replaces.as_ref() == Some(key))
                .map(|(id, _)| *id)
                .collect();
            for old in older {
                self.cancel_request(old);
            }
        }
        if self.book.live.len() >= MAX_LIVE {
            finish(&shared, Vec::new(), Some(Failure::Busy));
            return;
        }
        let document_uri = if spec.feature.about_document() {
            let Some(document_uri) = uri::file_uri(&spec.path) else {
                finish(&shared, Vec::new(), Some(Failure::Unsupported));
                return;
            };
            Some(document_uri)
        } else {
            None
        };
        let document_key = document_uri.as_deref().map(uri::key);

        struct Target {
            at: usize,
            rank: usize,
            start: crate::features::language_service::position::Position,
            end: crate::features::language_service::position::Position,
        }
        let mut targets: Vec<Target> = Vec::new();
        let mut stale = false;
        for (at, instance) in self.instances.iter().enumerate() {
            let Phase::Running(Running {
                ready: Some(ready), ..
            }) = &instance.phase
            else {
                continue;
            };
            let selected_key = uri::file_uri(&spec.path).map(|uri| uri::key(&uri));
            let selected = instance.documents.iter().find(|document| {
                selected_key
                    .as_ref()
                    .is_some_and(|key| uri::key(&document.uri) == *key)
            });
            if !spec.feature.supported(
                &ready
                    .registrations
                    .effective(&ready.capabilities.providers, selected),
            ) {
                continue;
            }
            if spec.target.is_some_and(|target| target != instance.id) {
                continue;
            }
            if spec
                .server
                .as_deref()
                .is_some_and(|name| name != &*instance.key.server)
            {
                continue;
            }
            if spec.feature.merge() == Merge::Target && spec.target.is_none() {
                continue;
            }
            let encoding = ready.capabilities.position_encoding;
            let (start, end) = match &document_key {
                Some(key) => {
                    let Some(document) =
                        instance.documents.iter().find(|d| uri::key(&d.uri) == *key)
                    else {
                        continue;
                    };
                    if document.stamp != spec.stamp {
                        stale = true;
                        continue;
                    }
                    let index = LineIndex::new(&document.text);
                    (
                        index.position(spec.start.min(document.text.len()), encoding),
                        index.position(spec.end.min(document.text.len()), encoding),
                    )
                }
                None => Default::default(),
            };
            targets.push(Target {
                at,
                rank: self
                    .servers
                    .rank(&instance.key.server)
                    .unwrap_or(usize::MAX),
                start,
                end,
            });
        }
        targets.sort_by_key(|target| (target.rank, target.at));
        if spec.feature.merge() == Merge::Primary {
            targets.truncate(1);
        }
        if targets.is_empty() {
            let failure = if stale {
                Failure::Stale
            } else {
                Failure::Unsupported
            };
            finish(&shared, Vec::new(), Some(failure));
            return;
        }

        let timeout = spec.timeout.unwrap_or(self.options.timeouts.request);
        let deadline = Instant::now() + timeout + GRACE;
        let mut waiting = HashMap::new();
        let mut attempts = HashMap::new();
        let mut errors = Vec::new();
        for target in targets {
            let instance = &self.instances[target.at];
            let Phase::Running(Running { client, .. }) = &instance.phase else {
                continue;
            };
            let mut extra = spec.extra.clone();
            if spec.feature == Feature::CodeAction
                && extra.pointer("/context/diagnostics").is_none()
            {
                if !extra["context"].is_object() {
                    extra["context"] = serde_json::json!({});
                }
                extra["context"]["diagnostics"] =
                    self.diagnostics_in(instance, &document_key, &spec).into();
            }
            let params = request::params(
                spec.feature,
                document_uri.as_deref().unwrap_or_default(),
                target.start,
                target.end,
                &extra,
            );
            self.book.next_tag += 1;
            let tag = TAG_BASE + self.book.next_tag;
            if client.request_until(spec.feature.method(), params.clone(), tag, deadline - GRACE) {
                waiting.insert(tag, instance.id);
                attempts.insert(
                    tag,
                    Attempt {
                        params,
                        run: instance.run,
                        retried: false,
                    },
                );
                self.book.tags.insert(tag, id);
            } else {
                errors.push(format!("{}: could not send", instance.key.server));
            }
        }
        if waiting.is_empty() {
            finish(
                &shared,
                Vec::new(),
                Some(Failure::Failed(errors.join("; "))),
            );
            return;
        }
        self.book.live.insert(
            id,
            Live {
                spec,
                shared,
                replaces: key,
                waiting,
                attempts,
                parts: Vec::new(),
                stale: false,
                errors,
                deadline,
            },
        );
    }

    /// The problems this instance reported that touch the range a code action
    /// request is about, in the protocol's terms.
    fn diagnostics_in(
        &self,
        instance: &super::Instance,
        key: &Option<String>,
        spec: &RequestSpec,
    ) -> Vec<Value> {
        let (
            Some(key),
            Phase::Running(Running {
                ready: Some(ready), ..
            }),
        ) = (key, &instance.phase)
        else {
            return Vec::new();
        };
        let store = self.results.lock().unwrap();
        let Some((stamp, found, text)) = store
            .get(key)
            .and_then(|entry| entry.diagnostics.get(&instance.id))
        else {
            return Vec::new();
        };
        if *stamp != spec.stamp {
            return Vec::new();
        }
        let encoding = ready.capabilities.position_encoding;
        found
            .iter()
            .filter(|d| {
                d.range.start <= spec.end.max(spec.start)
                    && spec.start <= d.range.end.max(d.range.start)
            })
            .map(|d| d.to_protocol(text, encoding))
            .collect()
    }

    /// A request only the application can answer.
    pub(super) fn server_request(&mut self, at: usize, id: Id, method: &str, params: &Value) {
        if matches!(
            method,
            "client/registerCapability" | "client/unregisterCapability"
        ) {
            self.registration_request(at, id, method, params);
            return;
        }
        let instance = &self.instances[at];
        let Phase::Running(Running {
            client,
            ready: Some(ready),
        }) = &instance.phase
        else {
            return;
        };
        let encoding = ready.capabilities.position_encoding;
        let reply = Reply {
            tx: self.tx.clone(),
            instance: instance.id,
            run: instance.run,
            id: id.clone(),
        };
        let refuse = |why: &str| {
            let answer = if method == "workspace/applyEdit" {
                serde_json::json!({ "applied": false, "failureReason": why })
            } else {
                serde_json::json!({ "success": false })
            };
            client.respond(id.clone(), Ok(answer));
        };
        match method {
            "workspace/applyEdit" => {
                if !edit_versions_current(instance, params.get("edit").unwrap_or(&Value::Null)) {
                    return refuse("a versioned document changed before the edit arrived");
                }
                match crate::features::language_service::reply::workspace_edit(
                    params.get("edit").unwrap_or(&Value::Null),
                    encoding,
                ) {
                    Ok(edit) => self.actions.lock().unwrap().push(ServerAction::ApplyEdit {
                        label: params
                            .get("label")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        edit,
                        reply,
                        expected: instance
                            .documents
                            .iter()
                            .filter_map(|d| uri::file_path(&d.uri).map(|p| (p, d.stamp)))
                            .collect(),
                    }),
                    Err(error) => refuse(&format!("the edit cannot be read: {error}")),
                }
            }
            "window/showDocument" => {
                let Some(uri) = params.get("uri").and_then(Value::as_str) else {
                    return refuse("no document");
                };
                // A page in a browser is not for the editor to show.
                if params.get("external").and_then(Value::as_bool) == Some(true) {
                    return refuse("external documents are not opened");
                }
                self.actions
                    .lock()
                    .unwrap()
                    .push(ServerAction::ShowDocument {
                        uri: uri.to_owned(),
                        selection: params.get("selection").and_then(|s| {
                            crate::features::language_service::reply::span(s, encoding)
                        }),
                        reply,
                    });
            }
            _ => {}
        }
        self.results_changed = true;
    }

    /// Gives up on a request: tells the servers and drops what was waiting.
    pub(super) fn cancel_request(&mut self, id: u64) {
        let Some(live) = self.book.live.remove(&id) else {
            return;
        };
        for (tag, instance_id) in &live.waiting {
            self.book.tags.remove(tag);
            if let Some(Phase::Running(Running { client, .. })) = self
                .instances
                .iter()
                .find(|i| i.id == *instance_id)
                .map(|i| &i.phase)
            {
                client.cancel(*tag);
            }
        }
        let mut progress = live.shared.lock().unwrap();
        progress.finished = true;
        progress.parts = live.parts;
    }

    /// An answer to a request made through the channel.
    pub(super) fn request_response(
        &mut self,
        at: usize,
        tag: u64,
        outcome: Result<Value, ResponseError>,
    ) {
        let Some(request) = self.book.tags.remove(&tag) else {
            return; // cancelled or finished already
        };
        let instance = &self.instances[at];
        let rank = self
            .servers
            .rank(&instance.key.server)
            .unwrap_or(usize::MAX);
        let encoding = match &instance.phase {
            Phase::Running(Running {
                ready: Some(ready), ..
            }) => ready.capabilities.position_encoding,
            _ => PositionEncoding::Utf16,
        };
        let part = Part {
            instance: instance.id,
            server: instance.key.server.clone(),
            rank,
            encoding,
            value: Value::Null,
        };
        let name = instance.key.server.to_string();
        let Some(live) = self.book.live.get_mut(&request) else {
            return;
        };
        live.waiting.remove(&tag);
        let attempt = live.attempts.remove(&tag);
        if let (Err(error), Some(mut attempt)) = (&outcome, attempt) {
            // Only replay a query against the exact same document and server
            // lifetime, once, inside the original deadline. Commands may have
            // already changed external state even if the server cancelled them.
            let same_document = live.spec.feature == Feature::WorkspaceSymbol
                || uri::file_uri(&live.spec.path).is_some_and(|uri| {
                    instance.documents.iter().any(|document| {
                        uri::key(&document.uri) == uri::key(&uri)
                            && document.stamp == live.spec.stamp
                    })
                });
            let remaining = live
                .deadline
                .saturating_duration_since(Instant::now() + GRACE);
            let retry = is_stale(error)
                && error
                    .data
                    .as_ref()
                    .and_then(|data| data.get("retriggerRequest"))
                    .and_then(Value::as_bool)
                    != Some(false)
                && !attempt.retried
                && same_document
                && attempt.run == instance.run
                && live.spec.feature != Feature::ExecuteCommand
                && !remaining.is_zero();
            if retry && let Phase::Running(Running { client, .. }) = &instance.phase {
                self.book.next_tag += 1;
                let new_tag = TAG_BASE + self.book.next_tag;
                if client.request_until(
                    live.spec.feature.method(),
                    attempt.params.clone(),
                    new_tag,
                    live.deadline - GRACE,
                ) {
                    attempt.retried = true;
                    live.waiting.insert(new_tag, instance.id);
                    live.attempts.insert(new_tag, attempt);
                    self.book.tags.insert(new_tag, request);
                    return;
                }
            }
        }
        match outcome {
            Ok(value) => {
                let valid = match live.spec.feature {
                    Feature::Rename => edit_versions_current(instance, &value),
                    Feature::CodeAction => value.as_array().is_none_or(|rows| {
                        rows.iter().all(|a| {
                            a.get("edit")
                                .is_none_or(|edit| edit_versions_current(instance, edit))
                        })
                    }),
                    Feature::CodeActionResolve => value
                        .get("edit")
                        .is_none_or(|edit| edit_versions_current(instance, edit)),
                    _ => true,
                };
                if !valid {
                    live.stale = true;
                    self.settle(request);
                    return;
                }
                if !value.is_null() {
                    live.parts.push(Arc::new(Part { value, ..part }));
                    live.parts.sort_by_key(|p| (p.rank, p.instance));
                }
            }
            Err(error) if error.code == REQUEST_CANCELLED => {}
            Err(error) if is_stale(&error) => live.stale = true,
            Err(error) => live.errors.push(format!("{name}: {}", error.message)),
        }
        self.settle(request);
    }

    /// Publishes progress; finishes the request once nothing is waited for.
    fn settle(&mut self, request: u64) {
        let Some(live) = self.book.live.get(&request) else {
            return;
        };
        let done = live.waiting.is_empty();
        let failure = (done && live.parts.is_empty()).then(|| {
            if live.stale {
                Some(Failure::Stale)
            } else if live.errors.is_empty() {
                None
            } else {
                Some(Failure::Failed(live.errors.join("; ")))
            }
        });
        let mut progress = live.shared.lock().unwrap();
        progress.parts = live.parts.clone();
        progress.finished = done;
        progress.failure = failure.flatten();
        drop(progress);
        if done {
            self.book.live.remove(&request);
        }
        self.results_changed = true;
    }

    /// The instance went away: whatever it still owed is a failure.
    pub(super) fn requests_lost(&mut self, instance_id: u64) {
        let affected: Vec<u64> = self
            .book
            .live
            .iter()
            .filter(|(_, live)| live.waiting.values().any(|i| *i == instance_id))
            .map(|(id, _)| *id)
            .collect();
        for request in affected {
            let lost: Vec<u64> = self.book.live[&request]
                .waiting
                .iter()
                .filter(|(_, i)| **i == instance_id)
                .map(|(tag, _)| *tag)
                .collect();
            if let Some(live) = self.book.live.get_mut(&request) {
                for tag in lost {
                    live.waiting.remove(&tag);
                    live.attempts.remove(&tag);
                    self.book.tags.remove(&tag);
                }
                live.errors.push("the server stopped".to_owned());
            }
            self.settle(request);
        }
    }

    /// Finishes requests whose time is up, with whatever has arrived.
    pub(super) fn expire_requests(&mut self, now: Instant) {
        let late: Vec<u64> = self
            .book
            .live
            .iter()
            .filter(|(_, live)| live.deadline <= now)
            .map(|(id, _)| *id)
            .collect();
        for request in late {
            if let Some(live) = self.book.live.get_mut(&request) {
                let tags: Vec<(u64, u64)> = live.waiting.drain().collect();
                live.errors.push("timed out".to_owned());
                for (tag, instance_id) in tags {
                    self.book.tags.remove(&tag);
                    if let Some(Phase::Running(Running { client, .. })) = self
                        .instances
                        .iter()
                        .find(|i| i.id == instance_id)
                        .map(|i| &i.phase)
                    {
                        client.cancel(tag);
                    }
                }
            }
            self.settle(request);
        }
    }
}

fn edit_versions_current(instance: &super::Instance, edit: &Value) -> bool {
    let Phase::Running(Running {
        ready: Some(ready), ..
    }) = &instance.phase
    else {
        return false;
    };
    edit.get("documentChanges")
        .and_then(Value::as_array)
        .is_none_or(|changes| {
            changes.iter().all(|change| {
                let Some(doc) = change.get("textDocument") else {
                    return true;
                };
                let Some(version) = doc.get("version").filter(|v| !v.is_null()) else {
                    return true;
                };
                let Some(version) = version.as_i64().and_then(|v| i32::try_from(v).ok()) else {
                    return false;
                };
                let Some(link) = doc.get("uri").and_then(Value::as_str) else {
                    return false;
                };
                ready.sync.stamp_of(link, version).is_some_and(|stamp| {
                    instance
                        .documents
                        .iter()
                        .any(|d| uri::key(&d.uri) == uri::key(link) && d.stamp == stamp)
                })
            })
        })
}

fn finish(shared: &Arc<Mutex<Progress>>, parts: Vec<Arc<Part>>, failure: Option<Failure>) {
    let mut progress = shared.lock().unwrap();
    progress.parts = parts;
    progress.finished = true;
    progress.failure = failure;
}
