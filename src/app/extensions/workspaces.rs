use super::*;
use loog_extension_host::workspace::{Command, Event as WorkspaceEvent, Reply, Submitted};
use loog_workspace_api::{Capability, Failure, FailureKind, Owner};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SettingNotice {
    Clean,
    Dirty,
    Sent,
}

fn consumer_capability(capability: Capability) -> &'static str {
    match capability {
        Capability::FilesRead | Capability::Watch | Capability::Search => "workspace.files.read",
        Capability::FilesWrite => "workspace.files.write",
        Capability::Process => "process.execute-environment",
        Capability::Terminal => "terminal.provide",
    }
}

impl ExtensionService {
    fn resolve_workspace_settings(
        &mut self,
        owner: &Owner,
        scope: Option<loog_workspace_api::ScopeId>,
        project: Option<&serde_json::Value>,
    ) -> Result<Reply, Failure> {
        let package = self
            .packages
            .get(&owner.id)
            .filter(|p| p.lifecycle.accepts(owner.generation))
            .ok_or_else(|| Failure::new(FailureKind::Unavailable, "Settings activation ended"))?;
        if project.is_some_and(|v| !v.is_object()) || project.is_some() && scope.is_none() {
            return Err(Failure::new(
                FailureKind::InvalidRequest,
                "Project settings require an object and resource scope",
            ));
        }
        if project.is_some_and(|v| serde_json::to_vec(v).map_or(true, |v| v.len() > 256 * 1024)) {
            return Err(Failure::new(
                FailureKind::LimitExceeded,
                "Project settings exceed their budget",
            ));
        }
        let trusted = if let Some(scope) = scope {
            let root = self
                .consumer_roots(owner)
                .into_iter()
                .find(|root| {
                    root.scope == scope && root.capabilities.contains(&Capability::FilesRead)
                })
                .ok_or_else(|| {
                    Failure::new(
                        FailureKind::PermissionDenied,
                        "Settings scope is unavailable or not readable",
                    )
                })?;
            self.resource_execution_trusted(&root.resource)
        } else {
            false
        };
        let resolved = crate::features::extensions::settings::resolve(
            &package.manifest.contributes.settings,
            &self.settings.user,
            project,
            trusted,
        );
        if resolved.values.len() > 256
            || serde_json::to_vec(&resolved.values).map_or(true, |v| v.len() > 256 * 1024)
        {
            return Err(Failure::new(
                FailureKind::LimitExceeded,
                "Resolved settings exceed their budget",
            ));
        }
        self.workspace_settings
            .insert(owner.clone(), SettingNotice::Clean);
        Ok(Reply::Settings {
            values: resolved.values,
        })
    }
    pub(super) fn publish_settings_notices(&mut self) -> Vec<Effect> {
        self.workspace_settings.retain(|owner, _| {
            self.packages
                .get(&owner.id)
                .is_some_and(|p| p.lifecycle.accepts(owner.generation))
        });
        let mut effects = vec![];
        for (owner, notice) in &mut self.workspace_settings {
            if *notice != SettingNotice::Dirty {
                continue;
            }
            let Some(credit) = self.workspace_budget.reserve(1024) else {
                effects.push(Effect::Submit(Job::WorkspaceWakeAfter { milliseconds: 16 }));
                break;
            };
            *notice = SettingNotice::Sent;
            effects.push(Effect::Submit(Job::WorkspaceEvent {
                id: owner.id.clone(),
                generation: owner.generation,
                event: WorkspaceEvent::SettingsChanged,
                _credit: credit,
            }));
        }
        effects
    }
    fn consumer_root_capabilities(
        &self,
        owner: &Owner,
        root: &loog_workspace_api::PresentedRoot,
        execute: bool,
    ) -> Vec<Capability> {
        let Some(package) = self
            .packages
            .get(&owner.id)
            .filter(|p| p.lifecycle.accepts(owner.generation))
        else {
            return vec![];
        };
        root.capabilities
            .iter()
            .copied()
            .filter(|cap| {
                let permission = consumer_capability(*cap);
                package
                    .manifest
                    .capabilities
                    .iter()
                    .any(|value| value == permission)
                    && self
                        .grants
                        .authorize(&package.identity, permission, None)
                        .is_ok()
                    && (!matches!(cap, Capability::Process | Capability::Terminal) || execute)
            })
            .collect()
    }
    fn consumer_roots(&self, owner: &Owner) -> Vec<loog_workspace_api::PresentedRoot> {
        let mut roots: Vec<_> = self
            .workspace_roots
            .iter()
            .filter(|root| self.workspace_router.scope(root.scope).is_some())
            .filter_map(|root| {
                let capabilities = self.consumer_root_capabilities(
                    owner,
                    root,
                    self.resource_execution_trusted(&root.resource)
                        && loog_workspace_api::Router::validate_execution_boundaries(
                            &self.resource_execution_exclusions(root),
                        )
                        .is_ok(),
                );
                (!capabilities.is_empty()).then(|| loog_workspace_api::PresentedRoot {
                    capabilities,
                    ..root.clone()
                })
            })
            .collect();
        roots.sort_by_key(|root| root.scope);
        roots
    }
    fn resource_execution_trusted(&self, resource: &loog_workspace_api::ResourceRef) -> bool {
        self.trust.as_ref().is_some_and(|trust| {
            trust.resource_decision(resource)
                == crate::features::extensions::trust::Decision::Trusted
        })
    }
    fn resource_execution_exclusions(
        &self,
        root: &loog_workspace_api::PresentedRoot,
    ) -> Vec<loog_workspace_api::ProviderPath> {
        self.workspace_roots
            .iter()
            .filter(|other| {
                other.scope != root.scope
                    && other.resource.environment() == root.resource.environment()
                    && !self.resource_execution_trusted(&other.resource)
            })
            .filter_map(|other| match &other.resource {
                loog_workspace_api::ResourceRef::Provided { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect()
    }
    fn refresh_resource_grants(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.workspace_consumers.retain(|owner, _| {
            self.packages
                .get(&owner.id)
                .is_some_and(|package| package.lifecycle.accepts(owner.generation))
        });
        let consumers: Vec<_> = self.workspace_consumers.keys().cloned().collect();
        let mut signatures = std::collections::BTreeMap::<Owner, Vec<_>>::new();
        for root in self.workspace_roots.clone() {
            let mut execute = self.resource_execution_trusted(&root.resource);
            let excluded = self.resource_execution_exclusions(&root);
            if let Err(error) = self
                .workspace_router
                .restrict_execution(root.scope, excluded)
            {
                execute = false;
                effects.push(Effect::Error(error.message));
            }
            let caps = root.capabilities.iter().copied().filter(|cap| {
                matches!(
                    cap,
                    Capability::FilesRead
                        | Capability::FilesWrite
                        | Capability::Watch
                        | Capability::Search
                ) || (execute && matches!(cap, Capability::Process | Capability::Terminal))
            });
            if let Err(error) = self
                .workspace_router
                .grant(self.native_owner(), root.scope, caps)
            {
                effects.push(Effect::Error(error.message));
            }
            for owner in &consumers {
                let caps = self.consumer_root_capabilities(owner, &root, execute);
                if !caps.is_empty() {
                    signatures
                        .entry(owner.clone())
                        .or_default()
                        .push((root.scope, caps.clone()));
                }
                if let Err(error) = self.workspace_router.grant(owner.clone(), root.scope, caps) {
                    effects.push(Effect::Error(error.message));
                }
            }
        }
        for owner in consumers {
            let mut signature = signatures.remove(&owner).unwrap_or_default();
            signature.sort_by_key(|(scope, _)| *scope);
            if self.workspace_consumers.get(&owner) != Some(&signature) {
                if let Some(credit) = self.workspace_budget.reserve(1024) {
                    self.workspace_consumers.insert(owner.clone(), signature);
                    effects.push(Effect::Submit(Job::WorkspaceEvent {
                        id: owner.id,
                        generation: owner.generation,
                        event: WorkspaceEvent::RootsChanged,
                        _credit: credit,
                    }));
                } else {
                    effects.push(Effect::Submit(Job::WorkspaceWakeAfter { milliseconds: 16 }));
                }
            }
        }
        effects
    }
    pub(super) fn restore_resource(
        &mut self,
        bookmark: loog_workspace_api::session::ResourceBookmark,
    ) -> Result<Vec<Effect>, String> {
        bookmark.validate()?;
        let id = bookmark
            .resource
            .environment()
            .unwrap()
            .provider()
            .to_owned();
        let package = self
            .packages
            .get(&id)
            .ok_or_else(|| format!("Resource provider {id} is not installed"))?;
        if package.disabled {
            return Err(format!("Resource provider {id} is disabled"));
        }
        if !wants(&package.manifest, "workspace.restore")
            || !package
                .manifest
                .capabilities
                .iter()
                .any(|cap| cap == "workspace.provide")
            || self
                .grants
                .authorize(&package.identity, "workspace.provide", None)
                .is_err()
        {
            return Err(format!(
                "Resource provider {id} does not allow workspace recovery"
            ));
        }
        if matches!(package.lifecycle.state(), State::Failed | State::Stopped) {
            self.retry(&id)?;
        }
        let package = self.packages.get_mut(&id).unwrap();
        let generation = package.lifecycle.generation();
        match package.lifecycle.state() {
            State::Active => Ok(self.workspace_restore_job(&id, generation, bookmark)),
            State::Dormant | State::Activating => {
                let pending = self
                    .pending_workspace_restores
                    .entry(id.clone())
                    .or_default();
                if !pending
                    .iter()
                    .any(|item| item.resource == bookmark.resource)
                {
                    if pending.len() >= 128 {
                        return Err("Too many pending resource recoveries".into());
                    }
                    pending.push(bookmark);
                }
                if package.lifecycle.state() == State::Activating {
                    return Ok(Vec::new());
                }
                package.lifecycle.activating().map_err(|e| e.to_string())?;
                Ok(vec![Effect::Submit(Job::Load {
                    id,
                    generation,
                    component: package.component.clone(),
                    package_dir: package.dir.clone(),
                    version: package.manifest.version.clone(),
                    cache: self.cache.clone(),
                    limits: self.guest_limits,
                    storage: storage_of(&self.grants, package)
                        .then(|| self.storage_dir.join(&package.manifest.id)),
                    files: files_of(&self.grants, package),
                    domains: domains_of(&self.grants, package),
                })])
            }
            _ => Err(format!("Resource provider {id} is stopping")),
        }
    }
    pub(super) fn workspace_restore_job(
        &self,
        id: &str,
        generation: u64,
        bookmark: loog_workspace_api::session::ResourceBookmark,
    ) -> Vec<Effect> {
        match self.workspace_budget.reserve(64 * 1024) {
            Some(credit) => vec![Effect::Submit(Job::WorkspaceEvent {
                id: id.into(),
                generation,
                event: WorkspaceEvent::Restore { bookmark },
                _credit: credit,
            })],
            None => vec![Effect::Error(
                "Resource recovery delivery budget exceeded".into(),
            )],
        }
    }
    fn native_owner(&self) -> Owner {
        // The reserved prefix cannot be declared by a package manifest.
        Owner {
            id: "@native".into(),
            generation: self.ui_session,
        }
    }
    pub(super) fn resource_request(
        &mut self,
        scope: loog_workspace_api::ScopeId,
        operation: loog_workspace_api::Operation,
    ) -> Result<loog_workspace_api::RequestId, Failure> {
        self.workspace_router.submit(
            &self.native_owner(),
            scope,
            operation,
            (self.clock)(),
            30_000,
        )
    }
    pub(super) fn resource_cancel(&mut self, request: loog_workspace_api::RequestId) {
        let _ = self.workspace_router.cancel(&self.native_owner(), request);
    }
    pub(super) fn workspace_submissions(
        &mut self,
        app: &AppState,
        id: &str,
        generation: u64,
        submissions: Vec<Submitted>,
    ) -> Vec<Effect> {
        if !self
            .packages
            .get(id)
            .is_some_and(|package| package.lifecycle.accepts(generation))
        {
            return Vec::new();
        }
        let owner = Owner {
            id: id.into(),
            generation,
        };
        let mut effects = Vec::new();
        for submitted in submissions {
            let result = match submitted.command {
                Command::ConfigureLanguageProviders { configurations } => self
                    .configure_resource_providers(&owner, configurations)
                    .map(|(reply, changed)| {
                        effects.extend(changed);
                        reply
                    }),
                Command::CancelWorkspaceEdit { ticket } => {
                    self.cancel_resource_edit(&owner, ticket)
                }
                Command::ApplyWorkspaceEdit {
                    origin,
                    title,
                    documents,
                } => {
                    match self.prepare_resource_edit(
                        app,
                        &owner,
                        submitted.ticket,
                        origin,
                        title,
                        documents,
                    ) {
                        Ok(effect) => {
                            effects.push(effect);
                            continue;
                        }
                        Err(error) => Err(error),
                    }
                }
                Command::ReportServices { reports } => {
                    self.report_resource_services(&owner, reports)
                }
                Command::BindLanguageProviders { providers } => {
                    self.bind_resource_providers(&owner, providers)
                }
                Command::ProvideLocations {
                    request,
                    stamp,
                    locations,
                } => self
                    .provide_resource_locations(app, &owner, request, stamp, locations)
                    .map(|replies| {
                        effects.extend(replies);
                        Reply::Done
                    }),
                Command::ProvideWorkspaceEdit {
                    request,
                    stamp,
                    documents,
                } => self
                    .provide_resource_edit(app, &owner, request, stamp, documents)
                    .map(|replies| {
                        effects.extend(replies);
                        Reply::Done
                    }),
                Command::ProvideCodeActions {
                    request,
                    stamp,
                    actions,
                } => self
                    .provide_resource_actions(app, &owner, request, stamp, actions)
                    .map(|replies| {
                        effects.extend(replies);
                        Reply::Done
                    }),
                Command::FinishLanguageRequest {
                    request,
                    stamp,
                    error,
                } => self
                    .finish_resource_language(app, &owner, request, stamp, error)
                    .map(|replies| {
                        effects.extend(replies);
                        Reply::Done
                    }),
                Command::PublishDiagnostics {
                    provider,
                    stamp,
                    diagnostics,
                } => self
                    .publish_resource_diagnostics(app, &owner, &provider, stamp, diagnostics)
                    .map(|publication| {
                        effects.push(publication);
                        Reply::Done
                    }),
                command @ (Command::Documents { .. } | Command::ReadDocument { .. }) => {
                    self.resource_document_command(app, &owner, command)
                }
                command => self.workspace_command(&owner, command),
            };
            let event = WorkspaceEvent::Reply {
                ticket: submitted.ticket,
                result,
            };
            if let Some(credit) = self.workspace_budget.reserve(1024 * 1024) {
                effects.push(Effect::Submit(Job::WorkspaceEvent {
                    id: id.into(),
                    generation,
                    event,
                    _credit: credit,
                }));
            } else {
                effects.extend(self.failed(
                    id,
                    generation,
                    ExtensionError::new(
                        ErrorKind::LimitExceeded,
                        "workspace delivery budget exceeded",
                    ),
                ));
                break;
            }
        }
        effects.extend(self.pump_workspaces());
        effects
    }
    pub(super) fn workspace_command(
        &mut self,
        owner: &Owner,
        command: Command,
    ) -> Result<Reply, Failure> {
        let package = self
            .packages
            .get(&owner.id)
            .ok_or_else(|| Failure::new(FailureKind::Unavailable, "extension stopped"))?;
        if package.lifecycle.generation() != owner.generation {
            return Err(Failure::new(
                FailureKind::PermissionDenied,
                "Stale provider generation",
            ));
        }
        let capability = match &command {
            Command::ResolveSettings { scope, project } => {
                return self.resolve_workspace_settings(owner, *scope, project.as_ref());
            }
            Command::ReportServices { .. }
            | Command::ConfigureLanguageProviders { .. }
            | Command::ApplyWorkspaceEdit { .. }
            | Command::CancelWorkspaceEdit { .. }
            | Command::BindLanguageProviders { .. }
            | Command::ProvideLocations { .. }
            | Command::ProvideWorkspaceEdit { .. }
            | Command::ProvideCodeActions { .. }
            | Command::FinishLanguageRequest { .. }
            | Command::Documents { .. }
            | Command::ReadDocument { .. }
            | Command::PublishDiagnostics { .. } => {
                return Err(Failure::new(
                    FailureKind::InvalidRequest,
                    "Document state is required",
                ));
            }
            Command::Roots { .. } => "workspace.files.read",
            Command::Register { .. }
            | Command::Unregister { .. }
            | Command::Mount { .. }
            | Command::Present { .. }
            | Command::Remember { .. }
            | Command::Forget { .. }
            | Command::Complete { .. }
            | Command::Notify { .. } => "workspace.provide",
            Command::Request { operation, .. } => consumer_capability(operation.capability()),
            Command::Cancel { request } => {
                self.workspace_router.cancel(owner, *request)?;
                return Ok(Reply::Done);
            }
        };
        if !package
            .manifest
            .capabilities
            .iter()
            .any(|value| value == capability)
            || self
                .grants
                .authorize(&package.identity, capability, None)
                .is_err()
        {
            return Err(Failure::new(
                FailureKind::PermissionDenied,
                "workspace capability is not granted",
            ));
        }
        match command {
            Command::ResolveSettings { .. } => unreachable!(),
            Command::ReportServices { .. }
            | Command::ConfigureLanguageProviders { .. }
            | Command::ApplyWorkspaceEdit { .. }
            | Command::CancelWorkspaceEdit { .. }
            | Command::BindLanguageProviders { .. }
            | Command::ProvideLocations { .. }
            | Command::ProvideWorkspaceEdit { .. }
            | Command::ProvideCodeActions { .. }
            | Command::FinishLanguageRequest { .. }
            | Command::Documents { .. }
            | Command::ReadDocument { .. }
            | Command::PublishDiagnostics { .. } => unreachable!(),
            Command::Roots { after, limit } => {
                if !(1..=32).contains(&limit) {
                    return Err(Failure::new(
                        FailureKind::InvalidRequest,
                        "Root page size must be 1..=32",
                    ));
                }
                let available = self.consumer_roots(owner);
                self.workspace_consumers
                    .entry(owner.clone())
                    .or_insert_with(|| {
                        available
                            .iter()
                            .map(|root| (root.scope, root.capabilities.clone()))
                            .collect()
                    });
                let mut roots = Vec::new();
                let mut bytes = 0;
                let mut more = false;
                for root in available
                    .into_iter()
                    .filter(|root| after.is_none_or(|after| root.scope > after))
                {
                    let size = serde_json::to_vec(&root).expect("validated root").len();
                    if roots.len() >= limit as usize || bytes + size > 256 * 1024 {
                        more = true;
                        break;
                    }
                    bytes += size;
                    roots.push(root);
                }
                let next = more.then(|| roots.last().map(|root| root.scope)).flatten();
                Ok(Reply::Roots { roots, next })
            }
            Command::Remember { scope, state } => {
                let root = self
                    .workspace_roots
                    .iter()
                    .find(|root| root.scope == scope)
                    .ok_or_else(|| {
                        Failure::new(FailureKind::Unavailable, "Resource root is not presented")
                    })?;
                if root
                    .resource
                    .environment()
                    .is_none_or(|key| key.provider() != owner.id)
                {
                    return Err(Failure::new(
                        FailureKind::PermissionDenied,
                        "Resource belongs to another provider",
                    ));
                }
                let bookmark = loog_workspace_api::session::ResourceBookmark {
                    resource: root.resource.clone(),
                    name: root.name.clone(),
                    state,
                };
                bookmark
                    .validate()
                    .map_err(|error| Failure::new(FailureKind::InvalidRequest, error))?;
                if let Some(old) = self
                    .workspace_bookmarks
                    .iter_mut()
                    .find(|old| old.resource == bookmark.resource)
                {
                    *old = bookmark;
                } else {
                    if self.workspace_bookmarks.len() >= 128 {
                        return Err(Failure::new(
                            FailureKind::LimitExceeded,
                            "Too many saved resource roots",
                        ));
                    }
                    self.workspace_bookmarks.push(bookmark);
                }
                self.workspace_bookmarks_dirty = true;
                Ok(Reply::Done)
            }
            Command::Forget { resource } => {
                if resource
                    .environment()
                    .is_none_or(|key| key.provider() != owner.id)
                {
                    return Err(Failure::new(
                        FailureKind::PermissionDenied,
                        "Resource belongs to another provider",
                    ));
                }
                self.workspace_bookmarks
                    .retain(|bookmark| bookmark.resource != resource);
                self.workspace_bookmarks_dirty = true;
                Ok(Reply::Done)
            }
            Command::Register { descriptor } => {
                let id = self.workspace_router.register(owner.clone(), descriptor)?;
                Ok(Reply::Environment { id })
            }
            Command::Mount { environment, root } => {
                let id = self.workspace_router.mount(owner, environment, root)?;
                Ok(Reply::Scope { id })
            }
            Command::Present { scope } => {
                let (environment, path) = self.workspace_router.scope(scope).ok_or_else(|| {
                    Failure::new(FailureKind::Unavailable, "scope is unavailable")
                })?;
                if self.workspace_router.provider(environment) != Some(owner) {
                    return Err(Failure::new(
                        FailureKind::PermissionDenied,
                        "scope belongs to another provider",
                    ));
                }
                let descriptor = self.workspace_router.environment(environment).unwrap();
                let root = loog_workspace_api::PresentedRoot {
                    scope,
                    resource: loog_workspace_api::ResourceRef::Provided {
                        environment: descriptor.key.clone(),
                        path: path.clone(),
                    },
                    name: descriptor.name.clone(),
                    capabilities: descriptor.capabilities.clone(),
                    badge: descriptor.badge.clone(),
                };
                // Presenting files does not authorize executing workspace code.
                let caps = root
                    .capabilities
                    .iter()
                    .copied()
                    .filter(|cap| {
                        matches!(
                            cap,
                            Capability::FilesRead
                                | Capability::FilesWrite
                                | Capability::Watch
                                | Capability::Search
                        )
                    })
                    .collect::<Vec<_>>();
                self.workspace_router
                    .grant(self.native_owner(), scope, caps)?;
                if !self.workspace_roots.iter().any(|r| r.scope == scope) {
                    self.workspace_roots.push(root);
                }
                Ok(Reply::Done)
            }
            Command::Unregister { environment } => {
                self.workspace_router.unregister(owner, environment)?;
                Ok(Reply::Done)
            }
            Command::Request {
                scope,
                operation,
                timeout_ms,
            } => {
                let id = self.workspace_router.submit(
                    owner,
                    scope,
                    operation,
                    (self.clock)(),
                    u64::from(timeout_ms),
                )?;
                Ok(Reply::Request { id })
            }
            Command::Complete { request, result } => {
                self.workspace_router.complete(owner, request, result)?;
                Ok(Reply::Done)
            }
            Command::Notify { scope, event } => {
                self.workspace_router.notify(owner, scope, event)?;
                Ok(Reply::Done)
            }
            Command::Cancel { .. } => unreachable!(),
        }
    }
    pub(super) fn pump_workspaces(&mut self) -> Vec<Effect> {
        let _ = self.workspace_router.expire((self.clock)());
        let mut effects = Vec::new();
        if std::mem::take(&mut self.workspace_bookmarks_dirty) {
            effects.push(Effect::ResourceBookmarks(self.workspace_bookmarks.clone()));
        }
        self.workspace_roots
            .retain(|root| self.workspace_router.scope(root.scope).is_some());
        effects.extend(self.refresh_resource_grants());
        effects.extend(self.publish_settings_notices());
        effects.push(Effect::ResourceRoots(self.workspace_roots.clone()));
        for owner in self.workspace_router.ready_owners() {
            if owner == self.native_owner() {
                effects.extend(
                    self.workspace_router
                        .poll(&owner, 32)
                        .into_iter()
                        .map(Effect::ResourceDelivery),
                );
                continue;
            }
            if !self
                .packages
                .get(&owner.id)
                .is_some_and(|package| package.lifecycle.accepts(owner.generation))
            {
                self.workspace_router.retire(&owner);
                continue;
            }
            for _ in 0..32 {
                let Some(credit) = self.workspace_budget.reserve(1024 * 1024) else {
                    break;
                };
                let Some(event) = self.workspace_router.poll(&owner, 1).pop() else {
                    break;
                };
                effects.push(Effect::Submit(Job::WorkspaceEvent {
                    id: owner.id.clone(),
                    generation: owner.generation,
                    event: WorkspaceEvent::Delivery { event },
                    _credit: credit,
                }));
            }
        }
        if self.workspace_router.pending_count() > 0
            || !self.workspace_router.ready_owners().is_empty()
        {
            effects.push(Effect::Submit(Job::WorkspaceWakeAfter { milliseconds: 16 }));
        }
        effects
    }
    pub(super) fn retire_workspaces(&mut self, id: &str) {
        self.workspace_settings.retain(|owner, _| owner.id != id);
        self.resource_documents.retire(id);
        self.resource_configurations.retire(id);
        self.workspace_consumers.retain(|owner, _| owner.id != id);
        for owner in self
            .workspace_router
            .owners()
            .into_iter()
            .filter(|owner| owner.id == id)
        {
            self.workspace_router.retire(&owner);
        }
    }
}
