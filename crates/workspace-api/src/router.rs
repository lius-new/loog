use crate::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Generation comes from the application activation, not from provider input.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Owner {
    pub id: String,
    pub generation: u64,
}
use serde::{Deserialize, Serialize};

pub type EnvironmentId = u64;
pub type ScopeId = u64;
pub type RequestId = u64;

#[derive(Clone, Copy, Debug)]
pub struct RouterLimits {
    pub environments: usize,
    pub scopes: usize,
    pub pending: usize,
    pub events: usize,
    pub handles: usize,
    pub payload_bytes: usize,
    pub queue_bytes: usize,
}
impl Default for RouterLimits {
    fn default() -> Self {
        Self {
            environments: 32,
            scopes: 128,
            pending: 256,
            events: 1024,
            handles: 1024,
            payload_bytes: 1024 * 1024,
            queue_bytes: 16 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: RequestId,
    pub environment: EnvironmentId,
    pub scope: ScopeId,
    pub root: ProviderPath,
    /// Other open roots with no execution grant. The provider applies its own
    /// containment rules to nested roots before starting a program or terminal.
    pub excluded_roots: Vec<ProviderPath>,
    pub operation: Operation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Delivery {
    Request(Request),
    Cancel {
        request: RequestId,
    },
    Release {
        scope: ScopeId,
        capability: Capability,
        handle: u64,
    },
    Complete {
        request: RequestId,
        result: Result<Answer, Failure>,
    },
    Notification {
        environment: EnvironmentId,
        scope: ScopeId,
        event: Notification,
    },
}

struct Environment {
    owner: Owner,
    descriptor: EnvironmentDescriptor,
}
struct Scope {
    environment: EnvironmentId,
    root: ProviderPath,
    execution_exclusions: Vec<ProviderPath>,
}
struct Pending {
    consumer: Owner,
    provider: Owner,
    scope: ScopeId,
    capability: Capability,
    answer: AnswerKind,
    remove_handle: Option<u64>,
    deadline: u64,
    changes_state: bool,
    delivered: bool,
}
struct Queued {
    event: Delivery,
    bytes: usize,
}

/// Pure, single-owner broker. It never calls providers, waits, or owns IO.
/// Both provider requests and consumer completions are delivered on later turns.
pub struct Router {
    limits: RouterLimits,
    next_environment: u64,
    next_scope: u64,
    next_request: u64,
    environments: BTreeMap<EnvironmentId, Environment>,
    scopes: BTreeMap<ScopeId, Scope>,
    grants: BTreeMap<(Owner, ScopeId), BTreeSet<Capability>>,
    pending: BTreeMap<RequestId, Pending>,
    handles: BTreeMap<(ScopeId, Capability, u64), Owner>,
    queues: BTreeMap<Owner, VecDeque<Queued>>,
    queued_bytes: usize,
    queued_events: usize,
}
impl Default for Router {
    fn default() -> Self {
        Self::new(RouterLimits::default())
    }
}
impl Router {
    pub fn new(limits: RouterLimits) -> Self {
        Self {
            limits,
            next_environment: 0,
            next_scope: 0,
            next_request: 0,
            environments: BTreeMap::new(),
            scopes: BTreeMap::new(),
            grants: BTreeMap::new(),
            pending: BTreeMap::new(),
            handles: BTreeMap::new(),
            queues: BTreeMap::new(),
            queued_bytes: 0,
            queued_events: 0,
        }
    }
    pub fn register(
        &mut self,
        owner: Owner,
        descriptor: EnvironmentDescriptor,
    ) -> Result<EnvironmentId, Failure> {
        if descriptor.key.provider() != owner.id
            || descriptor.name.is_empty()
            || descriptor.name.len() > 256
            || descriptor.name.contains('\0')
            || descriptor.platform.len() > 128
            || descriptor.platform.contains('\0')
            || descriptor.badge.as_deref().is_some_and(|badge| {
                badge.is_empty()
                    || badge.len() > crate::operation::MAX_BADGE_BYTES
                    || badge.chars().any(char::is_control)
            })
            || descriptor.capabilities.len() > 6
            || descriptor
                .capabilities
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len()
                != descriptor.capabilities.len()
        {
            return Err(fail(
                FailureKind::InvalidRequest,
                "invalid provider identity or descriptor",
            ));
        }
        if self
            .environments
            .values()
            .any(|item| item.descriptor.key == descriptor.key)
        {
            return Err(fail(
                FailureKind::Conflict,
                "environment is already registered",
            ));
        }
        if self.environments.len() >= self.limits.environments {
            return Err(fail(
                FailureKind::LimitExceeded,
                "environment limit exceeded",
            ));
        }
        self.next_environment = next(self.next_environment)?;
        let id = self.next_environment;
        self.environments
            .insert(id, Environment { owner, descriptor });
        Ok(id)
    }
    pub fn environment(&self, id: EnvironmentId) -> Option<&EnvironmentDescriptor> {
        self.environments.get(&id).map(|item| &item.descriptor)
    }
    pub fn provider(&self, id: EnvironmentId) -> Option<&Owner> {
        self.environments.get(&id).map(|item| &item.owner)
    }
    pub fn scope(&self, id: ScopeId) -> Option<(EnvironmentId, &ProviderPath)> {
        self.scopes
            .get(&id)
            .map(|scope| (scope.environment, &scope.root))
    }
    pub fn unregister(
        &mut self,
        provider: &Owner,
        environment: EnvironmentId,
    ) -> Result<(), Failure> {
        if self.provider(environment).ok_or_else(unavailable)? != provider {
            return Err(denied());
        }
        let scopes: Vec<_> = self
            .scopes
            .iter()
            .filter(|(_, item)| item.environment == environment)
            .map(|(id, _)| *id)
            .collect();
        for scope in scopes {
            let consumers: Vec<_> = self
                .grants
                .keys()
                .filter(|(_, id)| *id == scope)
                .map(|(owner, _)| owner.clone())
                .collect();
            for consumer in consumers {
                self.revoke(&consumer, scope);
            }
            self.scopes.remove(&scope);
        }
        self.environments.remove(&environment);
        Ok(())
    }
    pub fn resolve(&self, key: &EnvironmentKey) -> Option<EnvironmentId> {
        self.environments
            .iter()
            .find(|(_, item)| &item.descriptor.key == key)
            .map(|(id, _)| *id)
    }
    pub fn mount(
        &mut self,
        provider: &Owner,
        environment: EnvironmentId,
        root: ProviderPath,
    ) -> Result<ScopeId, Failure> {
        let item = self
            .environments
            .get(&environment)
            .ok_or_else(unavailable)?;
        if &item.owner != provider {
            return Err(denied());
        }
        if let Some((id, _)) = self
            .scopes
            .iter()
            .find(|(_, item)| item.environment == environment && item.root == root)
        {
            return Ok(*id);
        }
        if self.scopes.len() >= self.limits.scopes {
            return Err(fail(FailureKind::LimitExceeded, "scope limit exceeded"));
        }
        self.next_scope = next(self.next_scope)?;
        let id = self.next_scope;
        self.scopes.insert(
            id,
            Scope {
                environment,
                root,
                execution_exclusions: Vec::new(),
            },
        );
        Ok(id)
    }
    /// Called only by application policy after package grants and workspace
    /// trust have been checked. This is deliberately not a guest operation.
    pub fn grant(
        &mut self,
        consumer: Owner,
        scope: ScopeId,
        capabilities: impl IntoIterator<Item = Capability>,
    ) -> Result<(), Failure> {
        let item = self.scopes.get(&scope).ok_or_else(unavailable)?;
        let environment = &self.environments[&item.environment];
        let allowed: BTreeSet<_> = capabilities.into_iter().collect();
        if allowed
            .iter()
            .any(|cap| !environment.descriptor.capabilities.contains(cap))
        {
            return Err(fail(
                FailureKind::Unsupported,
                "provider does not implement this capability",
            ));
        }
        let removed = self
            .grants
            .get(&(consumer.clone(), scope))
            .map(|previous| previous.difference(&allowed).copied().collect())
            .unwrap_or_default();
        self.revoke_capabilities(&consumer, scope, &removed);
        self.grants.insert((consumer, scope), allowed);
        Ok(())
    }
    /// Application policy only; guests cannot remove execution boundaries.
    pub fn restrict_execution(
        &mut self,
        scope: ScopeId,
        mut excluded: Vec<ProviderPath>,
    ) -> Result<(), Failure> {
        Self::validate_execution_boundaries(&excluded)?;
        excluded.sort();
        excluded.dedup();
        let item = self.scopes.get_mut(&scope).ok_or_else(unavailable)?;
        if item.execution_exclusions == excluded {
            return Ok(());
        }
        item.execution_exclusions = excluded;
        let consumers: Vec<_> = self
            .grants
            .keys()
            .filter(|(_, id)| *id == scope)
            .map(|(owner, _)| owner.clone())
            .collect();
        for consumer in consumers {
            self.revoke_capabilities(
                &consumer,
                scope,
                &[Capability::Process, Capability::Terminal].into(),
            );
        }
        Ok(())
    }
    /// Shared admission check for policy previews and applying the boundary.
    pub fn validate_execution_boundaries(excluded: &[ProviderPath]) -> Result<(), Failure> {
        if excluded.len() > 128
            || excluded
                .iter()
                .map(|path| path.as_str().len())
                .sum::<usize>()
                > 128 * 1024
        {
            return Err(limit());
        }
        Ok(())
    }
    pub fn revoke(&mut self, consumer: &Owner, scope: ScopeId) {
        let removed = self
            .grants
            .remove(&(consumer.clone(), scope))
            .unwrap_or_default();
        self.revoke_capabilities(consumer, scope, &removed);
    }
    fn revoke_capabilities(
        &mut self,
        consumer: &Owner,
        scope: ScopeId,
        removed: &BTreeSet<Capability>,
    ) {
        if removed.is_empty() {
            return;
        }
        let requests: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, item)| {
                &item.consumer == consumer
                    && item.scope == scope
                    && removed.contains(&item.capability)
            })
            .map(|(id, _)| *id)
            .collect();
        for request in requests {
            let _ = self.terminate(request, FailureKind::PermissionDenied);
        }
        let handles: Vec<_> = self
            .handles
            .iter()
            .filter(|((s, capability, _), owner)| {
                *s == scope && *owner == consumer && removed.contains(capability)
            })
            .map(|(key, _)| *key)
            .collect();
        let mut ended = Vec::new();
        for key @ (_, capability, handle) in handles {
            self.handles.remove(&key);
            if let Some(root) = self.scopes.get(&scope)
                && let Some(environment) = self.environments.get(&root.environment)
            {
                let environment_id = root.environment;
                let provider = environment.owner.clone();
                let event = match capability {
                    Capability::Process => Some(Notification::Exited { handle, code: None }),
                    Capability::Terminal => {
                        Some(Notification::TerminalExited { handle, code: None })
                    }
                    Capability::Watch => Some(Notification::WatchStopped {
                        watch: handle,
                        error: fail(
                            FailureKind::PermissionDenied,
                            "subscription permission was revoked",
                        ),
                    }),
                    _ => None,
                };
                if let Some(event) = event {
                    ended.push(Delivery::Notification {
                        environment: environment_id,
                        scope,
                        event,
                    });
                }
                self.control(
                    provider,
                    Delivery::Release {
                        scope,
                        capability,
                        handle,
                    },
                );
            }
        }
        if let Some(queue) = self.queues.get_mut(consumer) {
            queue.retain(|item| {
                let remove =
                    matches!(&item.event, Delivery::Notification { scope: s, event, .. } if *s == scope && removed.contains(&event.capability()));
                if remove {
                    self.queued_bytes -= item.bytes;
                    self.queued_events -= 1;
                }
                !remove
            });
        }
        for event in ended {
            self.control(consumer.clone(), event);
        }
    }
    pub fn submit(
        &mut self,
        consumer: &Owner,
        scope: ScopeId,
        operation: Operation,
        now: u64,
        timeout: u64,
    ) -> Result<RequestId, Failure> {
        let item = self.scopes.get(&scope).ok_or_else(unavailable)?;
        let environment = &self.environments[&item.environment];
        if !self
            .grants
            .get(&(consumer.clone(), scope))
            .is_some_and(|set| set.contains(&operation.capability()))
        {
            return Err(denied());
        }
        let capability = operation.capability();
        if let Some(handle) = operation.handle()
            && self.handles.get(&(scope, capability, handle)) != Some(consumer)
        {
            return Err(denied());
        }
        if self.pending.len() >= self.limits.pending {
            return Err(fail(FailureKind::LimitExceeded, "request limit exceeded"));
        }
        let deadline = now
            .checked_add(timeout)
            .filter(|_| timeout > 0)
            .ok_or_else(|| fail(FailureKind::InvalidRequest, "invalid deadline"))?;
        let id = next(self.next_request)?;
        let provider = environment.owner.clone();
        let answer = operation.answer_kind();
        let remove_handle = match &operation {
            Operation::Unwatch { watch } => Some(*watch),
            Operation::SearchClose { search } => Some(*search),
            Operation::CommitWrite { write } | Operation::AbortWrite { write } => Some(*write),
            _ => None,
        };
        let request = Request {
            id,
            environment: item.environment,
            scope,
            root: item.root.clone(),
            excluded_roots: if matches!(
                &operation,
                Operation::Spawn { .. } | Operation::TerminalOpen { .. }
            ) {
                item.execution_exclusions.clone()
            } else {
                Vec::new()
            },
            operation,
        };
        let changes_state = request.operation.changes_state();
        self.enqueue(provider.clone(), Delivery::Request(request))?;
        self.next_request = id;
        self.pending.insert(
            id,
            Pending {
                consumer: consumer.clone(),
                provider,
                scope,
                capability,
                answer,
                remove_handle,
                deadline,
                changes_state,
                delivered: false,
            },
        );
        Ok(id)
    }
    pub fn complete(
        &mut self,
        provider: &Owner,
        request: RequestId,
        result: Result<Answer, Failure>,
    ) -> Result<(), Failure> {
        let item = self.pending.get(&request).ok_or_else(unavailable)?;
        if &item.provider != provider {
            return Err(denied());
        }
        if !item.delivered {
            return Err(fail(
                FailureKind::InvalidRequest,
                "request has not been delivered",
            ));
        }
        if result
            .as_ref()
            .is_ok_and(|answer| answer.kind() != item.answer)
        {
            return Err(fail(
                FailureKind::InvalidRequest,
                "answer does not match the request",
            ));
        }
        let started = match &result {
            Ok(Answer::Started { handle }) => {
                let key = (item.scope, item.capability, *handle);
                if self.handles.contains_key(&key) {
                    return Err(fail(FailureKind::Conflict, "handle is already owned"));
                }
                if self.handles.len() >= self.limits.handles {
                    return Err(limit());
                }
                Some(key)
            }
            _ => None,
        };
        let removed = result
            .is_ok()
            .then_some(item.remove_handle)
            .flatten()
            .map(|handle| (item.scope, item.capability, handle));
        let consumer = item.consumer.clone();
        self.enqueue(consumer.clone(), Delivery::Complete { request, result })?;
        if let Some(key) = started {
            self.handles.insert(key, consumer);
        }
        if let Some(key) = removed {
            self.handles.remove(&key);
        }
        self.pending.remove(&request);
        Ok(())
    }
    pub fn notify(
        &mut self,
        provider: &Owner,
        scope: ScopeId,
        event: Notification,
    ) -> Result<(), Failure> {
        let item = self.scopes.get(&scope).ok_or_else(unavailable)?;
        let environment = self
            .environments
            .get(&item.environment)
            .ok_or_else(unavailable)?;
        if &environment.owner != provider {
            return Err(denied());
        }
        let (capability, handle, ended) = match &event {
            Notification::Output { handle, .. } => (Capability::Process, *handle, false),
            Notification::Writable { handle, .. } => (Capability::Process, *handle, false),
            Notification::Exited { handle, .. } => (Capability::Process, *handle, true),
            Notification::TerminalOutput { handle, .. } => (Capability::Terminal, *handle, false),
            Notification::TerminalDirectory { handle, .. } => {
                (Capability::Terminal, *handle, false)
            }
            Notification::TerminalWritable { handle, .. } => (Capability::Terminal, *handle, false),
            Notification::TerminalExited { handle, .. } => (Capability::Terminal, *handle, true),
            Notification::Changed { watch, .. } => (Capability::Watch, *watch, false),
            Notification::WatchStopped { watch, .. } => (Capability::Watch, *watch, true),
        };
        let key = (scope, capability, handle);
        let consumer = self.handles.get(&key).ok_or_else(unavailable)?.clone();
        if !self
            .grants
            .get(&(consumer.clone(), scope))
            .is_some_and(|caps| caps.contains(&capability))
        {
            return Err(denied());
        }
        let delivery = Delivery::Notification {
            environment: item.environment,
            scope,
            event,
        };
        self.enqueue(consumer, delivery)?;
        if ended {
            self.handles.remove(&key);
        }
        Ok(())
    }
    pub fn poll(&mut self, owner: &Owner, limit: usize) -> Vec<Delivery> {
        let mut results = Vec::new();
        if let Some(queue) = self.queues.get_mut(owner) {
            for _ in 0..limit {
                let Some(item) = queue.pop_front() else { break };
                self.queued_bytes -= item.bytes;
                self.queued_events -= 1;
                if let Delivery::Request(request) = &item.event
                    && let Some(pending) = self.pending.get_mut(&request.id)
                {
                    pending.delivered = true;
                }
                results.push(item.event);
            }
            if queue.is_empty() {
                self.queues.remove(owner);
            }
        }
        results
    }
    pub fn cancel(&mut self, consumer: &Owner, request: RequestId) -> Result<(), Failure> {
        let item = self.pending.get(&request).ok_or_else(unavailable)?;
        if &item.consumer != consumer {
            return Err(denied());
        }
        self.terminate(request, FailureKind::Cancelled)
    }
    pub fn expire(&mut self, now: u64) -> Result<(), Failure> {
        let ids: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, item)| item.deadline <= now)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.terminate(id, FailureKind::DeadlineExceeded)?;
        }
        Ok(())
    }
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
    pub fn queued_bytes(&self) -> usize {
        self.queued_bytes
    }
    pub fn ready_owners(&self) -> Vec<Owner> {
        self.queues
            .iter()
            .filter(|(_, queue)| !queue.is_empty())
            .map(|(owner, _)| owner.clone())
            .collect()
    }
    pub fn owners(&self) -> Vec<Owner> {
        self.environments
            .values()
            .map(|item| item.owner.clone())
            .chain(self.grants.keys().map(|(owner, _)| owner.clone()))
            .chain(
                self.pending
                    .values()
                    .flat_map(|item| [item.consumer.clone(), item.provider.clone()]),
            )
            .chain(self.queues.keys().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    fn terminate(&mut self, id: RequestId, reason: FailureKind) -> Result<(), Failure> {
        let item = self.pending.get(&id).ok_or_else(unavailable)?;
        let kind = if item.delivered && item.changes_state {
            FailureKind::UnknownOutcome
        } else {
            reason
        };
        let complete = Delivery::Complete {
            request: id,
            result: Err(fail(kind, "request ended before a confirmed result")),
        };
        let cancel = Delivery::Cancel { request: id };
        let consumer = item.consumer.clone();
        let provider = item.provider.clone();
        let delivered = item.delivered;
        self.drop_queued_request(id);
        self.control(consumer, complete);
        if delivered {
            self.control(provider, cancel);
        }
        self.pending.remove(&id);
        Ok(())
    }
    /// Retiring an activation invalidates registrations, scopes and grants.
    /// Completion notifications have reserved control capacity, independent of
    /// the ordinary data budget, so backpressure never prevents retirement.
    pub fn retire(&mut self, owner: &Owner) {
        self.drop_queue(owner);
        let ids: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, item)| &item.provider == owner || &item.consumer == owner)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let item = self.pending.remove(&id).expect("collected pending request");
            self.drop_queued_request(id);
            if &item.consumer != owner {
                let kind = if item.delivered && item.changes_state {
                    FailureKind::UnknownOutcome
                } else {
                    FailureKind::Unavailable
                };
                let event = Delivery::Complete {
                    request: id,
                    result: Err(fail(kind, "capability owner stopped")),
                };
                self.control(item.consumer, event);
            }
            if &item.provider != owner {
                let event = Delivery::Cancel { request: id };
                self.control(item.provider, event);
            }
        }
        let environments: BTreeSet<_> = self
            .environments
            .iter()
            .filter(|(_, item)| &item.owner == owner)
            .map(|(id, _)| *id)
            .collect();
        let scopes: BTreeSet<_> = self
            .scopes
            .iter()
            .filter(|(_, item)| environments.contains(&item.environment))
            .map(|(id, _)| *id)
            .collect();
        self.scopes.retain(|id, _| !scopes.contains(id));
        self.environments.retain(|id, _| !environments.contains(id));
        self.grants
            .retain(|(consumer, scope), _| consumer != owner && !scopes.contains(scope));
        let handles: Vec<_> = self
            .handles
            .iter()
            .filter(|((scope, _, _), consumer)| *consumer == owner || scopes.contains(scope))
            .map(|(key, _)| *key)
            .collect();
        for (scope, capability, handle) in handles {
            self.handles.remove(&(scope, capability, handle));
            if let Some(root) = self.scopes.get(&scope)
                && let Some(environment) = self.environments.get(&root.environment)
            {
                self.control(
                    environment.owner.clone(),
                    Delivery::Release {
                        scope,
                        capability,
                        handle,
                    },
                );
            }
        }
    }
    fn enqueue(&mut self, owner: Owner, event: Delivery) -> Result<(), Failure> {
        let bytes = self.size(&event)?;
        self.reserve(bytes, 1)?;
        self.push(owner, event, bytes);
        Ok(())
    }
    fn push(&mut self, owner: Owner, event: Delivery, bytes: usize) {
        self.queues
            .entry(owner)
            .or_default()
            .push_back(Queued { event, bytes });
        self.queued_bytes += bytes;
        self.queued_events += 1;
    }
    /// At most two bounded controls per pending request and one per live handle
    /// can exceed the data budget. New ordinary work remains backpressured until
    /// these queues drain, so repeated retirement cannot grow them without bound.
    fn control(&mut self, owner: Owner, event: Delivery) {
        let bytes = serde_json::to_vec(&event).expect("bounded control").len();
        self.push(owner, event, bytes);
    }
    fn size(&self, event: &Delivery) -> Result<usize, Failure> {
        // Count without allocating a potentially huge serialized payload.
        struct Count {
            bytes: usize,
            max: usize,
        }
        impl std::io::Write for Count {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.bytes = self
                    .bytes
                    .checked_add(bytes.len())
                    .filter(|n| *n <= self.max)
                    .ok_or_else(|| std::io::Error::other("payload budget exceeded"))?;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut count = Count {
            bytes: 0,
            max: self.limits.payload_bytes,
        };
        serde_json::to_writer(&mut count, event).map_err(|_| limit())?;
        Ok(count.bytes)
    }
    fn reserve(&self, bytes: usize, events: usize) -> Result<(), Failure> {
        if self
            .queued_bytes
            .checked_add(bytes)
            .is_none_or(|n| n > self.limits.queue_bytes)
            || self
                .queued_events
                .checked_add(events)
                .is_none_or(|n| n > self.limits.events)
        {
            Err(limit())
        } else {
            Ok(())
        }
    }
    fn drop_queue(&mut self, owner: &Owner) {
        if let Some(queue) = self.queues.remove(owner) {
            self.queued_bytes -= queue.iter().map(|item| item.bytes).sum::<usize>();
            self.queued_events -= queue.len();
        }
    }
    fn drop_queued_request(&mut self, id: RequestId) {
        for queue in self.queues.values_mut() {
            queue.retain(|item| {
                let remove = matches!(&item.event, Delivery::Request(request) if request.id == id);
                if remove {
                    self.queued_bytes -= item.bytes;
                    self.queued_events -= 1;
                }
                !remove
            });
        }
    }
}
fn next(value: u64) -> Result<u64, Failure> {
    value.checked_add(1).ok_or_else(limit)
}
fn fail(kind: FailureKind, message: &str) -> Failure {
    Failure::new(kind, message)
}
fn unavailable() -> Failure {
    fail(
        FailureKind::Unavailable,
        "capability or request is no longer available",
    )
}
fn denied() -> Failure {
    fail(
        FailureKind::PermissionDenied,
        "capability access is not granted",
    )
}
fn limit() -> Failure {
    fail(
        FailureKind::LimitExceeded,
        "capability queue budget exceeded",
    )
}
