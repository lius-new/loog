//! Applying what a server asks to change: a `WorkspaceEdit` as one operation.
//!
//! Every text edit is checked against its document before any is made, so an
//! edit either happens completely or not at all. The edits of all documents are
//! one undo step each, tagged with a shared transaction; undoing from any of
//! them undoes them together, or says why it cannot.
use super::{AppContext, AppState};
use crate::app::workspace_edits;
use crate::domain::document::DocumentStamp;
use crate::domain::identity::FileId;
use crate::features::language_service::reply::{Change, DocumentEdit, WorkspaceEdit};
use crate::features::language_service::uri;
use crate::model::text::{Edit, EditKind, Selection, TextBuffer};
use lgui::prelude::UiRect;
use std::collections::HashMap;
use std::path::PathBuf;

/// Files larger than this are not opened to be edited by a server.
const MAX_FILE: u64 = 16 * 1024 * 1024;

fn remapped(base: &std::path::Path, suffix: &std::path::Path) -> PathBuf {
    if suffix.as_os_str().is_empty() {
        base.to_path_buf()
    } else {
        base.join(suffix)
    }
}

/// A workspace edit and the state of the open documents it was computed for.
#[derive(Clone)]
pub struct Request {
    pub edit: WorkspaceEdit,
    /// The documents that were open when the question was asked, with their state.
    pub stamps: Vec<(PathBuf, DocumentStamp)>,
    pub rect: UiRect,
    /// What the edit was for, for messages ("Rename", "Quick fix").
    pub title: String,
    /// A server waiting to hear whether the edit was made.
    pub reply: Option<Reply>,
}

#[derive(Clone)]
pub enum Reply {
    Native(std::sync::Arc<dyn Fn(Result<usize, String>) + Send + Sync>),
}

/// The stamps of every open text document now.
pub fn stamps(app: &AppState) -> Vec<(PathBuf, DocumentStamp)> {
    app.workspace
        .documents
        .iter()
        .filter(|(_, d)| d.is_text())
        .map(|(id, d)| (d.meta.path.clone(), d.stamp(*id)))
        .collect()
}

/// One document's edits as bytes of its text.
struct Planned {
    path: PathBuf,
    edits: Vec<Edit>,
    text: std::sync::Arc<String>,
}

#[derive(Default)]
struct Plan {
    steps: Vec<Step>,
}

enum Step {
    Text(Planned),
    Create {
        path: PathBuf,
        overwrite: bool,
        ignore_if_exists: bool,
    },
    Rename {
        from: PathBuf,
        to: PathBuf,
        overwrite: bool,
        ignore_if_exists: bool,
    },
    Delete {
        path: PathBuf,
        recursive: bool,
        ignore_if_not_exists: bool,
    },
}

fn path_of(link: &str) -> Result<PathBuf, String> {
    uri::file_path(link).ok_or_else(|| format!("a file the server named cannot be used: {link}"))
}

