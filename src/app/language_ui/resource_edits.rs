//! Bounded reads followed by one buffer transaction. No provider disk mutation
//! happens here; ordinary saves retain their individual revision receipts.
use super::{
    AppContext, AppState,
    edit::{self, Request},
};
use crate::{
    app::workspace_edits,
    domain::{document::DocumentStamp, workspace::PreparedTextFile},
    features::language_service::{
        position::{LineIndex, Position, PositionEncoding},
        reply::Change,
    },
};
use loog_workspace_api::{
    ResourceRef,
    session::{ColumnEncoding, ResourceDocumentEdit},
};
use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

const INPUT: usize = 8 * 1024 * 1024;
// A process-wide admission limit also bounds detached reads in multiple windows.
static READS: AtomicUsize = AtomicUsize::new(0);
struct Permit;
impl Permit {
    fn take() -> Option<Self> {
        READS
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n < 4).then_some(n + 1)
            })
            .ok()
            .map(|_| Self)
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        READS.fetch_sub(1, Ordering::Relaxed);
    }
}

type Guard = Vec<(ResourceRef, Option<DocumentStamp>)>;
fn documents(request: &Request) -> Result<Vec<&ResourceDocumentEdit>, String> {
    if request.edit.changes.len() > 16 {
        return Err("Too many edited resources".into());
    }
    let mut found = vec![];
    let mut seen = HashSet::new();
    let mut environment = None;
    let mut count = 0usize;
    let mut bytes = 0usize;
    for change in &request.edit.changes {
        let Change::ResourceText(document) = change else {
            return Err("Native and provider edits cannot share a resource transaction".into());
        };
        let current = document
            .resource
            .environment()
            .ok_or("Expected a provider resource")?;
        if environment.is_some_and(|environment| environment != current) {
            return Err("Cross-environment edits are not supported".into());
        }
        environment = Some(current);
        if !seen.insert(&document.resource) {
            return Err("A resource is listed twice".into());
        }
        count = count.saturating_add(document.edits.len());
        bytes = bytes.saturating_add(
            document
                .edits
                .iter()
                .map(|e| e.new_text.len())
                .sum::<usize>(),
        );
        if count > 1024 || bytes > 512 * 1024 {
            return Err("Resource edit budget exceeded".into());
        }
        if document.edits.iter().any(|e| e.range.start > e.range.end) {
            return Err("Reversed edit range".into());
        }
        if !document.edits.is_empty() {
            found.push(document);
        }
    }
    Ok(found)
}
fn guards(request: &Request, documents: &[&ResourceDocumentEdit]) -> Guard {
    documents
        .iter()
        .map(|doc| {
            (
                doc.resource.clone(),
                request
                    .stamps
                    .iter()
                    .find(|(resource, _)| resource == &doc.resource)
                    .map(|(_, stamp)| *stamp),
            )
        })
        .collect()
}
fn current(app: &AppState, guards: &Guard) -> bool {
    guards.iter().all(|(resource, expected)| {
        let now = app.workspace.file_id_for_resource(resource).and_then(|id| {
            app.workspace
                .documents
                .get(&id)
                .filter(|doc| doc.is_text())
                .map(|doc| doc.stamp(id))
        });
        match expected {
            Some(expected) => now == Some(*expected),
            None => app.workspace.file_id_for_resource(resource).is_none(),
        }
    })
}

struct Loading {
    state: AppContext,
    request: Option<Request>,
    guards: Guard,
    missing: Vec<ResourceRef>,
    prepared: HashMap<ResourceRef, PreparedTextFile>,
    bytes: usize,
    deadline: Instant,
    failure: Option<String>,
    valid: std::sync::Arc<dyn Fn(&AppState) -> bool + Send + Sync>,
    next: Option<(u64, crate::features::language_service::reply::Command)>,
    origin: Option<(ResourceRef, DocumentStamp)>,
    _permit: Permit,
}
impl Loading {
    fn fail(&mut self, reason: impl Into<String>) {
        self.failure = Some(reason.into());
    }
}
impl Drop for Loading {
    fn drop(&mut self) {
        // Dropping a consumer on invalidation must still complete the caller's
        // receipt. Post after the current state update, never re-enter it.
        let Some(request) = self.request.take() else {
            return;
        };
        let state = self.state.clone();
        let reason = self.failure.take().unwrap_or_else(|| {
            "Resource edit cancelled: a document, scope or deadline changed".into()
        });
        if let Some(application) = state.services().and_then(|r| r.application()) {
            application.post(move || edit::complete(&state, &request, Err(reason), None));
        }
    }
}

