//! Opt-in acceptance through the production native consumers and a real component.
use super::*;
use crate::app::test_support::SavingUi;
use crate::features::extensions::ui::{Node, Origin, Payload};
use loog_workspace_api::{Answer, FileInfo, Operation, ProviderPath, ScopeId};
use std::time::{Duration, Instant};
#[path = "resource_fixture_acceptance.rs"]
mod independent_provider;
#[path = "resource_multi_acceptance.rs"]
mod multiple_environments;
#[path = "resource_node_acceptance.rs"]
mod node_language;
#[path = "resource_git_acceptance.rs"]
mod resource_git;
#[path = "resource_image_acceptance.rs"]
mod resource_image;
#[path = "resource_markdown_acceptance.rs"]
mod resource_markdown;
#[path = "resource_options_acceptance.rs"]
mod resource_options;
#[path = "resource_process_acceptance.rs"]
mod resource_process;
#[path = "resource_project_acceptance.rs"]
mod resource_project;
#[path = "resource_search_acceptance.rs"]
mod resource_search;

// Keep independent phases in independent stack frames. Debug builds retain
// many large AppState snapshot temporaries in this end-to-end fixture.
#[inline(never)]
fn phase(work: impl FnOnce()) {
    work();
}

#[track_caller]
fn wait(ui: &SavingUi, mut ready: impl FnMut(&AppState) -> bool) {
    wait_for(ui, Duration::from_secs(45), &mut ready);
}
#[track_caller]
fn wait_for(ui: &SavingUi, timeout: Duration, mut ready: impl FnMut(&AppState) -> bool) {
    let deadline = Instant::now() + timeout;
    loop {
        ui.tasks.run_all();
        if ready(&ui.state.get()) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "native resource acceptance timed out: {} status: {:?} watches: {:?}",
            ui.runtime
                .extensions()
                .map(|host| host.report())
                .unwrap_or_default(),
            status(&ui.state.get().extension_widgets.roots()),
            ui.state
                .get()
                .explorer
                .resources
                .roots
                .iter()
                .map(|root| (
                    root.descriptor.scope,
                    root.watch.handle,
                    root.watch.error.clone()
                ))
                .collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn status(nodes: &[Arc<Node>]) -> Option<String> {
    nodes.iter().find_map(|node| {
        if node.origin.id.as_ref() == "status" {
            node.properties.text.clone()
        } else {
            status(&node.children)
        }
    })
}
fn origin(nodes: &[Arc<Node>], id: &str) -> Option<Origin> {
    nodes.iter().find_map(|node| {
        if node.origin.id.as_ref() == id {
            Some(node.origin.clone())
        } else {
            origin(&node.children, id)
        }
    })
}
fn event(ui: &SavingUi, host: &ExtensionHost, id: &str, payload: Payload) {
    let app = ui.state.get();
    let source = origin(&app.extension_widgets.roots(), id).expect("plugin UI control");
    host.ui_event(&app, source, payload);
}
#[track_caller]
fn request(ui: &SavingUi, host: &ExtensionHost, scope: ScopeId, operation: Operation) -> Answer {
    let result = Arc::new(Mutex::new(None));
    let received = result.clone();
    host.request_resource(
        scope,
        operation,
        Box::new(move |_, value| *received.lock().unwrap() = Some(value)),
    )
    .unwrap();
    wait(ui, |_| result.lock().unwrap().is_some());
    let value = result.lock().unwrap().take().unwrap().unwrap();
    value
}
fn read(ui: &SavingUi, host: &ExtensionHost, scope: ScopeId, path: &ProviderPath) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let Answer::Bytes {
            bytes: chunk, eof, ..
        } = request(
            ui,
            host,
            scope,
            Operation::Read {
                path: path.clone(),
                offset: bytes.len() as u64,
                limit: 128 * 1024,
            },
        )
        else {
            panic!("read")
        };
        bytes.extend(chunk);
        if eof {
            return bytes;
        }
    }
}

fn edit_resource(
    ui: &SavingUi,
    target: crate::features::resource_tree::Target,
    kind: crate::features::resource_tree::EditKind,
    text: &str,
) {
    let scope = target.scope;
    ui.state.update(|app| {
        app.explorer.resources.edit = Some(crate::features::resource_tree::Edit { target, kind });
        app.explorer.ui.create_input.set_text(text);
    });
    crate::app::resource_edit::submit(&ui.state);
    wait(ui, |app| {
        app.explorer.resources.edit.is_none() && !app.explorer.resources.busy.contains(&scope)
    });
}

fn child_path(
    ui: &SavingUi,
    host: &ExtensionHost,
    scope: ScopeId,
    parent: ProviderPath,
    name: &str,
) -> ProviderPath {
    match request(
        ui,
        host,
        scope,
        Operation::ChildPath {
            parent,
            name: name.into(),
        },
    ) {
        Answer::Path { path } => path,
        _ => panic!("child path"),
    }
}

