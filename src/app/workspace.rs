//! Application composition of the pure workspace and its platform adapters.
use crate::domain::identity::{FileId, PaneId, ViewId};
use crate::domain::pane_layout::Direction;
use crate::domain::workspace as domain;
use crate::features::git::DiffRequest;
use crate::features::git::diff_document::DiffDocument;
use crate::infrastructure::workspace_filesystem as filesystem;
pub use domain::{AppPage, OpenMode, ReconcileResult};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
pub(crate) type Core = domain::Workspace<DiffDocument, DiffRequest>;
pub(crate) type PreparedTextFile = domain::PreparedTextFile;
pub type PreparedDiskUpdate = domain::PreparedDiskUpdate;
#[derive(Clone)]
pub struct Workspace {
    pub(crate) core: Core,
}
impl Deref for Workspace {
    type Target = Core;
    fn deref(&self) -> &Core {
        &self.core
    }
}
impl DerefMut for Workspace {
    fn deref_mut(&mut self) -> &mut Core {
        &mut self.core
    }
}
impl Workspace {
    pub fn new() -> Self {
        Self { core: Core::new() }
    }

    /// Reinterpret retained bytes immediately. A manual decoding choice is never
    /// judged by whether the resulting text looks sensible or contains errors.
    pub fn set_encoding(
        &mut self,
        id: FileId,
        format: crate::domain::encoding::TextFormat,
    ) -> Result<bool, String> {
        use crate::features::encoding;
        use std::sync::Arc;
        format.validate()?;
        let document = self.core.documents.get(&id).ok_or("Document is not open")?;
        if !document.is_text() {
            return Err("Document is loading".into());
        }
        if document.format() == format {
            return Ok(false);
        }
        let limit = crate::infrastructure::document_filesystem::MAX_EDITABLE_FILE_BYTES as usize;
        let source = [&document.encoding_bytes, &document.disk_bytes]
            .into_iter()
            .flatten()
            .find(|bytes| {
                encoding::decode_explicit(bytes, document.format(), limit)
                    .is_ok_and(|decoded| decoded.text == document.buffer.text())
            })
            .cloned();
        let bytes = match source {
            Some(bytes) => bytes,
            None => match encoding::encode(document.buffer.text(), document.format()) {
                Ok(bytes) => Arc::new(bytes),
                Err(_) => {
                    // Unsaved Unicode edits may not fit the previous encoding
                    // (e.g. an emoji typed in a GBK document). Keep those edits
                    // intact, and let the newly selected save format represent them.
                    let changed = self.core.set_encoding(id, format)?;
                    let document = self.core.documents.get_mut(&id).unwrap();
                    document.meta.read_override = Some(format);
                    document.meta.encoding_source = encoding::EncodingSource::Explicit;
                    document.encoding_bytes = None;
                    return Ok(changed);
                }
            },
        };
        let mut decoded = encoding::decode_explicit(&bytes, format, limit)?;
        decoded.bytes = bytes;
        self.transaction(|core| core.set_decoded_encoding(id, decoded))
    }
    fn transaction<R>(&mut self, apply: impl FnOnce(&mut Core) -> R) -> R {
        let result = apply(&mut self.core);
        let retired = self.core.take_retired();
        for item in retired {
            match item {
                domain::Retired::Document(value) => {
                    crate::infrastructure::ui_dispatch::retire(value)
                }
                domain::Retired::Diff(value) => crate::infrastructure::ui_dispatch::retire(value),
                domain::Retired::Update(value) => crate::infrastructure::ui_dispatch::retire(value),
            }
        }
        result
    }
    pub fn mark_saved(&mut self, id: FileId) -> bool {
        let Some(doc) = self.core.documents.get(&id).filter(|doc| doc.is_text()) else {
            return false;
        };
        let Some(path) = doc.meta.local_path() else {
            return false;
        };
        let disk = filesystem::capture_format(path, doc.buffer.text(), doc.format());
        self.transaction(|core| core.mark_saved_with_disk(id, disk))
    }

    pub fn open_path(&mut self, path: PathBuf, contents: String) -> FileId {
        self.open_path_with_mode(path, contents, OpenMode::Permanent)
    }

    pub fn preview_path(&mut self, path: PathBuf, contents: String) -> FileId {
        self.open_path_with_mode(path, contents, OpenMode::Preview)
    }

    fn open_path_with_mode(&mut self, path: PathBuf, contents: String, mode: OpenMode) -> FileId {
        if let Some(id) = self
            .file_id_for_path(&path)
            .filter(|id| self.open_error(*id).is_none())
        {
            return self.transaction(|core| core.attach_text_tab(id, mode));
        }
        self.open_prepared_text(PreparedTextFile::new(path, contents), mode)
    }

