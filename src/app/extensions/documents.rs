use super::*;
#[derive(Clone, PartialEq, Eq)]
pub(super) struct ViewStamp {
    version: u64,
    incarnation: u64,
    lifecycle: u64,
    path: String,
    language: String,
    variant: Option<String>,
    language_generation: u64,
    synchronized: bool,
}
pub(super) fn allowed(grants: &Grants, package: &Package) -> bool {
    package
        .manifest
        .capabilities
        .iter()
        .any(|c| c == "documents.read")
        && grants
            .authorize(&package.identity, "documents.read", None)
            .is_ok()
}
impl ExtensionService {
    pub(super) fn workspace_files_changed(
        &mut self,
        app: &AppState,
        change: crate::features::providers::FileChange,
    ) -> Vec<Effect> {
        use crate::features::providers::FileChange;
        let in_workspace = |path: &Path| {
            app.explorer
                .model
                .roots()
                .iter()
                .any(|root| path.starts_with(root))
        };
        let change = match change {
            FileChange::Created(path) if in_workspace(&path) => {
                loom_extension_host::WorkspaceFileChange::Created(path.display().to_string())
            }
            FileChange::Deleted(path) if in_workspace(&path) => {
                loom_extension_host::WorkspaceFileChange::Deleted(path.display().to_string())
            }
            FileChange::Renamed { from, to } if in_workspace(&from) || in_workspace(&to) => {
                loom_extension_host::WorkspaceFileChange::Renamed((
                    from.display().to_string(),
                    to.display().to_string(),
                ))
            }
            _ => return Vec::new(),
        };
        let owners = self
            .packages
            .iter()
            .filter(|(_, p)| {
                wants(&p.manifest, "workspace.files")
                    && self.authorize(p, "workspace.files.read").is_ok()
            })
            .map(|(owner, _)| owner.clone())
            .collect::<Vec<_>>();
        let mut effects = Vec::new();
        for owner in owners {
            let package = self.packages.get_mut(&owner).unwrap();
            if matches!(
                package.lifecycle.state(),
                State::Stopped | State::Failed | State::Stopping
            ) {
                continue;
            }
            // A package for some languages is not woken by files: a document of one of its
            // languages does that, and it looks at the folder when it starts.
            if package.lifecycle.state() == State::Dormant
                && super::language_scope(&package.manifest).is_some()
            {
                continue;
            }
            crate::infrastructure::extensions::document_queue::merge_files(
                &mut package.pending_files,
                vec![change.clone()],
            );
            if package.lifecycle.state() == State::Dormant {
                let generation = package.lifecycle.generation();
                let _ = package.lifecycle.activating();
                effects.push(Effect::Submit(Job::Load {
                    id: owner.clone(),
                    generation,
                    component: package.component.clone(),
                    package_dir: package.dir.clone(),
                    version: package.manifest.version.clone(),
                    cache: self.cache.clone(),
                    limits: self.guest_limits,
                    storage: storage_of(&self.grants, package)
                        .then(|| self.storage_dir.join(&owner)),
                    files: files_of(&self.grants, package),
                    domains: domains_of(&self.grants, package),
                }));
            }
            effects.extend(self.flush_files(&owner));
        }
        effects
    }
    pub(super) fn flush_files(&mut self, owner: &str) -> Vec<Effect> {
        let Some(package) = self
            .packages
            .get_mut(owner)
            .filter(|p| p.lifecycle.state() == State::Active)
        else {
            return Vec::new();
        };
        let changes = std::mem::take(&mut package.pending_files);
        if changes.is_empty() {
            Vec::new()
        } else {
            vec![Effect::Submit(Job::WorkspaceFiles {
                id: owner.into(),
                generation: package.lifecycle.generation(),
                changes,
            })]
        }
    }
    pub(super) fn document_saved(
        &mut self,
        app: &AppState,
        saved: crate::domain::document::SaveSnapshot,
    ) -> Vec<Effect> {
        let mut effects = Vec::new();
        // A package for some languages hears only about their documents, as it does
        // for every other document event: saving a file of another language neither
        // tells it nor wakes it.
        let language = app
            .workspace
            .documents
            .get(&saved.stamp.document)
            .map(|document| document.meta.lang.definition().name.to_string());
        let owners = self
            .packages
            .iter()
            .filter(|(_, p)| {
                wants(&p.manifest, "document.save")
                    && documents::allowed(&self.grants, p)
                    && language
                        .as_deref()
                        .is_none_or(|language| serves(&p.manifest, language))
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for owner in owners {
            let package = self.packages.get_mut(&owner).unwrap();
            if matches!(
                package.lifecycle.state(),
                State::Stopped | State::Failed | State::Stopping
            ) {
                continue;
            }
            package
                .pending_saved
                .insert(saved.stamp.document, saved.clone());
            if package.lifecycle.state() == State::Dormant {
                let generation = package.lifecycle.generation();
                let _ = package.lifecycle.activating();
                effects.push(Effect::Submit(Job::Load {
                    id: owner.clone(),
                    generation,
                    component: package.component.clone(),
                    package_dir: package.dir.clone(),
                    version: package.manifest.version.clone(),
                    cache: self.cache.clone(),
                    limits: self.guest_limits,
                    storage: storage_of(&self.grants, package)
                        .then(|| self.storage_dir.join(&owner)),
                    files: files_of(&self.grants, package),
                    domains: domains_of(&self.grants, package),
                }));
            }
            effects.extend(self.flush_saved(app, &owner));
        }
        effects
    }
    pub(super) fn flush_saved(&mut self, app: &AppState, owner: &str) -> Vec<Effect> {
        let Some(package) = self
            .packages
            .get_mut(owner)
            .filter(|p| p.lifecycle.state() == State::Active && allowed(&self.grants, p))
        else {
            return Vec::new();
        };
        let mut effects = Vec::new();
        for (file, saved) in std::mem::take(&mut package.pending_saved) {
            let Some(document) =
                app.workspace.documents.get(&file).filter(|d| {
                    d.incarnation == saved.stamp.incarnation && d.meta.path == saved.path
                })
            else {
                continue;
            };
            if saved.text.len()
                > app
                    .preferences
                    .values()
                    .language_features
                    .max_document_bytes
            {
                continue;
            }
            let Some(&id) = self.seen.get(&file) else {
                continue;
            };
            let lifecycle = package
                .document_views
                .get(&id)
                .map_or(1, |view| view.lifecycle);
            effects.push(Effect::Submit(Job::DocumentSaved {
                id: owner.into(),
                generation: package.lifecycle.generation(),
                reference: DocumentRef {
                    document: id,
                    lifecycle,
                    version: saved.stamp.version,
                    path: saved.path.display().to_string(),
                    language: document.meta.lang.definition().name.to_string(),
                    variant: Some(
                        crate::domain::language::Registry::builtin()
                            .variant_name(document.meta.lang)
                            .to_owned(),
                    ),
                },
                text: saved.text,
            }));
        }
        effects
    }
    pub(super) fn sync_document_views(&mut self, app: &AppState) -> Vec<Effect> {
        let mut effects = vec![];
        for (owner, package) in self
            .packages
            .iter_mut()
            .filter(|(_, p)| p.lifecycle.state() == State::Active && allowed(&self.grants, p))
        {
            let mut next = HashMap::new();
            let mut changed = vec![];
            for (file, document) in app.workspace.documents.iter().filter(|(_, d)| d.is_text()) {
                let Some(&id) = self.seen.get(file) else {
                    continue;
                };
                let old = package.document_views.get(&id);
                let path = document.meta.path.display().to_string();
                let language = document.meta.lang.definition().name.to_string();
                let lifecycle = package
                    .feeds
                    .get(&id)
                    .map(|feed| feed.stream.lifecycle())
                    .unwrap_or_else(|| {
                        old.map_or(1, |old| {
                            old.lifecycle
                                + u64::from(
                                    old.incarnation != document.incarnation
                                        || old.path != path
                                        || old.language_generation
                                            != document.meta.language_generation
                                        || old.language != language,
                                )
                        })
                    });
                let synchronized = document.buffer.len()
                    <= app
                        .preferences
                        .values()
                        .language_features
                        .max_document_bytes;
                let stamp = ViewStamp {
                    version: document.buffer.version(),
                    incarnation: document.incarnation,
                    lifecycle,
                    path,
                    language,
                    variant: Some(
                        crate::domain::language::Registry::builtin()
                            .variant_name(document.meta.lang)
                            .to_owned(),
                    ),
                    language_generation: document.meta.language_generation,
                    synchronized,
                };
                if old != Some(&stamp) {
                    changed.push(loom_extension_host::DocumentView {
                        reference: DocumentRef {
                            document: id,
                            lifecycle,
                            version: stamp.version,
                            path: stamp.path.clone(),
                            language: stamp.language.clone(),
                            variant: stamp.variant.clone(),
                        },
                        text: synchronized.then(|| document.buffer.text_snapshot()),
                    });
                }
                next.insert(id, stamp);
            }
            let closed = package
                .document_views
                .keys()
                .filter(|id| !next.contains_key(id))
                .copied()
                .collect::<Vec<_>>();
            if !package.documents_initialized || !changed.is_empty() || !closed.is_empty() {
                effects.push(Effect::Submit(Job::Documents {
                    id: owner.clone(),
                    generation: package.lifecycle.generation(),
                    changed,
                    closed,
                }));
            }
            package.documents_initialized = true;
            package.document_views = next;
        }
        effects
    }
}