#[test]
#[ignore = "requires LOOG_REMOTE_PACKAGE and LOOG_REMOTE_PROFILE for a disposable workspace"]
fn native_resource_editor_real_provider_open_save_conflict_and_disconnect() {
    let package = PathBuf::from(std::env::var("LOOG_REMOTE_PACKAGE").unwrap());
    let profile: serde_json::Value =
        serde_json::from_str(&std::env::var("LOOG_REMOTE_PROFILE").unwrap()).unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(package.join("manifest.json")).unwrap()).unwrap();
    let id = manifest["id"].as_str().unwrap();
    let cache = std::env::temp_dir().join(format!("loog-native-resource-{}", std::process::id()));
    let ui = SavingUi::new(AppState::new());
    ui.runtime.trust().attach(cache.join("trust.json"));
    let host = ExtensionHost::start(
        ui.state.clone(),
        ui.tasks.application(),
        cache.join("compiled"),
        ui.runtime.trust().clone(),
        None,
    )
    .unwrap();
    host.inner
        .service
        .lock()
        .unwrap()
        .set_storage_dir(cache.join("storage"));
    host.load_package(&package).unwrap();
    ui.runtime.install_extensions(host);
    let host = ui.runtime.extensions().unwrap();
    host.run_command(&ui.state, &format!("{id}.connect"));
    wait(&ui, |app| {
        origin(&app.extension_widgets.roots(), "connect").is_some()
    });
    for field in ["target", "root", "config"] {
        event(
            &ui,
            &host,
            field,
            Payload::Input(profile[field].as_str().unwrap_or("").into()),
        );
    }
    event(
        &ui,
        &host,
        "port",
        Payload::Input(profile["port"].as_u64().unwrap_or(22).to_string()),
    );
    event(&ui, &host, "connect", Payload::Click);
    wait(&ui, |app| !app.explorer.resources.roots.is_empty());
    let root = ui.state.get().explorer.resources.roots[0]
        .descriptor
        .clone();
    let scope = root.scope;
    native_terminal(&ui, &host, &root);
    resource_process::run(&ui, &host, &root);
    resource_git::run(&ui, &host, &root);
    resource_options::run(&ui, &host, &root);
    resource_project::run(&ui, &host, &root);
    resource_image::run(&ui, &host, &root);
    resource_markdown::run(&ui, &host, &root);
    resource_search::run(&ui, &host, &root);
    if let Ok(package) = std::env::var("LOOG_REMOTE_LANGUAGE_PACKAGE") {
        native_language(&ui, &host, &root, Path::new(&package));
    }
    if let Ok(package) = std::env::var("LOOG_REMOTE_NODE_PACKAGE") {
        node_language::run(&ui, &host, &root, Path::new(&package));
    }
    phase(|| {
        let name = format!("native-acceptance-{}.txt", std::process::id());
        let path = ProviderPath::new(name.clone()).unwrap();
        let format = crate::domain::encoding::TextFormat::new(
            crate::domain::encoding::Charset::Utf16Le,
            true,
        );
        let original = "中文\r\noriginal";
        request(
            &ui,
            &host,
            scope,
            Operation::Write {
                path: path.clone(),
                bytes: crate::features::encoding::encode(original, format).unwrap(),
                expected_revision: None,
            },
        );
        let loog_workspace_api::ResourceRef::Provided {
            path: root_path, ..
        } = root.resource
        else {
            panic!("provided root")
        };
        if !ui
            .state
            .get()
            .explorer
            .resources
            .root(scope)
            .unwrap()
            .directories
            .get(&root_path)
            .is_some_and(|directory| directory.expanded)
        {
            crate::app::resources::directory(&ui.state, scope, root_path.clone(), false);
        }
        wait(&ui, |app| {
            app.explorer
                .resources
                .root(scope)
                .unwrap()
                .directories
                .get(&root_path)
                .is_some_and(|dir| dir.loaded && dir.entries.iter().any(|entry| entry.name == name))
        });
        let file: FileInfo = ui
            .state
            .get()
            .explorer
            .resources
            .root(scope)
            .unwrap()
            .directories[&root_path]
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .unwrap()
            .clone();
        crate::app::resources::open(&ui.state, scope, file.clone());
        wait(&ui, |app| app.workspace.active_document().is_some());
        let document = ui.state.get().workspace.active_document().unwrap();
        assert_eq!(
            ui.state.get().workspace.documents[&document].buffer.text(),
            original
        );
        assert_eq!(
            ui.state.get().workspace.documents[&document].format(),
            format
        );
        native_save_participants(&ui, &host, scope, document, &file.path, format);
        let large = "中文\r\n".repeat(70_000);
        ui.state.update(|app| {
            let len = app.workspace.documents[&document].buffer.len();
            app.workspace
                .document_editor_mut(document)
                .unwrap()
                .replace(0..len, &large, loog_editor_core::text::EditKind::Other)
                .unwrap();
        });
        assert!(crate::app::saving::document(
            &ui.state,
            document,
            |_, ok| assert!(ok)
        ));
        ui.state.update(|app| {
            let len = app.workspace.documents[&document].buffer.len();
            app.workspace
                .document_editor_mut(document)
                .unwrap()
                .replace(len..len, "later", loog_editor_core::text::EditKind::Other)
                .unwrap();
        });
        wait(&ui, |app| app.editor.saving.pending == 0);
        assert!(ui.state.get().workspace.is_dirty(document));
        assert_eq!(
            read(&ui, &host, scope, &file.path),
            crate::features::encoding::encode(&large, format).unwrap()
        );
        crate::app::saving::document(&ui.state, document, |_, ok| assert!(ok));
        wait(&ui, |app| app.editor.saving.pending == 0);
        assert!(!ui.state.get().workspace.is_dirty(document));
        request(
            &ui,
            &host,
            scope,
            Operation::Write {
                path: file.path.clone(),
                bytes: b"external".to_vec(),
                expected_revision: None,
            },
        );
        ui.state.update(|app| {
            app.workspace
                .document_editor_mut(document)
                .unwrap()
                .replace(0..0, "mine", loog_editor_core::text::EditKind::Other)
                .unwrap();
        });
        crate::app::saving::document(&ui.state, document, |_, ok| assert!(!ok));
        wait(&ui, |app| app.editor.saving.pending == 0);
        assert!(ui.state.get().workspace.documents[&document].disk_conflict);
        assert!(ui.state.get().workspace.is_dirty(document));
        assert_eq!(read(&ui, &host, scope, &file.path), b"external");
        crate::app::resources::resolve(
            &ui.state,
            document,
            crate::domain::workspace::ResourceResolution::KeepEditor,
        );
        wait(&ui, |app| !app.workspace.documents[&document].disk_conflict);
        crate::app::saving::document(&ui.state, document, |_, ok| assert!(ok));
        wait(&ui, |app| app.editor.saving.pending == 0);
        assert!(!ui.state.get().workspace.is_dirty(document));
        request(
            &ui,
            &host,
            scope,
            Operation::Remove {
                path: file.path,
                recursive: false,
            },
        );
        use crate::features::resource_tree::{EditKind, Target};
        use loog_workspace_api::FileKind;
        let root_target = Target {
            scope,
            path: root_path.clone(),
            parent: None,
            name: "root".into(),
            kind: FileKind::Directory,
        };
        let folder_name = format!("native-folder-{}", std::process::id());
        edit_resource(&ui, root_target.clone(), EditKind::NewFolder, &folder_name);
        let folder_path = child_path(&ui, &host, scope, root_path.clone(), &folder_name);
        let folder_target = Target {
            scope,
            path: folder_path.clone(),
            parent: Some(root_path.clone()),
            name: folder_name.clone(),
            kind: FileKind::Directory,
        };
        edit_resource(&ui, folder_target.clone(), EditKind::NewFile, "child.txt");
        wait(&ui, |app| {
            app.workspace
                .active_document()
                .is_some_and(|active| active != document)
        });
        let child = ui.state.get().workspace.active_document().unwrap();
        ui.state.update(|app| {
            app.workspace
                .document_editor_mut(child)
                .unwrap()
                .replace(
                    0..0,
                    "keep edits through directory rename",
                    loog_editor_core::text::EditKind::Other,
                )
                .unwrap();
        });
        let moved_name = format!("{folder_name}-moved");
        edit_resource(&ui, folder_target, EditKind::Rename, &moved_name);
        let moved_folder = child_path(&ui, &host, scope, root_path.clone(), &moved_name);
        let moved_child = child_path(&ui, &host, scope, moved_folder.clone(), "child.txt");
        wait(&ui, |app| !app.workspace.documents[&child].disk_conflict);
        assert_eq!(
            ui.state.get().workspace.documents[&child].buffer.text(),
            "keep edits through directory rename"
        );
        assert!(ui.state.get().workspace.is_dirty(child));
        assert!(
            matches!(&ui.state.get().workspace.documents[&child].meta.resource, loog_workspace_api::ResourceRef::Provided { path, .. } if path == &moved_child)
        );
        crate::app::saving::document(&ui.state, child, |_, ok| assert!(ok));
        wait(&ui, |app| app.editor.saving.pending == 0);
        assert_eq!(
            read(&ui, &host, scope, &moved_child),
            b"keep edits through directory rename"
        );
        ui.state.update(|app| {
            let len = app.workspace.documents[&child].buffer.len();
            app.workspace
                .document_editor_mut(child)
                .unwrap()
                .replace(
                    len..len,
                    " and cold restore",
                    loog_editor_core::text::EditKind::Other,
                )
                .unwrap();
        });
        wait(&ui, |app| app.explorer.resources.bookmarks.len() == 1);
        let child_resource = ui.state.get().workspace.documents[&child]
            .meta
            .resource
            .clone();
        let original_resource = ui.state.get().workspace.documents[&document]
            .meta
            .resource
            .clone();
        let session = crate::app::workspace_session::WorkspaceSession {
            resource_roots: ui.state.get().explorer.resources.bookmarks.clone(),
            editor_layout: crate::app::workspace_session::saved_editor_layout(
                &ui.state.get().workspace,
            ),
            ..Default::default()
        };
        let wire = serde_json::to_vec(&session).unwrap();
        ui.runtime.clear_extensions();
        drop(host);
        wait(&ui, |app| app.explorer.resources.roots.is_empty());
        assert_eq!(ui.state.get().explorer.resources.bookmarks.len(), 1);
        let session: crate::app::workspace_session::WorkspaceSession =
            serde_json::from_slice(&wire).unwrap();
        let mut restored = AppState::new();
        restored.explorer.resources.bookmarks = session.resource_roots;
        crate::app::workspace_commands::hydrate_editor_layout(&mut restored, session.editor_layout);
        let ui = SavingUi::new(restored);
        ui.runtime.trust().attach(cache.join("trust.json"));
        let child = *ui
            .state
            .get()
            .workspace
            .documents
            .iter()
            .find(|(_, doc)| doc.meta.resource == child_resource)
            .unwrap()
            .0;
        let document = *ui
            .state
            .get()
            .workspace
            .documents
            .iter()
            .find(|(_, doc)| doc.meta.resource == original_resource)
            .unwrap()
            .0;
        assert!(
            ui.state.get().workspace.documents[&child]
                .provider
                .is_none()
        );
        assert!(ui.state.get().workspace.is_dirty(child));
        let host = ExtensionHost::start(
            ui.state.clone(),
            ui.tasks.application(),
            cache.join("compiled"),
            ui.runtime.trust().clone(),
            None,
        )
        .unwrap();
        host.inner
            .service
            .lock()
            .unwrap()
            .set_storage_dir(cache.join("storage"));
        host.load_package(&package).unwrap();
        ui.runtime.install_extensions(host);
        let host = ui.runtime.extensions().unwrap();
        host.restore_resources();
        wait(&ui, |app| {
            !app.explorer.resources.roots.is_empty()
                && app.workspace.documents[&child].provider.is_some()
                && !app.workspace.documents[&child].disk_conflict
        });
        let scope = ui.state.get().explorer.resources.roots[0].descriptor.scope;
        assert_eq!(
            ui.state.get().workspace.documents[&child].buffer.text(),
            "keep edits through directory rename and cold restore"
        );
        assert!(ui.state.get().workspace.is_dirty(child));
        crate::app::saving::document(&ui.state, child, |_, ok| assert!(ok));
        wait(&ui, |app| app.editor.saving.pending == 0);
        assert_eq!(
            read(&ui, &host, scope, &moved_child),
            b"keep edits through directory rename and cold restore"
        );
        wait(&ui, |app| {
            app.explorer
                .resources
                .root(scope)
                .unwrap()
                .watch
                .handle
                .is_some()
        });
        request(
            &ui,
            &host,
            scope,
            Operation::Write {
                path: moved_child.clone(),
                bytes: b"watch changed the clean file".to_vec(),
                expected_revision: None,
            },
        );
        wait(&ui, |app| {
            app.workspace.documents[&child].buffer.text() == "watch changed the clean file"
                && !app.workspace.documents[&child].disk_conflict
        });
        ui.state.update(|app| {
            app.workspace
                .document_editor_mut(child)
                .unwrap()
                .replace(0..0, "unsaved ", loog_editor_core::text::EditKind::Other)
                .unwrap();
        });
        let Answer::Written { revision } = request(
            &ui,
            &host,
            scope,
            Operation::Write {
                path: moved_child.clone(),
                bytes: b"another external change".to_vec(),
                expected_revision: None,
            },
        ) else {
            panic!("write response")
        };
        wait(&ui, |app| {
            let doc = &app.workspace.documents[&child];
            doc.disk_conflict
                && doc
                    .provider
                    .as_ref()
                    .is_some_and(|(_, current)| *current == revision)
        });
        assert_eq!(
            ui.state.get().workspace.documents[&child].buffer.text(),
            "unsaved watch changed the clean file"
        );
        crate::app::resources::resolve(
            &ui.state,
            child,
            crate::domain::workspace::ResourceResolution::KeepEditor,
        );
        wait(&ui, |app| !app.workspace.documents[&child].disk_conflict);
        crate::app::saving::document(&ui.state, child, |_, ok| assert!(ok));
        wait(&ui, |app| app.editor.saving.pending == 0);
        crate::app::resources::directory(&ui.state, scope, root_path.clone(), false);
        let listed = |app: &AppState, name: &str| {
            app.explorer
                .resources
                .root(scope)
                .unwrap()
                .directories
                .get(&root_path)
                .is_some_and(|directory| directory.entries.iter().any(|entry| entry.name == name))
        };
        wait(&ui, |app| listed(app, &moved_name));
        let watched_path = child_path(&ui, &host, scope, root_path.clone(), "watch-created.txt");
        request(
            &ui,
            &host,
            scope,
            Operation::CreateFile {
                path: watched_path.clone(),
            },
        );
        wait(&ui, |app| listed(app, "watch-created.txt"));
        request(
            &ui,
            &host,
            scope,
            Operation::Remove {
                path: watched_path,
                recursive: false,
            },
        );
        wait(&ui, |app| !listed(app, "watch-created.txt"));
        edit_resource(
            &ui,
            Target {
                scope,
                path: moved_child,
                parent: Some(moved_folder.clone()),
                name: "child.txt".into(),
                kind: FileKind::File,
            },
            EditKind::Delete,
            "child.txt",
        );
        assert!(ui.state.get().workspace.documents[&child].missing_on_disk);
        assert_eq!(
            ui.state.get().workspace.documents[&child].buffer.text(),
            "unsaved watch changed the clean file"
        );
        edit_resource(
            &ui,
            Target {
                scope,
                path: moved_folder,
                parent: Some(root_path),
                name: moved_name.clone(),
                kind: FileKind::Directory,
            },
            EditKind::Delete,
            &moved_name,
        );
        host.run_command(&ui.state, &format!("{id}.disconnect"));
        wait(&ui, |app| {
            app.explorer.resources.roots.is_empty() && app.explorer.resources.bookmarks.is_empty()
        });
        assert!(host.inner.resource_callbacks.lock().unwrap().is_empty());
        assert!(ui.state.get().workspace.documents.contains_key(&document));
        println!(
            "PASS: native provider UI, terminal Unicode/resize/exit/trust revocation, directory, UTF-16 open, 560KB save, later edits, conflict resolution, create, directory rename, cold restore, live file/directory monitoring, external conflict preserving edits, delete and explicit disconnect"
        );
    });
}

