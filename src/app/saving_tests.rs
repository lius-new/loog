//! Real disk writes with a blocked worker and deterministic UI publication.
use super::{
    saving,
    state::{AppState, CloseContinuation, CloseRequest},
    test_support::SavingUi,
    workspace_commands,
};
use crate::domain::{
    document::FileId,
    encoding::{Charset, TextFormat},
};
use loog_editor_core::text::EditKind;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

struct Fixture {
    ui: SavingUi,
    root: PathBuf,
    path: PathBuf,
    id: FileId,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "loog-background-save-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("file.txt");
        std::fs::write(&path, "before").unwrap();
        let mut app = AppState::new();
        let id = app.workspace.open_path(path.clone(), "before".into());
        Self {
            ui: SavingUi::new(app),
            root,
            path,
            id,
        }
    }
    fn append(&self, text: &str) {
        self.ui.state.update(|app| {
            let mut editor = app.workspace.document_editor_mut(self.id).unwrap();
            editor.select_to(usize::MAX, false);
            editor.replace_selection(text, EditKind::Other).unwrap();
        });
    }
    fn block(&self) -> Gate {
        let (release, receive) = mpsc::channel();
        let (started, ready) = mpsc::channel();
        self.ui
            .runtime
            .save(Box::new(move || {
                started.send(()).unwrap();
                let _ = receive.recv();
            }))
            .unwrap();
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        Gate(Some(release))
    }
    fn close_request(&self, continuation: CloseContinuation) {
        self.ui.state.update(|app| {
            app.editor.close_request = Some(CloseRequest {
                targets: app.workspace.tabs_of(self.id),
                continuation,
            });
        });
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
struct Gate(Option<mpsc::Sender<()>>);
impl Drop for Gate {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            let _ = release.send(());
        }
    }
}

#[test]
fn blocked_disk_does_not_block_ui_and_completion_preserves_new_edits_and_undo() {
    let f = Fixture::new();
    f.append(" saved");
    let snapshot =
        f.ui.state
            .get()
            .workspace
            .shared_save_snapshot(f.id)
            .unwrap();
    assert!(Arc::ptr_eq(
        &snapshot.text,
        &f.ui.state.get().workspace.documents[&f.id]
            .buffer
            .text_snapshot()
    ));
    let gate = f.block();
    assert!(workspace_commands::save_document(&f.ui.state, f.id));
    // These UI mutations execute while the disk worker is deliberately blocked.
    f.append(" newer");
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "before");
    assert!(f.ui.state.get().workspace.is_dirty(f.id));
    drop(gate);
    f.ui.wait();
    let app = f.ui.state.get();
    let doc = &app.workspace.documents[&f.id];
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "before saved");
    assert_eq!(doc.buffer.text(), "before saved newer");
    assert_eq!(doc.saved_text.as_str(), snapshot.text.as_str());
    assert!(!Arc::ptr_eq(&doc.saved_text, &doc.buffer.text_snapshot()));
    assert!(app.workspace.is_dirty(f.id));
    drop(app);
    f.ui.state.update(|app| {
        app.workspace.document_editor_mut(f.id).unwrap().undo();
    });
    assert!(!f.ui.state.get().workspace.is_dirty(f.id));
}

#[test]
fn consecutive_saves_are_fifo_and_publish_the_exact_final_baseline() {
    let f = Fixture::new();
    let gate = f.block();
    f.append(" first");
    assert!(workspace_commands::save_document(&f.ui.state, f.id));
    f.append(" second");
    assert!(workspace_commands::save_document(&f.ui.state, f.id));
    drop(gate);
    f.ui.wait();
    assert_eq!(
        std::fs::read_to_string(&f.path).unwrap(),
        "before first second"
    );
    assert!(!f.ui.state.get().workspace.is_dirty(f.id));
}

#[test]
fn encoding_changed_during_save_remains_dirty_and_does_not_change_written_bytes() {
    let f = Fixture::new();
    let utf16 = TextFormat::new(Charset::Utf16Le, true);
    f.ui.state.update(|app| {
        app.workspace.core.set_encoding(f.id, utf16).unwrap();
    });
    let gate = f.block();
    assert!(workspace_commands::save_document(&f.ui.state, f.id));
    f.ui.state.update(|app| {
        app.workspace
            .core
            .set_encoding(f.id, TextFormat::default())
            .unwrap();
    });
    drop(gate);
    f.ui.wait();
    assert_eq!(
        std::fs::read(&f.path).unwrap(),
        crate::features::encoding::encode("before", utf16).unwrap()
    );
    let app = f.ui.state.get();
    assert_eq!(
        app.workspace.documents[&f.id].format(),
        TextFormat::default()
    );
    assert_eq!(app.workspace.documents[&f.id].saved_format, utf16);
    assert!(app.workspace.is_dirty(f.id));
}

