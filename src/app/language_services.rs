//! Transport-independent provider requests and retained document results.
//! Extensions own language clients; this service owns neither processes nor IO.
use super::state::AppState;
use crate::features::providers::{
    Cancellation, CodeActionContext, DocumentResults, PartSink, RequestHandle, RequestSpec,
    ResultStore, Stamp,
};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub(crate) trait ExternalAnswers: Send + Sync {
    fn document_languages(&self, _languages: Vec<(PathBuf, String)>) {}
    fn saved(&self, _saved: crate::domain::document::SaveSnapshot) {}
    fn files_changed(&self, _change: crate::features::providers::FileChange) {}
    fn capabilities(&self, _path: Option<&std::path::Path>) -> Vec<CapabilitySource> {
        Vec::new()
    }
    fn expected(&self, spec: &RequestSpec) -> usize;
    fn ask(&self, spec: RequestSpec, expected: usize, sink: PartSink) -> Option<Cancellation>;
}

#[derive(Clone, Debug)]
pub(crate) struct CapabilitySource {
    pub id: u64,
    pub run: u64,
    pub revision: u64,
    pub providers: crate::features::language_service::providers::Providers,
}

#[derive(Default)]
pub(crate) struct LanguageServices {
    sent: Mutex<Option<u64>>,
    external: Mutex<Option<Arc<dyn ExternalAnswers>>>,
    highlights: Mutex<std::collections::BTreeMap<(PathBuf, u64), HighlightPublication>>,
    results: Mutex<ResultStore>,
    /// What services found in files that are not open, by file and source.
    closed: Mutex<std::collections::BTreeMap<(String, u64), ClosedFindings>>,
}
struct ClosedFindings {
    path: PathBuf,
    items: Arc<[crate::features::problems::Finding]>,
}
struct HighlightPublication {
    stamp: crate::domain::document::DocumentStamp,
    length: usize,
    spans: Arc<[crate::features::syntax::TokenSpan]>,
}

impl LanguageServices {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set_external(&self, source: Arc<dyn ExternalAnswers>) {
        *self.external.lock().unwrap() = Some(source);
        *self.sent.lock().unwrap() = None;
    }

    /// Follow retained findings through edits and invalidate expired lifetimes.
    pub fn sync(&self, app: &AppState) {
        self.results
            .lock()
            .unwrap()
            .findings
            .retain(|(path, _), publication| {
                let Some(document) = app.workspace.documents.values().find(|d| {
                    d.meta.path == *path
                        && d.incarnation == publication.stamp.incarnation
                        && d.meta.language_generation == publication.stamp.language_generation
                }) else {
                    return false;
                };
                if document.buffer.version() == publication.stamp.version {
                    return true;
                }
                let Some(revisions) = document.buffer.revisions_since(
                    publication.stamp.version,
                    Arc::as_ptr(&publication.text) as usize,
                ) else {
                    return false;
                };
                let mut diagnostics = publication.diagnostics.to_vec();
                for revision in revisions {
                    diagnostics.retain_mut(|diagnostic| {
                        if revision.edits.iter().any(|edit| {
                            if edit.old.is_empty() {
                                diagnostic.range.start < edit.old.start
                                    && edit.old.start < diagnostic.range.end
                            } else {
                                edit.old.start < diagnostic.range.end
                                    && edit.old.end > diagnostic.range.start
                            }
                        }) {
                            return false;
                        }
                        diagnostic.range = if diagnostic.range.is_empty() {
                            let at = revision.map(diagnostic.range.start, false);
                            at..at
                        } else {
                            revision.map(diagnostic.range.start, true)
                                ..revision.map(diagnostic.range.end, false)
                        };
                        diagnostic.range.start <= diagnostic.range.end
                    });
                }
                publication.stamp = document.stamp(publication.stamp.document);
                publication.text = document.buffer.text_snapshot();
                publication.diagnostics = diagnostics.into();
                true
            });
        self.highlights.lock().unwrap().retain(|(path, _), p| {
            app.workspace.documents.values().any(|d| {
                d.meta.path == *path
                    && d.incarnation == p.stamp.incarnation
                    && d.meta.language_generation == p.stamp.language_generation
                    && d.buffer.version() == p.stamp.version
            })
        });
        let fingerprint = fingerprint(app);
        {
            let mut sent = self.sent.lock().unwrap();
            if *sent == Some(fingerprint) {
                return;
            }
            *sent = Some(fingerprint);
        }
        if let Some(external) = self.external.lock().unwrap().clone() {
            external.document_languages(
                app.workspace
                    .documents
                    .values()
                    .filter(|d| d.is_text())
                    .map(|d| {
                        (
                            d.meta.path.clone(),
                            d.meta.lang.definition().name.to_string(),
                        )
                    })
                    .collect(),
            );
        }
    }

    pub fn saved_document(&self, saved: crate::domain::document::SaveSnapshot) {
        if let Some(external) = self.external.lock().unwrap().clone() {
            external.saved(saved);
        }
    }
    pub fn files_changed(&self, change: crate::features::providers::FileChange) {
        if let Some(external) = self.external.lock().unwrap().clone() {
            external.files_changed(change);
        }
    }
    pub fn publish_external(
        &self,
        source: u64,
        path: &std::path::Path,
        stamp: crate::domain::document::DocumentStamp,
        text: Arc<String>,
        diagnostics: Arc<[crate::features::language_service::diagnostics::Diagnostic]>,
    ) {
        self.results
            .lock()
            .unwrap()
            .publish(source, path.into(), stamp, text, diagnostics);
    }
    /// Replace what `source` found in a file that is not open; none removes it.
    pub fn publish_findings(
        &self,
        source: u64,
        path: PathBuf,
        items: Vec<crate::features::problems::Finding>,
    ) {
        let key = (crate::features::problems::file_key(&path), source);
        let mut closed = self.closed.lock().unwrap();
        if items.is_empty() {
            closed.remove(&key);
        } else {
            closed.insert(
                key,
                ClosedFindings {
                    path,
                    items: items.into(),
                },
            );
        }
    }

