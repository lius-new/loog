//! Resource-aware buffer access, separate from the native-path document ABI.
//! Metadata is retained; text is read from the editor only for a bounded request.
use super::*;
use loog_workspace_api::session::{
    Command, DocumentResource, DocumentVersion, Event as WorkspaceEvent, Reply,
};
use loog_workspace_api::{Failure, FailureKind, Owner};
use std::collections::BTreeMap;

struct Entry {
    file: FileId,
    incarnation: u64,
    language_generation: u64,
    descriptor: DocumentResource,
}

struct PendingEdit {
    resource: loog_workspace_api::ResourceRef,
    file: FileId,
    stamp: DocumentStamp,
    deadline: u64,
    cancelled: bool,
}

#[derive(Default)]
pub(super) struct Documents {
    entries: BTreeMap<u64, Entry>,
    next: u64,
    revision: u64,
    // Zero means a notice is already pending. A query acknowledges it.
    subscribers: BTreeMap<Owner, u64>,
    providers: BTreeMap<Owner, std::collections::BTreeSet<String>>,
    reports: BTreeMap<Owner, Vec<loog_workspace_api::session::ResourceServiceReport>>,
    edits: BTreeMap<(Owner, u64), PendingEdit>,
}

impl Documents {
    pub(super) fn retire(&mut self, id: &str) {
        self.edits.retain(|(owner, _), _| owner.id != id);
        self.subscribers.retain(|owner, _| owner.id != id);
        self.providers.retain(|owner, _| owner.id != id);
        self.reports.retain(|owner, _| owner.id != id);
    }

    pub(super) fn reports(
        &self,
        owner: &Owner,
        document: Option<&loog_workspace_api::ResourceRef>,
    ) -> Vec<&loog_workspace_api::session::ResourceServiceReport> {
        self.reports
            .get(owner)
            .into_iter()
            .flatten()
            .filter(|report| match (&report.contexts, document) {
                (None, _) => true,
                (Some(contexts), Some(document)) => contexts.contains(document),
                (Some(_), None) => false,
            })
            .collect()
    }

    pub(super) fn language(&self, resource: &loog_workspace_api::ResourceRef) -> Option<&str> {
        self.entries
            .values()
            .find(|entry| &entry.descriptor.resource == resource)
            .map(|entry| entry.descriptor.language.as_str())
    }
    pub(super) fn bound(&self, owner: &Owner, provider: &str) -> bool {
        self.providers
            .get(owner)
            .is_some_and(|providers| providers.contains(provider))
    }
    pub(super) fn request(&self, file: FileId, stamp: DocumentStamp) -> Option<DocumentResource> {
        self.entries
            .values()
            .find(|entry| {
                entry.file == file
                    && entry.incarnation == stamp.incarnation
                    && entry.language_generation == stamp.language_generation
                    && entry.descriptor.stamp.version == stamp.version
                    && entry.descriptor.synchronized
            })
            .map(|entry| entry.descriptor.clone())
    }

    fn sync(&mut self, app: &AppState) {
        let mut previous = std::mem::take(&mut self.entries)
            .into_values()
            .map(|entry| (entry.file, entry))
            .collect::<HashMap<_, _>>();
        let mut changed = false;
        for (file, document) in app.workspace.documents.iter().filter(|(_, d)| d.is_text()) {
            let old = previous.remove(file);
            let id = old.as_ref().map_or_else(
                || {
                    self.next += 1;
                    self.next
                },
                |entry| entry.descriptor.stamp.document,
            );
            let synchronized = document.buffer.len()
                <= app
                    .preferences
                    .values()
                    .language_features
                    .max_document_bytes;
            let lifecycle = old.as_ref().map_or(1, |entry| {
                entry.descriptor.stamp.lifecycle
                    + u64::from(
                        entry.incarnation != document.incarnation
                            || entry.language_generation != document.meta.language_generation
                            || entry.descriptor.resource != document.meta.resource
                            || entry.descriptor.synchronized != synchronized,
                    )
            });
            let descriptor = DocumentResource {
                stamp: DocumentVersion {
                    document: id,
                    lifecycle,
                    version: document.buffer.version(),
                },
                resource: document.meta.resource.clone(),
                language: document.meta.lang.definition().name.to_string(),
                variant: document.meta.lang.variant_name().to_owned(),
                byte_length: document.buffer.len() as u64,
                synchronized,
            };
            changed |= old
                .as_ref()
                .is_none_or(|entry| entry.descriptor != descriptor);
            self.entries.insert(
                id,
                Entry {
                    file: *file,
                    incarnation: document.incarnation,
                    language_generation: document.meta.language_generation,
                    descriptor,
                },
            );
        }
        if changed || !previous.is_empty() || self.revision == 0 {
            self.revision += 1;
        }
    }