#[test]
fn renamed_document_rejects_completion_for_the_previous_path() {
    let f = Fixture::new();
    f.append(" saved");
    let gate = f.block();
    assert!(workspace_commands::save_document(&f.ui.state, f.id));
    let renamed = f.root.join("renamed.txt");
    f.ui.state.update(|app| {
        app.workspace
            .documents
            .get_mut(&f.id)
            .unwrap()
            .meta
            .resource = loog_workspace_api::ResourceRef::local(renamed.clone());
    });
    drop(gate);
    f.ui.wait();
    assert!(f.ui.state.get().workspace.is_dirty(f.id));
    assert!(!renamed.exists());
}

#[test]
fn queue_is_bounded_and_rejected_requests_do_not_corrupt_pending_count() {
    let f = Fixture::new();
    let gate = f.block();
    for _ in 0..8 {
        assert!(workspace_commands::save_document(&f.ui.state, f.id));
    }
    assert!(!workspace_commands::save_document(&f.ui.state, f.id));
    assert_eq!(f.ui.state.get().editor.saving.pending, 8);
    drop(gate);
    f.ui.wait();
}

#[test]
fn close_waits_for_writes_and_repeated_confirm_or_cancel_cannot_close_early() {
    let f = Fixture::new();
    f.append(" saved");
    f.close_request(CloseContinuation::CloseTabs);
    let gate = f.block();
    let completed = Arc::new(AtomicUsize::new(0));
    let done = completed.clone();
    assert!(workspace_commands::save_close_request(
        &f.ui.state,
        move |_, next| {
            assert_eq!(next, CloseContinuation::CloseTabs);
            done.fetch_add(1, Ordering::Relaxed);
        }
    ));
    assert!(!workspace_commands::save_close_request(
        &f.ui.state,
        |_, _| panic!("duplicate close")
    ));
    workspace_commands::cancel_close_request(&f.ui.state);
    assert!(f.ui.state.get().editor.close_request.is_some());
    assert!(f.ui.state.get().workspace.documents.contains_key(&f.id));
    assert_eq!(completed.load(Ordering::Relaxed), 0);
    drop(gate);
    f.ui.wait();
    assert_eq!(completed.load(Ordering::Relaxed), 1);
    assert!(!f.ui.state.get().workspace.documents.contains_key(&f.id));
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "before saved");
}

#[test]
fn failed_close_save_keeps_tabs_and_the_previous_disk_contents() {
    let f = Fixture::new();
    f.append(" saved");
    f.ui.state.update(|app| {
        app.workspace
            .documents
            .get_mut(&f.id)
            .unwrap()
            .meta
            .resource = loog_workspace_api::ResourceRef::local(f.root.join("missing/file.txt"));
    });
    f.close_request(CloseContinuation::ExitApplication);
    assert!(workspace_commands::save_close_request(
        &f.ui.state,
        |_, _| panic!("failed save exited")
    ));
    f.ui.wait();
    let app = f.ui.state.get();
    assert!(app.workspace.is_dirty(f.id));
    assert!(app.editor.close_request.is_some());
    assert!(!app.editor.saving.closing);
    assert!(
        app.chrome
            .toast
            .as_ref()
            .unwrap()
            .message
            .contains("Could not save")
    );
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "before");
}

#[test]
fn edits_arriving_during_close_are_never_discarded_by_a_save_completion() {
    let f = Fixture::new();
    f.append(" saved");
    f.close_request(CloseContinuation::CloseTabs);
    let gate = f.block();
    assert!(workspace_commands::save_close_request(
        &f.ui.state,
        |_, _| panic!("new edits were closed")
    ));
    f.append(" newer");
    drop(gate);
    f.ui.wait();
    assert!(f.ui.state.get().workspace.is_dirty(f.id));
    assert!(f.ui.state.get().editor.close_request.is_some());
}

#[test]
fn discard_for_exit_waits_for_an_already_accepted_save() {
    let f = Fixture::new();
    f.append(" saved");
    let gate = f.block();
    assert!(workspace_commands::save_document(&f.ui.state, f.id));
    f.close_request(CloseContinuation::ExitApplication);
    let done = Arc::new(AtomicUsize::new(0));
    let completed = done.clone();
    assert!(saving::close(&f.ui.state, false, move |_, next| {
        assert_eq!(next, CloseContinuation::ExitApplication);
        completed.fetch_add(1, Ordering::Relaxed);
    }));
    assert_eq!(done.load(Ordering::Relaxed), 0);
    drop(gate);
    f.ui.wait();
    assert_eq!(done.load(Ordering::Relaxed), 1);
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "before saved");
    assert!(!f.ui.state.get().workspace.is_dirty(f.id));
}