    /// Errors in files that are not open, for the Problems badge.
    pub fn closed_error_count(&self, open: &std::collections::BTreeSet<String>) -> usize {
        use crate::features::language_service::diagnostics::Severity;
        self.closed
            .lock()
            .unwrap()
            .iter()
            .filter(|((key, _), _)| !open.contains(key))
            .map(|(_, file)| {
                file.items
                    .iter()
                    .filter(|f| f.severity == Severity::Error)
                    .count()
            })
            .sum()
    }

    /// Everything `source` found in files that are not open.
    pub fn clear_findings(&self, source: u64) {
        self.closed
            .lock()
            .unwrap()
            .retain(|(_, owner), _| *owner != source);
    }

    /// Problems of files that are not open. A file that is open keeps the
    /// findings of its document, which follow its edits, so `open` leaves it out.
    pub fn closed_problems(
        &self,
        open: &std::collections::BTreeSet<String>,
    ) -> Vec<crate::features::problems::FileProblems> {
        let closed = self.closed.lock().unwrap();
        let mut by_file: std::collections::BTreeMap<&str, (PathBuf, Vec<_>)> = Default::default();
        for ((key, _), file) in closed.iter().filter(|((key, _), _)| !open.contains(key)) {
            let entry = by_file
                .entry(key.as_str())
                .or_insert_with(|| (file.path.clone(), Vec::new()));
            entry.1.extend(file.items.iter().cloned());
        }
        by_file
            .into_values()
            .filter_map(|(path, items)| {
                crate::features::problems::closed_file_problems(path, &items)
            })
            .collect()
    }

    pub fn clear_external(&self, source: u64) {
        self.clear_findings(source);
        self.results.lock().unwrap().clear(source);
        self.highlights
            .lock()
            .unwrap()
            .retain(|(_, id), _| *id != source);
    }
    pub fn publish_highlights(
        &self,
        source: u64,
        path: PathBuf,
        stamp: crate::domain::document::DocumentStamp,
        length: usize,
        spans: Vec<crate::features::syntax::TokenSpan>,
    ) {
        self.highlights.lock().unwrap().insert(
            (path, source),
            HighlightPublication {
                stamp,
                length,
                spans: spans.into(),
            },
        );
    }
    pub fn results(
        &self,
        path: &std::path::Path,
        stamp: &crate::domain::document::DocumentStamp,
    ) -> DocumentResults {
        let mut results = DocumentResults {
            diagnostics: self.results.lock().unwrap().diagnostics(
                path,
                Stamp {
                    incarnation: stamp.incarnation,
                    version: stamp.version,
                },
            ),
            ..Default::default()
        };
        let publications = self.highlights.lock().unwrap();
        let current = publications
            .iter()
            .filter(|((p, _), v)| {
                p == path
                    && v.stamp.incarnation == stamp.incarnation
                    && v.stamp.version == stamp.version
                    && v.stamp.language_generation == stamp.language_generation
            })
            .map(|(_, v)| v)
            .collect::<Vec<_>>();
        if let Some(first) = current.first() {
            let mut combined = vec![crate::features::syntax::TokenSpan {
                range: 0..first.length,
                class: None,
            }];
            for publication in &current {
                combined = crate::features::language_service::semantic::compose(
                    &combined,
                    &publication.spans,
                    0..first.length,
                );
            }
            combined.retain(|span| span.class.is_some());
            results.semantic = Some(combined.into());
        }
        results
    }
    pub fn request(&self, spec: RequestSpec) -> RequestHandle {
        let handle = RequestHandle::standalone();
        if let Some(source) = self.external.lock().unwrap().clone() {
            let expected = source.expected(&spec);
            if expected > 0 {
                let sink = handle.expect_external(
                    expected,
                    spec.timeout
                        .unwrap_or(crate::features::providers::DEFAULT_TIMEOUT),
                );
                if let Some(cancel) = source.ask(spec, expected, sink) {
                    handle.on_cancel(cancel);
                }
            }
        }
        handle
    }
    pub fn capability_sources(&self, path: Option<&std::path::Path>) -> Vec<CapabilitySource> {
        self.external
            .lock()
            .unwrap()
            .clone()
            .map(|source| source.capabilities(path))
            .unwrap_or_default()
    }
    pub fn code_action_context(&self, path: &std::path::Path, stamp: Stamp) -> CodeActionContext {
        CodeActionContext {
            sources: self
                .capability_sources(Some(path))
                .into_iter()
                .filter(|source| source.providers.code_action.is_some())
                .map(|source| (source.id, source.run, source.revision))
                .collect(),
            diagnostics: self.results.lock().unwrap().parts(path, stamp),
        }
    }
}

fn fingerprint(app: &AppState) -> u64 {
    let mut hasher = DefaultHasher::new();
    serde_json::to_string(&app.preferences.values().languages)
        .unwrap_or_default()
        .hash(&mut hasher);
    let mut ids = app
        .workspace
        .documents
        .values()
        .filter(|d| d.is_text())
        .map(|d| {
            (
                &d.meta.path,
                d.incarnation,
                d.buffer.version(),
                d.meta.language_generation,
            )
        })
        .collect::<Vec<_>>();
    ids.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    ids.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
#[path = "language_services_tests.rs"]
mod tests;
