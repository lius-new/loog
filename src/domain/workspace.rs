//! Open documents and the editor panes that show them.
//!
//! Documents are shared: a file opened in two panes has one text buffer, one
//! undo history and one dirty state. Each pane keeps its tab list and preview
//! slot. Independent ViewIds own selection, scroll and presentation state.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::domain::dirty::DirtyCache;
use crate::domain::document::{
    DiskRead, DiskState, DocumentStamp, FileId, FileMeta, ResolvedLanguage,
};
use crate::domain::geometry::Rect;
use crate::domain::identity::{DiffId, ViewId};
use crate::domain::language::LanguageRules;
use crate::domain::pane_layout::{Direction, PaneId, PaneNode};
use crate::domain::view::{DocumentView, Presentation, ReaderPosition, ViewTarget};
use loog_editor_core::text::{Editor, EditorMut, Selection, TextBuffer};
use loog_workspace_api::ResourceRef;

pub use crate::domain::view::AppPage;

#[derive(Clone)]
pub(crate) struct OpenDocument {
    /// Which lifetime of this document services and caches belong to. Zero until
    /// the workspace installs it; see `Workspace::insert_document`.
    pub(crate) incarnation: u64,
    pub(crate) meta: FileMeta,
    pub(crate) buffer: TextBuffer,
    pub(crate) saved_text: Arc<String>,
    pub(crate) saved_format: crate::domain::encoding::TextFormat,
    /// Raw disk bytes and the current unsaved byte interpretation. Neither is
    /// rebuilt from replacement characters when an encoding is selected again.
    pub(crate) disk_bytes: Option<Arc<Vec<u8>>>,
    pub(crate) encoding_bytes: Option<Arc<Vec<u8>>>,
    pub(crate) recovery: Option<crate::domain::workspace_session::SavedEncoding>,
    dirty_cache: Arc<Mutex<DirtyCache>>,
    /// Membership index only. Per-view state lives in Workspace::views.
    pub(crate) view_ids: BTreeSet<ViewId>,
    pub(crate) disk_state: DiskState,
    pub(crate) provider: Option<(loog_workspace_api::ScopeId, Option<String>)>,
    /// A notification requests a baseline check; it is not itself a conflict.
    pub(crate) resource_check: bool,
    /// Last watcher observation, separate from the saved disk baseline. Dirty
    /// conflicts deliberately leave disk_state unchanged.
    pub(crate) disk_observation: u64,
    pub(crate) observed_disk: Option<DiskState>,
    pub(crate) disk_conflict: bool,
    pub(crate) missing_on_disk: bool,
    /// A restored tab has metadata before its text is ready. Never expose its
    /// empty staging buffer to editing, saving or disk reconciliation.
    pub(crate) loading: Option<bool>,
    /// A failed file keeps its tab and path, but never exposes a staging buffer.
    pub(crate) open_error: Option<String>,
    pub(crate) image: Option<Arc<crate::domain::media::Image>>,
}

impl OpenDocument {
    pub(crate) fn new(meta: FileMeta, contents: String, disk_state: DiskState) -> Self {
        let saved_format = disk_state.format;
        let mut buffer = TextBuffer::new(contents);
        buffer.initialize_metadata(saved_format.code());
        let saved_text = buffer.text_snapshot();
        let dirty_cache = Arc::new(Mutex::new(DirtyCache::clean(&buffer, &saved_text)));
        Self {
            incarnation: 0,
            meta,
            buffer,
            saved_text,
            saved_format,
            disk_bytes: None,
            encoding_bytes: None,
            recovery: None,
            dirty_cache,
            view_ids: BTreeSet::new(),
            disk_state,
            provider: None,
            resource_check: false,
            disk_observation: 0,
            observed_disk: None,
            disk_conflict: false,
            missing_on_disk: false,
            loading: None,
            open_error: None,
            image: None,
        }
    }

    /// The identity of this document's text, language and lifetime right now.
    /// Results keyed by an older stamp must not be shown or published.
    pub(crate) fn stamp(&self, id: FileId) -> DocumentStamp {
        DocumentStamp::new(
            id,
            self.incarnation,
            self.meta.language_generation,
            &self.buffer,
        )
    }

    /// Whether this tab edits a file on disk (not a diff or the settings page).
    pub(crate) fn is_text(&self) -> bool {
        self.loading.is_none() && self.open_error.is_none() && self.image.is_none()
    }

    pub(crate) fn format(&self) -> crate::domain::encoding::TextFormat {
        crate::domain::encoding::TextFormat::from_code(self.buffer.metadata())
    }
    pub(crate) fn is_dirty(&self) -> bool {
        if !self.is_text() {
            return false;
        }
        if self.format() != self.saved_format {
            return true;
        }
        self.dirty_cache
            .lock()
            .unwrap()
            .is_dirty(&self.buffer, &self.saved_text)
    }
}

/// Diff payloads have their own identity and never allocate a text buffer.
#[derive(Clone)]
pub(crate) struct DiffRecord<D, K> {
    pub(crate) meta: FileMeta,
    pub(crate) content: Arc<D>,
    pub(crate) collapsed_diffs: BTreeSet<K>,
    pub(crate) focused_diff: Option<K>,
    pub(crate) view: ViewId,
}

/// One editor pane: an ordered tab list with an active and a preview tab.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pane {
    pub(crate) items: Vec<ViewId>,
    pub(crate) active: Option<ViewId>,
    /// The one replaceable tab of this pane.
    pub(crate) preview: Option<ViewId>,
}

impl Pane {
    pub fn items(&self) -> &[ViewId] {
        &self.items
    }

    pub fn active(&self) -> Option<ViewId> {
        self.active
    }

    pub fn preview(&self) -> Option<ViewId> {
        self.preview
    }

    pub fn is_preview(&self, id: ViewId) -> bool {
        self.preview == Some(id)
    }

