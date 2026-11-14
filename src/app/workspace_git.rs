//! Git payload adaptation stays outside the pure workspace rules.
use super::workspace::*;
use crate::domain::document::FileMeta;
use crate::domain::identity::{PaneId, ViewId};
use crate::features::git::diff_document::DiffDocument;
use crate::features::git::{DiffContent, DiffRequest, DiffTarget, GitResult, PollTarget};
use std::path::{Path, PathBuf};
use std::sync::Arc;
impl Workspace {
    pub fn open_diff(&mut self, diff: DiffDocument) -> ViewId {
        self.open_prepared_diff(Arc::new(diff))
    }

    pub fn open_prepared_file_changes(&mut self, diff: Arc<DiffDocument>) -> ViewId {
        if let Some((_, document)) = self.core.diffs.iter_mut().find(|(_, document)| {
            let current = &document.content;
            current.repository_root == diff.repository_root
                && current.sections.is_none()
                && current.revision.is_none()
        }) {
            let changed_file = {
                let current = &document.content;
                !current.same_view(&diff)
            };
            document.meta.name = diff.title();
            document.meta.resource = diff.resource();
            document.focused_diff = None;
            if changed_file {
                if let Some(view) = self.core.views.get_mut(&document.view) {
                    view.source.scroll_x = 0.0;
                    view.source.scroll_y = 0.0;
                }
            }
            crate::infrastructure::ui_dispatch::retire(std::mem::replace(
                &mut document.content,
                diff,
            ));
            let id = document.view;
            self.reveal(id);
            return id;
        }
        self.open_prepared_diff(diff)
    }

    pub fn open_prepared_commit_changes(&mut self, diff: Arc<DiffDocument>) -> ViewId {
        if let Some((_, document)) = self.core.diffs.iter_mut().find(|(_, document)| {
            let current = &document.content;
            current.repository_root == diff.repository_root
                && current.sections.is_some()
                && current.revision.is_some()
        }) {
            let changed_commit = {
                let current = &document.content;
                current.revision != diff.revision
            };
            document.meta.name = diff.title();
            document.meta.resource = diff.resource();
            if changed_commit {
                document.collapsed_diffs.clear();
                document.focused_diff = None;
                if let Some(view) = self.core.views.get_mut(&document.view) {
                    view.source.scroll_x = 0.0;
                    view.source.scroll_y = 0.0;
                }
            }
            crate::infrastructure::ui_dispatch::retire(std::mem::replace(
                &mut document.content,
                diff,
            ));
            let id = document.view;
            self.reveal(id);
            return id;
        }
        self.open_prepared_diff(diff)
    }

    pub fn commit_changes_id(&self, repository_root: &Path, revision: &str) -> Option<ViewId> {
        self.core.diffs.iter().find_map(|(_, document)| {
            {
                let diff = &document.content;
                diff.repository_root == repository_root
                    && diff.sections.is_some()
                    && diff.revision.as_deref() == Some(revision)
            }
            .then_some(document.view)
        })
    }

    pub fn open_prepared_diff(&mut self, diff: Arc<DiffDocument>) -> ViewId {
        if let Some((_, document)) = self
            .core
            .diffs
            .iter_mut()
            .find(|(_, document)| document.content.same_view(&diff))
        {
            document.meta.name = diff.title();
            document.meta.resource = diff.resource();
            crate::infrastructure::ui_dispatch::retire(std::mem::replace(
                &mut document.content,
                diff,
            ));
            let id = document.view;
            self.reveal(id);
            return id;
        }

        self.insert_prepared_diff(self.core.active_pane, diff)
    }

    pub fn open_prepared_diff_in(
        &mut self,
        pane: PaneId,
        diff: Arc<DiffDocument>,
    ) -> Option<ViewId> {
        self.core.panes.get(&pane)?;
        let id = self.open_prepared_diff(diff);
        let from = self.pane_of(id)?;
        if from != pane {
            self.move_item(from, id, pane, usize::MAX);
        } else {
            self.activate(pane, id);
        }
        Some(id)
    }

    fn insert_prepared_diff(&mut self, pane: PaneId, diff: Arc<DiffDocument>) -> ViewId {
        let mut meta = match diff.resource() {
            loog_workspace_api::ResourceRef::Local { path } => FileMeta::from_path(path),
            resource => FileMeta::provided(resource, diff.title()),
        };
        meta.name = diff.title();
        self.core.insert_diff(pane, meta, diff)
    }

    /// The paths of the files with unsaved edits as the Git model knows them.
    pub fn dirty_git_paths(&self) -> Vec<PathBuf> {
        self.dirty_file_ids()
            .into_iter()
            .filter_map(|id| self.documents.get(&id))
            .filter_map(|document| crate::features::git::location::git_path(&document.meta.resource))
            .collect()
    }

    /// The path of the active file as the Git model knows it, local or in an environment.
    pub fn active_git_path(&self) -> Option<PathBuf> {
        if self.active_page().is_some() {
            return None;
        }
        crate::features::git::location::git_path(&self.active_meta()?.resource)
    }

    pub fn git_poll_target(&self) -> PollTarget {
        let mut diffs = self
            .diffs
            .values()
            // A commit's diff never changes, so it is not refreshed.
            .filter_map(|document| {
                Some(&document.content).filter(|diff| diff.revision.is_none())
            })
            .map(|diff| DiffRequest {
                repository_root: diff.repository_root.clone(),
                path: diff.path.clone(),
                target: diff.target,
            })
            .collect::<Vec<_>>();
        diffs.sort();
        diffs.dedup();
        PollTarget {
            active_path: self.active_git_path(),
            diffs,
            pinned_repository: None,
        }
    }