fn native_save_participants(
    ui: &SavingUi,
    host: &ExtensionHost,
    scope: ScopeId,
    file: crate::domain::identity::FileId,
    path: &ProviderPath,
    format: crate::domain::encoding::TextFormat,
) {
    println!("CHECK: snapshot save participants commit before resource persistence (WASM fixture)");
    let language = ui.state.get().workspace.documents[&file]
        .meta
        .lang
        .name()
        .to_owned();
    let manifest = serde_json::json!({"schema":1,"id":"example.tools","version":"0.1.0",
    "api":"loog:extension@0.1.0","component":"extension.wasm",
    "activation":[{"kind":"event","name":"resource.documents"}],
    "capabilities":["documents.read","workspace.edit","languages","events"],
    "contributes":{"language_service":{"providers":[
        {"id":"example.tools.a-prefix","kind":"save-participant","selector":{"languages":[language]}},
        {"id":"example.tools.trim","kind":"save-participant","selector":{"languages":[language]}}
    ]}}});
    let package = crate::app::extensions::tests::package_dir_with(
        "resource-save-native",
        &manifest.to_string(),
    );
    host.load_package(&package).unwrap();
    host.observe(&ui.state.get());
    wait(ui, |_| {
        host.inner.service.lock().unwrap().packages["example.tools"]
            .lifecycle
            .state()
            == super::super::State::Active
    });
    {
        let mut service = host.inner.service.lock().unwrap();
        let owner = loog_workspace_api::Owner {
            id: "example.tools".into(),
            generation: service.packages["example.tools"].lifecycle.generation(),
        };
        service
            .bind_resource_providers(
                &owner,
                vec!["example.tools.a-prefix".into(), "example.tools.trim".into()],
            )
            .unwrap();
    }
    let before = "中文  \r\noriginal  ";
    ui.state.update(|app| {
        let length = app.workspace.documents[&file].buffer.len();
        app.workspace
            .document_editor_mut(file)
            .unwrap()
            .replace(0..length, before, loog_editor_core::text::EditKind::Other)
            .unwrap();
    });
    let saved = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done = saved.clone();
    assert!(crate::app::saving::document(
        &ui.state,
        file,
        move |_, ok| {
            assert!(ok);
            done.store(true, std::sync::atomic::Ordering::Release);
        }
    ));
    wait(ui, |_| saved.load(std::sync::atomic::Ordering::Acquire));
    let expected = "😀 中文\r\noriginal";
    assert_eq!(
        ui.state.get().workspace.documents[&file].buffer.text(),
        expected
    );
    assert_eq!(
        read(ui, host, scope, path),
        crate::features::encoding::encode(expected, format).unwrap()
    );
    ui.state.update(|app| {
        app.workspace.document_editor_mut(file).unwrap().undo();
        assert_eq!(app.workspace.documents[&file].buffer.text(), before);
    });
    let effects = host.inner.service.lock().unwrap().stop("example.tools");
    ui.state.update(|app| host.inner.apply(app, effects));
}