    pub fn contains(&self, id: ViewId) -> bool {
        self.items.contains(&id)
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Remove a tab, activating its right neighbour (or the new last tab).
    fn remove(&mut self, id: ViewId) -> bool {
        let Some(index) = self.items.iter().position(|item| *item == id) else {
            return false;
        };
        self.items.remove(index);
        if self.preview == Some(id) {
            self.preview = None;
        }
        if self.active == Some(id) {
            self.active = self
                .items
                .get(index.min(self.items.len().saturating_sub(1)))
                .copied();
        }
        true
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReconcileResult {
    Unchanged(PathBuf),
    Reloaded(PathBuf),
    Conflict(PathBuf),
    Missing(PathBuf),
    Failed(PathBuf, String),
    /// A local watcher must not reconcile a provided resource.
    Provided(ResourceRef),
}

#[derive(Clone)]
pub struct PreparedDiskUpdate {
    pub(crate) id: FileId,
    pub(crate) path: PathBuf,
    pub(crate) expected_disk: DiskState,
    pub(crate) expected_observation: u64,
    pub(crate) expected_saved: Arc<String>,
    pub(crate) expected_version: u64,
    pub(crate) replacement: OpenDocument,
    pub(crate) outcome: ReconcileResult,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenMode {
    Preview,
    Permanent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResourceResolution {
    Check,
    KeepEditor,
    Reload,
}

/// Constructed entirely on a worker; installing it does not scan the text.
pub(crate) struct PreparedTextFile(OpenDocument);

impl PreparedTextFile {
    pub(crate) fn text_snapshot(&self) -> Arc<String> {
        self.0.buffer.text_snapshot()
    }
    pub(crate) fn with_provider_root(mut self, root: Option<ResourceRef>) -> Self {
        self.0.meta.provider_root = root;
        self
    }
    pub(crate) fn with_read_override(
        mut self,
        format: Option<crate::domain::encoding::TextFormat>,
    ) -> Self {
        self.0.meta.read_override = format;
        self
    }
    pub(crate) fn with_resource_language(
        mut self,
        relative: Option<Vec<String>>,
        language: ResolvedLanguage,
    ) -> Self {
        self.0.meta.provider_relative = relative;
        self.0.meta.set_language(language);
        self
    }
    pub(crate) fn offline(
        resource: ResourceRef,
        name: String,
        recovery: Option<crate::domain::workspace_session::SavedEncoding>,
    ) -> Self {
        let contents = recovery
            .as_ref()
            .and_then(|r| r.saved_text.as_ref())
            .map(|s| s.as_ref().clone())
            .unwrap_or_default();
        let mut disk = DiskState::from_contents(&contents);
        if let Some(recovery) = &recovery {
            disk.format = recovery.saved;
        }
        let mut value = Self::with_resource(resource, name, contents, disk);
        if recovery
            .as_ref()
            .is_none_or(|r| r.saved_text.is_none() && r.unsaved_text.is_none())
        {
            value.0.open_error =
                Some("Resource is offline. Connect its provider and open the file again.".into());
        }
        value.0.recovery = recovery;
        if value.0.open_error.is_none() {
            restore_encoding(&mut value.0);
        }
        value
    }
    pub(crate) fn provided(
        resource: ResourceRef,
        name: String,
        scope: loog_workspace_api::ScopeId,
        revision: Option<String>,
        decoded: crate::domain::encoding::DecodedText,
    ) -> Self {
        let mut disk = DiskState::from_contents(&decoded.text);
        disk.size = decoded.bytes.len() as u64;
        disk.format = decoded.format;
        let mut value = Self::with_resource(resource, name, decoded.text, disk);
        value.0.meta.encoding_source = decoded.source;
        value.0.disk_bytes = Some(decoded.bytes.clone());
        value.0.encoding_bytes = Some(decoded.bytes);
        value.0.provider = Some((scope, revision));
        value
    }
    pub(crate) fn with_file_options(
        mut self,
        options: loog_editor_core::text::FileOptions,
    ) -> Self {
        self.0.buffer.initialize_file_options(options);
        self
    }

    pub(crate) fn image(
        path: PathBuf,
        image: crate::domain::media::Image,
        disk: DiskState,
    ) -> Self {
        let mut value = Self::with_disk_state(path, String::new(), disk);
        value.0.image = Some(Arc::new(image));
        value
    }
    pub(crate) fn provided_image(
        resource: ResourceRef,
        name: String,
        scope: loog_workspace_api::ScopeId,
        revision: Option<String>,
        image: crate::domain::media::Image,
    ) -> Self {
        let disk = DiskState {
            modified_time: None,
            size: image.byte_len,
            content_hash: image.identity,
            format: Default::default(),
        };
        let mut value = Self::with_resource(resource, name, String::new(), disk);
        value.0.image = Some(Arc::new(image));
        value.0.provider = Some((scope, revision));
        value
    }
    pub(crate) fn failed(path: PathBuf, error: String) -> Self {
        let mut value = Self::with_disk_state(path, String::new(), DiskState::from_contents(""));
        value.0.open_error = Some(error);
        value
    }

    pub fn with_encoding(
        path: PathBuf,
        decoded: crate::domain::encoding::DecodedText,
        disk: DiskState,
    ) -> Self {
        let mut value = Self::with_disk_state(path, decoded.text, disk);
        value.0.meta.encoding_source = decoded.source;
        value.0.disk_bytes = Some(decoded.bytes.clone());
        value.0.encoding_bytes = Some(decoded.bytes);
        value
    }
    pub fn with_disk_state(path: PathBuf, contents: String, disk: DiskState) -> Self {
        Self(OpenDocument::new(FileMeta::from_path(path), contents, disk))
    }

    pub fn with_resource(
        resource: ResourceRef,
        name: String,
        contents: String,
        disk: DiskState,
    ) -> Self {
        Self(OpenDocument::new(
            FileMeta::provided(resource, name),
            contents,
            disk,
        ))
    }
}

#[derive(Clone)]
pub struct Workspace<D, K> {
    pub(crate) documents: HashMap<FileId, OpenDocument>,
    pub(crate) paths: HashMap<ResourceRef, FileId>,
    pub(crate) views: HashMap<ViewId, DocumentView>,
    pub(crate) diffs: HashMap<DiffId, DiffRecord<D, K>>,
    next_view_id: u64,
    next_diff_id: u64,
    pub(crate) panes: HashMap<PaneId, Pane>,
    pub(crate) layout: PaneNode,
    pub(crate) active_pane: PaneId,
    /// Panes by recency of focus, most recent first.
    pub(crate) pane_mru: Vec<PaneId>,
    pub(crate) next_file_id: u64,
    next_incarnation: u64,
    /// File associations from settings, tried before the built-in recognition.
    language_rules: Arc<LanguageRules>,
    language_registry: Arc<crate::domain::language::Registry>,
    pub(crate) next_pane_id: u64,
    retired: Vec<Retired<D, K>>,
}

#[derive(Clone)]
pub(crate) enum Retired<D, K> {
    Document(OpenDocument),
    Update(PreparedDiskUpdate),
    Diff(DiffRecord<D, K>),
}

impl<D: Clone, K: Clone + Ord> Workspace<D, K> {
    pub(crate) fn take_retired(&mut self) -> Vec<Retired<D, K>> {
        std::mem::take(&mut self.retired)
    }

    pub fn mark_saved_with_disk(&mut self, id: FileId, disk: DiskState) -> bool {
        let Some(document) = self.documents.get_mut(&id) else {
            return false;
        };
        if !document.is_text() {
            return false;
        }
        document.saved_text = Arc::new(document.buffer.text().to_owned());
        document
            .buffer
            .initialize_metadata((document.buffer.metadata() & !255) | disk.format.code());
        document.saved_format = disk.format;
        document.disk_bytes = None;
        document.encoding_bytes = None;
        if document.meta.read_override.is_some() {
            document.meta.read_override = Some(disk.format);
        }
        document.disk_state = disk;
        document.disk_conflict = false;
        document.missing_on_disk = false;
        document.buffer.break_undo_group();
        true
    }

    pub fn reconcile_with_disk(&mut self, id: FileId, observation: DiskRead) -> ReconcileResult {
        let Some(document) = self.documents.get(&id) else {
            return ReconcileResult::Failed(PathBuf::new(), "Document is not open.".into());
        };
        let Some(path) = document.meta.resource.local_path().map(Path::to_path_buf) else {
            return ReconcileResult::Provided(document.meta.resource.clone());
        };
        if !document.is_text() {
            return ReconcileResult::Unchanged(path);
        }
        let dirty = document.is_dirty();
        let previous = document.disk_state.clone();
        let (contents, current, bytes) = match observation {
            DiskRead::Found {
                contents,
                state,
                bytes,
            } => (contents, state, bytes),
            DiskRead::Missing => {
                if let Some(document) = self.documents.get_mut(&id) {
                    document.disk_observation += 1;
                    document.observed_disk = None;
                    document.missing_on_disk = true;
                    document.disk_conflict = dirty;
                }
                return ReconcileResult::Missing(path);
            }
            DiskRead::Failed(error) => return ReconcileResult::Failed(path, error),
        };
        if let Some(document) = self.documents.get_mut(&id) {
            document.disk_observation += 1;
            document.observed_disk = Some(current.clone());
        }
        if previous.content_hash == current.content_hash && previous.size == current.size {
            if let Some(document) = self.documents.get_mut(&id) {
                document.disk_state = current;
                document.missing_on_disk = false;
                document.disk_conflict = false;
            }
            return ReconcileResult::Unchanged(path);
        }
        let document = self.documents.get_mut(&id).expect("document remains open");
        document.missing_on_disk = false;
        if dirty {
            document.disk_conflict = true;
            ReconcileResult::Conflict(path)
        } else {
            replace_buffer_from_disk(document, contents.clone());
            document.saved_text = Arc::new(contents);
            document.buffer.initialize_metadata(current.format.code());
            document.saved_format = current.format;
            document.disk_bytes = bytes.clone();
            document.encoding_bytes = bytes;
            document.disk_state = current;
            document.disk_conflict = false;
            self.clamp_document_views(id);
            ReconcileResult::Reloaded(path)
        }
    }

    pub fn accept_disk_with(
        &mut self,
        id: FileId,
        contents: String,
        disk: DiskState,
    ) -> Result<(), String> {
        let document = self
            .documents
            .get_mut(&id)
            .ok_or_else(|| "Document is not open.".to_owned())?;
        if !document.is_text() {
            return Err("Diff documents are read-only.".to_owned());
        }
        replace_buffer_from_disk(document, contents.clone());
        document.saved_text = Arc::new(contents);
        document.buffer.initialize_metadata(disk.format.code());
        document.saved_format = disk.format;
        document.disk_bytes = None;
        document.encoding_bytes = None;
        if document.meta.read_override.is_some() {
            document.meta.read_override = Some(disk.format);
        }
        document.disk_state = disk;
        document.disk_conflict = false;
        document.missing_on_disk = false;
        self.clamp_document_views(id);
        Ok(())
    }

    pub fn keep_editor_with_disk(&mut self, id: FileId, disk: Option<DiskState>) -> bool {
        let Some(document) = self.documents.get_mut(&id) else {
            return false;
        };
        if !document.is_text() {
            return false;
        }
        if let Some(state) = disk {
            document.disk_state = state;
        }
        document.disk_conflict = false;
        true
    }

    pub fn move_path_with_disk(
        &mut self,
        old_path: &Path,
        new_path: PathBuf,
        disk: DiskState,
    ) -> bool {
        let Some(id) = self.paths.remove(&ResourceRef::local(old_path)) else {
            return false;
        };
        let language = self.language_for(&new_path);
        let Some(document) = self.documents.get_mut(&id) else {
            return false;
        };
        // The file decides the language, so a rename may change it. Results
        // computed for the old language must stop matching, which only a bumped
        // generation guarantees; a plain rename within one language keeps it.
        let previous = std::mem::replace(&mut document.meta, FileMeta::from_path(new_path.clone()));
        document.meta.lang = language;
        document.meta.language_generation =
            previous.language_generation + u64::from(previous.lang != document.meta.lang);
        document.meta.encoding_source = previous.encoding_source;
        document.meta.read_override = previous.read_override;
        document.disk_state = disk;
        document.missing_on_disk = false;
        self.paths.insert(ResourceRef::local(new_path), id);
        self.clamp_document_views(id);
        true
    }

    /// Apply provider-supplied path mappings without interpreting their paths.
    /// Disk revisions are checked again before saving at the new identities.
    pub fn relocate_resources(
        &mut self,
        environment: &loog_workspace_api::EnvironmentKey,
        entries: &[loog_workspace_api::ResourceMove],
    ) -> Result<(), String> {
        let mut sources = std::collections::HashSet::new();
        let mut targets = std::collections::HashSet::new();
        let mut updates = Vec::new();
        for entry in entries {
            let source = ResourceRef::Provided {
                environment: environment.clone(),
                path: entry.source.clone(),
            };
            let target = ResourceRef::Provided {
                environment: environment.clone(),
                path: entry.target.clone(),
            };
            if !sources.insert(source.clone())
                || !targets.insert(target.clone())
                || entry.name.is_empty()
            {
                return Err("Invalid resource rename mapping".into());
            }
            if let Some(id) = self.file_id_for_resource(&source) {
                let language = self.language_registry.resolve_filename(&entry.name);
                updates.push((id, source, target, entry.name.clone(), language));
            }
        }
        for (_, _, target, _, _) in &updates {
            if self.file_id_for_resource(target).is_some() && !sources.contains(target) {
                return Err("Renamed resource is already open as another document".into());
            }
        }
        for (_, source, _, _, _) in &updates {
            self.paths.remove(source);
        }
        for (id, _, target, name, language) in updates {
            let document = self.documents.get_mut(&id).unwrap();
            document.meta.resource = target.clone();
            document.meta.name = name;
            document.meta.provider_relative = None;
            document.meta.lang = language;
            document.meta.language_generation += 1;
            document.disk_conflict = true;
            document.missing_on_disk = false;
            self.paths.insert(target, id);
        }
        Ok(())
    }

    pub fn new() -> Self {
        let pane = PaneId::new(1);
        Self {
            documents: HashMap::new(),
            paths: HashMap::new(),
            views: HashMap::new(),
            diffs: HashMap::new(),
            next_view_id: 1,
            next_diff_id: 1,
            panes: HashMap::from([(pane, Pane::default())]),
            layout: PaneNode::Leaf(pane),
            active_pane: pane,
            pane_mru: vec![pane],
            next_file_id: 1,
            next_incarnation: 1,
            language_rules: Arc::default(),
            language_registry: crate::domain::language::Registry::builtin(),
            next_pane_id: 2,
            retired: Vec::new(),
        }
    }

    // ---- Panes ------------------------------------------------------------

    pub fn layout(&self) -> &PaneNode {
        &self.layout
    }

    pub fn layout_mut(&mut self) -> &mut PaneNode {
        &mut self.layout
    }

    /// Panes in reading order.
    pub fn pane_ids(&self) -> Vec<PaneId> {
        self.layout.leaves()
    }

    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }

    pub fn active_pane(&self) -> PaneId {
        self.active_pane
    }

    pub fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.panes.get(&id)
    }

    pub(crate) fn active_pane_ref(&self) -> &Pane {
        &self.panes[&self.active_pane]
    }

    pub(crate) fn active_pane_mut(&mut self) -> &mut Pane {
        self.panes
            .get_mut(&self.active_pane)
            .expect("the active pane exists")
    }

    /// Focus a pane. Returns whether the focus changed.
    pub fn activate_pane(&mut self, pane: PaneId) -> bool {
        if !self.panes.contains_key(&pane) {
            return false;
        }
        self.pane_mru.retain(|id| *id != pane);
        self.pane_mru.insert(0, pane);
        let changed = self.active_pane != pane;
        self.active_pane = pane;
        changed
    }

    /// The most recently focused of several candidate panes.
    pub fn most_recent_pane(&self, candidates: &[PaneId]) -> Option<PaneId> {
        self.pane_mru
            .iter()
            .copied()
            .find(|id| candidates.contains(id))
            .or_else(|| candidates.first().copied())
    }

    /// Which pane shows `id`, preferring the active pane, then recency.
    pub fn pane_of(&self, id: ViewId) -> Option<PaneId> {
        self.pane_ids()
            .into_iter()
            .find(|pane| self.panes[pane].contains(id))
    }

    /// Split `pane`, copying its active tab (with its view) into a new pane on
    /// the `direction` side, and focus the new pane. Built-in pages and diffs
    /// exist once, so a pane showing one splits into an empty pane.
    pub fn split(&mut self, pane: PaneId, direction: Direction) -> Option<PaneId> {
        let copied = self.panes.get(&pane)?.active.filter(|id| self.is_file(*id));
        let new = self.add_pane(pane, direction)?;
        if let Some(id) = copied {
            self.copy_item(pane, id, new, 0);
        }
        self.activate_pane(new);
        Some(new)
    }

    /// Replace the layout of a workspace without open tabs by `shape`, whose
    /// leaves are numbered `0..` in reading order. Each leaf becomes a new empty
    /// pane; the new panes are returned in that order.
    pub fn restore_layout(&mut self, mut shape: PaneNode) -> Vec<PaneId> {
        if !self.views.is_empty() {
            return self.pane_ids();
        }
        let count = shape.leaves().len();
        let ids = (0..count)
            .map(|_| {
                let id = PaneId::new(self.next_pane_id);
                self.next_pane_id += 1;
                id
            })
            .collect::<Vec<_>>();
        shape.map_leaves(&mut |leaf| ids[leaf.get() as usize]);
        shape.normalize();
        self.panes = ids.iter().map(|id| (*id, Pane::default())).collect();
        self.layout = shape;
        self.active_pane = ids[0];
        self.pane_mru = ids.clone();
        ids
    }

    /// Split `pane`, leaving the new pane empty and unfocused.
    pub fn split_empty(&mut self, pane: PaneId, direction: Direction) -> Option<PaneId> {
        self.add_pane(pane, direction)
    }

    /// Insert an empty pane next to `pane` without focusing it.
    fn add_pane(&mut self, pane: PaneId, direction: Direction) -> Option<PaneId> {
        let new = PaneId::new(self.next_pane_id);
        if !self.panes.contains_key(&pane) || !self.layout.split(pane, new, direction) {
            return None;
        }
        self.next_pane_id += 1;
        self.panes.insert(new, Pane::default());
        Some(new)
    }

    /// The pane next to the active one in `direction`; among several, the
    /// most recently focused.
    pub fn neighbor(&self, direction: Direction) -> Option<PaneId> {
        // Sizes are proportional, so any area gives the same adjacency.
        const AREA: Rect = Rect::new(0.0, 0.0, 10_000.0, 10_000.0);
        let candidates = self.layout.neighbors(AREA, self.active_pane, direction);
        self.most_recent_pane(&candidates)
    }

    /// Focus the neighbouring pane in `direction`.
    pub fn activate_neighbor(&mut self, direction: Direction) -> bool {
        self.neighbor(direction)
            .is_some_and(|pane| self.activate_pane(pane))
    }

    /// Focus the next (or previous) pane in reading order, wrapping around.
    pub fn activate_next_pane(&mut self, forward: bool) -> bool {
        let panes = self.pane_ids();
        let Some(index) = panes.iter().position(|pane| *pane == self.active_pane) else {
            return false;
        };
        let len = panes.len();
        let next = if forward {
            (index + 1) % len
        } else {
            (index + len - 1) % len
        };
        self.activate_pane(panes[next])
    }

    /// Move the active tab to the neighbouring pane in `direction`, creating
    /// that pane when there is none. A pane's only tab stays put when there is
    /// no neighbour, since splitting would just move the whole pane.
    pub fn move_active_item(&mut self, direction: Direction) -> bool {
        let from = self.active_pane;
        let Some(id) = self.active() else {
            return false;
        };
        let target = match self.neighbor(direction) {
            Some(pane) => pane,
            None if self.active_pane_ref().items.len() > 1 => {
                match self.add_pane(from, direction) {
                    Some(pane) => pane,
                    None => return false,
                }
            }
            None => return false,
        };
        self.move_item(from, id, target, usize::MAX)
    }

    /// Close a pane and all its tabs without prompting; callers resolve dirty
    /// documents first. The last pane only loses its tabs.
    pub fn close_pane(&mut self, pane: PaneId) {
        let Some(items) = self.panes.get(&pane).map(|p| p.items.clone()) else {
            return;
        };
        for id in items {
            self.close_item(pane, id);
        }
        self.remove_pane_if_empty(pane);
    }

    fn remove_pane_if_empty(&mut self, pane: PaneId) {
        if self.panes.len() <= 1 || !self.panes.get(&pane).is_some_and(Pane::is_empty) {
            return;
        }
        if !self.layout.remove(pane) {
            return;
        }
        self.panes.remove(&pane);
        self.pane_mru.retain(|id| *id != pane);
        if self.active_pane == pane {
            let next = self
                .pane_mru
                .first()
                .copied()
                .unwrap_or_else(|| self.layout.leaves()[0]);
            self.activate_pane(next);
        }
    }

    /// Insert a reference to an open document into a pane, creating its view
    /// from `view_source`'s view (or the most recently focused one).
    pub(crate) fn insert_item(&mut self, pane: PaneId, id: ViewId, index: usize) {
        let target = self.panes.get_mut(&pane).expect("view destination exists");
        assert!(!target.contains(id), "one view occupies exactly one slot");
        target.items.insert(index.min(target.items.len()), id);
    }

    pub(crate) fn create_view(&mut self, target: ViewTarget, presentation: Presentation) -> ViewId {
        let id = ViewId::new(self.next_view_id);
        self.next_view_id += 1;
        self.views
            .insert(id, DocumentView::new(id, target, presentation));
        if let ViewTarget::Text(document) = target {
            self.documents
                .get_mut(&document)
                .expect("document exists")
                .view_ids
                .insert(id);
        }
        id
    }

    pub(crate) fn insert_diff(
        &mut self,
        pane: PaneId,
        mut meta: FileMeta,
        content: Arc<D>,
    ) -> ViewId {
        if meta.local_path().is_some() {
            meta.lang = self.language_for(meta.language_hint());
        }
        let id = DiffId::new(self.next_diff_id);
        self.next_diff_id += 1;
        let view = self.create_view(ViewTarget::Diff(id), Presentation::Source);
        self.diffs.insert(
            id,
            DiffRecord {
                meta,
                content,
                collapsed_diffs: BTreeSet::new(),
                focused_diff: None,
                view,
            },
        );
        self.insert_item(pane, view, usize::MAX);
        self.activate(pane, view);
        view
    }

    pub fn view_document(&self, id: ViewId) -> Option<FileId> {
        self.views.get(&id)?.document()
    }
    pub fn pane_document(&self, pane: PaneId) -> Option<FileId> {
        self.view_document(self.panes.get(&pane)?.active?)
    }
    pub fn active_document(&self) -> Option<FileId> {
        self.view_document(self.active()?)
    }
    pub fn view(&self, id: ViewId) -> Option<&DocumentView> {
        self.views.get(&id)
    }
    pub fn view_meta(&self, id: ViewId) -> Option<&FileMeta> {
        match self.views.get(&id)?.target {
            ViewTarget::Text(id) => self.meta(id),
            ViewTarget::Diff(id) => Some(&self.diffs.get(&id)?.meta),
            ViewTarget::Builtin(page) => {
                static SETTINGS: std::sync::LazyLock<FileMeta> = std::sync::LazyLock::new(|| {
                    FileMeta::from_path(PathBuf::from(AppPage::Settings.title()))
                });
                static KEYMAP: std::sync::LazyLock<FileMeta> = std::sync::LazyLock::new(|| {
                    FileMeta::from_path(PathBuf::from(AppPage::Keymap.title()))
                });
                static EXTENSIONS: std::sync::LazyLock<FileMeta> = std::sync::LazyLock::new(|| {
                    FileMeta::from_path(PathBuf::from(AppPage::Extensions.title()))
                });
                Some(match page {
                    AppPage::Settings => &SETTINGS,
                    AppPage::Keymap => &KEYMAP,
                    AppPage::Extensions => &EXTENSIONS,
                })
            }
        }
    }
    pub fn is_view_dirty(&self, id: ViewId) -> bool {
        self.view_document(id).is_some_and(|id| self.is_dirty(id))
    }
    pub fn set_presentation(&mut self, id: ViewId, presentation: Presentation) -> bool {
        if presentation == Presentation::Image
            && self
                .view_document(id)
                .and_then(|id| self.documents.get(&id))
                .is_some_and(|doc| {
                    doc.image.is_none() && !crate::domain::media::is_svg(doc.meta.language_hint())
                })
        {
            return false;
        }
        if self
            .view_document(id)
            .and_then(|id| self.documents.get(&id))
            .is_some_and(|doc| doc.image.is_some())
            && presentation != Presentation::Image
        {
            return false;
        }
        let Some(view) = self.views.get_mut(&id).filter(|v| v.document().is_some()) else {
            return false;
        };
        if view.presentation == presentation {
            return false;
        }
        view.presentation = presentation;
        if presentation == Presentation::Source {
            view.associated_source = None;
        }
        view.input_generation += 1;
        if let Some(document) = view.document().and_then(|id| self.documents.get_mut(&id)) {
            document.buffer.break_undo_group();
        }
        if presentation != Presentation::Source {
            for other in self.views.values_mut() {
                if other.associated_source == Some(id) {
                    other.associated_source = None;
                }
            }
        }
        true
    }
    pub fn set_reader_position(&mut self, id: ViewId, position: ReaderPosition) -> bool {
        let Some(document) = self
            .view_document(id)
            .and_then(|id| self.documents.get(&id))
        else {
            return false;
        };
        let view = self.views.get_mut(&id).unwrap();
        view.reader = ReaderPosition {
            source_byte: document
                .buffer
                .clamp(Selection::caret(position.source_byte))
                .cursor,
            block_fraction: if position.block_fraction.is_finite() {
                position.block_fraction.clamp(0.0, 1.0)
            } else {
                0.0
            },
        };
        true
    }
    pub fn activate_document(&mut self, id: FileId) {
        let candidates = self.tabs_of(id);
        let pane = if candidates.iter().any(|(p, _)| *p == self.active_pane) {
            Some(self.active_pane)
        } else {
            self.most_recent_pane(&candidates.iter().map(|(p, _)| *p).collect::<Vec<_>>())
        };
        if let Some(pane) = pane {
            if let Some((_, view)) = candidates
                .iter()
                .find(|(p, v)| *p == pane && self.views[v].accepts_text())
            {
                self.activate(pane, *view);
                return;
            }
            self.activate_pane(pane);
        }
        if self.documents.contains_key(&id) {
            self.attach_text_tab(id, OpenMode::Permanent);
        }
    }
    pub(crate) fn restore_view(
        &mut self,
        pane: PaneId,
        document: FileId,
        saved: &crate::domain::workspace_session::SavedView,
    ) -> ViewId {
        let id = self.create_view(ViewTarget::Text(document), Presentation::Source);
        saved.apply(self.views.get_mut(&id).unwrap());
        self.insert_item(pane, id, usize::MAX);
        if !self.is_loading(document) {
            self.clamp_document_views(document);
        }
        id
    }
    fn clamp_document_views(&mut self, id: FileId) {
        if let Some(document) = self.documents.get(&id) {
            for view in self.views.values_mut().filter(|v| v.document() == Some(id)) {
                if document.image.is_some() && view.presentation != Presentation::Image {
                    view.presentation = Presentation::Image;
                    view.input_generation += 1;
                } else if document.image.is_none()
                    && view.presentation == Presentation::Image
                    && !crate::domain::media::is_svg(document.meta.language_hint())
                {
                    view.presentation = Presentation::Source;
                    view.associated_source = None;
                    view.input_generation += 1;
                }
                view.source.selection = document.buffer.clamp(view.source.selection);
                for selection in &mut view.source.secondary {
                    *selection = document.buffer.clamp(*selection);
                }
                view.reader.source_byte = document
                    .buffer
                    .clamp(Selection::caret(view.reader.source_byte))
                    .cursor;
            }
        }
    }

    /// Put a tab into a new pane on `direction`'s side of `target`, moving it
    /// (or copying a file when `copy`). Splitting a pane with its only tab is
    /// a no-op, like VSCode.
    pub fn split_with_item(
        &mut self,
        from: PaneId,
        id: ViewId,
        target: PaneId,
        direction: Direction,
        copy: bool,
    ) -> bool {
        if !self.panes.get(&from).is_some_and(|p| p.contains(id)) || (copy && !self.is_file(id)) {
            return false;
        }
        if !copy && from == target && self.panes[&from].items.len() == 1 {
            return false;
        }
        let Some(new) = self.add_pane(target, direction) else {
            return false;
        };
        if copy {
            self.copy_item(from, id, new, 0)
        } else {
            self.move_item(from, id, new, 0)
        }
    }

    /// Move a tab to another pane (or reorder within one) at `index`, and
    /// activate it there. A pane left empty is removed.
    pub fn move_item(&mut self, from: PaneId, id: ViewId, to: PaneId, index: usize) -> bool {
        if from == to {
            return self.move_tab(from, id, index);
        }
        if !self.panes.get(&from).is_some_and(|p| p.contains(id)) || !self.panes.contains_key(&to) {
            return false;
        }
        let preview = self.panes[&from].is_preview(id);
        self.panes.get_mut(&from).unwrap().remove(id);
        self.insert_item(to, id, index);
        if preview {
            self.replace_preview_slot(to, id);
        }
        self.activate(to, id);
        self.remove_pane_if_empty(from);
        true
    }

    /// Copy a tab into another pane at `index` and activate it there.
    /// Built-in pages and diffs cannot be copied.
    pub fn copy_item(&mut self, from: PaneId, id: ViewId, to: PaneId, index: usize) -> bool {
        if !self.panes.get(&from).is_some_and(|p| p.contains(id))
            || !self.panes.contains_key(&to)
            || !self.is_file(id)
        {
            return false;
        }
        let source = self.views[&id].clone();
        let new = self.create_view(source.target, source.presentation);
        self.views.insert(
            new,
            DocumentView {
                id: new,
                input_generation: 0,
                associated_source: None,
                ..source
            },
        );
        self.insert_item(to, new, index);
        self.activate(to, new);
        true
    }

    /// Give `pane`'s preview slot to `id`, closing a clean previous preview.
    fn replace_preview_slot(&mut self, pane: PaneId, id: ViewId) {
        if let Some(old) = self.panes[&pane].preview.filter(|old| *old != id) {
            if self.is_view_dirty(old) {
                self.panes.get_mut(&pane).unwrap().preview = None;
            } else {
                self.close_item(pane, old);
            }
        }
        self.panes.get_mut(&pane).unwrap().preview = Some(id);
    }

    // ---- Tabs of one pane ---------------------------------------------------

    /// Labels for a pane's tabs. Duplicate file names receive the shortest
    /// parent path suffix that distinguishes them within that pane.
    pub fn tab_labels(&self, pane: PaneId) -> HashMap<ViewId, String> {
        let Some(pane) = self.panes.get(&pane) else {
            return HashMap::new();
        };
        let mut groups: HashMap<String, Vec<(ViewId, &FileMeta)>> = HashMap::new();
        for id in &pane.items {
            let Some(meta) = self.view_meta(*id) else {
                continue;
            };
            groups
                .entry(meta.name.clone())
                .or_default()
                .push((*id, meta));
        }

        let mut labels = HashMap::with_capacity(pane.items.len());
        for (name, documents) in groups {
            if documents.len() == 1 {
                labels.insert(documents[0].0, name);
                continue;
            }

            let parents = documents
                .iter()
                .map(|(_, meta)| match &meta.resource {
                    ResourceRef::Local { path } => parent_components(path),
                    ResourceRef::Provided { environment, path } => {
                        vec![environment.instance().to_owned(), path.as_str().to_owned()]
                    }
                })
                .collect::<Vec<_>>();
            for (index, (id, _)) in documents.iter().enumerate() {
                let parts = &parents[index];
                let mut distinguishing = String::new();
                for depth in 1..=parts.len().max(1) {
                    let candidate = parent_suffix(parts, depth);
                    let normalized = candidate.to_lowercase();
                    let unique = parents.iter().enumerate().all(|(other_index, other)| {
                        other_index == index
                            || parent_suffix(other, depth).to_lowercase() != normalized
                    });
                    distinguishing = candidate;
                    if unique {
                        break;
                    }
                }
                let label = if distinguishing.is_empty() {
                    name.clone()
                } else {
                    format!("{name} — {distinguishing}")
                };
                labels.insert(*id, label);
            }
        }
        labels
    }

    /// Activate a tab of a pane and focus that pane.
    pub fn activate(&mut self, pane: PaneId, id: ViewId) -> bool {
        let Some(target) = self.panes.get_mut(&pane) else {
            return false;
        };
        if !target.contains(id) {
            return false;
        }
        let changed = target.active != Some(id) || self.active_pane != pane;
        target.active = Some(id);
        self.activate_pane(pane);
        changed
    }

    /// Reveal an open document: activate it in the active pane if it is there,
    /// otherwise in the most recent pane that shows it.
    pub fn set_active(&mut self, id: ViewId) {
        if let Some(pane) = self.pane_of(id) {
            self.activate(pane, id);
        }
    }

    pub fn activate_at(&mut self, index: usize) -> bool {
        let Some(id) = self.active_pane_ref().items.get(index).copied() else {
            return false;
        };
        let pane = self.active_pane_mut();
        let changed = pane.active != Some(id);
        pane.active = Some(id);
        changed
    }

    pub fn activate_last(&mut self) -> bool {
        let Some(id) = self.active_pane_ref().items.last().copied() else {
            return false;
        };
        let pane = self.active_pane_mut();
        let changed = pane.active != Some(id);
        pane.active = Some(id);
        changed
    }

    pub fn next(&mut self) {
        self.cycle(1);
    }

    pub fn prev(&mut self) {
        self.cycle(-1);
    }

    fn cycle(&mut self, step: isize) {
        let pane = self.active_pane_mut();
        if let Some(position) = pane
            .active
            .and_then(|active| pane.items.iter().position(|id| *id == active))
        {
            let len = pane.items.len() as isize;
            pane.active = Some(pane.items[(position as isize + step).rem_euclid(len) as usize]);
        }
    }

    /// Move a tab to a final index within its pane while preserving its
    /// document and active state. Returns whether the visible order changed.
    pub fn move_tab(&mut self, pane: PaneId, id: ViewId, target_index: usize) -> bool {
        let Some(pane) = self.panes.get_mut(&pane) else {
            return false;
        };
        let Some(source_index) = pane.items.iter().position(|&file| file == id) else {
            return false;
        };
        let target_index = target_index.min(pane.items.len().saturating_sub(1));
        if source_index == target_index {
            return false;
        }
        let id = pane.items.remove(source_index);
        pane.items.insert(target_index, id);
        true
    }

    /// Remove a tab from one pane. The document is released when no pane
    /// shows it any more, and an emptied pane (other than the last) closes.
    pub fn close_item(&mut self, pane: PaneId, id: ViewId) {
        if !self.panes.get_mut(&pane).is_some_and(|p| p.remove(id)) {
            return;
        }
        for other in self.views.values_mut() {
            if other.associated_source == Some(id) {
                other.associated_source = None;
            }
        }
        if let Some(view) = self.views.remove(&id) {
            match view.target {
                ViewTarget::Text(document) => {
                    let orphan = self.documents.get_mut(&document).is_some_and(|d| {
                        d.view_ids.remove(&id);
                        d.view_ids.is_empty()
                    });
                    if orphan && let Some(d) = self.documents.remove(&document) {
                        self.paths.remove(&d.meta.resource);
                        self.retired.push(Retired::Document(d));
                    }
                }
                ViewTarget::Diff(diff) => {
                    if let Some(d) = self.diffs.remove(&diff) {
                        self.retired.push(Retired::Diff(d));
                    }
                }
                ViewTarget::Builtin(_) => {}
            }
        }
        self.remove_pane_if_empty(pane);
    }

    /// Close a document in every pane that shows it.
    pub fn close(&mut self, id: FileId) {
        for (pane, view) in self.tabs_of(id) {
            self.close_item(pane, view);
        }
    }

    /// Every tab showing a document, in pane order.
    pub fn tabs_of(&self, id: FileId) -> Vec<(PaneId, ViewId)> {
        self.pane_ids()
            .into_iter()
            .flat_map(|pane| {
                self.panes[&pane]
                    .items
                    .iter()
                    .copied()
                    .filter(move |view| self.view_document(*view) == Some(id))
                    .map(move |view| (pane, view))
            })
            .collect()
    }

    /// The documents that closing these tabs would release, i.e. those with no
    /// remaining tab elsewhere.
    pub fn released_by(&self, targets: &[(PaneId, ViewId)]) -> Vec<FileId> {
        let mut released = Vec::new();
        for &(pane, view) in targets {
            if self.pane_of(view) != Some(pane) {
                continue;
            }
            let Some(id) = self.view_document(view) else {
                continue;
            };
            if !released.contains(&id) && self.tabs_of(id).iter().all(|pair| targets.contains(pair))
            {
                released.push(id);
            }
        }
        released
    }

    // ---- The focused editor ------------------------------------------------

    /// Active tab of the active pane.
    pub fn active(&self) -> Option<ViewId> {
        self.active_pane_ref().active
    }

    /// Tabs of the active pane.
    pub fn active_items(&self) -> &[ViewId] {
        &self.active_pane_ref().items
    }

    pub fn preview(&self) -> Option<ViewId> {
        self.active_pane_ref().preview
    }

    pub fn promote_preview(&mut self, pane: PaneId, id: ViewId) -> bool {
        match self.panes.get_mut(&pane) {
            Some(pane) if pane.preview == Some(id) => {
                pane.preview = None;
                true
            }
            _ => false,
        }
    }

    pub fn promote_active_preview(&mut self) -> bool {
        let pane = self.active_pane;
        self.active()
            .is_some_and(|id| self.promote_preview(pane, id))
    }

    pub fn active_meta(&self) -> Option<&FileMeta> {
        self.view_meta(self.active()?)
    }

    /// Path of the active file or diff; built-in pages have none.
    pub fn active_path(&self) -> Option<&Path> {
        if self.active_page().is_some() {
            return None;
        }
        self.active_meta()
            .and_then(|meta| meta.resource.local_path())
    }

    pub fn active_diff(&self) -> Option<&D> {
        self.diff_of(self.active()?)
    }

    pub fn active_page(&self) -> Option<AppPage> {
        self.active().and_then(|id| self.page(id))
    }

    pub fn active_is_settings(&self) -> bool {
        self.active_page() == Some(AppPage::Settings)
    }

    pub fn active_editor(&self) -> Option<Editor<'_>> {
        self.editor(self.active_pane)
    }

    pub fn active_editor_mut(&mut self) -> Option<EditorMut<'_>> {
        self.editor_mut(self.active_pane)
    }

    /// The text editor of a pane's active tab, if that tab is a file.
    pub fn editor(&self, pane: PaneId) -> Option<Editor<'_>> {
        let view = self
            .views
            .get(&self.panes.get(&pane)?.active?)
            .filter(|v| v.accepts_text())?;
        let document = self
            .documents
            .get(&view.document()?)
            .filter(|d| d.is_text())?;
        Some(
            Editor::new(&document.buffer, view.source.selection)
                .with_secondary(&view.source.secondary),
        )
    }