#[test]
fn closed_and_reopened_document_does_not_receive_a_previous_lifetimes_baseline() {
    let f = Fixture::new();
    f.append(" saved");
    let gate = f.block();
    assert!(workspace_commands::save_document(&f.ui.state, f.id));
    let mut reopened = None;
    f.ui.state.update(|app| {
        app.workspace.close(f.id);
        let id = app.workspace.open_path(f.path.clone(), "reopened".into());
        app.workspace
            .document_editor_mut(id)
            .unwrap()
            .replace_selection("new ", EditKind::Other)
            .unwrap();
        reopened = Some(id);
    });
    drop(gate);
    f.ui.wait();
    let app = f.ui.state.get();
    assert!(app.workspace.is_dirty(reopened.unwrap()));
    assert_eq!(
        app.workspace.documents[&reopened.unwrap()]
            .saved_text
            .as_str(),
        "reopened"
    );
}

#[test]
fn exit_barrier_checks_documents_that_became_dirty_after_the_close_request() {
    let f = Fixture::new();
    let gate = f.block();
    assert!(workspace_commands::save_document(&f.ui.state, f.id));
    f.ui.state.update(|app| {
        app.editor.close_request = Some(CloseRequest {
            targets: vec![],
            continuation: CloseContinuation::ExitApplication,
        });
    });
    assert!(workspace_commands::save_close_request(
        &f.ui.state,
        |_, _| panic!("new edits were lost on exit")
    ));
    f.append(" newer");
    drop(gate);
    f.ui.wait();
    assert!(f.ui.state.get().editor.close_request.is_some());
    assert!(f.ui.state.get().workspace.is_dirty(f.id));
}

#[test]
fn an_exit_barrier_does_not_hide_failure_of_an_earlier_clean_save() {
    let f = Fixture::new();
    f.ui.state.update(|app| {
        app.workspace
            .documents
            .get_mut(&f.id)
            .unwrap()
            .meta
            .resource = loog_workspace_api::ResourceRef::local(f.root.join("missing/file.txt"));
    });
    let gate = f.block();
    assert!(workspace_commands::save_document(&f.ui.state, f.id));
    f.ui.state.update(|app| {
        app.editor.close_request = Some(CloseRequest {
            targets: vec![],
            continuation: CloseContinuation::ExitApplication,
        });
    });
    assert!(workspace_commands::save_close_request(
        &f.ui.state,
        |_, _| panic!("failed pending save exited")
    ));
    drop(gate);
    f.ui.wait();
    assert!(f.ui.state.get().editor.close_request.is_some());
    assert!(
        f.ui.state
            .get()
            .chrome
            .toast
            .as_ref()
            .unwrap()
            .message
            .contains("Could not save")
    );
}

#[test]
fn discard_does_not_remove_edits_that_arrive_after_the_decision() {
    let f = Fixture::new();
    f.append(" unsaved");
    f.close_request(CloseContinuation::CloseTabs);
    let gate = f.block();
    assert!(saving::close(&f.ui.state, false, |_, _| panic!(
        "new edits were discarded"
    )));
    f.append(" newer");
    drop(gate);
    f.ui.wait();
    assert!(f.ui.state.get().editor.close_request.is_some());
    assert!(f.ui.state.get().workspace.is_dirty(f.id));
}

#[test]
fn delayed_save_publication_preserves_a_newer_watcher_conflict_and_prevents_close() {
    let f = Fixture::new();
    f.append(" saved");
    f.close_request(CloseContinuation::CloseTabs);
    assert!(workspace_commands::save_close_request(
        &f.ui.state,
        |_, _| panic!("external conflict was closed")
    ));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    // Wait for the real write but hold its UI publication back.
    while f.ui.tasks.pending_ids().is_empty() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    std::fs::write(&f.path, "older external writer").unwrap();
    let batch = crate::infrastructure::file_watcher::FileChangeBatch {
        changes: Vec::new(),
        requires_full_rescan: true,
    };
    let older = super::file_watcher::prepare_batch(&f.ui.state.get(), &batch);
    std::fs::write(&f.path, "external writer").unwrap();
    let prepared = super::file_watcher::prepare_batch(&f.ui.state.get(), &batch);
    f.ui.state.update(|app| {
        super::file_watcher::apply_prepared_batch(app, prepared);
        let (_, retry) = super::file_watcher::apply_prepared_batch(app, older);
        assert!(
            retry,
            "an older observation must be retried rather than replace a newer one"
        );
    });
    let observed = f.ui.state.get().workspace.documents[&f.id]
        .disk_state
        .clone();
    assert!(f.ui.state.get().workspace.documents[&f.id].disk_conflict);
    f.ui.wait();
    let app = f.ui.state.get();
    assert!(app.editor.close_request.is_some());
    assert!(app.workspace.documents[&f.id].disk_conflict);
    assert_eq!(app.workspace.documents[&f.id].disk_state, observed);
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "external writer");
}
