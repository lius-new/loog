//! Private search lifecycle data, with no framework or application dependency.
use super::{FileMatches, Results, SearchSpec};
use loog_workspace_api::ResourceRef;
use std::sync::Arc;

#[derive(Clone, PartialEq, Eq)]
pub struct SearchKey {
    pub spec: SearchSpec,
    pub roots: Vec<ResourceRef>,
    pub scopes: Vec<loog_workspace_api::ScopeId>,
    pub versions: Vec<(ResourceRef, u64)>,
    pub composing: bool,
    pub refresh: u64,
}
impl SearchKey {
    /// A filesystem hint can refresh the list while an explicitly selected
    /// result is read. Navigation independently verifies that file's content.
    pub fn same_navigation(&self, other: &Self) -> bool {
        self.spec == other.spec
            && self.roots == other.roots
            && self.scopes == other.scopes
            && self.versions == other.versions
            && self.composing == other.composing
    }
}

#[derive(Clone, Default)]
pub struct SearchStatus {
    pub results: Arc<Results>,
    pub running: bool,
    pub cancelled: bool,
    pub error: Option<String>,
    pub generation: u64,
    pub retaining_results: bool,
    pub applied_key: Option<SearchKey>,
}

#[derive(Clone, Default)]
pub struct SearchModel {
    status: SearchStatus,
}

pub struct Begin {
    pub generation: u64,
    pub running: bool,
    pub retained: bool,
    pub retired: Option<Arc<Results>>,
}

pub struct Applied {
    pub changed: bool,
    pub replaced: bool,
    pub retired: Option<Arc<Results>>,
}

impl SearchModel {
    pub fn status(&self) -> &SearchStatus {
        &self.status
    }

    pub fn begin(&mut self, key: SearchKey) -> Begin {
        let s = &mut self.status;
        s.generation += 1;
        let retained = !key.spec.query.is_empty()
            && s.applied_key
                .as_ref()
                .is_some_and(|old| old.spec == key.spec && old.roots == key.roots)
            && !s.results.files.is_empty();
        s.running = !key.composing && !key.spec.query.is_empty() && !key.roots.is_empty();
        s.cancelled = false;
        s.error = None;
        s.retaining_results = retained;
        s.applied_key = Some(key);
        Begin {
            generation: s.generation,
            running: s.running,
            retained,
            retired: (!retained).then(|| std::mem::take(&mut s.results)),
        }
    }

    pub fn apply(
        &mut self,
        generation: u64,
        expected: &SearchKey,
        current: &SearchKey,
        cancelled: bool,
        update: Result<Results, String>,
    ) -> Applied {
        let s = &mut self.status;
        if s.generation != generation || current != expected {
            return Applied {
                changed: false,
                replaced: false,
                retired: None,
            };
        }
        let mut replaced = false;
        let mut retired = None;
        match update {
            Ok(mut results) => {
                results.cancelled |= cancelled;
                s.running = !results.complete && !results.cancelled;
                s.cancelled = results.cancelled;
                if !(s.retaining_results
                    && results.files.is_empty()
                    && (!results.complete || results.cancelled))
                {
                    s.retaining_results = false;
                    retired = Some(std::mem::replace(&mut s.results, Arc::new(results)));
                    replaced = true;
                }
            }
            Err(error) => {
                s.running = false;
                s.retaining_results = false;
                s.error = Some(error);
            }
        }
        Applied {
            changed: true,
            replaced,
            retired,
        }
    }

    pub fn clear(&mut self) -> Arc<Results> {
        let s = &mut self.status;
        s.generation += 1;
        s.running = false;
        s.cancelled = false;
        s.error = None;
        s.retaining_results = false;
        std::mem::take(&mut s.results)
    }

    pub fn cancel(&mut self) -> bool {
        if !self.status.running {
            return false;
        }
        self.status.cancelled = true;
        self.status.running = false;
        true
    }

    pub fn navigation(
        &self,
        current: &SearchKey,
        file: Arc<FileMatches>,
        index: usize,
        permanent: bool,
        focus_editor: bool,
    ) -> Option<NavigationIntent> {
        if !self
            .status
            .applied_key
            .as_ref()
            .is_some_and(|key| key.same_navigation(current))
            || !self
                .status
                .results
                .files
                .iter()
                .any(|f| Arc::ptr_eq(f, &file))
            || index >= file.matches.len()
        {
            return None;
        }
        Some(NavigationIntent {
            generation: self.status.generation,
            key: current.clone(),
            spec: current.spec.clone(),
            file,
            index,
            permanent,
            focus_editor,
        })
    }

    #[cfg(test)]
    pub(crate) fn fixture_status(&mut self) -> &mut SearchStatus {
        &mut self.status
    }
}

#[derive(Clone)]
pub struct NavigationIntent {
    pub generation: u64,
    pub key: SearchKey,
    pub spec: SearchSpec,
    pub file: Arc<FileMatches>,
    pub index: usize,
    pub permanent: bool,
    pub focus_editor: bool,
}