    /// The editor of one specific view: `document` as shown in `pane`, whether
    /// or not it is the pane's active tab.
    pub fn view_editor_mut(&mut self, pane: PaneId, view: ViewId) -> Option<EditorMut<'_>> {
        if !self.panes.get(&pane)?.contains(view) {
            return None;
        }
        let id = self
            .views
            .get(&view)
            .filter(|v| v.accepts_text())?
            .document()?;
        self.document_view_editor_mut(id, view)
    }
    pub(crate) fn document_editor_mut(&mut self, id: FileId) -> Option<EditorMut<'_>> {
        let document = self.documents.get(&id)?;
        let view = self
            .active()
            .filter(|v| {
                self.views
                    .get(v)
                    .is_some_and(|v| v.document() == Some(id) && v.accepts_text())
            })
            .or_else(|| {
                document
                    .view_ids
                    .iter()
                    .copied()
                    .find(|v| self.views.get(v).is_some_and(|v| v.accepts_text()))
            })
            .or_else(|| document.view_ids.iter().next().copied())?;
        self.document_view_editor_mut(id, view)
    }
    fn document_view_editor_mut(&mut self, id: FileId, view: ViewId) -> Option<EditorMut<'_>> {
        let document = self.documents.get_mut(&id).filter(|d| d.is_text())?;
        let mut own = None;
        let mut extra = None;
        let mut peers = Vec::new();
        let mut readers = Vec::new();
        for v in self.views.values_mut().filter(|v| v.document() == Some(id)) {
            if v.id == view {
                own = Some(&mut v.source.selection);
                extra = Some(&mut v.source.secondary);
            } else {
                peers.push(&mut v.source.selection);
                peers.extend(v.source.secondary.iter_mut());
            }
            readers.push((
                &mut v.reader.source_byte,
                loog_editor_core::text::Assoc::After,
            ));
        }
        Some(
            EditorMut::new(&mut document.buffer, own?, peers)
                .with_secondary(extra?)
                .with_anchors(readers),
        )
    }

    pub fn editor_mut(&mut self, pane: PaneId) -> Option<EditorMut<'_>> {
        self.view_editor_mut(pane, self.panes.get(&pane)?.active?)
    }

    /// The diff shown by a pane's active tab.
    pub fn diff(&self, pane: PaneId) -> Option<&D> {
        self.diff_of(self.panes.get(&pane)?.active?)
    }

    pub fn collapsed_diffs(&self, pane: PaneId) -> BTreeSet<K> {
        self.panes
            .get(&pane)
            .and_then(|p| p.active)
            .and_then(|id| self.diff_record(id))
            .map(|d| d.collapsed_diffs.clone())
            .unwrap_or_default()
    }

    pub fn toggle_diff_section(&mut self, pane: PaneId, request: K) {
        let Some(ViewTarget::Diff(id)) = self
            .panes
            .get(&pane)
            .and_then(|p| p.active)
            .and_then(|v| self.views.get(&v))
            .map(|v| v.target)
        else {
            return;
        };
        let d = self.diffs.get_mut(&id).unwrap();
        if !d.collapsed_diffs.insert(request.clone()) {
            d.collapsed_diffs.remove(&request);
        }
    }

    /// The built-in page shown by a pane's active tab.
    pub fn pane_page(&self, pane: PaneId) -> Option<AppPage> {
        let id = self.panes.get(&pane)?.active?;
        self.page(id)
    }

    /// Scroll offset of a pane's active tab.
    pub fn scroll(&self, pane: PaneId) -> (f32, f32) {
        self.panes
            .get(&pane)
            .and_then(|p| p.active)
            .and_then(|v| self.views.get(&v))
            .map_or((0.0, 0.0), |v| (v.source.scroll_x, v.source.scroll_y))
    }

    pub fn set_scroll(&mut self, pane: PaneId, x: f32, y: f32) {
        if let Some(v) = self
            .panes
            .get(&pane)
            .and_then(|p| p.active)
            .and_then(|id| self.views.get_mut(&id))
            .filter(|v| v.presentation == Presentation::Source)
        {
            v.source.scroll_x = x.max(0.0);
            v.source.scroll_y = y.max(0.0);
        }
    }

    pub fn active_scroll(&self) -> (f32, f32) {
        self.scroll(self.active_pane)
    }

    /// Keeps every code view on the same line after the code font changes.
    /// `text` and `diff` are the new-to-old ratios of the editor line height
    /// and the diff row height.
    pub fn rescale_code_scroll(&mut self, text: f32, diff: f32) {
        for v in self.views.values_mut() {
            match v.target {
                ViewTarget::Text(_) => v.source.scroll_y *= text,
                ViewTarget::Diff(_) => v.source.scroll_y *= diff,
                ViewTarget::Builtin(_) => {}
            }
        }
    }

    pub fn set_active_scroll(&mut self, x: f32, y: f32) {
        self.set_scroll(self.active_pane, x, y);
    }

    pub fn active_save_snapshot(&self) -> Option<(FileId, PathBuf, String)> {
        self.save_snapshot(self.active_document()?)
    }

    // ---- Documents ---------------------------------------------------------

    /// Every open document, in pane order then tab order, without repeats.
    pub(crate) fn ordered_documents(&self) -> Vec<FileId> {
        let mut ids = Vec::new();
        for pane in self.pane_ids() {
            for view in &self.panes[&pane].items {
                if let Some(id) = self.view_document(*view)
                    && !ids.contains(&id)
                {
                    ids.push(id);
                }
            }
        }
        ids
    }

    pub fn open_paths(&self) -> Vec<PathBuf> {
        self.ordered_documents()
            .into_iter()
            .filter_map(|id| self.documents.get(&id))
            .filter(|document| document.is_text())
            .filter_map(|document| document.meta.resource.local_path().map(Path::to_path_buf))
            .collect()
    }

    pub fn meta(&self, id: FileId) -> Option<&FileMeta> {
        self.documents.get(&id).map(|document| &document.meta)
    }

    /// The diff shown by tab `id`, if it is a diff tab.
    pub fn diff_of(&self, id: ViewId) -> Option<&D> {
        Some(&self.diff_record(id)?.content)
    }
    pub(crate) fn diff_record(&self, id: ViewId) -> Option<&DiffRecord<D, K>> {
        let ViewTarget::Diff(id) = self.views.get(&id)?.target else {
            return None;
        };
        self.diffs.get(&id)
    }

    pub fn is_diff(&self, id: ViewId) -> bool {
        self.diff_record(id).is_some()
    }

    pub fn file_id_for_path(&self, path: &Path) -> Option<FileId> {
        self.file_id_for_resource(&ResourceRef::local(path))
    }

    pub fn file_id_for_resource(&self, resource: &ResourceRef) -> Option<FileId> {
        if let Some(id) = self.paths.get(resource) {
            return Some(*id);
        }
        let path = resource.local_path()?;
        // `\\?\D:\dir` (from `fs::canonicalize`, as the file tree reports) and
        // `D:\dir` (from the shell or a dialog) name the same file.
        let wanted = comparable(path);
        self.paths.iter().find_map(|(open, id)| {
            open.local_path()
                .filter(|path| comparable(path) == wanted)
                .map(|_| *id)
        })
    }

    pub fn is_dirty(&self, id: FileId) -> bool {
        self.documents
            .get(&id)
            .is_some_and(|document| document.is_dirty())
    }

    pub fn dirty_paths(&self) -> Vec<PathBuf> {
        self.dirty_file_ids()
            .into_iter()
            .filter_map(|id| self.documents.get(&id))
            .filter_map(|document| document.meta.resource.local_path().map(Path::to_path_buf))
            .collect()
    }

    pub fn dirty_file_ids(&self) -> Vec<FileId> {
        self.ordered_documents()
            .into_iter()
            .filter(|id| self.is_dirty(*id))
            .collect()
    }

    pub fn has_dirty_paths_under(&self, root: &Path) -> bool {
        self.documents.iter().any(|(id, document)| {
            document
                .meta
                .resource
                .local_path()
                .is_some_and(|path| path.starts_with(root))
                && self.is_dirty(*id)
        })
    }

    pub fn has_disk_conflict(&self, id: FileId) -> bool {
        self.documents
            .get(&id)
            .is_some_and(|document| document.is_text() && document.disk_conflict)
    }

    pub fn is_missing_on_disk(&self, id: FileId) -> bool {
        self.documents
            .get(&id)
            .is_some_and(|document| document.is_text() && document.missing_on_disk)
    }

    pub fn set_encoding(
        &mut self,
        id: FileId,
        format: crate::domain::encoding::TextFormat,
    ) -> Result<bool, String> {
        format.validate()?;
        let document = self.documents.get_mut(&id).ok_or("Document is not open")?;
        if !document.is_text() {
            return Err("Document is loading".into());
        }
        if document.format() == format {
            return Ok(false);
        }
        document
            .buffer
            .set_metadata((document.buffer.metadata() & !255) | format.code());
        document.buffer.break_undo_group();

        Ok(true)
    }

    pub(crate) fn set_decoded_encoding(
        &mut self,
        id: FileId,
        decoded: crate::domain::encoding::DecodedText,
    ) -> Result<bool, String> {
        let metadata = self
            .documents
            .get(&id)
            .ok_or("Document is not open")?
            .buffer
            .metadata();
        self.document_editor_mut(id)
            .ok_or("Document is not editable")?
            .replace_with_metadata(&decoded.text, (metadata & !255) | decoded.format.code())
            .map_err(|error| error.to_string())?;
        let document = self.documents.get_mut(&id).ok_or("Document is not open")?;
        document.encoding_bytes = Some(decoded.bytes);
        document.meta.read_override = Some(decoded.format);
        document.meta.encoding_source = decoded.source;
        self.clamp_document_views(id);
        Ok(true)
    }

    pub fn shared_save_snapshot(
        &self,
        id: FileId,
    ) -> Option<crate::domain::document::SaveSnapshot> {
        let document = self.documents.get(&id).filter(|d| d.is_text())?;
        Some(crate::domain::document::SaveSnapshot {
            stamp: document.stamp(id),
            path: document.meta.resource.local_path()?.to_path_buf(),
            text: document.buffer.text_snapshot(),
            disk_observation: document.disk_observation,
            format: document.format(),
        })
    }

    pub(crate) fn resource_save_snapshot(
        &self,
        id: FileId,
    ) -> Option<crate::domain::document::ResourceSaveSnapshot> {
        let document = self.documents.get(&id).filter(|d| d.is_text())?;
        let (scope, revision) = document.provider.as_ref()?;
        Some(crate::domain::document::ResourceSaveSnapshot {
            stamp: document.stamp(id),
            resource: document.meta.resource.clone(),
            scope: *scope,
            revision: revision.clone(),
            text: document.buffer.text_snapshot(),
            format: document.format(),
        })
    }

    pub(crate) fn mark_resource_saved(
        &mut self,
        saved: &crate::domain::document::ResourceSaveSnapshot,
        revision: Option<String>,
    ) -> bool {
        let Some(document) = self.documents.get_mut(&saved.stamp.document).filter(|d| {
            d.is_text()
                && d.incarnation == saved.stamp.incarnation
                && d.meta.resource == saved.resource
                && d.provider.as_ref() == Some(&(saved.scope, saved.revision.clone()))
        }) else {
            return false;
        };
        document.provider = Some((saved.scope, revision));
        document.saved_text = saved.text.clone();
        document.saved_format = saved.format;
        document.disk_bytes = None;
        document.encoding_bytes = None;
        document.disk_conflict = false;
        if document.buffer.version() == saved.stamp.version {
            document.buffer.break_undo_group();
            if document.buffer.snapshot_identity() == saved.stamp.snapshot_identity {
                *document.dirty_cache.lock().unwrap() =
                    DirtyCache::clean(&document.buffer, &document.saved_text);
            }
        }
        true
    }

    /// A new scope must observe the baseline before it may save recovered edits.
    pub(crate) fn reconnect_resource(
        &mut self,
        id: FileId,
        prepared: PreparedTextFile,
    ) -> Result<(), &'static str> {
        self.resolve_resource(id, prepared, ResourceResolution::Check)
    }
    pub(crate) fn resolve_resource(
        &mut self,
        id: FileId,
        prepared: PreparedTextFile,
        resolution: ResourceResolution,
    ) -> Result<(), &'static str> {
        let Some(document) = self
            .documents
            .get_mut(&id)
            .filter(|d| d.meta.resource == prepared.0.meta.resource)
        else {
            return Err("Resource is no longer open");
        };
        let mut replacement = prepared.0;
        document.resource_check = false;
        document.meta.provider_relative = replacement.meta.provider_relative.clone();
        document.meta.set_language(replacement.meta.lang.clone());
        if replacement.is_text() {
            document
                .buffer
                .initialize_file_options(replacement.buffer.file_options());
        }
        if document.is_text()
            && replacement.is_text()
            && document.buffer.text() == replacement.saved_text.as_str()
            && document.format() == replacement.saved_format
        {
            document.provider = replacement.provider.take();
            document.meta.provider_root = replacement.meta.provider_root.clone();
            document.saved_text = replacement.saved_text.clone();
            document.saved_format = replacement.saved_format;
            document.disk_bytes = replacement.disk_bytes.clone();
            document.disk_conflict = false;
            document.missing_on_disk = false;
            self.retired.push(Retired::Document(replacement));
            return Ok(());
        }
        if document.is_text() && document.is_dirty() && resolution != ResourceResolution::Reload {
            document.provider = replacement.provider.take();
            document.meta.provider_root = replacement.meta.provider_root.clone();
            let matches_current = document.buffer.text() == replacement.saved_text.as_str()
                && document.format() == replacement.saved_format;
            if resolution == ResourceResolution::KeepEditor || matches_current {
                document.saved_text = replacement.saved_text.clone();
                document.saved_format = replacement.saved_format;
                document.disk_bytes = replacement.disk_bytes.clone();
            } else if document.saved_text != replacement.saved_text
                || document.saved_format != replacement.saved_format
            {
                document.disk_conflict = true;
                self.retired.push(Retired::Document(replacement));
                return Err(
                    "Resource changed while disconnected. Your edits are preserved; resolve the conflict before saving.",
                );
            }
            document.disk_conflict = false;
            self.retired.push(Retired::Document(replacement));
        } else {
            replacement.view_ids = document.view_ids.clone();
            let old = self.documents.remove(&id).unwrap();
            self.insert_document(id, replacement);
            self.retired.push(Retired::Document(old));
            self.clamp_document_views(id);
        }
        Ok(())
    }

    /// A completed write advances only the saved baseline, never newer edits.
    pub fn mark_snapshot_saved(
        &mut self,
        saved: &crate::domain::document::SaveSnapshot,
        disk: DiskState,
    ) -> bool {
        let Some(document) = self.documents.get_mut(&saved.stamp.document).filter(|d| {
            d.is_text()
                && d.incarnation == saved.stamp.incarnation
                && d.meta.resource.local_path() == Some(saved.path.as_path())
        }) else {
            return false;
        };
        // A watcher may have observed another writer after this save started.
        // A known conflict must not be cleared by a delayed completion or an
        // older save; an echo of these exact bytes is safe to acknowledge.
        let newer_conflict = document.disk_conflict
            && document.disk_observation != saved.disk_observation
            && (document.missing_on_disk
                || document.observed_disk.as_ref().is_none_or(|observed| {
                    observed.content_hash != disk.content_hash || observed.size != disk.size
                }));
        document.saved_text = saved.text.clone();
        document.saved_format = saved.format;
        if !newer_conflict {
            document.disk_state = disk;
            document.disk_bytes = None;
            document.disk_conflict = false;
            document.missing_on_disk = false;
        }
        document.encoding_bytes = None;
        if document.buffer.version() == saved.stamp.version {
            document.buffer.break_undo_group();
            if document.buffer.snapshot_identity() == saved.stamp.snapshot_identity {
                *document.dirty_cache.lock().unwrap() =
                    DirtyCache::clean(&document.buffer, &document.saved_text);
            }
        }
        true
    }

    pub fn save_snapshot(&self, id: FileId) -> Option<(FileId, PathBuf, String)> {
        let document = self.documents.get(&id)?;
        if !document.is_text() {
            return None;
        }
        Some((
            id,
            document.meta.resource.local_path()?.to_path_buf(),
            document.buffer.text().to_owned(),
        ))
    }

    /// Documents are shared across panes, so each text document appears once.
    pub fn text_snapshots(&self) -> Vec<crate::domain::document_snapshot::DocumentSnapshot> {
        self.documents
            .values()
            .filter(|document| document.is_text())
            .filter_map(|document| {
                Some(crate::domain::document_snapshot::DocumentSnapshot {
                    path: document.meta.resource.local_path()?.to_path_buf(),
                    text: document.buffer.text_snapshot(),
                    version: document.buffer.version(),
                })
            })
            .collect()
    }

    pub fn dirty_text_versions(&self) -> Vec<(PathBuf, u64)> {
        let mut versions = self
            .documents
            .values()
            .filter(|document| document.is_dirty())
            .filter_map(|document| {
                Some((
                    document.meta.resource.local_path()?.to_path_buf(),
                    document.buffer.version(),
                ))
            })
            .collect::<Vec<_>>();
        versions.sort();
        versions
    }

    // ---- Opening -----------------------------------------------------------

    /// Restore only metadata and tab order on the UI thread, without touching
    /// the file or building a line index. The background restorer fills it in.
    pub(crate) fn open_deferred(&mut self, path: PathBuf) -> FileId {
        if let Some(id) = self.file_id_for_path(&path) {
            return self.attach_text_tab(id, OpenMode::Permanent);
        }
        let mut document = OpenDocument::new(
            FileMeta::from_path(path.clone()),
            String::new(),
            DiskState::from_contents(""),
        );
        document.loading = Some(false);
        let id = self.allocate_file_id();
        self.insert_document(id, document);
        self.paths.insert(ResourceRef::local(path), id);
        self.attach_text_tab(id, OpenMode::Permanent)
    }

    pub(crate) fn is_loading(&self, id: FileId) -> bool {
        self.documents.get(&id).is_some_and(|d| d.loading.is_some())
    }

    pub(crate) fn open_error(&self, id: FileId) -> Option<&str> {
        self.documents.get(&id)?.open_error.as_deref()
    }

    pub(crate) fn pane_open_error(&self, pane: PaneId) -> Option<&str> {
        self.open_error(self.pane_document(pane)?)
    }

    pub(crate) fn fail_open(&mut self, id: FileId, path: &Path, error: String) -> bool {
        let Some(document) = self.documents.get_mut(&id).filter(|document| {
            document.loading.is_some() && document.meta.resource.local_path() == Some(path)
        }) else {
            return false;
        };
        document.loading = None;
        document.open_error = Some(error);
        true
    }

    pub(crate) fn loading_indicator(&self, id: FileId) -> bool {
        self.documents
            .get(&id)
            .is_some_and(|d| d.loading == Some(true))
    }

    pub(crate) fn show_restore_indicators(&mut self) -> bool {
        let mut changed = false;
        for document in self.documents.values_mut() {
            if document.loading == Some(false) {
                document.loading = Some(true);
                changed = true;
            }
        }
        changed
    }

    /// Active documents first, followed by tabs in visible order. Shared
    /// documents are prepared once even when shown in more than one pane.
    pub(crate) fn loading_files(&self) -> Vec<(FileId, PathBuf)> {
        let mut ids = Vec::new();
        if let Some(id) = self.active_document() {
            ids.push(id);
        }
        for pane in self.pane_ids() {
            if let Some(id) = self.pane_document(pane) {
                ids.push(id);
            }
        }
        ids.extend(self.ordered_documents());
        let mut seen = std::collections::HashSet::new();
        ids.into_iter()
            .filter(|id| seen.insert(*id) && self.is_loading(*id))
            .filter_map(|id| Some((id, self.meta(id)?.resource.local_path()?.to_path_buf())))
            .collect()
    }

    /// Fill a surviving placeholder in place. Loading must not reactivate,
    /// reorder or resurrect a tab the user moved or closed in the meantime.
    pub(crate) fn install_restored(
        &mut self,
        id: FileId,
        path: &Path,
        prepared: PreparedTextFile,
    ) -> bool {
        let Some(document) = self.documents.get_mut(&id).filter(|d| {
            d.loading.is_some()
                && d.meta.resource.local_path() == Some(path)
                && prepared.0.meta.resource.local_path() == Some(path)
        }) else {
            self.retired.push(Retired::Document(prepared.0));
            return false;
        };
        let mut replacement = prepared.0;
        replacement.recovery = document.recovery.take();
        restore_encoding(&mut replacement);
        replacement.view_ids = std::mem::take(&mut document.view_ids);
        replacement.incarnation = document.incarnation;
        self.retired
            .push(Retired::Document(std::mem::replace(document, replacement)));
        self.clamp_document_views(id);
        true
    }

    pub(crate) fn attach_text_tab(&mut self, id: FileId, mode: OpenMode) -> FileId {
        let pane = self.active_pane;
        let svg = crate::domain::media::is_svg(self.documents[&id].meta.language_hint());
        let presentation = if self.documents[&id].image.is_some() || svg {
            Presentation::Image
        } else {
            Presentation::Source
        };
        let same_document = |v: &ViewId| {
            self.views[v].document() == Some(id)
                && (svg || self.views[v].presentation == presentation)
        };
        let existing = self.panes[&pane]
            .active
            .filter(same_document)
            .or_else(|| self.panes[&pane].items.iter().copied().find(same_document));
        let view = if let Some(view) = existing {
            if mode == OpenMode::Permanent {
                self.promote_preview(pane, view);
            }
            view
        } else {
            let index = self
                .preview_replacement_index(pane, mode)
                .unwrap_or(usize::MAX);
            let source = self
                .tabs_of(id)
                .first()
                .and_then(|(_, v)| self.views.get(v))
                .cloned();
            let view = self.create_view(ViewTarget::Text(id), presentation);
            if let Some(source) = source {
                self.views.get_mut(&view).unwrap().source = source.source;
            }
            self.insert_item(pane, view, index);
            if mode == OpenMode::Preview {
                self.replace_preview_slot(pane, view);
            }
            view
        };
        self.active_pane_mut().active = Some(view);
        id
    }

    pub(crate) fn open_prepared_text(
        &mut self,
        prepared: PreparedTextFile,
        mode: OpenMode,
    ) -> FileId {
        let resource = prepared.0.meta.resource.clone();
        if let Some(id) = self.file_id_for_resource(&resource) {
            if self.open_error(id).is_some() {
                let old = self.documents.remove(&id).expect("failed document exists");
                let mut replacement = prepared.0;
                replacement.view_ids = old.view_ids.clone();
                replacement.recovery = old.recovery.clone();
                if replacement.open_error.is_none() {
                    restore_encoding(&mut replacement);
                }
                self.insert_document(id, replacement);
                self.retired.push(Retired::Document(old));
                self.clamp_document_views(id);
            } else {
                self.retired.push(Retired::Document(prepared.0));
            }
            return self.attach_text_tab(id, mode);
        }
        let id = self.allocate_file_id();
        self.insert_document(id, prepared.0);
        self.paths.insert(resource, id);
        self.attach_text_tab(id, mode)
    }

    /// Where a new preview tab goes: in place of the pane's clean preview.
    fn preview_replacement_index(&mut self, pane: PaneId, mode: OpenMode) -> Option<usize> {
        if mode != OpenMode::Preview {
            return None;
        }
        let preview = self.panes[&pane].preview?;
        if self.is_view_dirty(preview) {
            // Defensive promotion: UI edit paths promote eagerly, but no
            // missed path may allow a dirty preview to be replaced.
            self.panes.get_mut(&pane).unwrap().preview = None;
            return None;
        }
        self.panes[&pane]
            .items
            .iter()
            .position(|id| *id == preview)
            // The new tab is inserted before the old preview is closed.
            .map(|index| index + 1)
    }

    /// The one way a document enters the workspace: it gets a fresh incarnation,
    /// so a stale result for an earlier document can never match it.
    pub(crate) fn insert_document(&mut self, id: FileId, mut document: OpenDocument) {
        document.incarnation = self.next_incarnation;
        self.next_incarnation += 1;
        if document.meta.local_path().is_some() {
            document.meta.lang = self.language_for(document.meta.language_hint());
        }
        self.documents.insert(id, document);
    }

    /// The language the file decides: an association first, then the built-in
    /// recognition by name and extension.
    pub(crate) fn language_for(&self, path: &Path) -> ResolvedLanguage {
        self.language_rules
            .resolve(path)
            .unwrap_or_else(|| self.language_registry.resolve_path(path))
    }

    /// Installs new associations and re-recognizes every open file. A document
    /// whose language changes gets a new language generation, so results for
    /// the old language stop matching. Returns whether any document changed.
    pub(crate) fn set_language_rules(
        &mut self,
        registry: Arc<crate::domain::language::Registry>,
        rules: Arc<LanguageRules>,
    ) -> bool {
        let mut changed = false;
        let text = self
            .documents
            .values_mut()
            .filter(|document| document.is_text())
            .map(|document| &mut document.meta);
        let diffs = self.diffs.values_mut().map(|diff| &mut diff.meta);
        for meta in text.chain(diffs) {
            if meta.resource.environment().is_some() {
                continue;
            }
            let language = rules
                .resolve(meta.language_hint())
                .unwrap_or_else(|| registry.resolve_path(meta.language_hint()));
            let generation = meta.language_generation;
            meta.set_language(language);
            changed |= meta.language_generation != generation;
        }
        self.language_registry = registry;
        self.language_rules = rules;
        changed
    }

    pub(crate) fn allocate_file_id(&mut self) -> FileId {
        let id = FileId::new(self.next_file_id);
        self.next_file_id += 1;
        id
    }

    /// Activate an existing single-instance tab wherever it is.
    pub(crate) fn reveal(&mut self, id: ViewId) -> bool {
        match self.pane_of(id) {
            Some(pane) => {
                self.activate(pane, id);
                true
            }
            None => false,
        }
    }

    /// Open the settings page, or focus it if it is already open.
    pub fn open_settings(&mut self) -> ViewId {
        self.open_page(AppPage::Settings)
    }

    /// Open the keymap page, or focus it if it is already open.
    pub fn open_keymap(&mut self) -> ViewId {
        self.open_page(AppPage::Keymap)
    }

    /// Open the extensions page, or focus it if it is already open.
    pub fn open_extensions(&mut self) -> ViewId {
        self.open_page(AppPage::Extensions)
    }

    /// Open a built-in page; each page has at most one tab in the workspace.
    pub fn open_page(&mut self, page: AppPage) -> ViewId {
        if let Some(id) = self.page_id(page) {
            self.reveal(id);
            return id;
        }
        let id = self.create_view(ViewTarget::Builtin(page), Presentation::Source);
        let pane = self.active_pane;
        self.insert_item(pane, id, usize::MAX);
        self.activate(pane, id);
        id
    }

    pub fn page_id(&self, page: AppPage) -> Option<ViewId> {
        self.views
            .values()
            .find(|v| v.target == ViewTarget::Builtin(page))
            .map(|v| v.id)
    }

    /// The built-in page a tab shows, if it is not a file or diff.
    pub fn page(&self, id: ViewId) -> Option<AppPage> {
        match self.views.get(&id)?.target {
            ViewTarget::Builtin(page) => Some(page),
            _ => None,
        }
    }

    pub fn settings_id(&self) -> Option<ViewId> {
        self.page_id(AppPage::Settings)
    }

    pub fn is_settings(&self, id: ViewId) -> bool {
        self.page(id) == Some(AppPage::Settings)
    }

    /// Whether a tab is backed by a file (rather than a diff or settings).
    pub fn is_file(&self, id: ViewId) -> bool {
        self.view_document(id).is_some()
    }

    // ---- Disk --------------------------------------------------------------

    /// Delayed disk notifications never overwrite a save, rename, closed tab,
    /// or editor changes made while the worker was reading the file.
    pub fn apply_disk_updates(&mut self, updates: Vec<PreparedDiskUpdate>) -> Vec<ReconcileResult> {
        let mut outcomes = Vec::new();
        let mut retired = Vec::new();
        for update in updates {
            let Some(document) = self.documents.get_mut(&update.id) else {
                retired.push(update);
                continue;
            };
            if document.meta.resource.local_path() != Some(update.path.as_path())
                || document.disk_state != update.expected_disk
                || document.disk_observation != update.expected_observation
                || !Arc::ptr_eq(&document.saved_text, &update.expected_saved)
            {
                retired.push(update);
                continue;
            }
            document.disk_observation += 1;
            document.observed_disk = update.replacement.observed_disk.clone();
            if matches!(update.outcome, ReconcileResult::Reloaded(_))
                && document.buffer.version() != update.expected_version
            {
                document.disk_conflict = true;
                outcomes.push(ReconcileResult::Conflict(update.path.clone()));
                retired.push(update);
                continue;
            }
            let old = document.clone();
            if matches!(update.outcome, ReconcileResult::Reloaded(_)) {
                document
                    .buffer
                    .install_prepared(update.replacement.buffer.clone());
                document.saved_text = update.replacement.saved_text.clone();
                document.saved_format = update.replacement.saved_format;
                document.disk_bytes = update.replacement.disk_bytes.clone();
                document.encoding_bytes = update.replacement.encoding_bytes.clone();
            }
            document.disk_state = update.replacement.disk_state.clone();
            document.disk_conflict = update.replacement.disk_conflict;
            if matches!(update.outcome, ReconcileResult::Missing(_))
                && document.buffer.version() != update.expected_version
            {
                document.disk_conflict = true;
            }
            document.missing_on_disk = update.replacement.missing_on_disk;
            if document.image.is_some() || update.replacement.image.is_some() {
                document.image = update.replacement.image.clone();
                document.open_error = update.replacement.open_error.clone();
            }
            outcomes.push(update.outcome.clone());
            // Keep old shared text alive until the reclaimer frees it.
            self.retired.push(Retired::Document(old));
            self.clamp_document_views(update.id);
            retired.push(update);
        }
        self.retired
            .extend(retired.into_iter().map(Retired::Update));
        outcomes
    }

    /// Close every editable document below a directory removed on disk.
    pub fn close_paths_under(&mut self, root: &Path) -> usize {
        let ids = self
            .paths
            .iter()
            .filter_map(|(path, id)| {
                path.local_path()
                    .is_some_and(|path| path.starts_with(root))
                    .then_some(*id)
            })
            .collect::<Vec<_>>();
        let count = ids.len();
        for id in ids {
            self.close(id);
        }
        count
    }
}

