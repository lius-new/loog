//! Real simultaneous project processes and restart at the four-process limit.
use super::*;

pub(super) fn run(
    ui: &SavingUi,
    host: &ExtensionHost,
    root: &loog_workspace_api::PresentedRoot,
    project: &ProviderPath,
    resource: &loog_workspace_api::ResourceRef,
    path: &ProviderPath,
    language: &str,
) {
    println!("CHECK: four target projects restart at the process limit without losing documents");
    let loog_workspace_api::ResourceRef::Provided {
        environment,
        path: root_path,
    } = &root.resource
    else {
        panic!("provided root")
    };
    let mut projects = Vec::new();
    for index in 0..3 {
        let directory = child_path(
            ui,
            host,
            root.scope,
            root_path.clone(),
            &format!("native-capacity-{}-{index}", std::process::id()),
        );
        request(
            ui,
            host,
            root.scope,
            Operation::CreateDirectory {
                path: directory.clone(),
            },
        );
        let marker = child_path(ui, host, root.scope, directory.clone(), "package.json");
        let document = child_path(ui, host, root.scope, directory.clone(), "settings.json");
        for (target, bytes) in [
            (&marker, b"{}".as_slice()),
            (&document, b"{\"enabled\":true}".as_slice()),
        ] {
            request(
                ui,
                host,
                root.scope,
                Operation::Write {
                    path: target.clone(),
                    bytes: bytes.to_vec(),
                    expected_revision: None,
                },
            );
        }
        let identity =
            loog_workspace_api::ResourceRef::provided(environment.clone(), document.as_str())
                .unwrap();
        projects.push((directory, marker, document, identity));
    }
    // Finish all disk setup before opening documents: marker notifications are
    // allowed to invalidate discovery, and should not obscure this capacity test.
    for (_, _, document, identity) in &projects {
        let Answer::Info { file } = request(
            ui,
            host,
            root.scope,
            Operation::Stat {
                path: document.clone(),
            },
        ) else {
            panic!("project document")
        };
        crate::app::resources::open(&ui.state, root.scope, file);
        wait(ui, |app| {
            app.workspace.file_id_for_resource(identity).is_some()
        });
        ui.state.update(|app| {
            app.workspace.promote_active_preview();
        });
    }
    host.observe(&ui.state.get());
    let identities = std::iter::once(resource)
        .chain(projects.iter().map(|entry| &entry.3))
        .collect::<Vec<_>>();
    let ready = || {
        let service = host.inner.service.lock().unwrap();
        let owner = loog_workspace_api::Owner {
            id: language.into(),
            generation: service.packages[language].lifecycle.generation(),
        };
        identities
            .iter()
            .map(|identity| {
                service
                    .resource_documents
                    .reports(&owner, Some(identity))
                    .iter()
                    .find(|report| report.state == loog_workspace_api::session::ServiceState::Ready)
                    .map(|report| report.key.clone())
            })
            .collect::<Option<Vec<_>>>()
    };
    wait(ui, |_| {
        ready()
            .is_some_and(|keys| keys.iter().collect::<std::collections::BTreeSet<_>>().len() == 4)
    });
    let before = ready().unwrap();
    let edited = ui
        .state
        .get()
        .workspace
        .file_id_for_resource(&projects[0].3)
        .unwrap();
    ui.state.update(|app| {
        let length = app.workspace.documents[&edited].buffer.len();
        app.workspace
            .document_editor_mut(edited)
            .unwrap()
            .replace(0..length, "{}", loog_editor_core::text::EditKind::Other)
            .unwrap();
    });
    host.observe(&ui.state.get());
    let Answer::Info { file } =
        request(ui, host, root.scope, Operation::Stat { path: path.clone() })
    else {
        panic!("original project document")
    };
    crate::app::resources::open(&ui.state, root.scope, file);
    let original = ui
        .state
        .get()
        .workspace
        .file_id_for_resource(resource)
        .unwrap();
    wait(ui, |app| app.workspace.active_document() == Some(original));
    settings_watch::run(ui, host, root, project, resource, path, language);
    wait(ui, |_| {
        ready().is_some_and(|keys| {
            keys.iter().zip(&before).all(|(new, old)| new != old)
                && keys.iter().collect::<std::collections::BTreeSet<_>>().len() == 4
        })
    });
    let services = ui.runtime.language_services().unwrap();
    for (index, (_, _, _, identity)) in projects.iter().enumerate() {
        let file = ui
            .state
            .get()
            .workspace
            .file_id_for_resource(identity)
            .unwrap();
        wait(ui, |app| {
            services
                .results(identity, &app.workspace.documents[&file].stamp(file))
                .diagnostics
                .as_ref()
                .is_some_and(|items| {
                    if index == 0 {
                        items.iter().any(|item| item.message.contains("enabled"))
                            && items
                                .iter()
                                .all(|item| !item.message.contains("remoteSetting"))
                    } else {
                        items.is_empty()
                    }
                })
        });
    }
    ui.state.update(|app| {
        assert!(app.workspace.is_dirty(edited));
        assert_eq!(app.workspace.documents[&edited].buffer.text(), "{}");
        app.workspace.document_editor_mut(edited).unwrap().undo();
        assert_eq!(
            app.workspace.documents[&edited].buffer.text(),
            "{\"enabled\":true}"
        );
        for (_, _, _, identity) in &projects {
            app.workspace
                .close(app.workspace.file_id_for_resource(identity).unwrap());
        }
    });
    host.observe(&ui.state.get());
    for (directory, marker, document, _) in projects {
        for path in [document, marker, directory] {
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
    }
    println!(
        "PASS: four real Node processes restart, preserving project configuration, dirty text and undo"
    );
}