/// Which files must be read from disk before the edit can be planned.
pub(crate) fn unopened(app: &AppState, edit: &WorkspaceEdit) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    for (at, change) in edit.changes.iter().enumerate() {
        if let Change::Edit(DocumentEdit { uri, .. }) = change {
            let path = path_of(uri)?;
            let mut candidates = vec![path.clone()];
            let mut created = false;
            for prior in edit.changes[..at].iter().rev() {
                match prior {
                    Change::Rename { from, to, .. } => {
                        let from = path_of(from)?;
                        let to = path_of(to)?;
                        for p in candidates.clone() {
                            if let Ok(suffix) = p.strip_prefix(&to) {
                                let old = remapped(&from, suffix);
                                if !candidates.contains(&old) {
                                    candidates.push(old);
                                }
                            }
                        }
                    }
                    Change::Create { uri, .. } => {
                        let p = path_of(uri)?;
                        created |= candidates.contains(&p);
                    }
                    _ => {}
                }
            }
            let mut available = created;
            for candidate in candidates {
                if app.workspace.file_id_for_path(&candidate).is_some() {
                    available = true;
                    continue;
                }
                if candidate.is_file() {
                    available = true;
                    if !paths.contains(&candidate) {
                        paths.push(candidate);
                    }
                }
            }
            if !available && !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    Ok(paths)
}

pub(super) fn load(paths: Vec<PathBuf>) -> Result<HashMap<PathBuf, String>, String> {
    paths
        .into_iter()
        .map(|path| {
            let text = std::fs::metadata(&path)
                .and_then(|meta| {
                    if meta.len() > MAX_FILE {
                        Err(std::io::Error::other("the file is too large"))
                    } else {
                        crate::infrastructure::document_filesystem::read_diff_file(&path)
                    }
                })
                .map_err(|e| format!("{}: {e}", path.display()))?;
            Ok((path, text))
        })
        .collect()
}

/// Applies `request`, reading files that are not open first (off the UI thread).
pub fn apply(state: &AppContext, request: Request) {
    apply_with(state, request, None);
}

pub fn apply_then(
    state: &AppContext,
    request: Request,
    instance: u64,
    command: crate::features::language_service::reply::Command,
) {
    apply_with(state, request, Some((instance, command)));
}

fn apply_with(
    state: &AppContext,
    request: Request,
    next: Option<(u64, crate::features::language_service::reply::Command)>,
) {
    let app = state.get();
    let missing = match unopened(&app, &request.edit) {
        Ok(missing) => missing,
        Err(error) => {
            complete(state, &request, Err(error), next);
            return;
        }
    };
    if missing.is_empty() {
        let mut result = Ok(0);
        state.update(|app| result = apply_now(app, &request, HashMap::new()));
        complete(state, &request, result, next);
        return;
    }
    let Some(application) = state.services().and_then(|runtime| runtime.application()) else {
        complete(
            state,
            &request,
            Err("the application is no longer running".into()),
            next,
        );
        return;
    };
    let worker_state = state.clone();
    let worker_request = request.clone();
    let spawned = std::thread::Builder::new()
        .name("loom-edit-read".to_owned())
        .spawn(move || {
            let request = worker_request;
            let mut loaded = HashMap::new();
            let mut failure = None;
            for path in missing {
                let read = std::fs::metadata(&path)
                    .map_err(|error| error.to_string())
                    .and_then(|meta| {
                        if meta.len() > MAX_FILE {
                            Err("the file is too large".to_owned())
                        } else {
                            std::fs::read(&path).map_err(|error| error.to_string())
                        }
                    })
                    .and_then(|bytes| {
                        String::from_utf8(bytes).map_err(|_| "not UTF-8 text".to_owned())
                    });
                match read {
                    Ok(text) => {
                        loaded.insert(path, text);
                    }
                    Err(error) => {
                        failure = Some(format!("{}: {error}", path.display()));
                        break;
                    }
                }
            }
            application.post(move || match failure {
                Some(error) => complete(&worker_state, &request, Err(error), next),
                None => {
                    let mut result = Ok(0);
                    worker_state.update(|app| result = apply_now(app, &request, loaded));
                    complete(&worker_state, &request, result, next);
                }
            });
        });
    if spawned.is_err() {
        report(
            state,
            &request,
            Err("Could not start reading the files".into()),
        );
    }
}

fn complete(
    state: &AppContext,
    request: &Request,
    result: Result<usize, String>,
    next: Option<(u64, crate::features::language_service::reply::Command)>,
) {
    let success = result.is_ok();
    report(state, request, result);
    if success && let Some((instance, command)) = next {
        super::refactor::execute(state, instance, command, request.rect);
    }
}

fn report(state: &AppContext, request: &Request, result: Result<usize, String>) {
    super::flush_files(state);
    if let Some(reply) = &request.reply {
        match reply {
            Reply::Native(reply) => reply(result.clone()),
        }
    }
    let title = request.title.as_str();
    state.update(|app| match result {
        Ok(0) => app.show_toast(format!("{title}: nothing to change")),
        Ok(1) => {}
        Ok(files) => app.show_toast(format!("{title}: changed {files} files")),
        Err(error) => app.show_error(format!("{title}: {error}")),
    });
}

/// Plans and performs the edit. Either every text edit is made or none; returns
/// how many files it changed.
pub fn apply_now(
    app: &mut AppState,
    request: &Request,
    loaded: HashMap<PathBuf, String>,
) -> Result<usize, String> {
    let plan = plan(app, request, &loaded)?;
    let before = app.workspace.clone();
    let counter = app.editor.language.next_transaction;
    let transactions = app.editor.language.transactions.clone();
    let events = app.editor.language.file_changes.len();
    let mut journal = crate::infrastructure::edit_journal::Journal::default();
    match apply_steps(app, plan, &mut journal) {
        Ok(changed) => {
            journal.commit();
            Ok(changed)
        }
        Err(error) => {
            app.workspace = before;
            app.editor.language.next_transaction = counter;
            app.editor.language.transactions = transactions;
            app.editor.language.file_changes.truncate(events);
            match journal.rollback() {
                Ok(()) => Err(error),
                Err(rollback) => Err(format!(
                    "{error}; filesystem rollback incomplete: {rollback}; backups retained in .loom-edit-* directories"
                )),
            }
        }
    }
}

fn apply_steps(
    app: &mut AppState,
    plan: Plan,
    journal: &mut crate::infrastructure::edit_journal::Journal,
) -> Result<usize, String> {
    let active = app.workspace.active();
    let transaction = next_transaction(app);
    let mut documents: Vec<FileId> = Vec::new();
    let mut changed = 0;
    for step in plan.steps {
        match step {
            Step::Text(planned) => {
                let id = match app.workspace.file_id_for_path(&planned.path) {
                    Some(id) => id,
                    None => {
                        let text = planned.text.as_ref().clone();
                        app.workspace.open_path(planned.path.clone(), text)
                    }
                };
                workspace_edits::apply_document_edits(
                    app,
                    id,
                    transaction,
                    !documents.contains(&id),
                    planned.edits,
                )?;
                if !documents.contains(&id) {
                    documents.push(id);
                }
                changed += 1;
            }
            Step::Create {
                path,
                overwrite,
                ignore_if_exists,
            } => {
                if journal
                    .create(&path, overwrite, ignore_if_exists)
                    .map_err(|e| e.to_string())?
                {
                    if let Some(id) = app.workspace.file_id_for_path(&path) {
                        app.workspace
                            .documents
                            .get_mut(&id)
                            .unwrap()
                            .buffer
                            .reload(String::new());
                    }
                    changed += 1;
                    app.editor
                        .language
                        .file_changes
                        .push(crate::features::providers::FileChange::Created(path));
                }
            }
            Step::Rename {
                from,
                to,
                overwrite,
                ignore_if_exists,
            } => {
                if journal
                    .rename(&from, &to, overwrite, ignore_if_exists)
                    .map_err(|e| e.to_string())?
                {
                    app.workspace.close_paths_under(&to);
                    app.workspace.move_open_path(&from, to.clone());
                    changed += 1;
                    app.editor
                        .language
                        .file_changes
                        .push(crate::features::providers::FileChange::Renamed { from, to });
                }
            }
            Step::Delete {
                path,
                recursive,
                ignore_if_not_exists,
            } => {
                if journal
                    .delete(&path, recursive, ignore_if_not_exists)
                    .map_err(|e| e.to_string())?
                {
                    app.workspace.close_paths_under(&path);
                    changed += 1;
                    app.editor
                        .language
                        .file_changes
                        .push(crate::features::providers::FileChange::Deleted(path));
                }
            }
        }
    }
    workspace_edits::finish_transaction(app, transaction, documents);
    if let Some(active) = active {
        app.workspace.reveal(active);
    }
    Ok(changed)
}

fn next_transaction(app: &mut AppState) -> u64 {
    workspace_edits::next_transaction(app)
}

fn plan(
    app: &AppState,
    request: &Request,
    loaded: &HashMap<PathBuf, String>,
) -> Result<Plan, String> {
    let mut plan = Plan::default();
    let mut virtual_docs: HashMap<PathBuf, (std::sync::Arc<String>, Option<PathBuf>)> = loaded
        .iter()
        .map(|(p, t)| (p.clone(), (std::sync::Arc::new(t.clone()), Some(p.clone()))))
        .collect();
    for d in app.workspace.documents.values().filter(|d| d.is_text()) {
        virtual_docs.insert(
            d.meta.path.clone(),
            (d.buffer.text_snapshot(), Some(d.meta.path.clone())),
        );
    }
    for change in &request.edit.changes {
        match change {
            Change::Edit(document) => {
                let path = path_of(&document.uri)?;
                let (text, origin) = virtual_docs
                    .get(&path)
                    .cloned()
                    .ok_or("a file was not read or was deleted earlier in this edit")?;
                if let Some(origin) = &origin {
                    check_stamps(app, request, origin)?;
                }
                let ending = origin
                    .as_ref()
                    .and_then(|p| app.workspace.documents.values().find(|d| d.meta.path == *p))
                    .map(|d| d.buffer.line_ending())
                    .unwrap_or_else(|| if text.contains("\r\n") { "\r\n" } else { "\n" });
                let edits: Vec<Edit> = document
                    .edits
                    .iter()
                    .map(|e| {
                        Edit::replace(
                            e.span.bytes(&text),
                            loom_editor_core::text::normalize_endings(&e.new_text, ending),
                        )
                    })
                    .collect();
                let mut preview = TextBuffer::new(text.as_ref().clone());
                let mut selection = Selection::default();
                crate::model::text::EditorMut::new(&mut preview, &mut selection, Vec::new())
                    .edit(EditKind::Other, edits.clone(), |_, _| Selection::default())
                    .map_err(|e| format!("the edits do not fit the file: {e}"))?;
                virtual_docs.insert(path.clone(), (preview.text_snapshot(), origin));
                plan.steps.push(Step::Text(Planned { path, edits, text }));
            }
            Change::Create {
                uri,
                overwrite,
                ignore_if_exists,
            } => {
                let path = path_of(uri)?;
                check_stamps(app, request, &path)?;
                if logical_exists(&path, &plan.steps) && !overwrite {
                    if *ignore_if_exists {
                        continue;
                    } else {
                        return Err(format!("{} already exists", path.display()));
                    }
                }
                virtual_docs.insert(path.clone(), (std::sync::Arc::new(String::new()), None));
                plan.steps.push(Step::Create {
                    path,
                    overwrite: *overwrite,
                    ignore_if_exists: *ignore_if_exists,
                });
            }
            Change::Rename {
                from,
                to,
                overwrite,
                ignore_if_exists,
            } => {
                let from = path_of(from)?;
                let to = path_of(to)?;
                check_stamps(app, request, &from)?;
                check_stamps(app, request, &to)?;
                if from == to {
                    continue;
                }
                if logical_exists(&to, &plan.steps) && !overwrite {
                    if *ignore_if_exists {
                        continue;
                    } else {
                        return Err(format!("{} already exists", to.display()));
                    }
                }
                virtual_docs.retain(|p, _| !p.starts_with(&to));
                let moved: Vec<_> = virtual_docs
                    .keys()
                    .filter(|p| p.starts_with(&from))
                    .cloned()
                    .collect();
                for p in moved {
                    let value = virtual_docs.remove(&p).unwrap();
                    virtual_docs.insert(remapped(&to, p.strip_prefix(&from).unwrap()), value);
                }
                plan.steps.push(Step::Rename {
                    from,
                    to,
                    overwrite: *overwrite,
                    ignore_if_exists: *ignore_if_exists,
                });
            }
            Change::Delete {
                uri,
                recursive,
                ignore_if_not_exists,
            } => {
                let path = path_of(uri)?;
                check_stamps(app, request, &path)?;
                virtual_docs.retain(|p, _| !p.starts_with(&path));
                plan.steps.push(Step::Delete {
                    path,
                    recursive: *recursive,
                    ignore_if_not_exists: *ignore_if_not_exists,
                });
            }
        }
    }
    Ok(plan)
}

fn check_stamps(app: &AppState, request: &Request, path: &std::path::Path) -> Result<(), String> {
    for (p, before) in &request.stamps {
        if p.starts_with(path)
            && let Some(id) = app.workspace.file_id_for_path(p)
            && app
                .workspace
                .documents
                .get(&id)
                .is_none_or(|d| d.stamp(id) != *before)
        {
            return Err(format!(
                "{} changed while the language server was working",
                p.display()
            ));
        }
    }
    Ok(())
}
fn logical_exists(path: &std::path::Path, steps: &[Step]) -> bool {
    let mut path = path.to_path_buf();
    for step in steps.iter().rev() {
        match step {
            Step::Create { path: p, .. } if *p == path => return true,
            Step::Delete { path: p, .. } if path.starts_with(p) => return false,
            Step::Rename { from, to, .. } => {
                if let Ok(suffix) = path.strip_prefix(to) {
                    path = remapped(from, suffix);
                } else if path.starts_with(from) {
                    return false;
                }
            }
            _ => {}
        }
    }
    path.exists()
}

/// Undoes or redoes the change spanning several documents that the active one
/// is part of. `None` when the active document's next step is not one.
pub fn undo_redo(app: &mut AppState, undo: bool) -> Option<Result<(), String>> {
    app.workspace.active_editor()?;
    let top = |app: &AppState, id: FileId| {
        app.workspace.documents.get(&id).and_then(|d| {
            if undo {
                d.buffer.undo_transaction()
            } else {
                d.buffer.redo_transaction()
            }
        })
    };
    let active = app.workspace.active_document()?;
    let transaction = top(app, active)?;
    let documents = app.editor.language.transactions.get(&transaction)?.clone();
    for id in &documents {
        if top(app, *id) != Some(transaction) {
            return Some(Err(
                "that change spans several files and some were edited since".to_owned(),
            ));
        }
    }
    for id in documents {
        let mut editor = app.workspace.document_editor_mut(id)?;
        if undo {
            editor.undo();
        } else {
            editor.redo();
        }
    }
    Some(Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::language_service::capabilities::PositionEncoding;
    use crate::features::language_service::reply::{TextEdit, workspace_edit};
    use serde_json::json;

    fn uri_of(path: &std::path::Path) -> String {
        uri::file_uri(path).unwrap()
    }

    fn app_with(files: &[(&str, &str)]) -> (AppState, Vec<PathBuf>) {
        let mut app = AppState::new();
        let mut paths = Vec::new();
        for (name, text) in files {
            let path =
                std::env::temp_dir().join(format!("loom-edit-{}-{name}", std::process::id()));
            app.workspace.open_path(path.clone(), (*text).into());
            paths.push(path);
        }
        (app, paths)
    }

    type Replace<'a> = (&'a std::path::Path, (u32, u32), (u32, u32), &'a str);

    fn edit_json(entries: &[Replace]) -> WorkspaceEdit {
        let changes: Vec<_> = entries
            .iter()
            .map(|(path, from, to, text)| {
                json!({
                    "textDocument": { "uri": uri_of(path), "version": 1 },
                    "edits": [{ "range": {
                        "start": { "line": from.0, "character": from.1 },
                        "end": { "line": to.0, "character": to.1 } }, "newText": text }]
                })
            })
            .collect();
        workspace_edit(
            &json!({ "documentChanges": changes }),
            PositionEncoding::Utf16,
        )
        .unwrap()
    }

    fn request(app: &AppState, edit: WorkspaceEdit) -> Request {
        Request {
            edit,
            stamps: stamps(app),
            rect: UiRect::new(0.0, 0.0, 400.0, 300.0),
            title: "Test".into(),
            reply: None,
        }
    }

    fn text_of(app: &AppState, path: &std::path::Path) -> String {
        let id = app.workspace.file_id_for_path(path).unwrap();
        app.workspace.documents[&id].buffer.text().to_owned()
    }

    #[test]
    fn edits_in_several_open_files_apply_together_and_undo_together() {
        let (mut app, paths) = app_with(&[("a.rs", "one two"), ("b.rs", "three four")]);
        let edit = edit_json(&[
            (&paths[0], (0, 0), (0, 3), "ONE"),
            (&paths[1], (0, 6), (0, 10), "FOUR"),
        ]);
        let request = request(&app, edit);
        assert_eq!(apply_now(&mut app, &request, HashMap::new()), Ok(2));
        assert_eq!(text_of(&app, &paths[0]), "ONE two");
        assert_eq!(text_of(&app, &paths[1]), "three FOUR");
        // Undo from the active file takes the other file back too.
        assert_eq!(undo_redo(&mut app, true), Some(Ok(())));
        assert_eq!(text_of(&app, &paths[0]), "one two");
        assert_eq!(text_of(&app, &paths[1]), "three four");
        assert_eq!(undo_redo(&mut app, false), Some(Ok(())));
        assert_eq!(text_of(&app, &paths[0]), "ONE two");
        assert_eq!(text_of(&app, &paths[1]), "three FOUR");
    }

    #[test]
    fn one_bad_edit_changes_nothing_anywhere() {
        let (mut app, paths) = app_with(&[("c.rs", "alpha"), ("d.rs", "beta")]);
        // The second edit overlaps another edit in the same file.
        let mut edit = edit_json(&[(&paths[0], (0, 0), (0, 2), "xx")]);
        let bad = workspace_edit(
            &json!({ "documentChanges": [{
                "textDocument": { "uri": uri_of(&paths[1]), "version": 1 },
                "edits": [
                    { "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 3 } }, "newText": "a" },
                    { "range": { "start": { "line": 0, "character": 2 }, "end": { "line": 0, "character": 4 } }, "newText": "b" }
                ] }] }),
            PositionEncoding::Utf16,
        )
        .unwrap();
        edit.changes.extend(bad.changes);
        let request = request(&app, edit);
        let result = apply_now(&mut app, &request, HashMap::new());
        assert!(result.unwrap_err().contains("do not fit"));
        assert_eq!(text_of(&app, &paths[0]), "alpha");
        assert_eq!(text_of(&app, &paths[1]), "beta");
    }

    #[test]
    fn an_edit_for_an_older_state_of_a_file_is_refused() {
        let (mut app, paths) = app_with(&[("e.rs", "text")]);
        let edit = edit_json(&[(&paths[0], (0, 0), (0, 1), "T")]);
        let request = request(&app, edit);
        // The person typed after the question was asked.
        app.workspace.active_editor_mut().unwrap().set_cursor(4);
        {
            let mut editor = app.workspace.active_editor_mut().unwrap();
            loom_editor_core::normal::insert(&mut editor, "!");
        }
        let result = apply_now(&mut app, &request, HashMap::new());
        assert!(result.unwrap_err().contains("changed while"));
        assert_eq!(text_of(&app, &paths[0]), "text!");
    }

    #[test]
    fn a_file_that_is_not_open_is_opened_with_the_edit_unsaved() {
        let (mut app, _) = app_with(&[("open.rs", "x")]);
        let closed =
            std::env::temp_dir().join(format!("loom-edit-{}-closed.rs", std::process::id()));
        let edit = edit_json(&[(&closed, (0, 0), (0, 0), "// ")]);
        let request = request(&app, edit);
        let loaded = HashMap::from([(closed.clone(), "code".to_owned())]);
        assert_eq!(apply_now(&mut app, &request, loaded), Ok(1));
        assert_eq!(text_of(&app, &closed), "// code");
        let id = app.workspace.file_id_for_path(&closed).unwrap();
        assert!(app.workspace.is_dirty(id));
        // The file that was active before is active again.
        assert_eq!(
            app.workspace
                .active_path()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy(),
            format!("loom-edit-{}-open.rs", std::process::id())
        );
    }

    #[test]
    fn undo_is_refused_when_one_of_the_files_was_edited_since() {
        let (mut app, paths) = app_with(&[("f.rs", "aa"), ("g.rs", "bb")]);
        let edit = edit_json(&[
            (&paths[0], (0, 0), (0, 1), "X"),
            (&paths[1], (0, 0), (0, 1), "Y"),
        ]);
        let request = request(&app, edit);
        apply_now(&mut app, &request, HashMap::new()).unwrap();
        // Edit the first file after the transaction.
        let first = app.workspace.file_id_for_path(&paths[0]).unwrap();
        app.workspace.activate_document(first);
        {
            let mut editor = app.workspace.active_editor_mut().unwrap();
            editor.set_cursor(2);
            loom_editor_core::normal::insert(&mut editor, "!");
        }
        // The active document's next undo is its own typing, not the transaction.
        assert_eq!(undo_redo(&mut app, true), None);
        // From the other file the transaction is blocked.
        let second = app.workspace.file_id_for_path(&paths[1]).unwrap();
        app.workspace.activate_document(second);
        let refused = undo_redo(&mut app, true).unwrap();
        assert!(refused.unwrap_err().contains("edited since"));
        assert_eq!(text_of(&app, &paths[1]), "Yb");
    }

    #[test]
    fn text_edit_spans_follow_the_servers_encoding() {
        let (mut app, paths) = app_with(&[("h.rs", "a😀b")]);
        // UTF-16 columns: the emoji is two units wide, so 'b' is at 3.
        let edit = edit_json(&[(&paths[0], (0, 3), (0, 4), "B")]);
        let request = request(&app, edit);
        apply_now(&mut app, &request, HashMap::new()).unwrap();
        assert_eq!(text_of(&app, &paths[0]), "a😀B");
        let _ = TextEdit {
            span: crate::features::language_service::reply::Span::of(
                "x",
                0..1,
                PositionEncoding::Utf8,
            ),
            new_text: String::new(),
        };
    }

    #[test]
    fn a_late_resource_failure_restores_disk_documents_selections_and_undo() {
        let root = std::env::temp_dir().join(format!("loom-edit-rollback-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let from = root.join("from.rs");
        let to = root.join("to.rs");
        let removed = root.join("removed.rs");
        let created = root.join("new/created.rs");
        std::fs::write(&from, "original").unwrap();
        std::fs::write(&to, "destination").unwrap();
        std::fs::write(&removed, "keep").unwrap();
        let mut app = AppState::new();
        let id = app
            .workspace
            .open_path(from.clone(), "unsaved original".into());
        app.workspace.active_editor_mut().unwrap().set_cursor(3);
        let stamp = app.workspace.documents[&id].stamp(id);
        let mut changes = edit_json(&[(&from, (0, 0), (0, 7), "edited")]).changes;
        changes.extend([
            Change::Rename {
                from: uri_of(&from),
                to: uri_of(&to),
                overwrite: true,
                ignore_if_exists: false,
            },
            Change::Create {
                uri: uri_of(&created),
                overwrite: false,
                ignore_if_exists: false,
            },
            Change::Delete {
                uri: uri_of(&removed),
                recursive: false,
                ignore_if_not_exists: false,
            },
            Change::Delete {
                uri: uri_of(&root.join("missing")),
                recursive: false,
                ignore_if_not_exists: false,
            },
        ]);
        let req = request(&app, WorkspaceEdit { changes });
        assert!(apply_now(&mut app, &req, HashMap::new()).is_err());
        assert_eq!(std::fs::read_to_string(&from).unwrap(), "original");
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "destination");
        assert_eq!(std::fs::read_to_string(&removed).unwrap(), "keep");
        assert!(!created.parent().unwrap().exists());
        assert_eq!(app.workspace.documents[&id].stamp(id), stamp);
        assert_eq!(
            app.workspace.active_editor().unwrap().text(),
            "unsaved original"
        );
        assert_eq!(app.workspace.active_editor().unwrap().cursor(), 3);
        assert!(app.workspace.active_editor_mut().unwrap().undo().is_none());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 3);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ordered_create_edit_rename_edit_uses_one_text_undo_group() {
        let root = std::env::temp_dir().join(format!("loom-edit-ordered-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let a = root.join("a.rs");
        let b = root.join("b.rs");
        let mut app = AppState::new();
        let mut changes = vec![Change::Create {
            uri: uri_of(&a),
            overwrite: false,
            ignore_if_exists: false,
        }];
        changes.extend(edit_json(&[(&a, (0, 0), (0, 0), "ab")]).changes);
        changes.push(Change::Rename {
            from: uri_of(&a),
            to: uri_of(&b),
            overwrite: false,
            ignore_if_exists: false,
        });
        changes.extend(edit_json(&[(&b, (0, 1), (0, 2), "CD")]).changes);
        let request = request(&app, WorkspaceEdit { changes });
        assert!(unopened(&app, &request.edit).unwrap().is_empty());
        assert_eq!(apply_now(&mut app, &request, HashMap::new()), Ok(4));
        assert_eq!(text_of(&app, &b), "aCD");
        assert!(!a.exists());
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "");
        app.workspace.active_editor_mut().unwrap().undo();
        assert_eq!(text_of(&app, &b), "");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_unopened_rename_reads_the_original_and_preserves_unsaved_text_edits() {
        let root = std::env::temp_dir().join(format!("loom-edit-alias-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let a = root.join("a.rs");
        let b = root.join("b.rs");
        std::fs::write(&a, "abc").unwrap();
        let mut app = AppState::new();
        let mut changes = vec![Change::Rename {
            from: uri_of(&a),
            to: uri_of(&b),
            overwrite: false,
            ignore_if_exists: false,
        }];
        changes.extend(edit_json(&[(&b, (0, 1), (0, 2), "B")]).changes);
        let request = request(&app, WorkspaceEdit { changes });
        assert_eq!(unopened(&app, &request.edit).unwrap(), vec![a.clone()]);
        assert_eq!(
            apply_now(
                &mut app,
                &request,
                HashMap::from([(a.clone(), "abc".into())])
            ),
            Ok(2)
        );
        assert_eq!(text_of(&app, &b), "aBc");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "abc");
        assert!(!a.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