    pub fn apply_git_diffs(
        &mut self,
        diffs: &std::collections::BTreeMap<DiffRequest, GitResult<DiffContent>>,
    ) -> bool {
        let mut changed = false;
        let core = &mut self.core;
        for document in core.diffs.values_mut() {
            let current = &document.content;
            let request = DiffRequest {
                repository_root: current.repository_root.clone(),
                path: current.path.clone(),
                target: current.target,
            };
            let Some(Ok(content)) = diffs.get(&request) else {
                continue;
            };
            let next = DiffDocument::from_content(request, content.clone());
            if **current != next {
                document.content = Arc::new(next);
                changed = true;
            }
        }
        changed
    }

    pub fn apply_prepared_git_diffs(
        &mut self,
        diffs: &std::collections::BTreeMap<DiffRequest, Arc<DiffDocument>>,
        split: bool,
    ) -> Vec<Arc<DiffDocument>> {
        self.replace_prepared_diffs(diffs, split, |_| true)
    }

    pub fn loading_diff(
        &self,
        request: &DiffRequest,
        revision: Option<&str>,
        sequence: u64,
    ) -> Option<Arc<DiffDocument>> {
        self.core.diffs.values().find_map(|record| {
            let doc = &record.content;
            (doc.loading == Some(sequence)
                && doc.request() == *request
                && doc.revision.as_deref() == revision)
                .then(|| doc.clone())
        })
    }

    /// Fill an existing progressive tab without reopening it or taking focus.
    /// A closed, refreshed or superseded tab rejects the old worker's result.
    pub fn update_loading_diff(
        &mut self,
        next: Arc<DiffDocument>,
        sequence: u64,
        split: bool,
    ) -> Vec<Arc<DiffDocument>> {
        let revision = next.revision.clone();
        let diffs = [(next.request(), next)].into_iter().collect();
        self.replace_prepared_diffs(&diffs, split, |current| {
            current.loading == Some(sequence) && current.revision == revision
        })
    }

    fn replace_prepared_diffs(
        &mut self,
        diffs: &std::collections::BTreeMap<DiffRequest, Arc<DiffDocument>>,
        split: bool,
        accept: impl Fn(&DiffDocument) -> bool,
    ) -> Vec<Arc<DiffDocument>> {
        let mut retired = Vec::new();
        let core = &mut self.core;
        for document in core.diffs.values_mut() {
            let current = &document.content;
            let request = DiffRequest {
                repository_root: current.repository_root.clone(),
                path: current.path.clone(),
                target: current.target,
            };
            if let Some(next) = diffs.get(&request)
                && accept(current)
                && !Arc::ptr_eq(current, next)
            {
                if let (Some(old_sections), Some(new_sections)) =
                    (&current.sections, &next.sections)
                {
                    let old_layout = current.changes_layout(split, &document.collapsed_diffs);
                    let new_layout = next.changes_layout(split, &document.collapsed_diffs);
                    if let Some(view) = core.views.get_mut(&document.view) {
                        let row = view.source.scroll_y / crate::ui::diff_editor::row_h();
                        if let Some(old) = old_layout
                            .sections
                            .iter()
                            .rfind(|item| item.top as f32 <= row)
                        {
                            let request = old_sections[old.index].request();
                            if let Some(new) = new_layout
                                .sections
                                .iter()
                                .find(|item| new_sections[item.index].request() == request)
                            {
                                view.source.scroll_y = (new.top as f32 + row - old.top as f32)
                                    .max(0.0)
                                    * crate::ui::diff_editor::row_h();
                            }
                        }
                    }
                }
                retired.push(std::mem::replace(&mut document.content, next.clone()));
            }
        }
        retired
    }

    pub fn focus_diff_section(&mut self, request: &DiffRequest, split: bool) {
        let Some(id) = self.active() else { return };
        let Some(crate::domain::view::ViewTarget::Diff(diff_id)) =
            self.core.views.get(&id).map(|v| v.target)
        else {
            return;
        };
        let Some(document) = self.core.diffs.get_mut(&diff_id) else {
            return;
        };
        let diff = &document.content;
        let Some(sections) = &diff.sections else {
            return;
        };
        document.focused_diff = Some(request.clone());
        document.collapsed_diffs.remove(request);
        let layout = diff.changes_layout(split, &document.collapsed_diffs);
        if let Some(section) = layout
            .sections
            .iter()
            .find(|item| sections[item.index].request() == *request)
            && let Some(view) = self.core.views.get_mut(&id)
        {
            view.source.scroll_y = section.top as f32 * crate::ui::diff_editor::row_h();
        }
    }

    pub fn active_diff_focus(&self) -> Option<&DiffRequest> {
        let document = self.core.diff_record(self.active()?)?;
        document.content.sections.as_ref()?;
        document.focused_diff.as_ref()
    }

    pub fn diff_id_for(
        &self,
        repository_root: &Path,
        path: &Path,
        target: DiffTarget,
    ) -> Option<ViewId> {
        self.core.diffs.iter().find_map(|(_, document)| {
            document
                .content
                .matches(repository_root, path, target)
                .then_some(document.view)
        })
    }
}
