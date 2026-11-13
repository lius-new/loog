//! The same native consumers with an independent opaque-token WASM provider.
use super::*;
use loog_workspace_api::ResourceRef;
use std::sync::atomic::{AtomicU64, Ordering};

fn logged(host: &ExtensionHost, text: &str) -> bool {
    host.inner.service.lock().unwrap().packages["example.tools"]
        .logs
        .iter()
        .any(|(_, message)| message.contains(text))
}
#[test]
fn native_resource_memory_provider_paging_stale_reads_and_lost_save_receipts() {
    let commands = ["start", "hold", "release", "lose", "inspect"];
    let manifest = serde_json::json!({
        "schema":1,"id":"example.tools","version":"0.1.0","api":"loog:extension@0.1.0",
        "component":"extension.wasm","capabilities":["workspace.provide"],
        "activation":commands.iter().map(|name| serde_json::json!({"kind":"command","id":format!("example.tools.memory.{name}")})).collect::<Vec<_>>(),
        "contributes":{"commands":commands.iter().map(|name| serde_json::json!({"id":format!("example.tools.memory.{name}"),"title":name,"context":"window"})).collect::<Vec<_>>()}
    }).to_string();
    let package =
        super::super::super::tests::package_dir_with("memory-resource-provider", &manifest);
    let ui = SavingUi::new(AppState::new());
    let host = ExtensionHost::start(
        ui.state.clone(),
        ui.tasks.application(),
        super::super::super::tests::precompiled_cache().clone(),
        ui.runtime.trust().clone(),
        None,
    )
    .unwrap();
    let time = Arc::new(AtomicU64::new(1));
    let clock = time.clone();
    host.inner
        .service
        .lock()
        .unwrap()
        .set_clock(move || clock.load(Ordering::Relaxed));
    host.load_package(&package).unwrap();
    ui.runtime.install_extensions(host);
    let host = ui.runtime.extensions().unwrap();
    host.run_command(&ui.state, "example.tools.memory.start");
    wait(&ui, |app| !app.explorer.resources.roots.is_empty());
    let root = ui.state.get().explorer.resources.roots[0]
        .descriptor
        .clone();
    let ResourceRef::Provided {
        path: root_path,
        environment,
    } = root.resource.clone()
    else {
        panic!("provided root")
    };
    assert_eq!(root_path.as_str(), "collection:7");
    assert!(root.resource.local_path().is_none());

    crate::app::resources::directory(&ui.state, root.scope, root_path.clone(), false);
    wait(&ui, |app| {
        app.explorer.resources.root(root.scope).unwrap().directories[&root_path].loaded
    });
    assert!(
        ui.state
            .get()
            .explorer
            .resources
            .root(root.scope)
            .unwrap()
            .directories[&root_path]
            .cursor
            .is_some()
    );
    crate::app::resources::directory(&ui.state, root.scope, root_path.clone(), true);
    wait(&ui, |app| {
        app.explorer.resources.root(root.scope).unwrap().directories[&root_path]
            .entries
            .len()
            == 2
    });
    let files = ui
        .state
        .get()
        .explorer
        .resources
        .root(root.scope)
        .unwrap()
        .directories[&root_path]
        .entries
        .clone();
    let (file, other) = (&files[0], &files[1]);
    let resource = ResourceRef::provided(environment.clone(), file.path.as_str()).unwrap();
    let other_resource = ResourceRef::provided(environment, other.path.as_str()).unwrap();

    // A held read cannot block another document or overwrite its active view.
    host.run_command(&ui.state, "example.tools.memory.hold");
    wait(&ui, |_| logged(&host, "memory holding"));
    crate::app::resources::open(&ui.state, root.scope, file.clone());
    wait(&ui, |_| logged(&host, "memory read held"));
    crate::app::resources::open(&ui.state, root.scope, other.clone());
    wait(&ui, |app| {
        app.workspace
            .active_meta()
            .is_some_and(|m| m.resource == other_resource)
    });
    assert_eq!(
        ui.state
            .get()
            .workspace
            .active_document()
            .map(|id| ui.state.get().workspace.documents[&id]
                .buffer
                .text()
                .to_owned()),
        Some("second".into())
    );
    host.run_command(&ui.state, "example.tools.memory.release");
    wait(&ui, |_| {
        host.inner
            .service
            .lock()
            .unwrap()
            .workspace_router
            .pending_count()
            == 0
    });
    assert_eq!(
        ui.state.get().workspace.active_meta().unwrap().resource,
        other_resource
    );

    crate::app::resources::open(&ui.state, root.scope, file.clone());
    wait(&ui, |app| {
        app.workspace
            .active_meta()
            .is_some_and(|m| m.resource == resource)
    });
    let document = ui.state.get().workspace.active_document().unwrap();
    let original = ui.state.get().workspace.documents[&document]
        .buffer
        .text()
        .to_owned();
    let format = ui.state.get().workspace.documents[&document].format();
    assert_eq!(original, "中文\r\nfirst");
    crate::app::resources::open(&ui.state, root.scope, file.clone());
    assert_eq!(
        ui.state.get().workspace.file_id_for_resource(&resource),
        Some(document)
    );
    ui.state.update(|app| {
        app.workspace
            .document_editor_mut(document)
            .unwrap()
            .replace(0..0, "saved ", loog_editor_core::text::EditKind::Other)
            .unwrap();
    });
    assert!(crate::app::saving::document(
        &ui.state,
        document,
        |_, ok| assert!(ok)
    ));
    wait(&ui, |app| app.editor.saving.pending == 0);
    assert!(!ui.state.get().workspace.is_dirty(document));
    assert_eq!(
        read(&ui, &host, root.scope, &file.path),
        crate::features::encoding::encode(&format!("saved {original}"), format).unwrap()
    );

    host.run_command(&ui.state, "example.tools.memory.lose");
    wait(&ui, |_| logged(&host, "memory losing next receipt"));
    ui.state.update(|app| {
        app.workspace
            .document_editor_mut(document)
            .unwrap()
            .replace(
                0..0,
                "unconfirmed ",
                loog_editor_core::text::EditKind::Other,
            )
            .unwrap();
    });
    assert!(crate::app::saving::document(
        &ui.state,
        document,
        |_, ok| assert!(!ok)
    ));
    wait(&ui, |_| logged(&host, "memory committed without receipt"));
    let saved_text = ui.state.get().workspace.documents[&document]
        .buffer
        .text()
        .to_owned();
    assert_eq!(
        read(&ui, &host, root.scope, &file.path),
        crate::features::encoding::encode(&saved_text, format).unwrap()
    );
    time.store(31_001, Ordering::Relaxed);
    let effects = host.inner.service.lock().unwrap().pump_workspaces();
    ui.state.update(|app| host.inner.apply(app, effects));
    wait(&ui, |app| app.editor.saving.pending == 0);
    assert!(ui.state.get().workspace.is_dirty(document));
    assert!(ui.state.get().workspace.documents[&document].disk_conflict);
    assert_eq!(
        ui.state.get().workspace.documents[&document].buffer.text(),
        saved_text
    );
    assert!(!crate::app::saving::document(
        &ui.state,
        document,
        |_, ok| assert!(!ok)
    ));
    host.run_command(&ui.state, "example.tools.memory.inspect");
    wait(&ui, |_| logged(&host, "memory commits=2"));
    crate::app::resources::resolve(
        &ui.state,
        document,
        crate::domain::workspace::ResourceResolution::KeepEditor,
    );
    wait(&ui, |app| !app.workspace.documents[&document].disk_conflict);
    assert_eq!(
        ui.state.get().workspace.documents[&document].buffer.text(),
        saved_text
    );
    assert!(crate::app::saving::document(
        &ui.state,
        document,
        |_, ok| assert!(ok)
    ));
    wait(&ui, |app| app.editor.saving.pending == 0);
    host.run_command(&ui.state, "example.tools.memory.inspect");
    wait(&ui, |_| logged(&host, "memory commits=3"));

    ui.state.update(|app| {
        app.workspace
            .document_editor_mut(document)
            .unwrap()
            .replace(0..0, "offline ", loog_editor_core::text::EditKind::Other)
            .unwrap();
    });
    host.stop_all(&ui.state);
    wait(&ui, |app| app.explorer.resources.roots.is_empty());
    assert!(ui.state.get().workspace.is_dirty(document));
    assert!(!crate::app::saving::document(
        &ui.state,
        document,
        |_, ok| assert!(!ok)
    ));
    ui.runtime.clear_extensions();
}