pub(super) fn apply(
    state: &AppContext,
    request: Request,
    next: Option<(u64, crate::features::language_service::reply::Command)>,
) {
    apply_with(state, request, std::sync::Arc::new(|_| true), next);
}
pub(crate) fn apply_guarded(
    state: &AppContext,
    request: Request,
    valid: std::sync::Arc<dyn Fn(&AppState) -> bool + Send + Sync>,
) {
    apply_with(state, request, valid, None);
}
fn apply_with(
    state: &AppContext,
    request: Request,
    valid: std::sync::Arc<dyn Fn(&AppState) -> bool + Send + Sync>,
    next: Option<(u64, crate::features::language_service::reply::Command)>,
) {
    let parsed = match documents(&request) {
        Ok(value) => value,
        Err(reason) => {
            edit::complete(state, &request, Err(reason), None);
            return;
        }
    };
    let guard = guards(&request, &parsed);
    let app = state.get();
    let origin = app.workspace.active_document().and_then(|id| {
        app.workspace
            .documents
            .get(&id)
            .map(|doc| (doc.meta.resource.clone(), doc.stamp(id)))
    });
    if !current(&app, &guard) || !valid(&app) {
        edit::complete(
            state,
            &request,
            Err("An edited resource changed or was closed".into()),
            None,
        );
        return;
    }
    let mut bytes = 0;
    let mut missing = vec![];
    for document in parsed {
        if let Some(id) = app.workspace.file_id_for_resource(&document.resource) {
            bytes += app.workspace.documents[&id].buffer.len();
        } else {
            missing.push(document.resource.clone());
        }
    }
    if bytes > INPUT {
        edit::complete(
            state,
            &request,
            Err("Resource edit input budget exceeded".into()),
            None,
        );
        return;
    }
    let Some(permit) = Permit::take() else {
        edit::complete(
            state,
            &request,
            Err("Too many resource edits are awaiting reads".into()),
            None,
        );
        return;
    };
    advance(Loading {
        state: state.clone(),
        request: Some(request),
        guards: guard,
        missing,
        prepared: HashMap::new(),
        bytes,
        deadline: Instant::now() + Duration::from_secs(30),
        failure: None,
        valid,
        next,
        origin,
        _permit: permit,
    });
}
fn advance(mut loading: Loading) {
    if Instant::now() >= loading.deadline
        || !current(&loading.state.get(), &loading.guards)
        || !(loading.valid)(&loading.state.get())
    {
        return;
    }
    let Some(resource) = loading.missing.pop() else {
        let request = loading.request.take().unwrap();
        let state = loading.state.clone();
        let loaded = std::mem::take(&mut loading.prepared);
        let mut result = Ok(0);
        state.update(|app| {
            result = if (loading.valid)(app) {
                commit(app, &request, loaded)
            } else {
                Err("Resource edit origin or authorization changed".into())
            }
        });
        let command_requested = loading.next.is_some();
        let next = loading.next.take().filter(|_| {
            let app = state.get();
            app.workspace
                .active_document()
                .and_then(|id| app.workspace.documents.get(&id).map(|doc| (id, doc)))
                .is_some_and(|(id, doc)| {
                    loading.origin.as_ref().is_some_and(|(resource, stamp)| {
                        let current = doc.stamp(id);
                        &doc.meta.resource == resource
                            && current.incarnation == stamp.incarnation
                            && current.language_generation == stamp.language_generation
                    })
                })
        });
        let skipped = next.is_none() && command_requested && result.is_ok();
        edit::complete(&state, &request, result, next);
        if skipped {
            state.update(|app| app.show_toast("The edit was applied; its command was cancelled because the source view changed"));
        }
        return;
    };
    let limit = INPUT.saturating_sub(loading.bytes);
    if limit == 0 {
        loading.fail("Resource edit input budget exceeded");
        return;
    }
    let state = loading.state.clone();
    let guard = loading.guards.clone();
    let deadline = loading.deadline;
    let valid = loading.valid.clone();
    crate::app::resources::read_text(
        &state,
        resource.clone(),
        limit,
        crate::app::resources::TextConsumer {
            valid: Box::new(move |app| {
                Instant::now() < deadline && current(app, &guard) && valid(app)
            }),
            ready: Box::new(move |_, result| match result {
                Ok(prepared) => {
                    loading.bytes += prepared.text_snapshot().len();
                    if loading.bytes > INPUT {
                        loading.fail("Resource edit input budget exceeded");
                        return;
                    }
                    loading.prepared.insert(resource, prepared);
                    if let Some(application) =
                        loading.state.services().and_then(|r| r.application())
                    {
                        application.post(move || advance(loading));
                    }
                }
                Err(reason) => loading.fail(reason),
            }),
        },
    );
}

