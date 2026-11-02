//! Document results and diagnostic identity shared by every provider source.
use super::Stamp;
use crate::domain::document::DocumentStamp;
use crate::features::{language_service::diagnostics::Diagnostic, syntax::TokenSpan};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug, Default)]
pub(crate) struct DocumentResults {
    pub diagnostics: Option<Arc<[Diagnostic]>>,
    pub semantic: Option<Arc<[TokenSpan]>>,
}
#[derive(Clone, Default)]
pub(crate) struct CodeActionContext {
    pub sources: Vec<(u64, u64, u64)>,
    pub diagnostics: Vec<Arc<[Diagnostic]>>,
}
impl PartialEq for CodeActionContext {
    fn eq(&self, other: &Self) -> bool {
        self.sources == other.sources
            && self.diagnostics.len() == other.diagnostics.len()
            && self
                .diagnostics
                .iter()
                .zip(&other.diagnostics)
                .all(|(a, b)| Arc::ptr_eq(a, b))
    }
}
impl Eq for CodeActionContext {}
impl CodeActionContext {
    pub fn has_sources(&self) -> bool {
        !self.sources.is_empty()
    }
}

pub(crate) struct Publication {
    pub stamp: DocumentStamp,
    pub text: Arc<String>,
    pub diagnostics: Arc<[Diagnostic]>,
}
#[derive(Default)]
pub(crate) struct ResultStore {
    pub findings: BTreeMap<(PathBuf, u64), Publication>,
}
impl ResultStore {
    pub fn publish(
        &mut self,
        source: u64,
        path: PathBuf,
        stamp: DocumentStamp,
        text: Arc<String>,
        diagnostics: Arc<[Diagnostic]>,
    ) {
        self.findings.insert(
            (path, source),
            Publication {
                stamp,
                text,
                diagnostics,
            },
        );
    }
    pub fn clear(&mut self, source: u64) {
        self.findings.retain(|(_, id), _| *id != source);
    }
    pub fn parts(&self, path: &Path, stamp: Stamp) -> Vec<Arc<[Diagnostic]>> {
        self.findings
            .iter()
            .filter(|((p, _), f)| {
                p == path
                    && f.stamp.incarnation == stamp.incarnation
                    && f.stamp.version == stamp.version
            })
            .map(|(_, f)| f.diagnostics.clone())
            .collect()
    }
    pub fn diagnostics(&self, path: &Path, stamp: Stamp) -> Option<Arc<[Diagnostic]>> {
        let parts = self.parts(path, stamp);
        if parts.len() == 1 {
            return parts.into_iter().next();
        }
        if parts.is_empty() {
            return None;
        }
        let mut all = parts
            .iter()
            .flat_map(|d| d.iter().cloned())
            .collect::<Vec<_>>();
        all.sort_by_key(|d| (d.range.start, d.range.end));
        Some(all.into())
    }
}