    fn command(
        &mut self,
        app: &AppState,
        owner: &Owner,
        command: Command,
    ) -> Result<Reply, Failure> {
        self.sync(app);
        match command {
            Command::Documents {
                after,
                revision,
                limit,
            } => {
                if !(1..=32).contains(&limit) || (after.is_some() && revision.is_none()) {
                    return Err(Failure::new(
                        FailureKind::InvalidRequest,
                        "Document pages require 1..=32 items and a revision after the first page",
                    ));
                }
                if revision.is_some_and(|revision| revision != self.revision) {
                    return Err(Failure::new(
                        FailureKind::Conflict,
                        "Open documents changed; restart pagination",
                    ));
                }
                let mut documents = Vec::new();
                let mut bytes = 0;
                let mut more = false;
                for (id, entry) in self
                    .entries
                    .iter()
                    .filter(|(id, _)| after.is_none_or(|after| **id > after))
                {
                    let size = serde_json::to_vec(&entry.descriptor)
                        .map_err(|_| {
                            Failure::new(
                                FailureKind::Unsupported,
                                "Document identity cannot be serialized",
                            )
                        })?
                        .len();
                    if size > 256 * 1024 {
                        return Err(Failure::new(
                            FailureKind::LimitExceeded,
                            "Document identity exceeds the page budget",
                        ));
                    }
                    if documents.len() >= limit as usize || bytes + size > 256 * 1024 {
                        more = true;
                        break;
                    }
                    debug_assert_eq!(*id, entry.descriptor.stamp.document);
                    bytes += size;
                    documents.push(entry.descriptor.clone());
                }
                let next = more
                    .then(|| documents.last().map(|doc| doc.stamp.document))
                    .flatten();
                self.subscribers.insert(owner.clone(), self.revision);
                Ok(Reply::Documents {
                    documents,
                    revision: self.revision,
                    next,
                })
            }
            Command::ReadDocument {
                stamp,
                offset,
                limit,
            } => {
                if !(4..=64 * 1024).contains(&limit) {
                    return Err(Failure::new(
                        FailureKind::InvalidRequest,
                        "Document chunk size must be 4..=65536 bytes",
                    ));
                }
                let entry = self
                    .entries
                    .get(&stamp.document)
                    .filter(|entry| entry.descriptor.stamp == stamp)
                    .ok_or_else(|| {
                        Failure::new(
                            FailureKind::Conflict,
                            "Document revision is no longer current",
                        )
                    })?;
                if !entry.descriptor.synchronized {
                    return Err(Failure::new(
                        FailureKind::LimitExceeded,
                        "Document exceeds the synchronization budget",
                    ));
                }
                let text = app.workspace.documents[&entry.file].buffer.text();
                let start = usize::try_from(offset)
                    .ok()
                    .filter(|start| text.is_char_boundary(*start))
                    .ok_or_else(|| {
                        Failure::new(
                            FailureKind::InvalidRequest,
                            "Document offset is not a UTF-8 boundary",
                        )
                    })?;
                let mut end = start.saturating_add(limit as usize).min(text.len());
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                Ok(Reply::DocumentChunk {
                    stamp,
                    text: text[start..end].to_owned(),
                    next: (end < text.len()).then_some(end as u64),
                })
            }
            _ => Err(Failure::new(
                FailureKind::InvalidRequest,
                "Expected a document command",
            )),
        }
    }
}