    pub(crate) fn open_failed(&mut self, path: PathBuf, error: String, mode: OpenMode) -> FileId {
        self.open_prepared_text(PreparedTextFile::failed(path, error), mode)
    }

    pub fn reconcile_disk(&mut self) -> Vec<ReconcileResult> {
        self.ordered_documents()
            .into_iter()
            .map(|id| self.reconcile_document(id))
            .collect()
    }

    pub fn prepare_disk_updates(
        &self,
        changed_paths: &[PathBuf],
        full_rescan: bool,
    ) -> Vec<PreparedDiskUpdate> {
        let mut working = self.clone();
        let ids = self
            .ordered_documents()
            .into_iter()
            .filter(|id| {
                self.core.documents.get(id).is_some_and(|document| {
                    (document.is_text() || document.image.is_some())
                        && document.meta.local_path().is_some_and(|local| {
                            full_rescan || changed_paths.iter().any(|path| local.starts_with(path))
                        })
                })
            })
            .collect::<Vec<_>>();
        let mut updates = Vec::new();
        for id in ids {
            let original = &self.core.documents[&id];
            let outcome = working.reconcile_document(id);
            if matches!(outcome, ReconcileResult::Failed(_, _)) {
                continue;
            }
            let replacement = working.documents.remove(&id).unwrap();
            updates.push(PreparedDiskUpdate {
                id,
                path: original
                    .meta
                    .local_path()
                    .expect("local watcher document")
                    .to_path_buf(),
                expected_disk: original.disk_state.clone(),
                expected_observation: original.disk_observation,
                expected_saved: original.saved_text.clone(),
                expected_version: original.buffer.version(),
                replacement,
                outcome,
            });
        }
        updates
    }

    pub fn reconcile_changed_paths(
        &mut self,
        changed_paths: &[PathBuf],
        full_rescan: bool,
    ) -> Vec<ReconcileResult> {
        let ids = self
            .ordered_documents()
            .into_iter()
            .filter(|id| {
                full_rescan
                    || self.core.documents.get(id).is_some_and(|document| {
                        (document.is_text() || document.image.is_some())
                            && changed_paths.iter().any(|changed| {
                                document
                                    .meta
                                    .local_path()
                                    .is_some_and(|path| path.starts_with(changed))
                            })
                    })
            })
            .collect::<Vec<_>>();
        ids.into_iter()
            .map(|id| self.reconcile_document(id))
            .collect()
    }

    pub fn reconcile_document(&mut self, id: FileId) -> ReconcileResult {
        if let Some(document) = self.core.documents.get(&id)
            && document.meta.local_path().is_none()
        {
            return ReconcileResult::Provided(document.meta.resource.clone());
        }
        if self
            .core
            .documents
            .get(&id)
            .is_some_and(|d| d.image.is_some())
        {
            let document = &self.core.documents[&id];
            let path = document
                .meta
                .local_path()
                .expect("local document checked above")
                .to_path_buf();
            let unchanged = std::fs::metadata(&path).is_ok_and(|metadata| {
                metadata.len() == document.disk_state.size
                    && metadata.modified().ok() == document.disk_state.modified_time
            });
            if unchanged && document.open_error.is_none() {
                return ReconcileResult::Unchanged(path);
            }
            let loaded = crate::infrastructure::image_filesystem::load(&path);
            let document = self.core.documents.get_mut(&id).unwrap();
            return match loaded {
                Ok((image, disk)) => {
                    document.image = Some(std::sync::Arc::new(image));
                    document.disk_state = disk;
                    document.open_error = None;
                    document.missing_on_disk = false;
                    ReconcileResult::Reloaded(path)
                }
                Err(error) => {
                    document.missing_on_disk = !path.exists();
                    document.open_error =
                        Some(format!("Could not open {}: {error}", path.display()));
                    if document.missing_on_disk {
                        ReconcileResult::Missing(path)
                    } else {
                        ReconcileResult::Reloaded(path)
                    }
                }
            };
        }
        let observation = self
            .core
            .documents
            .get(&id)
            .filter(|doc| doc.is_text())
            .map_or(crate::domain::document::DiskRead::Missing, |doc| {
                filesystem::read_with_encoding(
                    doc.meta.local_path().expect("local document checked above"),
                    doc.meta.read_override.map(|_| doc.format()),
                )
            });
        self.transaction(|core| core.reconcile_with_disk(id, observation))
    }