fn native_language(
    ui: &SavingUi,
    host: &ExtensionHost,
    root: &loog_workspace_api::PresentedRoot,
    package: &Path,
) {
    let program = std::env::var("LOOG_REMOTE_LANGUAGE_PROGRAM").unwrap();
    let managed = program == "@bundled";
    let loog_workspace_api::ResourceRef::Provided {
        path: root_path,
        environment,
    } = &root.resource
    else {
        panic!("root")
    };
    let project = child_path(
        ui,
        host,
        root.scope,
        root_path.clone(),
        &format!("native-language-project-{}", std::process::id()),
    );
    request(
        ui,
        host,
        root.scope,
        Operation::CreateDirectory {
            path: project.clone(),
        },
    );
    let source = child_path(ui, host, root.scope, project.clone(), "src-中文");
    request(
        ui,
        host,
        root.scope,
        Operation::CreateDirectory {
            path: source.clone(),
        },
    );
    let path = child_path(ui, host, root.scope, source.clone(), "main.rs");
    let marker = child_path(ui, host, root.scope, project.clone(), "rust-project.json");
    request(ui, host, root.scope, Operation::Write {
        path: marker.clone(),
        bytes: serde_json::json!({"crates":[{"root_module":path.as_str(),"edition":"2021","deps":[],"cfg":[],"env":{}}]}).to_string().into_bytes(),
        expected_revision: None,
    });
    let good = "// 中文😀\nmod helper;\n/// Remote completion documentation 中文.\nfn value() -> i32 { 7 }\nfn main() { let result = value(); let other = helper::remote_target(); }\n";
    let helper = child_path(ui, host, root.scope, source.clone(), "helper.rs");
    let helper_text = "// 中文😀\npub fn remote_target() -> i32 { 9 }\n";
    request(
        ui,
        host,
        root.scope,
        Operation::Write {
            path: helper.clone(),
            bytes: helper_text.as_bytes().to_vec(),
            expected_revision: None,
        },
    );
    request(
        ui,
        host,
        root.scope,
        Operation::Write {
            path: path.clone(),
            bytes: good.as_bytes().to_vec(),
            expected_revision: None,
        },
    );
    ui.runtime
        .install_language_services(crate::app::language_services::LanguageServices::new());
    let id = host.load_package(package).unwrap();
    let formatter = std::env::var("LOOG_REMOTE_FORMATTER").ok();
    let mut initialization = serde_json::json!({
        "procMacro":{"enable":false},"cargo":{"buildScripts":{"enable":false}},
        "check":{"enable":false},"checkOnSave":false
    });
    if let Some(formatter) = &formatter {
        initialization["rustfmt"] =
            serde_json::json!({"overrideCommand":[formatter,"--edition","2021"]});
    }
    let config_dir = child_path(ui, host, root.scope, project.clone(), ".loog");
    request(
        ui,
        host,
        root.scope,
        Operation::CreateDirectory {
            path: config_dir.clone(),
        },
    );
    let config_file = child_path(
        ui,
        host,
        root.scope,
        config_dir.clone(),
        "extension-settings.json",
    );
    let project_settings = serde_json::json!({
        format!("{id}.initialization"): initialization.to_string()
    });
    request(
        ui,
        host,
        root.scope,
        Operation::Write {
            path: config_file.clone(),
            bytes: project_settings.to_string().into_bytes(),
            expected_revision: None,
        },
    );
    let mut settings = serde_json::json!({
        format!("{id}.environmentProgram"): program,
        format!("{id}.initialization"): "{}"
    });
    if managed {
        settings[format!("{id}.environmentCache")] = serde_json::json!(
            std::env::var("LOOG_REMOTE_TOOL_CACHE")
                .expect("managed acceptance requires an isolated target cache")
        );
    }
    let unrelated_local_project = serde_json::json!({
        format!("{id}.environmentProgram"): "/loog-acceptance-wrong-native-project-server",
        format!("{id}.initialization"): "{}"
    });
    let effects = host.inner.service.lock().unwrap().set_setting_sources(
        settings.clone(),
        Some(unrelated_local_project),
        true,
    );
    ui.state.update(|app| host.inner.apply(app, effects));
    ui.runtime
        .trust()
        .decide_resource(&root.resource, true)
        .unwrap();
    host.refresh_resource_grants();
    let Answer::Info { file: info } =
        request(ui, host, root.scope, Operation::Stat { path: path.clone() })
    else {
        panic!("file")
    };
    crate::app::resources::open(&ui.state, root.scope, info);
    let resource =
        loog_workspace_api::ResourceRef::provided(environment.clone(), path.as_str()).unwrap();
    wait(ui, |app| {
        app.workspace
            .documents
            .values()
            .any(|d| d.meta.resource == resource)
    });
    let file = ui
        .state
        .get()
        .workspace
        .documents
        .iter()
        .find(|(_, d)| d.meta.resource == resource)
        .map(|(id, _)| *id)
        .unwrap();
    host.observe(&ui.state.get());
    // Each acceptance run owns a fresh compiler cache. Cold compilation is
    // separate from the subsequent 45-second resource-operation deadlines.
    wait_for(ui, Duration::from_secs(120), |_| {
        host.inner.service.lock().unwrap().packages[&id]
            .lifecycle
            .state()
            == super::super::State::Active
    });
    ui.state.update(|app| {
        let length = app.workspace.documents[&file].buffer.len();
        app.workspace
            .document_editor_mut(file)
            .unwrap()
            .replace(
                0..length,
                "// 中文😀\nfn broken( {",
                loog_editor_core::text::EditKind::Other,
            )
            .unwrap();
    });
    host.observe(&ui.state.get());
    let services = ui.runtime.language_services().unwrap();
    println!("CHECK: resource language diagnostics after project discovery");
    wait_for(
        ui,
        Duration::from_secs(if managed { 300 } else { 45 }),
        |app| {
            assert!(
                !host
                    .language_services(Some(&resource))
                    .iter()
                    .any(|service| service.extension == id
                        && service.state == crate::features::language_services_menu::State::Failed),
                "{}",
                host.report()
            );
            services
                .results(&resource, &app.workspace.documents[&file].stamp(file))
                .diagnostics
                .is_some_and(|items| !items.is_empty())
        },
    );
    let local = loog_workspace_api::ResourceRef::local(PathBuf::from(path.as_str()));
    wait(ui, |_| {
        host.language_services(Some(&resource))
            .iter()
            .any(|service| service.extension == id && service.note == "Ready")
    });
    assert!(
        host.language_services(Some(&local))
            .iter()
            .all(|service| service.note.is_empty())
    );
    let foreign = loog_workspace_api::ResourceRef::provided(
        loog_workspace_api::EnvironmentKey::new(environment.provider(), "another-environment")
            .unwrap(),
        path.as_str(),
    )
    .unwrap();
    assert!(
        host.language_services(Some(&foreign))
            .iter()
            .all(|service| service.note.is_empty())
    );
    assert!(
        services
            .results(
                &local,
                &ui.state.get().workspace.documents[&file].stamp(file)
            )
            .diagnostics
            .is_none()
    );
    ui.state.update(|app| {
        let length = app.workspace.documents[&file].buffer.len();
        app.workspace
            .document_editor_mut(file)
            .unwrap()
            .replace(0..length, good, loog_editor_core::text::EditKind::Other)
            .unwrap();
    });
    host.observe(&ui.state.get());
    wait(ui, |app| {
        services
            .results(&resource, &app.workspace.documents[&file].stamp(file))
            .diagnostics
            .is_some_and(|items| items.is_empty())
    });
    ui.state.update(|app| {
        app.workspace
            .document_editor_mut(file)
            .unwrap()
            .set_cursor(good.rfind("value();").unwrap() + 2)
    });
    crate::app::language_ui::hover::show_at_cursor(&ui.state);
    println!("CHECK: resource language hover");
    wait(ui, |app| {
        app.editor
            .language
            .hover
            .card_for(
                app.workspace.active_pane(),
                &app.workspace.documents[&file].stamp(file),
            )
            .is_some()
    });
    {
        let app = ui.state.get();
        let card = app
            .editor
            .language
            .hover
            .card_for(
                app.workspace.active_pane(),
                &app.workspace.documents[&file].stamp(file),
            )
            .unwrap();
        assert!(
            card.documentation.text.contains("value"),
            "{}",
            card.documentation.text
        );
    }
    // Request through the actual completion model, resolve the selected server
    // item and apply its edit to the provider-backed native buffer.
    use crate::app::language_ui::{actions, navigate, peek, popup};
    use crate::features::providers::Feature;
    let rect = lgui::prelude::UiRect::new(0.0, 0.0, 800.0, 600.0);
    let helper_resource =
        loog_workspace_api::ResourceRef::provided(environment.clone(), helper.as_str()).unwrap();
    assert!(
        ui.state
            .get()
            .workspace
            .file_id_for_resource(&helper_resource)
            .is_none()
    );
    ui.state.update(|app| {
        app.workspace
            .document_editor_mut(file)
            .unwrap()
            .set_cursor(good.rfind("remote_target()").unwrap() + 3)
    });
    actions::begin(
        &ui.state,
        actions::Wanted::Peek(Feature::Definition),
        rect,
        None,
    );
    println!("CHECK: resource language preview of unopened target");
    wait(ui, |app| {
        peek::view(app, app.workspace.active_pane()).is_some_and(|view| {
            view.lines
                .iter()
                .any(|(_, line, _)| line.contains("pub fn remote_target"))
        })
    });
    assert_eq!(ui.state.get().workspace.active_document(), Some(file));
    assert!(
        ui.state
            .get()
            .workspace
            .file_id_for_resource(&helper_resource)
            .is_none()
    );
    actions::begin(
        &ui.state,
        actions::Wanted::Goto(Feature::Definition),
        rect,
        None,
    );
    println!("CHECK: resource language definition opens target");
    wait(ui, |app| {
        app.workspace
            .active_document()
            .is_some_and(|id| app.workspace.documents[&id].meta.resource == helper_resource)
    });
    let helper_file = ui.state.get().workspace.active_document().unwrap();
    assert_eq!(
        ui.state.get().workspace.documents[&helper_file]
            .buffer
            .text(),
        helper_text
    );
    host.observe(&ui.state.get());
    wait(ui, |app| {
        services
            .results(
                &helper_resource,
                &app.workspace.documents[&helper_file].stamp(helper_file),
            )
            .diagnostics
            .is_some()
    });
    ui.state.update(|app| {
        app.workspace
            .document_editor_mut(helper_file)
            .unwrap()
            .set_cursor(helper_text.find("remote_target").unwrap() + 3)
    });
    actions::begin(&ui.state, actions::Wanted::References, rect, None);
    println!("CHECK: resource language references retain target identities");
    wait(ui, |app| {
        app.editor.language.popup.as_ref().is_some_and(|panel| {
            panel.kind == popup::Kind::References && panel.items.iter().any(|item|
            matches!(&item.payload, popup::Payload::Place(place) if place.resource == resource))
        })
    });
    navigate::back(&ui.state, rect);
    assert_eq!(ui.state.get().workspace.active_document(), Some(file));
    navigate::forward(&ui.state, rect);
    assert_eq!(
        ui.state.get().workspace.active_document(),
        Some(helper_file)
    );
    navigate::back(&ui.state, rect);
    assert_eq!(ui.state.get().workspace.active_document(), Some(file));
    ui.state.update(|app| {
        app.workspace.close(helper_file);
        app.editor.language.close_popups();
    });
    let at = good.rfind("value();").unwrap();
    println!("CHECK: resource rename reads unopened target and commits one undo group");
    host.observe(&ui.state.get());
    ui.state.update(|app| {
        app.workspace
            .document_editor_mut(file)
            .unwrap()
            .set_cursor(good.rfind("remote_target()").unwrap() + 3)
    });
    crate::app::language_ui::refactor::begin_rename(&ui.state, rect);
    wait(ui, |app| {
        app.editor
            .language
            .popup
            .as_ref()
            .is_some_and(|p| p.kind == popup::Kind::Rename)
    });
    ui.state.update(|app| {
        let panel = app.editor.language.popup.as_mut().unwrap();
        assert_eq!(panel.query.as_deref(), Some("remote_target"));
        panel.query = Some("renamed_target".into());
    });
    crate::app::language_ui::refactor::commit_rename(&ui.state, rect);
    wait(ui, |app| {
        app.workspace
            .file_id_for_resource(&helper_resource)
            .is_some_and(|helper| {
                app.workspace.documents[&helper]
                    .buffer
                    .text()
                    .contains("fn renamed_target")
                    && app.workspace.documents[&file]
                        .buffer
                        .text()
                        .contains("helper::renamed_target()")
            })
    });
    let renamed_helper = ui
        .state
        .get()
        .workspace
        .file_id_for_resource(&helper_resource)
        .unwrap();
    assert_eq!(ui.state.get().workspace.active_document(), Some(file));
    assert_eq!(read(ui, host, root.scope, &path), good.as_bytes());
    assert_eq!(read(ui, host, root.scope, &helper), helper_text.as_bytes());
    ui.state.update(|app| {
        assert_eq!(
            crate::app::language_ui::edit::undo_redo(app, true),
            Some(Ok(()))
        );
        assert_eq!(app.workspace.documents[&file].buffer.text(), good);
        assert_eq!(
            app.workspace.documents[&renamed_helper].buffer.text(),
            helper_text
        );
        assert_eq!(
            crate::app::language_ui::edit::undo_redo(app, false),
            Some(Ok(()))
        );
        assert!(
            app.workspace.documents[&renamed_helper]
                .buffer
                .text()
                .contains("fn renamed_target")
        );
        assert_eq!(
            crate::app::language_ui::edit::undo_redo(app, true),
            Some(Ok(()))
        );
        app.workspace.close(renamed_helper);
    });
    ui.state.update(|app| {
        let mut editor = app.workspace.document_editor_mut(file).unwrap();
        editor
            .replace(
                at..at + "value()".len(),
                "val",
                loog_editor_core::text::EditKind::Other,
            )
            .unwrap();
        editor.set_cursor(at + 3);
    });
    host.observe(&ui.state.get());
    ui.state
        .update(|app| crate::app::language_ui::completion::request(&ui.state, app, true, None));
    println!("CHECK: resource language completion");
    wait(ui, |app| {
        app.editor
            .language
            .completion
            .as_ref()
            .is_some_and(|session| {
                session.shown.iter().any(|shown| {
                    let entry = &session.data.entries[shown.entry];
                    entry.item.label.starts_with("value")
                        && entry.item.raw["x-loog"]["data"].is_string()
                })
            })
    });
    ui.state.update(|app| {
        let session = app.editor.language.completion.as_mut().unwrap();
        let selected = session
            .shown
            .iter()
            .position(|shown| {
                let entry = &session.data.entries[shown.entry];
                entry.item.label.starts_with("value")
                    && entry.item.raw["x-loog"]["data"].is_string()
            })
            .unwrap();
        session.set_selected(selected);
        crate::app::language_ui::completion::resolve_selected(&ui.state, app);
    });
    wait(ui, |app| {
        app.editor
            .language
            .completion
            .as_ref()
            .is_some_and(|session| {
                let entry = &session.data.entries[session.shown[session.selected].entry];
                entry
                    .item
                    .documentation
                    .as_ref()
                    .is_some_and(|text| text.contains("Remote completion documentation"))
            })
    });
    ui.state.update(|app| {
        assert!(crate::app::language_ui::completion::accept(app, None));
        assert!(
            app.workspace.documents[&file]
                .buffer
                .text()
                .contains("let result = value()")
        );
        assert_eq!(app.workspace.documents[&file].meta.resource, resource);
    });
    if formatter.is_some() {
        println!("CHECK: resource language formatting and undo");
        let before = ui.state.get().workspace.documents[&file]
            .buffer
            .text()
            .to_owned();
        host.observe(&ui.state.get());
        crate::app::language_ui::refactor::format(&ui.state, rect, false);
        wait(ui, |app| app.editor.language.refactor.is_none());
        let formatted = ui.state.get().workspace.documents[&file]
            .buffer
            .text()
            .to_owned();
        assert_ne!(formatted, before);
        assert!(formatted.contains("fn value() -> i32 {\n    7\n}"));
        assert_eq!(read(ui, host, root.scope, &path), good.as_bytes());
        ui.state.update(|app| {
            app.workspace.document_editor_mut(file).unwrap().undo();
            assert_eq!(app.workspace.documents[&file].buffer.text(), before);
            let language = app.workspace.documents[&file].meta.lang.name().to_owned();
            app.preferences
                .values_mut()
                .languages
                .entry(language)
                .or_default()
                .format_on_save = true;
        });
        println!("CHECK: resource language format before save");
        let _budget = crate::app::language_ui::save::test_step_budget(Duration::from_secs(10));
        let saved = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done = saved.clone();
        crate::app::language_ui::save::save(
            &ui.state,
            rect,
            Arc::new(move |state| {
                let done = done.clone();
                crate::app::saving::document(state, file, move |_, ok| {
                    assert!(ok);
                    done.store(true, std::sync::atomic::Ordering::Release);
                });
            }),
        );
        wait(ui, |_| saved.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(read(ui, host, root.scope, &path), formatted.as_bytes());
        ui.state.update(|app| {
            let language = app.workspace.documents[&file].meta.lang.name().to_owned();
            app.preferences
                .values_mut()
                .languages
                .get_mut(&language)
                .unwrap()
                .format_on_save = false;
        });
    }
    ui.state.update(|app| {
        app.workspace.close(file);
    });
    host.observe(&ui.state.get());
    request(
        ui,
        host,
        root.scope,
        Operation::Write {
            path: path.clone(),
            bytes: good.as_bytes().to_vec(),
            expected_revision: None,
        },
    );
    // Reopen the on-disk revision after closing an edited resource. A former
    // document lifecycle must not suppress or satisfy the new hover request.
    let Answer::Info { file: info } =
        request(ui, host, root.scope, Operation::Stat { path: path.clone() })
    else {
        panic!("file")
    };
    crate::app::resources::open(&ui.state, root.scope, info);
    println!("CHECK: resource language reopen");
    wait(ui, |app| {
        app.workspace
            .active_editor()
            .is_some_and(|editor| editor.text() == good)
    });
    let reopened = ui.state.get().workspace.active_document().unwrap();
    host.observe(&ui.state.get());
    // A cold language session has its own readiness; a hover request keeps its
    // ordinary deadline. Wait for this new buffer revision to reach the server.
    wait(ui, |app| {
        services
            .results(
                &resource,
                &app.workspace.documents[&reopened].stamp(reopened),
            )
            .diagnostics
            .is_some()
    });
    ui.state.update(|app| {
        app.workspace
            .document_editor_mut(reopened)
            .unwrap()
            .set_cursor(at + 2)
    });
    host.observe(&ui.state.get());
    crate::app::language_ui::hover::show_at_cursor(&ui.state);
    wait(ui, |app| {
        app.editor
            .language
            .hover
            .card_for(
                app.workspace.active_pane(),
                &app.workspace.documents[&reopened].stamp(reopened),
            )
            .is_some()
    });
    println!("CHECK: resource language service restart");
    let generation = host
        .inner
        .service
        .lock()
        .unwrap()
        .package(&id)
        .unwrap()
        .lifecycle
        .generation();
    host.restart(&ui.state, &id);
    assert!(
        host.language_services(Some(&resource))
            .iter()
            .all(|service| service.note.is_empty())
    );
    ui.state.update(|app| {
        let len = app.workspace.documents[&reopened].buffer.len();
        app.workspace
            .document_editor_mut(reopened)
            .unwrap()
            .replace(
                0..len,
                "fn restart_broken( {",
                loog_editor_core::text::EditKind::Other,
            )
            .unwrap();
    });
    host.observe(&ui.state.get());
    wait(ui, |app| {
        let current = host
            .inner
            .service
            .lock()
            .unwrap()
            .package(&id)
            .unwrap()
            .lifecycle
            .generation();
        current > generation
            && services
                .results(
                    &resource,
                    &app.workspace.documents[&reopened].stamp(reopened),
                )
                .diagnostics
                .as_ref()
                .is_some_and(|diagnostics| !diagnostics.is_empty())
    });
    ui.state.update(|app| {
        let len = app.workspace.documents[&reopened].buffer.len();
        app.workspace
            .document_editor_mut(reopened)
            .unwrap()
            .replace(0..len, good, loog_editor_core::text::EditKind::Other)
            .unwrap();
    });
    host.observe(&ui.state.get());
    wait(ui, |app| {
        services
            .results(
                &resource,
                &app.workspace.documents[&reopened].stamp(reopened),
            )
            .diagnostics
            .as_ref()
            .is_some_and(|diagnostics| diagnostics.is_empty())
    });
    println!("CHECK: resource language startup failure and recovery status");
    let mut broken_settings = settings.clone();
    broken_settings[format!("{id}.environmentProgram")] =
        serde_json::json!("/loog-acceptance-intentionally-missing-server");
    let effects =
        host.inner
            .service
            .lock()
            .unwrap()
            .set_setting_sources(broken_settings, None, false);
    ui.state.update(|app| host.inner.apply(app, effects));
    wait(ui, |_| {
        host.language_services(Some(&resource))
            .iter()
            .any(|service| {
                service.extension == id
                    && service.state == crate::features::language_services_menu::State::Failed
            })
    });
    assert!(
        host.language_services(Some(&foreign))
            .iter()
            .all(|service| service.note.is_empty())
    );
    let effects =
        host.inner
            .service
            .lock()
            .unwrap()
            .set_setting_sources(settings.clone(), None, false);
    ui.state.update(|app| host.inner.apply(app, effects));
    wait(ui, |_| {
        host.language_services(Some(&resource))
            .iter()
            .any(|service| service.extension == id && service.note == "Ready")
    });
    if let Ok(probe) = std::env::var("LOOG_REMOTE_EDIT_SERVER") {
        phase(|| {
            println!("CHECK: server applyEdit waits for native buffer receipts (protocol fixture)");
            println!("CHECK: target project initialization and configuration override user values");
            request(ui, host, root.scope, Operation::Write {
            path: config_file.clone(),
            bytes: serde_json::json!({
                format!("{id}.initialization"): serde_json::json!({"acceptanceSettings":"project-中文"}).to_string(),
                format!("{id}.configuration"): serde_json::json!({"acceptance":{"marker":"project-中文"}}).to_string()
            }).to_string().into_bytes(),
            expected_revision: None,
        });
            let mut probe_settings = settings.clone();
            probe_settings[format!("{id}.environmentProgram")] = serde_json::json!(probe);
            probe_settings[format!("{id}.configuration")] =
                serde_json::json!("{\"acceptance\":{\"marker\":\"wrong-user\"}}");
            let effects =
                host.inner
                    .service
                    .lock()
                    .unwrap()
                    .set_setting_sources(probe_settings, None, false);
            ui.state.update(|app| host.inner.apply(app, effects));
            wait(ui, |app| {
                services
                    .results(
                        &resource,
                        &app.workspace.documents[&reopened].stamp(reopened),
                    )
                    .diagnostics
                    .as_ref()
                    .is_some_and(|items| {
                        items
                            .iter()
                            .any(|d| d.message == "Both edit receipts verified")
                    })
            });
            let edited_helper = ui
                .state
                .get()
                .workspace
                .file_id_for_resource(&helper_resource)
                .unwrap();
            assert_eq!(
                ui.state.get().workspace.documents[&reopened].buffer.text(),
                format!("// server edit\n{good}")
            );
            assert_eq!(
                ui.state.get().workspace.documents[&edited_helper]
                    .buffer
                    .text(),
                format!("// helper edit\n{helper_text}")
            );
            assert_eq!(read(ui, host, root.scope, &path), good.as_bytes());
            assert_eq!(read(ui, host, root.scope, &helper), helper_text.as_bytes());
            ui.state.update(|app| {
                assert_eq!(
                    crate::app::language_ui::edit::undo_redo(app, true),
                    Some(Ok(()))
                );
                assert_eq!(app.workspace.documents[&reopened].buffer.text(), good);
                assert_eq!(
                    app.workspace.documents[&edited_helper].buffer.text(),
                    helper_text
                );
                app.workspace.close(edited_helper);
            });
            println!(
                "CHECK: resource capability activates native on-type formatting (protocol fixture)"
            );
            ui.state.update(|app| {
                let at = app.workspace.documents[&reopened]
                    .buffer
                    .text()
                    .find('\n')
                    .unwrap();
                {
                    let mut editor = app.workspace.document_editor_mut(reopened).unwrap();
                    editor.set_cursor(at);
                    loog_editor_core::normal::insert(&mut editor, ";");
                }
                crate::app::language_ui::refactor::on_type(&ui.state, app, ';');
            });
            wait(ui, |app| {
                app.workspace.documents[&reopened]
                    .buffer
                    .text()
                    .starts_with("// type formatting\n")
            });
            ui.state.update(|app| {
                app.workspace.document_editor_mut(reopened).unwrap().undo();
                app.workspace.document_editor_mut(reopened).unwrap().undo();
                assert_eq!(app.workspace.documents[&reopened].buffer.text(), good);
            });
            println!(
                "CHECK: native resource action resolve, unopened edits and command receipts (protocol fixture)"
            );
            let choose = |title: &str| {
                ui.state.update(|app| {
                    app.editor.language.popup = None;
                    app.chrome.toast = None;
                });
                crate::app::language_ui::refactor::code_actions(&ui.state, rect);
                wait(ui, |app| {
                    app.editor
                        .language
                        .popup
                        .as_ref()
                        .is_some_and(|p| p.items.iter().any(|i| i.label == title))
                });
                let mut selected = None;
                ui.state.update(|app| {
                    let panel = app.editor.language.popup.take().unwrap();
                    selected = panel.items.iter().find_map(|item| match &item.payload {
                        crate::app::language_ui::popup::Payload::Action { instance, action }
                            if action.title == title =>
                        {
                            Some((*instance, action.clone()))
                        }
                        _ => None,
                    });
                });
                let (instance, action) = selected.unwrap();
                assert!(action.needs_resolve);
                crate::app::language_ui::refactor::run_action(&ui.state, instance, action, rect);
            };
            choose("Reject invalid action atomically");
            wait(ui, |app| {
                app.chrome
                    .toast
                    .as_ref()
                    .is_some_and(|t| t.message.contains("outside its resource"))
            });
            assert_eq!(
                ui.state.get().workspace.documents[&reopened].buffer.text(),
                good
            );
            assert!(
                ui.state
                    .get()
                    .workspace
                    .file_id_for_resource(&helper_resource)
                    .is_none()
            );
            choose("Apply resource action and command");
            wait(ui, |app| {
                app.editor.language.refactor.is_none()
                    && services
                        .results(
                            &resource,
                            &app.workspace.documents[&reopened].stamp(reopened),
                        )
                        .diagnostics
                        .as_ref()
                        .is_some_and(|items| {
                            items
                                .iter()
                                .any(|d| d.message == "Action command receipt verified")
                        })
            });
            let action_helper = ui
                .state
                .get()
                .workspace
                .file_id_for_resource(&helper_resource)
                .unwrap();
            assert_eq!(
                ui.state.get().workspace.documents[&reopened].buffer.text(),
                format!("// command edit\n// action edit\n{good}")
            );
            assert_eq!(
                ui.state.get().workspace.documents[&action_helper]
                    .buffer
                    .text(),
                format!("// action helper\n{helper_text}")
            );
            assert_eq!(read(ui, host, root.scope, &path), good.as_bytes());
            assert_eq!(read(ui, host, root.scope, &helper), helper_text.as_bytes());
            ui.state.update(|app| {
                // The command edited only the source. Native Undo first falls back
                // to its ordinary buffer entry, then reaches the two-file group.
                assert_eq!(crate::app::language_ui::edit::undo_redo(app, true), None);
                app.workspace.document_editor_mut(reopened).unwrap().undo();
                assert_eq!(
                    crate::app::language_ui::edit::undo_redo(app, true),
                    Some(Ok(()))
                );
                assert_eq!(app.workspace.documents[&reopened].buffer.text(), good);
                assert_eq!(
                    app.workspace.documents[&action_helper].buffer.text(),
                    helper_text
                );
                app.workspace.close(action_helper);
            });
            choose("Fail resource command");
            wait(ui, |app| {
                app.editor.language.refactor.is_none()
                    && app
                        .chrome
                        .toast
                        .as_ref()
                        .is_some_and(|t| t.message.contains("fixture command failure"))
            });
            assert_eq!(
                ui.state.get().workspace.documents[&reopened].buffer.text(),
                good
            );
            println!(
                "CHECK: save action reads unopened resource and saves only requested document (protocol fixture)"
            );
            ui.state.update(|app| {
                let language = app.workspace.documents[&reopened]
                    .meta
                    .lang
                    .name()
                    .to_owned();
                app.preferences
                    .values_mut()
                    .languages
                    .entry(language)
                    .or_default()
                    .code_actions_on_save = vec!["source.fixAll".into()];
            });
            let _budget = crate::app::language_ui::save::test_step_budget(Duration::from_secs(10));
            let saved = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let done = saved.clone();
            crate::app::language_ui::save::save(
                &ui.state,
                rect,
                Arc::new(move |state| {
                    let done = done.clone();
                    crate::app::saving::document(state, reopened, move |_, ok| {
                        assert!(ok);
                        done.store(true, std::sync::atomic::Ordering::Release);
                    });
                }),
            );
            wait(ui, |_| saved.load(std::sync::atomic::Ordering::Acquire));
            let save_helper = ui
                .state
                .get()
                .workspace
                .file_id_for_resource(&helper_resource)
                .unwrap();
            assert_eq!(
                ui.state.get().workspace.documents[&reopened].buffer.text(),
                format!("// save action\n{good}")
            );
            assert_eq!(
                ui.state.get().workspace.documents[&save_helper]
                    .buffer
                    .text(),
                format!("// save helper\n{helper_text}")
            );
            assert_eq!(
                read(ui, host, root.scope, &path),
                format!("// save action\n{good}").as_bytes()
            );
            assert_eq!(read(ui, host, root.scope, &helper), helper_text.as_bytes());
            ui.state.update(|app| {
                let language = app.workspace.documents[&reopened]
                    .meta
                    .lang
                    .name()
                    .to_owned();
                app.preferences
                    .values_mut()
                    .languages
                    .get_mut(&language)
                    .unwrap()
                    .code_actions_on_save
                    .clear();
                assert_eq!(
                    crate::app::language_ui::edit::undo_redo(app, true),
                    Some(Ok(()))
                );
                assert_eq!(app.workspace.documents[&reopened].buffer.text(), good);
                assert_eq!(
                    app.workspace.documents[&save_helper].buffer.text(),
                    helper_text
                );
                app.workspace.close(save_helper);
            });
            saved.store(false, std::sync::atomic::Ordering::Release);
            let done = saved.clone();
            crate::app::saving::document(&ui.state, reopened, move |_, ok| {
                assert!(ok);
                done.store(true, std::sync::atomic::Ordering::Release);
            });
            wait(ui, |_| saved.load(std::sync::atomic::Ordering::Acquire));
            request(
                ui,
                host,
                root.scope,
                Operation::Write {
                    path: config_file.clone(),
                    bytes: project_settings.to_string().into_bytes(),
                    expected_revision: None,
                },
            );
            let effects = host
                .inner
                .service
                .lock()
                .unwrap()
                .set_setting_sources(settings, None, false);
            ui.state.update(|app| host.inner.apply(app, effects));
            wait(ui, |_| {
                host.language_services(Some(&resource))
                    .iter()
                    .any(|s| s.extension == id && s.note == "Ready")
            });
        });
    }
    ui.state.update(|app| {
        app.workspace.close(reopened);
    });
    host.observe(&ui.state.get());
    for path in [
        path,
        helper,
        marker,
        config_file,
        config_dir,
        source,
        project,
    ] {
        request(
            ui,
            host,
            root.scope,
            Operation::Remove {
                path,
                recursive: false,
            },
        );
    }
    println!(
        "PASS: official language WASM -> resource buffers -> SSH WASM -> rust-analyzer -> project discovery, native hover, unopened definition/preview/references/history, completion/resolve/insertion, optional formatting/undo/save, reopen, service restart and versioned diagnostics; local identity remains isolated"
    );
}

fn native_terminal(ui: &SavingUi, host: &ExtensionHost, root: &loog_workspace_api::PresentedRoot) {
    use crate::features::terminal::{
        ShellKind, TerminalSize, TerminalStatus, model::ProvidedTerminal, screen::Palette,
    };
    use crate::infrastructure::terminal::{
        notification::FrameNotifier, session::TerminalController,
    };
    use loog_workspace_api::{FailureKind, ResourceRef};
    let ResourceRef::Provided { path, .. } = &root.resource else {
        panic!("provided root")
    };
    ui.runtime
        .trust()
        .decide_resource(&root.resource, false)
        .unwrap();
    host.refresh_resource_grants();
    let denied = host
        .request_resource(
            root.scope,
            Operation::TerminalOpen {
                cwd: path.clone(),
                program: None,
                args: vec![],
                cols: 80,
                rows: 24,
            },
            Box::new(|_, _| panic!("untrusted terminal was submitted")),
        )
        .unwrap_err();
    assert_eq!(denied.kind, FailureKind::PermissionDenied);
    wait(ui, |app| {
        app.explorer
            .resources
            .root(root.scope)
            .unwrap()
            .watch
            .handle
            .is_some()
    });
    let watch = ui
        .state
        .get()
        .explorer
        .resources
        .root(root.scope)
        .unwrap()
        .watch
        .handle;
    ui.runtime
        .trust()
        .decide_resource(&root.resource, true)
        .unwrap();
    host.refresh_resource_grants();
    let controller = TerminalController::with_transport(
        root.name.clone(),
        crate::app::resource_terminal::transport(
            ui.state.clone(),
            ProvidedTerminal {
                root: root.resource.clone(),
                cwd: path.clone(),
                name: root.name.clone(),
            },
        ),
        true,
    );
    let palette = Palette {
        ansi: [0; 16],
        ansi_backgrounds: [None; 16],
        foreground: 0xffffff,
        background: 0,
    };
    let status = || controller.snapshot_with_palette(&palette).status;
    let size = TerminalSize {
        cols: 80,
        rows: 24,
        pixel_width: 0,
        pixel_height: 0,
    };
    let notifier = FrameNotifier::new(|| {});
    controller.ensure_started(None, size, notifier.clone());
    wait(ui, |_| {
        assert!(
            !matches!(status(), TerminalStatus::Failed(_)),
            "{:?}",
            status()
        );
        status() == TerminalStatus::Running
    });
    controller.write("stty -echo; printf '\\033c'; printf 'NATIVE中文\\n'; stty size\r".as_bytes());
    wait(ui, |_| {
        let output = controller.test_parser().screen().contents();
        output.contains("NATIVE中文") && output.contains("24 80")
    });
    controller.resize(TerminalSize {
        cols: 100,
        rows: 40,
        ..size
    });
    controller.write(b"stty size\r");
    wait(ui, |_| {
        controller
            .test_parser()
            .screen()
            .contents()
            .contains("40 100")
    });
    assert!(controller.working_directory().is_none());
    wait(ui, |_| {
        controller.reported_directory() == Some(Some(path.clone()))
    });
    let directory = child_path(
        ui,
        host,
        root.scope,
        path.clone(),
        &format!("native-terminal-中文-{}", std::process::id()),
    );
    request(
        ui,
        host,
        root.scope,
        Operation::CreateDirectory {
            path: directory.clone(),
        },
    );
    controller.write(format!("cd -- '{}'\r", directory.as_str().replace('\'', "'\\''")).as_bytes());
    wait(ui, |_| {
        controller.reported_directory() == Some(Some(directory.clone()))
    });
    controller.write(b"cd /\r");
    wait(ui, |_| controller.reported_directory() == Some(None));
    controller.write(format!("cd -- '{}'\r", path.as_str().replace('\'', "'\\''")).as_bytes());
    wait(ui, |_| {
        controller.reported_directory() == Some(Some(path.clone()))
    });
    request(
        ui,
        host,
        root.scope,
        Operation::Remove {
            path: directory,
            recursive: false,
        },
    );
    controller.write(b"exit 0\r");
    wait(ui, |_| status() == TerminalStatus::Exited(0));
    controller.restart(ShellKind::System, None, size, notifier.clone());
    wait(ui, |_| status() == TerminalStatus::Running);
    ui.runtime
        .trust()
        .decide_resource(&root.resource, false)
        .unwrap();
    host.refresh_resource_grants();
    wait(ui, |_| status() == TerminalStatus::Stopped);
    assert_eq!(
        ui.state
            .get()
            .explorer
            .resources
            .root(root.scope)
            .unwrap()
            .watch
            .handle,
        watch
    );
    assert!(matches!(
        request(ui, host, root.scope, Operation::Stat { path: path.clone() }),
        Answer::Info { .. }
    ));
    controller.terminate(notifier);
    ui.tasks.run_all();
    assert!(host.inner.resource_observers.lock().unwrap().is_empty());
}