fn validate_edits(
    state: &Documents,
    environment: &loog_workspace_api::EnvironmentKey,
    documents: &[loog_workspace_api::session::ResourceDocumentEdit],
) -> Result<(), Failure> {
    if documents.len() > 16
        || documents.iter().map(|d| d.edits.len()).sum::<usize>() > 1024
        || documents
            .iter()
            .flat_map(|d| &d.edits)
            .map(|e| e.new_text.len())
            .sum::<usize>()
            > 512 * 1024
        || serde_json::to_vec(documents).map_or(true, |bytes| bytes.len() > 1024 * 1024)
    {
        return Err(Failure::new(
            FailureKind::LimitExceeded,
            "Resource edit budget exceeded",
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for document in documents {
        if document.resource.environment() != Some(environment)
            || !seen.insert(&document.resource)
            || document.edits.iter().any(|e| e.range.start > e.range.end)
        {
            return Err(Failure::new(
                FailureKind::InvalidRequest,
                "Invalid edit range or resource identity",
            ));
        }
        if let Some(version) = document.version {
            if state.entries.get(&version.document).is_none_or(|entry| {
                entry.descriptor.resource != document.resource || entry.descriptor.stamp != version
            }) {
                return Err(Failure::new(
                    FailureKind::Conflict,
                    "An edited document revision is no longer current",
                ));
            }
        }
    }
    Ok(())
}

impl ExtensionService {
    pub(super) fn prepare_resource_edit(
        &mut self,
        app: &AppState,
        owner: &Owner,
        ticket: u64,
        origin: DocumentVersion,
        title: String,
        documents: Vec<loog_workspace_api::session::ResourceDocumentEdit>,
    ) -> Result<Effect, Failure> {
        let package = self
            .packages
            .get(&owner.id)
            .filter(|p| p.lifecycle.accepts(owner.generation))
            .ok_or_else(|| Failure::new(FailureKind::Unavailable, "Edit activation ended"))?;
        if ["documents.read", "workspace.edit"]
            .iter()
            .any(|c| self.authorize(package, c).is_err())
        {
            return Err(Failure::new(
                FailureKind::PermissionDenied,
                "Resource edits are not granted",
            ));
        }
        if title.is_empty() || title.len() > 256 || title.contains(['\0', '\r', '\n']) {
            return Err(Failure::new(
                FailureKind::InvalidRequest,
                "Invalid resource edit title",
            ));
        }
        if self.resource_documents.edits.len() >= 4
            || self
                .resource_documents
                .edits
                .contains_key(&(owner.clone(), ticket))
        {
            return Err(Failure::new(
                FailureKind::LimitExceeded,
                "Too many pending resource edits",
            ));
        }
        self.resource_documents.sync(app);
        let entry = self
            .resource_documents
            .entries
            .get(&origin.document)
            .filter(|entry| entry.descriptor.stamp == origin)
            .ok_or_else(|| {
                Failure::new(
                    FailureKind::Conflict,
                    "Edit origin revision is no longer current",
                )
            })?;
        let environment = entry.descriptor.resource.environment().ok_or_else(|| {
            Failure::new(
                FailureKind::InvalidRequest,
                "Expected a provided edit origin",
            )
        })?;
        validate_edits(&self.resource_documents, environment, &documents)?;
        self.resource_documents.edits.insert(
            (owner.clone(), ticket),
            PendingEdit {
                resource: entry.descriptor.resource.clone(),
                file: entry.file,
                stamp: app.workspace.documents[&entry.file].stamp(entry.file),
                deadline: (self.clock)().saturating_add(30_000),
                cancelled: false,
            },
        );
        Ok(Effect::ResourceWorkspaceEdit {
            owner: owner.clone(),
            ticket,
            title,
            edit: crate::features::language_service::reply::WorkspaceEdit {
                changes: documents
                    .into_iter()
                    .map(crate::features::language_service::reply::Change::ResourceText)
                    .collect(),
            },
            stamps: crate::app::language_ui::edit::stamps(app),
        })
    }
    pub(super) fn resource_edit_current(&self, app: &AppState, owner: &Owner, ticket: u64) -> bool {
        self.packages.get(&owner.id).is_some_and(|package| {
            package.lifecycle.accepts(owner.generation)
                && ["documents.read", "workspace.edit"]
                    .iter()
                    .all(|c| self.authorize(package, c).is_ok())
        }) && self
            .resource_documents
            .edits
            .get(&(owner.clone(), ticket))
            .is_some_and(|pending| {
                !pending.cancelled
                    && (self.clock)() < pending.deadline
                    && app
                        .workspace
                        .documents
                        .get(&pending.file)
                        .is_some_and(|doc| {
                            doc.meta.resource == pending.resource
                                && doc.stamp(pending.file) == pending.stamp
                        })
            })
    }
    pub(super) fn cancel_resource_edit(
        &mut self,
        owner: &Owner,
        ticket: u64,
    ) -> Result<Reply, Failure> {
        let pending = self
            .resource_documents
            .edits
            .get_mut(&(owner.clone(), ticket))
            .ok_or_else(|| {
                Failure::new(
                    FailureKind::Conflict,
                    "Resource edit already finished or is unknown",
                )
            })?;
        pending.cancelled = true;
        Ok(Reply::Done)
    }
    pub(super) fn finish_resource_edit(
        &mut self,
        owner: &Owner,
        ticket: u64,
        result: Result<Reply, Failure>,
    ) -> Vec<Effect> {
        if self
            .resource_documents
            .edits
            .remove(&(owner.clone(), ticket))
            .is_none()
            || !self
                .packages
                .get(&owner.id)
                .is_some_and(|p| p.lifecycle.accepts(owner.generation))
        {
            return vec![];
        }
        match self.workspace_budget.reserve(1024 * 1024) {
            Some(credit) => vec![Effect::Submit(Job::WorkspaceEvent {
                id: owner.id.clone(),
                generation: owner.generation,
                event: WorkspaceEvent::Reply { ticket, result },
                _credit: credit,
            })],
            None => self.failed(
                &owner.id,
                owner.generation,
                ExtensionError::new(
                    ErrorKind::LimitExceeded,
                    "Resource edit receipt budget exceeded",
                ),
            ),
        }
    }
    pub(super) fn report_resource_services(
        &mut self,
        owner: &Owner,
        mut reports: Vec<loog_workspace_api::session::ResourceServiceReport>,
    ) -> Result<Reply, Failure> {
        let package = self
            .packages
            .get(&owner.id)
            .filter(|package| package.lifecycle.accepts(owner.generation))
            .ok_or_else(|| Failure::new(FailureKind::Unavailable, "Service activation ended"))?;
        if self.authorize(package, "documents.read").is_err()
            || self.authorize(package, "languages").is_err()
        {
            return Err(Failure::new(
                FailureKind::PermissionDenied,
                "Service reporting is not granted",
            ));
        }
        if reports.len() > 16
            || serde_json::to_vec(&reports).map_or(true, |bytes| bytes.len() > 256 * 1024)
        {
            return Err(Failure::new(
                FailureKind::LimitExceeded,
                "Service report budget exceeded",
            ));
        }
        let mut keys = std::collections::BTreeSet::new();
        for report in &mut reports {
            if report.key.is_empty()
                || report.key.len() > 128
                || report.key.contains('\0')
                || !keys.insert(report.key.clone())
                || report
                    .detail
                    .as_ref()
                    .is_some_and(|text| text.len() > 200 || text.contains(['\r', '\n', '\0']))
                || report
                    .contexts
                    .as_ref()
                    .is_some_and(|contexts| contexts.len() > 128)
            {
                return Err(Failure::new(
                    FailureKind::InvalidRequest,
                    "Invalid service report",
                ));
            }
            if let Some(contexts) = &mut report.contexts {
                let mut seen = std::collections::HashSet::new();
                contexts.retain(|resource| seen.insert(resource.clone()));
            }
        }
        if reports.is_empty() {
            self.resource_documents.reports.remove(owner);
        } else {
            self.resource_documents
                .reports
                .insert(owner.clone(), reports);
        }
        Ok(Reply::Done)
    }

    pub(super) fn provide_resource_locations(
        &mut self,
        app: &AppState,
        owner: &Owner,
        request: u64,
        stamp: DocumentVersion,
        locations: Vec<loog_workspace_api::session::ResourceLocation>,
    ) -> Result<Vec<Effect>, Failure> {
        use loog_workspace_api::session::ColumnEncoding;
        let package = self
            .packages
            .get(&owner.id)
            .filter(|p| p.lifecycle.accepts(owner.generation))
            .ok_or_else(|| Failure::new(FailureKind::Unavailable, "Provider activation ended"))?;
        if self.authorize(package, "documents.read").is_err()
            || self.authorize(package, "languages").is_err()
        {
            return Err(Failure::new(
                FailureKind::PermissionDenied,
                "Language document access is not granted",
            ));
        }
        let call = self
            .provider_calls
            .get(&request)
            .filter(|call| {
                call.extension == owner.id
                    && call.generation == owner.generation
                    && matches!(call.job.kind, WireKind::Definition | WireKind::References)
                    && self.resource_documents.bound(owner, &call.provider)
                    && self.language_calls.contains_key(&request)
            })
            .ok_or_else(|| {
                Failure::new(
                    FailureKind::PermissionDenied,
                    "Resource location request is not owned by this provider",
                )
            })?;
        if (self.clock)() > call.deadline
            || call
                .job
                .resource
                .as_ref()
                .is_none_or(|context| context.stamp != stamp)
            || app
                .workspace
                .documents
                .get(&call.file)
                .is_none_or(|document| {
                    document.meta.resource != call.resource
                        || document.stamp(call.file) != call.stamp
                })
        {
            return Err(Failure::new(
                FailureKind::Conflict,
                "Location source revision is no longer current",
            ));
        }
        let environment = call.resource.environment().ok_or_else(|| {
            Failure::new(
                FailureKind::InvalidRequest,
                "Expected a provided source resource",
            )
        })?;
        if locations.len() > 256
            || serde_json::to_vec(&locations).map_or(true, |bytes| bytes.len() > 256 * 1024)
        {
            return Err(Failure::new(
                FailureKind::LimitExceeded,
                "Location result budget exceeded",
            ));
        }
        if locations.iter().any(|location| {
            location.resource.environment() != Some(environment)
                || location.range.start > location.range.end
        }) {
            return Err(Failure::new(
                FailureKind::InvalidRequest,
                "Invalid location range or target environment",
            ));
        }
        let value = serde_json::Value::Array(locations.into_iter().map(|location| serde_json::json!({
            "resource":location.resource,
            "range":{"start":location.range.start,"end":location.range.end,
                "x-loog-column-unit":match location.range.encoding { ColumnEncoding::Utf8 => "utf8", ColumnEncoding::Utf16 => "utf16" }}
        })).collect());
        let call = self.provider_calls.remove(&request).unwrap();
        let token = self.language_calls.remove(&request).unwrap();
        self.queued.retain(|queued| *queued != request);
        let source = self.source_for(&call.extension, &call.provider);
        Ok(vec![
            Effect::Submit(Job::CancelProvider {
                id: owner.id.clone(),
                generation: owner.generation,
                request,
            }),
            Effect::LanguageAnswer {
                token,
                answer: Some(LanguageAnswer {
                    source,
                    provider: call.provider,
                    value,
                    rank: call.rank,
                }),
            },
        ])
    }
    pub(super) fn provide_resource_edit(
        &mut self,
        app: &AppState,
        owner: &Owner,
        request: u64,
        stamp: DocumentVersion,
        documents: Vec<loog_workspace_api::session::ResourceDocumentEdit>,
    ) -> Result<Vec<Effect>, Failure> {
        let package = self
            .packages
            .get(&owner.id)
            .filter(|p| p.lifecycle.accepts(owner.generation))
            .ok_or_else(|| Failure::new(FailureKind::Unavailable, "Provider activation ended"))?;
        if ["documents.read", "languages", "workspace.edit"]
            .iter()
            .any(|capability| self.authorize(package, capability).is_err())
        {
            return Err(Failure::new(
                FailureKind::PermissionDenied,
                "Resource edits are not granted",
            ));
        }
        self.resource_documents.sync(app);
        let call = self
            .provider_calls
            .get(&request)
            .filter(|call| {
                call.extension == owner.id
                    && call.generation == owner.generation
                    && matches!(call.job.kind, WireKind::Rename)
                    && self.resource_documents.bound(owner, &call.provider)
                    && self.language_calls.contains_key(&request)
            })
            .ok_or_else(|| {
                Failure::new(
                    FailureKind::PermissionDenied,
                    "Resource edit request is not owned by this provider",
                )
            })?;
        if (self.clock)() > call.deadline
            || call
                .job
                .resource
                .as_ref()
                .is_none_or(|context| context.stamp != stamp)
            || app
                .workspace
                .documents
                .get(&call.file)
                .is_none_or(|document| {
                    document.meta.resource != call.resource
                        || document.stamp(call.file) != call.stamp
                })
        {
            return Err(Failure::new(
                FailureKind::Conflict,
                "Edit source revision is no longer current",
            ));
        }
        let environment = call.resource.environment().ok_or_else(|| {
            Failure::new(
                FailureKind::InvalidRequest,
                "Expected a provided source resource",
            )
        })?;
        validate_edits(&self.resource_documents, environment, &documents)?;
        let call = self.provider_calls.remove(&request).unwrap();
        let token = self.language_calls.remove(&request).unwrap();
        self.queued.retain(|queued| *queued != request);
        let source = self.source_for(&call.extension, &call.provider);
        Ok(vec![
            Effect::Submit(Job::CancelProvider {
                id: owner.id.clone(),
                generation: owner.generation,
                request,
            }),
            Effect::LanguageAnswer {
                token,
                answer: Some(LanguageAnswer {
                    source,
                    provider: call.provider,
                    value: serde_json::json!({"resourceChanges":documents}),
                    rank: call.rank,
                }),
            },
        ])
    }
    pub(super) fn finish_resource_language(
        &mut self,
        app: &AppState,
        owner: &Owner,
        request: u64,
        stamp: DocumentVersion,
        error: Option<String>,
    ) -> Result<Vec<Effect>, Failure> {
        let package = self
            .packages
            .get(&owner.id)
            .filter(|p| p.lifecycle.accepts(owner.generation))
            .ok_or_else(|| Failure::new(FailureKind::Unavailable, "Provider activation ended"))?;
        if ["documents.read", "languages"]
            .iter()
            .any(|c| self.authorize(package, c).is_err())
        {
            return Err(Failure::new(
                FailureKind::PermissionDenied,
                "Language document access is not granted",
            ));
        }
        let call = self
            .provider_calls
            .get(&request)
            .filter(|call| {
                call.extension == owner.id
                    && call.generation == owner.generation
                    && self.resource_documents.bound(owner, &call.provider)
                    && self.language_calls.contains_key(&request)
                    && call.resource.environment().is_some()
            })
            .ok_or_else(|| {
                Failure::new(
                    FailureKind::PermissionDenied,
                    "Resource request is not owned by this provider",
                )
            })?;
        let command = matches!(call.job.kind, WireKind::ExecuteCommand);
        if error
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 1024 || s.contains('\0'))
            || error.is_none() && !command
        {
            return Err(Failure::new(
                FailureKind::InvalidRequest,
                "Expected a bounded failure or command completion",
            ));
        }
        if (self.clock)() > call.deadline
            || call.job.resource.as_ref().is_none_or(|c| c.stamp != stamp)
            || app.workspace.documents.get(&call.file).is_none_or(|doc| {
                let current = doc.stamp(call.file);
                doc.meta.resource != call.resource
                    || if command {
                        current.incarnation != call.stamp.incarnation
                            || current.language_generation != call.stamp.language_generation
                    } else {
                        current != call.stamp
                    }
            })
        {
            return Err(Failure::new(
                FailureKind::Conflict,
                "Provider request is no longer current",
            ));
        }
        let call = self.provider_calls.remove(&request).unwrap();
        let token = self.language_calls.remove(&request).unwrap();
        self.queued.retain(|queued| *queued != request);
        let source = self.source_for(&call.extension, &call.provider);
        let answer = match error {
            Some(message) => Effect::LanguageFailure { token, message },
            None => Effect::LanguageAnswer {
                token,
                answer: Some(LanguageAnswer {
                    source,
                    provider: call.provider,
                    value: serde_json::Value::Null,
                    rank: call.rank,
                }),
            },
        };
        Ok(vec![
            Effect::Submit(Job::CancelProvider {
                id: owner.id.clone(),
                generation: owner.generation,
                request,
            }),
            answer,
        ])
    }
    pub(super) fn provide_resource_actions(
        &mut self,
        app: &AppState,
        owner: &Owner,
        request: u64,
        stamp: DocumentVersion,
        actions: Vec<loog_workspace_api::session::ResourceCodeAction>,
    ) -> Result<Vec<Effect>, Failure> {
        let package = self
            .packages
            .get(&owner.id)
            .filter(|p| p.lifecycle.accepts(owner.generation))
            .ok_or_else(|| Failure::new(FailureKind::Unavailable, "Provider activation ended"))?;
        if ["documents.read", "languages", "workspace.edit"]
            .iter()
            .any(|capability| self.authorize(package, capability).is_err())
        {
            return Err(Failure::new(
                FailureKind::PermissionDenied,
                "Resource actions are not granted",
            ));
        }
        self.resource_documents.sync(app);
        let call = self
            .provider_calls
            .get(&request)
            .filter(|call| {
                call.extension == owner.id
                    && call.generation == owner.generation
                    && matches!(
                        call.job.kind,
                        WireKind::CodeActions | WireKind::CodeActionResolve
                    )
                    && self.resource_documents.bound(owner, &call.provider)
                    && self.language_calls.contains_key(&request)
            })
            .ok_or_else(|| {
                Failure::new(
                    FailureKind::PermissionDenied,
                    "Resource action request is not owned by this provider",
                )
            })?;
        if (self.clock)() > call.deadline
            || call
                .job
                .resource
                .as_ref()
                .is_none_or(|context| context.stamp != stamp)
            || app
                .workspace
                .documents
                .get(&call.file)
                .is_none_or(|document| {
                    document.meta.resource != call.resource
                        || document.stamp(call.file) != call.stamp
                })
        {
            return Err(Failure::new(
                FailureKind::Conflict,
                "Action source revision is no longer current",
            ));
        }
        let environment = call.resource.environment().ok_or_else(|| {
            Failure::new(
                FailureKind::InvalidRequest,
                "Expected a provided source resource",
            )
        })?;
        let resolving = matches!(call.job.kind, WireKind::CodeActionResolve);
        if actions.len() > if resolving { 1 } else { 64 }
            || serde_json::to_vec(&actions).map_or(true, |v| v.len() > 768 * 1024)
        {
            return Err(Failure::new(
                FailureKind::LimitExceeded,
                "Code action result budget exceeded",
            ));
        }
        let text = |s: &str, max: usize| {
            !s.is_empty() && s.len() <= max && !s.contains(['\r', '\n', '\0'])
        };
        for action in &actions {
            if !text(&action.title, 1024)
                || action.kind.as_ref().is_some_and(|s| !text(s, 256))
                || action.disabled.as_ref().is_some_and(|s| !text(s, 1024))
                || action.data.as_ref().is_some_and(|s| s.len() > 64 * 1024)
                || action
                    .command
                    .as_ref()
                    .is_some_and(|c| !text(&c.name, 256) || c.arguments.len() > 64)
            {
                return Err(Failure::new(
                    FailureKind::InvalidRequest,
                    "Invalid code action metadata",
                ));
            }
            if let Some(documents) = &action.documents {
                validate_edits(&self.resource_documents, environment, documents)?;
            }
        }
        let mut values = actions.into_iter().map(|action| {
            let mut value = serde_json::json!({"title":action.title,"kind":action.kind,
                "isPreferred":action.preferred,"data":action.data});
            if let Some(reason) = action.disabled { value["disabled"] = serde_json::json!({"reason":reason}); }
            if let Some(documents) = action.documents { value["edit"] = serde_json::json!({"resourceChanges":documents}); }
            if let Some(command) = action.command { value["command"] = serde_json::json!({"title":command.name,"command":command.name,"arguments":command.arguments}); }
            value
        }).collect::<Vec<_>>();
        let value = if resolving {
            values.pop().unwrap_or(serde_json::Value::Null)
        } else {
            values.into()
        };
        let call = self.provider_calls.remove(&request).unwrap();
        let token = self.language_calls.remove(&request).unwrap();
        self.queued.retain(|queued| *queued != request);
        let source = self.source_for(&call.extension, &call.provider);
        Ok(vec![
            Effect::Submit(Job::CancelProvider {
                id: owner.id.clone(),
                generation: owner.generation,
                request,
            }),
            Effect::LanguageAnswer {
                token,
                answer: Some(LanguageAnswer {
                    source,
                    provider: call.provider,
                    value,
                    rank: call.rank,
                }),
            },
        ])
    }
    pub(super) fn bind_resource_providers(
        &mut self,
        owner: &Owner,
        providers: Vec<String>,
    ) -> Result<Reply, Failure> {
        let package = self
            .packages
            .get(&owner.id)
            .filter(|p| p.lifecycle.accepts(owner.generation))
            .ok_or_else(|| Failure::new(FailureKind::Unavailable, "Provider activation ended"))?;
        if self.authorize(package, "documents.read").is_err()
            || self.authorize(package, "languages").is_err()
        {
            return Err(Failure::new(
                FailureKind::PermissionDenied,
                "Language document access is not granted",
            ));
        }
        if providers.len() > 128
            || providers.iter().any(|id| {
                !package
                    .manifest
                    .contributes
                    .language_service
                    .providers
                    .iter()
                    .any(|p| &p.id == id)
            })
        {
            return Err(Failure::new(
                FailureKind::InvalidRequest,
                "Expected at most 128 declared language providers",
            ));
        }
        self.resource_documents
            .providers
            .insert(owner.clone(), providers.into_iter().collect());
        Ok(Reply::Done)
    }
    pub(super) fn resource_provider_bound(&self, owner: &str, provider: &str) -> bool {
        self.packages.get(owner).is_some_and(|package| {
            package.lifecycle.state() == State::Active
                && self.resource_documents.bound(
                    &Owner {
                        id: owner.into(),
                        generation: package.lifecycle.generation(),
                    },
                    provider,
                )
        })
    }
    pub(super) fn describe_resource_document(
        &mut self,
        app: &AppState,
        file: FileId,
    ) -> Option<DocumentResource> {
        self.resource_documents.sync(app);
        self.resource_documents
            .request(file, app.workspace.documents.get(&file)?.stamp(file))
    }
    pub(super) fn publish_resource_diagnostics(
        &mut self,
        app: &AppState,
        owner: &Owner,
        provider: &str,
        stamp: DocumentVersion,
        diagnostics: Vec<loog_workspace_api::session::ResourceDiagnostic>,
    ) -> Result<Effect, Failure> {
        let package = self
            .packages
            .get(&owner.id)
            .filter(|package| {
                package.lifecycle.accepts(owner.generation)
                    && documents::allowed(&self.grants, package)
                    && package
                        .manifest
                        .contributes
                        .language_service
                        .providers
                        .iter()
                        .any(|decl| decl.id == provider && decl.kind == DeclKind::Diagnostics)
            })
            .ok_or_else(|| {
                Failure::new(
                    FailureKind::PermissionDenied,
                    "Diagnostics provider is not enabled or granted",
                )
            })?;
        self.resource_documents.sync(app);
        let entry = self
            .resource_documents
            .entries
            .get(&stamp.document)
            .filter(|entry| entry.descriptor.stamp == stamp && entry.descriptor.synchronized)
            .ok_or_else(|| {
                Failure::new(
                    FailureKind::Conflict,
                    "Diagnostic document revision is no longer current",
                )
            })?;
        if self
            .provider_configuration(package, provider, Some(&entry.descriptor.resource))
            .is_some_and(|config| !config.enabled)
        {
            return Err(Failure::new(
                FailureKind::PermissionDenied,
                "Diagnostics provider is disabled",
            ));
        }
        if !package
            .manifest
            .contributes
            .language_service
            .providers
            .iter()
            .any(|decl| {
                decl.id == provider && decl.selector.languages.contains(&entry.descriptor.language)
            })
        {
            return Err(Failure::new(
                FailureKind::PermissionDenied,
                "Diagnostics provider does not serve this language",
            ));
        }
        if diagnostics.len() > 2048 {
            return Err(Failure::new(
                FailureKind::LimitExceeded,
                "Too many diagnostics",
            ));
        }
        let document = &app.workspace.documents[&entry.file];
        let text = document.buffer.text_snapshot();
        let mut reported = Vec::with_capacity(diagnostics.len());
        let mut bytes = 0usize;
        for diagnostic in diagnostics {
            let start = usize::try_from(diagnostic.start).ok();
            let end = usize::try_from(diagnostic.end).ok();
            let (Some(start), Some(end)) = (start, end) else {
                return Err(Failure::new(
                    FailureKind::InvalidRequest,
                    "Invalid diagnostic byte range",
                ));
            };
            if start > end
                || end > text.len()
                || !text.is_char_boundary(start)
                || !text.is_char_boundary(end)
                || !(1..=4).contains(&diagnostic.severity)
            {
                return Err(Failure::new(
                    FailureKind::InvalidRequest,
                    "Invalid diagnostic range or severity",
                ));
            }
            bytes = bytes
                .saturating_add(diagnostic.message.len())
                .saturating_add(diagnostic.source.as_ref().map_or(0, String::len))
                .saturating_add(diagnostic.code.as_ref().map_or(0, String::len))
                .saturating_add(diagnostic.documentation_url.as_ref().map_or(0, String::len));
            if bytes > 512 * 1024
                || diagnostic.message.len() > 16 * 1024
                || diagnostic.source.as_ref().is_some_and(|s| s.len() > 1024)
                || diagnostic.code.as_ref().is_some_and(|s| s.len() > 1024)
                || diagnostic
                    .documentation_url
                    .as_ref()
                    .is_some_and(|s| s.len() > 4096)
            {
                return Err(Failure::new(
                    FailureKind::LimitExceeded,
                    "Diagnostic text exceeds the publication budget",
                ));
            }
            reported.push(Reported {
                start,
                end,
                severity: diagnostic.severity,
                message: diagnostic.message,
                source: diagnostic.source,
                code: diagnostic.code,
                documentation_url: diagnostic.documentation_url,
            });
        }
        let converted = providers::convert(&text, &reported, provider);
        let native_stamp = document.stamp(entry.file);
        let resource = document.meta.resource.clone();
        let source = self.source_for(&owner.id, provider);
        Ok(Effect::PublishDiagnostics {
            source,
            resource,
            stamp: native_stamp,
            text,
            items: converted.diagnostics,
        })
    }

    pub(super) fn resource_document_command(
        &mut self,
        app: &AppState,
        owner: &Owner,
        command: Command,
    ) -> Result<Reply, Failure> {
        if !self.packages.get(&owner.id).is_some_and(|package| {
            package.lifecycle.accepts(owner.generation) && documents::allowed(&self.grants, package)
        }) {
            return Err(Failure::new(
                FailureKind::PermissionDenied,
                "Document access is not granted to this activation",
            ));
        }
        self.resource_documents.command(app, owner, command)
    }

    pub(super) fn observe_resource_documents(&mut self, app: &AppState) -> Vec<Effect> {
        let mut effects = Vec::new();
        for (id, package) in &mut self.packages {
            if package.lifecycle.state() != State::Dormant
                || package.disabled
                || !package.manifest.has_behaviour()
                || !wants(&package.manifest, "resource.documents")
                || !documents::allowed(&self.grants, package)
                || !package
                    .manifest
                    .capabilities
                    .iter()
                    .any(|cap| cap == "events")
                || self
                    .grants
                    .authorize(&package.identity, "events", None)
                    .is_err()
                || !app.workspace.documents.values().any(|document| {
                    document.is_text()
                        && document.meta.resource.environment().is_some()
                        && serves(&package.manifest, &document.meta.lang.definition().name)
                })
            {
                continue;
            }
            let generation = package.lifecycle.generation();
            let _ = package.lifecycle.activating();
            effects.push(Effect::Submit(Job::Load {
                id: id.clone(),
                generation,
                component: package.component.clone(),
                package_dir: package.dir.clone(),
                version: package.manifest.version.clone(),
                cache: self.cache.clone(),
                limits: self.guest_limits,
                storage: storage_of(&self.grants, package).then(|| self.storage_dir.join(id)),
                files: files_of(&self.grants, package),
                domains: domains_of(&self.grants, package),
            }));
        }
        self.resource_documents.subscribers.retain(|owner, _| {
            self.packages.get(&owner.id).is_some_and(|package| {
                package.lifecycle.accepts(owner.generation)
                    && documents::allowed(&self.grants, package)
            })
        });
        if self.resource_documents.subscribers.is_empty() {
            return effects;
        }
        self.resource_documents.sync(app);
        for (owner, notified) in &mut self.resource_documents.subscribers {
            if *notified == 0 || *notified == self.resource_documents.revision {
                continue;
            }
            if let Some(credit) = self.workspace_budget.reserve(1024) {
                *notified = 0;
                effects.push(Effect::Submit(Job::WorkspaceEvent {
                    id: owner.id.clone(),
                    generation: owner.generation,
                    event: WorkspaceEvent::DocumentsChanged {
                        revision: self.resource_documents.revision,
                    },
                    _credit: credit,
                }));
            } else {
                effects.push(Effect::Submit(Job::WorkspaceWakeAfter { milliseconds: 16 }));
                break;
            }
        }
        effects
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::document::DiskState;
    use crate::domain::workspace::{OpenMode, PreparedTextFile};
    use loog_workspace_api::{EnvironmentKey, ResourceRef};

    fn open(app: &mut AppState, environment: &str, text: &str) -> FileId {
        let resource = ResourceRef::provided(
            EnvironmentKey::new("memory", environment).unwrap(),
            "/project/中.rs",
        )
        .unwrap();
        app.workspace.open_prepared_text(
            PreparedTextFile::with_resource(
                resource,
                "中.rs".into(),
                text.into(),
                DiskState::from_contents(text),
            ),
            OpenMode::Permanent,
        )
    }
    fn owner() -> Owner {
        Owner {
            id: "language".into(),
            generation: 1,
        }
    }
    fn list(documents: &mut Documents, app: &AppState) -> Vec<DocumentResource> {
        let Reply::Documents { documents, .. } = documents
            .command(
                app,
                &owner(),
                Command::Documents {
                    after: None,
                    revision: None,
                    limit: 32,
                },
            )
            .unwrap()
        else {
            panic!("documents")
        };
        documents
    }

    #[test]
    fn resource_buffers_keep_environment_identity_and_chunks_never_mix_revisions() {
        let mut app = AppState::new();
        let first = open(&mut app, "one", "a😀中z");
        open(&mut app, "two", "other");
        let mut documents = Documents::default();
        let descriptors = list(&mut documents, &app);
        assert_eq!(descriptors.len(), 2);
        assert_ne!(descriptors[0].stamp.document, descriptors[1].stamp.document);
        assert_ne!(descriptors[0].resource, descriptors[1].resource);
        let descriptor = descriptors
            .iter()
            .find(|d| d.resource.environment().unwrap().instance() == "one")
            .unwrap();
        let mut offset = 0;
        let mut joined = String::new();
        loop {
            let Reply::DocumentChunk { text, next, stamp } = documents
                .command(
                    &app,
                    &owner(),
                    Command::ReadDocument {
                        stamp: descriptor.stamp,
                        offset,
                        limit: 4,
                    },
                )
                .unwrap()
            else {
                panic!("chunk")
            };
            assert_eq!(stamp, descriptor.stamp);
            assert!(text.len() <= 4);
            joined.push_str(&text);
            let Some(next) = next else {
                break;
            };
            assert!(next > offset);
            offset = next;
        }
        assert_eq!(joined, "a😀中z");
        assert_eq!(
            documents
                .command(
                    &app,
                    &owner(),
                    Command::ReadDocument {
                        stamp: descriptor.stamp,
                        offset: 2,
                        limit: 4
                    }
                )
                .unwrap_err()
                .kind,
            FailureKind::InvalidRequest
        );
        loog_editor_core::normal::insert(
            &mut app.workspace.document_editor_mut(first).unwrap(),
            "edit",
        );
        assert_eq!(
            documents
                .command(
                    &app,
                    &owner(),
                    Command::ReadDocument {
                        stamp: descriptor.stamp,
                        offset: 0,
                        limit: 16
                    }
                )
                .unwrap_err()
                .kind,
            FailureKind::Conflict
        );
        let updated = list(&mut documents, &app)
            .into_iter()
            .find(|d| d.stamp.document == descriptor.stamp.document)
            .unwrap();
        assert_eq!(updated.stamp.lifecycle, descriptor.stamp.lifecycle);
        assert_ne!(updated.stamp.version, descriptor.stamp.version);
        app.workspace.close(first);
        assert_eq!(
            documents
                .command(
                    &app,
                    &owner(),
                    Command::ReadDocument {
                        stamp: updated.stamp,
                        offset: 0,
                        limit: 16
                    }
                )
                .unwrap_err()
                .kind,
            FailureKind::Conflict
        );
        open(&mut app, "one", "new lifetime");
        let reopened = list(&mut documents, &app);
        assert!(
            reopened
                .iter()
                .all(|d| d.stamp.document != descriptor.stamp.document)
        );
    }

    #[test]
    fn document_pages_reject_changes_and_retargeting_invalidates_a_same_version_stamp() {
        let mut app = AppState::new();
        let first = open(&mut app, "one", "abc");
        open(&mut app, "two", "def");
        let mut documents = Documents::default();
        let Reply::Documents {
            documents: page,
            revision,
            next,
        } = documents
            .command(
                &app,
                &owner(),
                Command::Documents {
                    after: None,
                    revision: None,
                    limit: 1,
                },
            )
            .unwrap()
        else {
            panic!("page")
        };
        assert_eq!(page.len(), 1);
        assert!(next.is_some());
        let original = list(&mut documents, &app)
            .into_iter()
            .find(|d| d.resource.environment().unwrap().instance() == "one")
            .unwrap();
        app.workspace
            .documents
            .get_mut(&first)
            .unwrap()
            .meta
            .resource = ResourceRef::provided(
            EnvironmentKey::new("memory", "one").unwrap(),
            "/project/renamed.rs",
        )
        .unwrap();
        assert_eq!(
            documents
                .command(
                    &app,
                    &owner(),
                    Command::Documents {
                        after: next,
                        revision: Some(revision),
                        limit: 1
                    }
                )
                .unwrap_err()
                .kind,
            FailureKind::Conflict
        );
        assert_eq!(
            documents
                .command(
                    &app,
                    &owner(),
                    Command::ReadDocument {
                        stamp: original.stamp,
                        offset: 0,
                        limit: 16
                    }
                )
                .unwrap_err()
                .kind,
            FailureKind::Conflict
        );
        let changed = list(&mut documents, &app)
            .into_iter()
            .find(|d| d.stamp.document == original.stamp.document)
            .unwrap();
        assert_eq!(changed.stamp.version, original.stamp.version);
        assert!(changed.stamp.lifecycle > original.stamp.lifecycle);
    }
}
