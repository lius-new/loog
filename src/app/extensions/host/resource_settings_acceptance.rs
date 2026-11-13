//! Real target-side configuration changes, consumed by the language component.
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
    println!("CHECK: language plugin watches target project settings without a manual reload");
    let file = ui
        .state
        .get()
        .workspace
        .file_id_for_resource(resource)
        .unwrap();
    let saved = ui.state.get().workspace.documents[&file]
        .buffer
        .text()
        .to_owned();
    let services = ui.runtime.language_services().unwrap();
    let directory = child_path(ui, host, root.scope, project.clone(), ".loog");
    request(
        ui,
        host,
        root.scope,
        Operation::CreateDirectory {
            path: directory.clone(),
        },
    );
    let configuration = child_path(
        ui,
        host,
        root.scope,
        directory.clone(),
        "extension-settings.json",
    );
    let config = serde_json::json!({
        format!("{language}.configuration"): serde_json::json!({"json":{"validate":{"enable":true},"schemas":[{
            "fileMatch":["*/settings.json"],"schema":{"type":"object", "required":["remoteSetting"],
            "properties":{"remoteSetting":{"type":"string", "description":"Live target configuration"}}}
        }]}}).to_string()
    });
    request(
        ui,
        host,
        root.scope,
        Operation::Write {
            path: configuration.clone(),
            bytes: config.to_string().into_bytes(),
            expected_revision: None,
        },
    );
    wait(ui, |app| {
        services
            .results(resource, &app.workspace.documents[&file].stamp(file))
            .diagnostics
            .as_ref()
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.message.contains("remoteSetting"))
            })
    });
    let report_key = || {
        let service = host.inner.service.lock().unwrap();
        let owner = loog_workspace_api::Owner {
            id: language.into(),
            generation: service.packages[language].lifecycle.generation(),
        };
        service
            .resource_documents
            .reports(&owner, Some(resource))
            .iter()
            .find(|report| report.state == loog_workspace_api::session::ServiceState::Ready)
            .map(|report| report.key.clone())
    };
    wait(ui, |_| report_key().is_some());
    let configured = report_key().unwrap();
    ui.state.update(|app| {
        let length = app.workspace.documents[&file].buffer.len();
        let mut editor = app.workspace.document_editor_mut(file).unwrap();
        editor
            .replace(
                0..length,
                "{\n  \n}",
                loog_editor_core::text::EditKind::Other,
            )
            .unwrap();
        editor.set_cursor(4);
    });
    host.observe(&ui.state.get());
    wait(ui, |app| {
        services
            .results(resource, &app.workspace.documents[&file].stamp(file))
            .diagnostics
            .as_ref()
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.message.contains("remoteSetting"))
            })
    });
    wait(ui, |_| report_key().is_some());
    ui.state
        .update(|app| crate::app::language_ui::completion::request(&ui.state, app, true, None));
    wait(ui, |app| {
        app.editor
            .language
            .completion
            .as_ref()
            .is_some_and(|session| {
                session.shown.iter().any(|shown| {
                    session.data.entries[shown.entry]
                        .item
                        .label
                        .contains("remoteSetting")
                })
            })
    });
    ui.state.update(|app| app.editor.language.completion = None);
    assert_eq!(read(ui, host, root.scope, path), saved.as_bytes());
    request(
        ui,
        host,
        root.scope,
        Operation::Remove {
            path: configuration,
            recursive: false,
        },
    );
    wait(ui, |_| report_key().is_some_and(|key| key != configured));
    ui.state
        .update(|app| crate::app::language_ui::completion::request(&ui.state, app, true, None));
    wait(ui, |app| {
        app.editor
            .language
            .completion
            .as_ref()
            .is_some_and(|session| {
                session.shown.iter().any(|shown| {
                    session.data.entries[shown.entry]
                        .item
                        .label
                        .contains("enabled")
                })
            })
    });
    ui.state.update(|app| {
        app.editor.language.completion = None;
        assert_eq!(app.workspace.documents[&file].buffer.text(), "{\n  \n}");
        assert!(app.workspace.is_dirty(file));
        app.workspace.document_editor_mut(file).unwrap().undo();
        assert_eq!(app.workspace.documents[&file].buffer.text(), saved);
        assert!(!app.workspace.is_dirty(file));
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
    host.observe(&ui.state.get());
    wait(ui, |app| {
        services
            .results(resource, &app.workspace.documents[&file].stamp(file))
            .diagnostics
            .as_ref()
            .is_some_and(|items| items.is_empty())
    });
    println!(
        "PASS: target settings create/delete update real JSON diagnostics and completion, preserving dirty text and undo"
    );
}