/// External reloads reset edit history but retain each view's caret and
/// selection as far as the new document length permits. Scroll positions
/// live in the views and are therefore preserved as well.
pub(crate) fn replace_buffer_from_disk(document: &mut OpenDocument, contents: String) {
    document.buffer.reload(contents);
}

fn parent_components(path: &Path) -> Vec<String> {
    path.parent()
        .into_iter()
        .flat_map(Path::components)
        .filter_map(|component| {
            let value = component.as_os_str().to_string_lossy();
            (!value.is_empty() && value != "\\" && value != "/").then(|| value.into_owned())
        })
        .collect()
}

/// A path without the Windows verbatim prefix, for comparing spellings.
fn comparable(path: &Path) -> &Path {
    let Some(value) = path.to_str() else {
        return path;
    };
    match value.strip_prefix(r"\\?\") {
        Some(local) if local.as_bytes().get(1) == Some(&b':') => Path::new(local),
        _ => path,
    }
}

fn parent_suffix(parts: &[String], depth: usize) -> String {
    let start = parts.len().saturating_sub(depth);
    parts[start..].join("/")
}

#[cfg(test)]
mod media_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod view_tests;

pub(crate) fn restore_encoding(document: &mut OpenDocument) {
    if let Some(recovery) = document.recovery.take() {
        let restore_unsaved = recovery.unsaved_text.is_some();
        let ending = recovery.line_ending.as_deref().and_then(|e| match e {
            "LF" => Some(loog_editor_core::text::LineEnding::Lf),
            "CRLF" => Some(loog_editor_core::text::LineEnding::Crlf),
            _ => None,
        });
        document.meta.read_override = recovery.read_override;
        if let Some(text) = recovery.unsaved_text {
            document.open_error = None;
            document.loading = None;
            let disk_changed = document.buffer.text()
                != recovery
                    .saved_text
                    .as_ref()
                    .map_or(document.buffer.text(), |text| text.as_str())
                || document.saved_format != recovery.saved;
            document.buffer = TextBuffer::new(text.as_ref().clone());
            document.saved_text = recovery
                .saved_text
                .unwrap_or_else(|| Arc::new(String::new()));
            document.saved_format = recovery.saved;
            document.disk_conflict = disk_changed;
            document.buffer.initialize_metadata(recovery.current.code());
            document.encoding_bytes = recovery.source_bytes;
        }
        if let Some(ending) = ending.filter(|e| {
            restore_unsaved
                || document.buffer.line_count() == 1
                || document.buffer.line_ending() == e.text()
        }) {
            document.buffer.initialize_line_ending(ending);
        }
    }
}
