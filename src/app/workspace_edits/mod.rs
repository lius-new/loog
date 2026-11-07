//! Protocol-neutral text edits on open documents (design §6.2, §6.2.1).
//!
//! The LSP `WorkspaceEdit` path and extensions both commit through the
//! helpers here: one undo transaction across the affected documents, shared
//! transaction numbers, and all-or-nothing publication. Callers translate
//! their own input (LSP positions, extension byte ranges) and their results.
use super::state::AppState;
use crate::domain::document::DocumentStamp;
use crate::domain::identity::FileId;
use crate::model::text::{Assoc, Edit, EditKind, Selection};
use loog_extension_api::admission::{self, DocumentLoad, EditLimits};
use loog_extension_api::edit::{ByteRange, TextEdit};
use loog_extension_api::{ErrorKind, ExtensionError};
use std::ops::Range;

pub(super) fn next_transaction(app: &mut AppState) -> u64 {
    app.editor.language.next_transaction += 1;
    app.editor.language.next_transaction
}

/// Make `edits` in one document as part of `transaction`. `first` is true the
/// first time this transaction touches the document.
pub(super) fn apply_document_edits(
    app: &mut AppState,
    id: FileId,
    transaction: u64,
    first: bool,
    edits: Vec<Edit>,
) -> Result<(), String> {
    let Some(mut editor) = app.workspace.document_editor_mut(id) else {
        return Err("a changed file cannot be edited".to_owned());
    };
    let cursor = editor.cursor();
    if first {
        editor.begin_transaction(transaction);
    }
    editor
        .edit(EditKind::Other, edits, |changes, _| {
            Selection::caret(changes.map(cursor, Assoc::After))
        })
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Close the transaction in every document; a change spanning several
/// documents is registered so undo and redo can treat it as one.
pub(super) fn finish_transaction(app: &mut AppState, transaction: u64, documents: Vec<FileId>) {
    for id in &documents {
        if let Some(mut editor) = app.workspace.document_editor_mut(*id) {
            editor.end_transaction();
        }
    }
    if documents.len() > 1 {
        app.editor
            .language
            .transactions
            .insert(transaction, documents);
    }
}

/// One document's changes, as byte ranges of the text its stamp describes.
#[derive(Clone, Debug)]
pub struct DocumentChange {
    pub file: FileId,
    pub stamp: DocumentStamp,
    pub edits: Vec<(Range<usize>, String)>,
}

#[derive(Clone, Debug)]
pub struct TextTransaction {
    pub title: String,
    pub documents: Vec<DocumentChange>,
}

/// What a committed transaction reports back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub transaction: u64,
    pub versions: Vec<(FileId, u64)>,
}

fn load_of(app: &AppState, change: &DocumentChange) -> Option<DocumentLoad> {
    let document = app.workspace.documents.get(&change.file)?;
    let input = document.buffer.text_snapshot().len();
    let insert: usize = change.edits.iter().map(|(_, text)| text.len()).sum();
    let removed: usize = change.edits.iter().map(|(range, _)| range.len()).sum();
    let views = app
        .workspace
        .views
        .values()
        .filter(|view| view.document() == Some(change.file))
        .count();
    Some(DocumentLoad {
        input_bytes: input,
        edits: change.edits.len(),
        insert_bytes: insert,
        output_bytes: (input + insert).saturating_sub(removed),
        views,
        anchors: views,
    })
}

/// Commit `transaction` atomically: stamps, admission limits and edit
/// validity are all checked before the first change is made, and a failure
/// afterwards restores the previous state.
pub fn commit(
    app: &mut AppState,
    transaction: &TextTransaction,
    limits: &EditLimits,
) -> Result<Receipt, ExtensionError> {
    let mut seen = Vec::new();
    let mut loads = Vec::new();
    for change in &transaction.documents {
        if seen.contains(&change.file) {
            return Err(ExtensionError::new(
                ErrorKind::InvalidEdit,
                "a document is listed twice",
            ));
        }
        seen.push(change.file);
        let Some(document) = app.workspace.documents.get(&change.file) else {
            return Err(ExtensionError::new(
                ErrorKind::StaleSnapshot,
                "the document was closed",
            ));
        };
        if document.stamp(change.file) != change.stamp {
            return Err(ExtensionError::new(
                ErrorKind::StaleSnapshot,
                "the document changed since it was read",
            ));
        }
        let load = load_of(app, change).expect("the document exists");
        // Duplicates were rejected above, so the position is a unique key.
        loads.push((loads.len() as u64, load));
    }
    admission::admit(limits, &loads)?;

    let mut planned = Vec::new();
    for change in &transaction.documents {
        let document = &app.workspace.documents[&change.file];
        let text = document.buffer.text_snapshot();
        let ending = document.buffer.line_ending();
        let wire: Vec<TextEdit> = change
            .edits
            .iter()
            .map(|(range, new_text)| TextEdit {
                range: ByteRange::new(range.start, range.end),
                text: new_text.clone(),
            })
            .collect();
        loog_extension_api::edit::validate(&text, &wire)?;
        let edits = change
            .edits
            .iter()
            .map(|(range, new_text)| {
                Edit::replace(
                    range.clone(),
                    loog_editor_core::text::normalize_endings(new_text, ending),
                )
            })
            .collect::<Vec<_>>();
        planned.push((change.file, edits));
    }

    let before = app.workspace.clone();
    let counter = app.editor.language.next_transaction;
    let registered = app.editor.language.transactions.clone();
    let id = next_transaction(app);
    let mut touched = Vec::new();
    for (file, edits) in planned {
        if let Err(error) = apply_document_edits(app, file, id, true, edits) {
            app.workspace = before;
            app.editor.language.next_transaction = counter;
            app.editor.language.transactions = registered;
            return Err(ExtensionError::new(ErrorKind::InvalidEdit, error));
        }
        touched.push(file);
    }
    finish_transaction(app, id, touched.clone());
    let versions = touched
        .iter()
        .filter_map(|file| {
            app.workspace
                .documents
                .get(file)
                .map(|document| (*file, document.buffer.version()))
        })
        .collect();
    Ok(Receipt {
        transaction: id,
        versions,
    })
}

#[cfg(test)]
mod tests;