    pub fn accept_disk_version(&mut self, id: FileId) -> Result<(), String> {
        let document = self
            .core
            .documents
            .get(&id)
            .ok_or_else(|| "Document is not open.".to_owned())?;
        if !document.is_text() {
            return Err("Diff documents are read-only.".to_owned());
        }
        let (decoded, disk) = crate::infrastructure::document_filesystem::read_encoded(
            document
                .meta
                .local_path()
                .ok_or("Resource reload requires its provider")?,
            document.meta.read_override.map(|_| document.format()),
            crate::infrastructure::document_filesystem::MAX_EDITABLE_FILE_BYTES,
        )
        .map_err(|error| {
            format!(
                "Could not reload {}: {error}",
                document.meta.resource.display()
            )
        })?;
        self.transaction(|core| {
            core.accept_disk_with(id, decoded.text, disk)?;
            if let Some(document) = core.documents.get_mut(&id) {
                document.disk_bytes = Some(decoded.bytes.clone());
                document.encoding_bytes = Some(decoded.bytes);
                document.meta.encoding_source = decoded.source;
            }
            Ok(())
        })
    }

    pub fn keep_editor_version(&mut self, id: FileId) -> bool {
        let Some(document) = self.core.documents.get(&id).filter(|doc| doc.is_text()) else {
            return false;
        };
        let Some(path) = document.meta.local_path() else {
            return false;
        };
        let disk = filesystem::read_contents(path)
            .ok()
            .map(|contents| filesystem::capture(path, &contents));
        self.transaction(|core| core.keep_editor_with_disk(id, disk))
    }

    pub fn move_open_path(&mut self, old_path: &Path, new_path: PathBuf) -> bool {
        let Some(id) = self
            .core
            .paths
            .get(&loog_workspace_api::ResourceRef::local(old_path))
        else {
            return false;
        };
        let Some(document) = self.core.documents.get(id) else {
            return false;
        };
        let disk = filesystem::capture(&new_path, document.buffer.text());
        self.transaction(|core| core.move_path_with_disk(old_path, new_path, disk))
    }

    pub fn move_open_paths_under(&mut self, old_root: &Path, new_root: &Path) -> usize {
        let moves = self
            .paths
            .keys()
            .filter_map(|path| {
                let path = path.local_path()?;
                path.strip_prefix(old_root)
                    .ok()
                    .map(|relative| (path.to_path_buf(), new_root.join(relative)))
            })
            .collect::<Vec<_>>();
        let mut moved = 0;
        for (old_path, new_path) in moves {
            moved += usize::from(self.move_open_path(&old_path, new_path));
        }
        moved
    }
    pub fn close_item(&mut self, pane: PaneId, id: ViewId) {
        self.transaction(|core| core.close_item(pane, id))
    }

    pub fn close(&mut self, id: FileId) {
        self.transaction(|core| core.close(id))
    }

    pub fn close_pane(&mut self, pane: PaneId) {
        self.transaction(|core| core.close_pane(pane))
    }

    pub fn close_paths_under(&mut self, root: &Path) -> usize {
        self.transaction(|core| core.close_paths_under(root))
    }

    pub(crate) fn open_prepared_text(
        &mut self,
        prepared: PreparedTextFile,
        mode: OpenMode,
    ) -> FileId {
        self.transaction(|core| core.open_prepared_text(prepared, mode))
    }

    pub(crate) fn install_restored(
        &mut self,
        id: FileId,
        path: &Path,
        prepared: PreparedTextFile,
    ) -> bool {
        self.transaction(|core| core.install_restored(id, path, prepared))
    }

    pub fn apply_disk_updates(&mut self, updates: Vec<PreparedDiskUpdate>) -> Vec<ReconcileResult> {
        self.transaction(|core| core.apply_disk_updates(updates))
    }

    pub fn split(&mut self, pane: PaneId, direction: Direction) -> Option<PaneId> {
        self.transaction(|core| core.split(pane, direction))
    }

    pub fn split_empty(&mut self, pane: PaneId, direction: Direction) -> Option<PaneId> {
        self.transaction(|core| core.split_empty(pane, direction))
    }

    pub fn split_with_item(
        &mut self,
        from: PaneId,
        id: ViewId,
        target: PaneId,
        direction: Direction,
        copy: bool,
    ) -> bool {
        self.transaction(|core| core.split_with_item(from, id, target, direction, copy))
    }

    pub fn move_item(&mut self, from: PaneId, id: ViewId, to: PaneId, index: usize) -> bool {
        self.transaction(|core| core.move_item(from, id, to, index))
    }

    pub fn copy_item(&mut self, from: PaneId, id: ViewId, to: PaneId, index: usize) -> bool {
        self.transaction(|core| core.copy_item(from, id, to, index))
    }
    pub fn move_tab(&mut self, pane: PaneId, id: ViewId, index: usize) -> bool {
        self.core.move_tab(pane, id, index)
    }
    pub fn move_active_item(&mut self, direction: Direction) -> bool {
        self.transaction(|core| core.move_active_item(direction))
    }
}
impl PreparedTextFile {
    pub fn new(path: PathBuf, contents: String) -> Self {
        let disk = filesystem::capture(&path, &contents);
        let options = crate::infrastructure::editorconfig::load(&path);
        Self::with_disk_state(path, contents, disk).with_file_options(options)
    }
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