pub(crate) fn commit(
    app: &mut AppState,
    request: &Request,
    mut loaded: HashMap<ResourceRef, PreparedTextFile>,
) -> Result<usize, String> {
    let documents = documents(request)?;
    if !current(app, &guards(request, &documents)) {
        return Err("An edited resource changed or was closed".into());
    }
    let before = app.workspace.clone();
    let active = app.workspace.active();
    let result = (|| {
        let mut changes = vec![];
        for document in documents {
            let id = match app.workspace.file_id_for_resource(&document.resource) {
                Some(id) => id,
                None => app.workspace.open_prepared_text(
                    loaded
                        .remove(&document.resource)
                        .ok_or("An edited resource was not read")?,
                    crate::domain::workspace::OpenMode::Permanent,
                ),
            };
            let buffer = &app.workspace.documents[&id];
            if buffer.meta.resource != document.resource || !buffer.is_text() {
                return Err("The read returned a different resource".into());
            }
            let index = LineIndex::new(buffer.buffer.text());
            let mut edits = vec![];
            for edit in &document.edits {
                let unit = match edit.range.encoding {
                    ColumnEncoding::Utf8 => PositionEncoding::Utf8,
                    ColumnEncoding::Utf16 => PositionEncoding::Utf16,
                };
                let start = Position {
                    line: edit.range.start.line,
                    character: edit.range.start.character,
                };
                let end = Position {
                    line: edit.range.end.line,
                    character: edit.range.end.character,
                };
                let bytes = index.offset(start, unit)..index.offset(end, unit);
                if bytes.start > bytes.end
                    || index.position(bytes.start, unit) != start
                    || index.position(bytes.end, unit) != end
                {
                    return Err("An edit position is outside its resource".into());
                }
                edits.push((bytes, edit.new_text.clone()));
            }
            changes.push(workspace_edits::DocumentChange {
                file: id,
                stamp: buffer.stamp(id),
                edits,
            });
        }
        if changes.is_empty() {
            return Ok(0);
        }
        let count = changes.len();
        workspace_edits::commit(
            app,
            &workspace_edits::TextTransaction {
                title: request.title.clone(),
                documents: changes,
            },
            &loog_extension_api::admission::EditLimits::default(),
        )
        .map_err(|e| e.message)?;
        Ok(count)
    })();
    if result.is_err() {
        app.workspace = before;
    } else if let Some(active) = active {
        app.workspace.reveal(active);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{document::DiskState, workspace::OpenMode};
    use loog_workspace_api::{
        EnvironmentKey,
        session::{ResourcePosition, ResourceRange, ResourceTextEdit},
    };

    fn resource(environment: &str, path: &str) -> ResourceRef {
        ResourceRef::provided(
            EnvironmentKey::new("test.provider", environment).unwrap(),
            path,
        )
        .unwrap()
    }
    fn prepared(resource: &ResourceRef, text: &str) -> PreparedTextFile {
        PreparedTextFile::with_resource(
            resource.clone(),
            "file.rs".into(),
            text.into(),
            DiskState::from_contents(text),
        )
    }
    fn document(resource: &ResourceRef) -> ResourceDocumentEdit {
        ResourceDocumentEdit {
            resource: resource.clone(),
            version: None,
            edits: vec![ResourceTextEdit {
                range: ResourceRange {
                    start: ResourcePosition {
                        line: 0,
                        character: 3,
                    },
                    end: ResourcePosition {
                        line: 0,
                        character: 4,
                    },
                    encoding: ColumnEncoding::Utf16,
                },
                new_text: "值\n新".into(),
            }],
        }
    }
    fn request(app: &AppState, documents: Vec<ResourceDocumentEdit>) -> Request {
        Request {
            edit: crate::features::language_service::reply::WorkspaceEdit {
                changes: documents.into_iter().map(Change::ResourceText).collect(),
            },
            stamps: edit::stamps(app),
            title: "Rename".into(),
            reply: None,
            rect: lgui::prelude::UiRect::new(0.0, 0.0, 400.0, 300.0),
        }
    }
    #[test]
    fn unopened_resource_joins_atomic_unicode_edit_and_group_undo_without_touching_other_environment()
     {
        let mut app = AppState::new();
        let a = resource("one", "/a.rs");
        let b = resource("one", "/b.rs");
        let other = resource("two", "/a.rs");
        let original = "中😀x\r\n";
        let other_id = app
            .workspace
            .open_prepared_text(prepared(&other, original), OpenMode::Permanent);
        let a_id = app
            .workspace
            .open_prepared_text(prepared(&a, original), OpenMode::Permanent);
        let active = app.workspace.active();
        let request = request(&app, vec![document(&a), document(&b)]);
        assert_eq!(
            commit(
                &mut app,
                &request,
                HashMap::from([(b.clone(), prepared(&b, original))])
            ),
            Ok(2)
        );
        let b_id = app.workspace.file_id_for_resource(&b).unwrap();
        assert_eq!(app.workspace.active(), active);
        for id in [a_id, b_id] {
            assert_eq!(
                app.workspace.documents[&id].buffer.text(),
                "中😀值\r\n新\r\n"
            );
            assert!(app.workspace.is_dirty(id));
        }
        assert_eq!(app.workspace.documents[&other_id].buffer.text(), original);
        assert_eq!(edit::undo_redo(&mut app, true), Some(Ok(())));
        for id in [a_id, b_id] {
            assert_eq!(app.workspace.documents[&id].buffer.text(), original);
        }
        assert_eq!(edit::undo_redo(&mut app, false), Some(Ok(())));
        assert!(commit(&mut app, &request, HashMap::new()).is_err());
    }
    #[test]
    fn bad_second_resource_rolls_back_tabs_edits_and_undo_for_every_target() {
        let mut app = AppState::new();
        let a = resource("one", "/a.rs");
        let b = resource("one", "/b.rs");
        let a_id = app
            .workspace
            .open_prepared_text(prepared(&a, "中😀x"), OpenMode::Permanent);
        let active = app.workspace.active();
        let before = app.workspace.documents[&a_id].stamp(a_id);
        let mut bad = document(&b);
        bad.edits[0].range.start.character = 2; // Inside the UTF-16 surrogate pair.
        let req = request(&app, vec![document(&a), bad]);
        assert!(
            commit(
                &mut app,
                &req,
                HashMap::from([(b.clone(), prepared(&b, "中😀x"))])
            )
            .is_err()
        );
        assert_eq!(app.workspace.active(), active);
        assert_eq!(app.workspace.documents.len(), 1);
        assert_eq!(app.workspace.documents[&a_id].stamp(a_id), before);
        assert!(app.workspace.active_editor_mut().unwrap().undo().is_none());
        let req = request(&app, vec![document(&a), document(&b)]);
        let other = resource("two", "/b.rs");
        assert!(
            commit(
                &mut app,
                &req,
                HashMap::from([(b, prepared(&other, "中😀x"))])
            )
            .is_err()
        );
        assert_eq!(app.workspace.documents.len(), 1);
        assert_eq!(app.workspace.documents[&a_id].stamp(a_id), before);
    }
    #[test]
    fn edited_opened_duplicate_and_cross_environment_targets_are_rejected() {
        let mut app = AppState::new();
        let a = resource("one", "/a.rs");
        let b = resource("one", "/b.rs");
        app.workspace
            .open_prepared_text(prepared(&a, "中😀x"), OpenMode::Permanent);
        let req = request(&app, vec![document(&a), document(&b)]);
        app.workspace
            .open_prepared_text(prepared(&b, "中😀x"), OpenMode::Permanent);
        assert!(commit(&mut app, &req, HashMap::new()).is_err());
        let req = request(&app, vec![document(&a), document(&b)]);
        loog_editor_core::normal::insert(&mut app.workspace.active_editor_mut().unwrap(), "!");
        assert!(commit(&mut app, &req, HashMap::new()).is_err());
        let other = resource("two", "/a.rs");
        assert!(documents(&request(&app, vec![document(&a), document(&other)])).is_err());
        assert!(documents(&request(&app, vec![document(&a), document(&a)])).is_err());
    }
}
