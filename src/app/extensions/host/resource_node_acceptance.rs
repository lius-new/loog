//! Optional real Node language component acceptance, without a visible window.
use super::*;
#[path = "resource_projects_acceptance.rs"]
mod multiple_projects;
#[path = "resource_settings_acceptance.rs"]
mod settings_watch;

pub(super) fn run(
    ui: &SavingUi,
    host: &ExtensionHost,
    root: &loog_workspace_api::PresentedRoot,
    package: &Path,
) {
    let program = std::env::var("LOOG_REMOTE_NODE_PROGRAM").unwrap();
    let managed = program == "@bundled";
    let args = if managed {
        serde_json::json!(["--stdio"])
    } else {
        serde_json::json!([std::env::var("LOOG_REMOTE_NODE_SERVER").unwrap(), "--stdio"])
    };
    if let Ok(runtime) = std::env::var("LOOG_REMOTE_RUNTIME_PACKAGE") {
        host.load_package(Path::new(&runtime)).unwrap();
    }
    let id = host.load_package(package).unwrap();
    let loog_workspace_api::ResourceRef::Provided {
        path: root_path,
        environment,
    } = &root.resource
    else {
        panic!("provided root")
    };
    let project = child_path(
        ui,
        host,
        root.scope,
        root_path.clone(),
        &format!("native-node-project-{}", std::process::id()),
    );
    request(
        ui,
        host,
        root.scope,
        Operation::CreateDirectory {
            path: project.clone(),
        },
    );
    let marker = child_path(ui, host, root.scope, project.clone(), "package.json");
    request(
        ui,
        host,
        root.scope,
        Operation::Write {
            path: marker.clone(),
            bytes: b"{}".to_vec(),
            expected_revision: None,
        },
    );
    let path = child_path(ui, host, root.scope, project.clone(), "settings.json");
    let good = "{\"message\":\"中文😀\",\"enabled\":true}";
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
    let config = serde_json::json!({"json":{"validate":{"enable":true},"schemas":[{
        "fileMatch":["*/settings.json"],"schema":{"type":"object","properties":{
            "message":{"type":"string","description":"Environment message"},
            "enabled":{"type":"boolean","description":"Environment flag"}}}
    }]}});
    let mut settings = serde_json::json!({
        format!("{id}.environmentProgram"):program,
        format!("{id}.environmentArguments"):args.to_string(),
        format!("{id}.configuration"):config.to_string()
    });
    if managed {
        settings[format!("{id}.environmentCache")] = serde_json::json!(
            std::env::var("LOOG_REMOTE_TOOL_CACHE")
                .expect("managed acceptance requires an isolated target cache")
        );
    }
    let effects = host
        .inner
        .service
        .lock()
        .unwrap()
        .set_setting_sources(settings, None, false);
    ui.state.update(|app| host.inner.apply(app, effects));
    ui.runtime
        .trust()
        .decide_resource(&root.resource, true)
        .unwrap();
    host.refresh_resource_grants();
    if ui.runtime.language_services().is_none() {
        ui.runtime
            .install_language_services(crate::app::language_services::LanguageServices::new());
    }
    let Answer::Info { file: info } =
        request(ui, host, root.scope, Operation::Stat { path: path.clone() })
    else {
        panic!("file")
    };
    crate::app::resources::open(&ui.state, root.scope, info);
    let resource =
        loog_workspace_api::ResourceRef::provided(environment.clone(), path.as_str()).unwrap();
    wait(ui, |app| {
        app.workspace.file_id_for_resource(&resource).is_some()
    });
    let file = ui
        .state
        .get()
        .workspace
        .file_id_for_resource(&resource)
        .unwrap();
    host.observe(&ui.state.get());
    wait_for(ui, Duration::from_secs(120), |_| {
        host.inner.service.lock().unwrap().packages[&id]
            .lifecycle
            .state()
            == super::super::super::State::Active
    });
    let services = ui.runtime.language_services().unwrap();
    let replace = |text: &str| {
        ui.state.update(|app| {
            let len = app.workspace.documents[&file].buffer.len();
            app.workspace
                .document_editor_mut(file)
                .unwrap()
                .replace(0..len, text, loog_editor_core::text::EditKind::Other)
                .unwrap();
        });
        host.observe(&ui.state.get());
    };
    println!("CHECK: Node JSON language server diagnostics bind to the current resource revision");
    replace("{\"message\":\"中文😀\",\"enabled\": }");
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
                .as_ref()
                .is_some_and(|items| !items.is_empty())
        },
    );
    let foreign = loog_workspace_api::ResourceRef::provided(
        loog_workspace_api::EnvironmentKey::new("example.other", "same-path").unwrap(),
        path.as_str(),
    )
    .unwrap();
    assert!(
        services
            .results(
                &foreign,
                &ui.state.get().workspace.documents[&file].stamp(file)
            )
            .diagnostics
            .is_none()
    );
    replace(good);
    wait(ui, |app| {
        services
            .results(&resource, &app.workspace.documents[&file].stamp(file))
            .diagnostics
            .as_ref()
            .is_some_and(|items| items.is_empty())
    });
    println!("CHECK: Node JSON schema completion from environment session settings");
    replace("{\n  \n}");
    ui.state.update(|app| {
        app.workspace
            .document_editor_mut(file)
            .unwrap()
            .set_cursor(4)
    });
    wait(ui, |_| {
        host.language_services(Some(&resource))
            .iter()
            .any(|service| service.extension == id && service.note == "Ready")
    });
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
    });
    replace(good);
    wait(ui, |app| {
        services
            .results(&resource, &app.workspace.documents[&file].stamp(file))
            .diagnostics
            .as_ref()
            .is_some_and(|items| items.is_empty())
    });
    println!("CHECK: Node JSON native formatting, undo and resource persistence");
    crate::app::language_ui::refactor::format(
        &ui.state,
        lgui::prelude::UiRect::new(0.0, 0.0, 800.0, 600.0),
        false,
    );
    wait(ui, |app| app.editor.language.refactor.is_none());
    let formatted = ui.state.get().workspace.documents[&file]
        .buffer
        .text()
        .to_owned();
    assert_ne!(formatted, good);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&formatted).unwrap(),
        serde_json::from_str::<serde_json::Value>(good).unwrap()
    );
    assert_eq!(read(ui, host, root.scope, &path), good.as_bytes());
    ui.state.update(|app| {
        app.workspace.document_editor_mut(file).unwrap().undo();
    });
    assert_eq!(
        ui.state.get().workspace.documents[&file].buffer.text(),
        good
    );
    replace(&formatted);
    let saved = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done = saved.clone();
    crate::app::saving::document(&ui.state, file, move |_, ok| {
        assert!(ok);
        done.store(true, std::sync::atomic::Ordering::Release);
    });
    wait(ui, |_| saved.load(std::sync::atomic::Ordering::Acquire));
    assert_eq!(read(ui, host, root.scope, &path), formatted.as_bytes());
    multiple_environments::run(ui, host, root, &resource, &path, &id);
    multiple_projects::run(ui, host, root, &project, &resource, &path, &id);
    println!("CHECK: target JSONC URI allows comments and reports trailing commas as warnings");
    let comments_path = child_path(ui, host, root.scope, project.clone(), "settings.jsonc");
    request(
        ui,
        host,
        root.scope,
        Operation::Write {
            path: comments_path.clone(),
            bytes: "// 中文 comment\n{\"enabled\":true,}\n".as_bytes().to_vec(),
            expected_revision: None,
        },
    );
    let Answer::Info { file: info } = request(
        ui,
        host,
        root.scope,
        Operation::Stat {
            path: comments_path.clone(),
        },
    ) else {
        panic!("jsonc")
    };
    crate::app::resources::open(&ui.state, root.scope, info);
    let comments_resource =
        loog_workspace_api::ResourceRef::provided(environment.clone(), comments_path.as_str())
            .unwrap();
    wait(ui, |app| {
        app.workspace
            .file_id_for_resource(&comments_resource)
            .is_some()
    });
    let comments_file = ui
        .state
        .get()
        .workspace
        .file_id_for_resource(&comments_resource)
        .unwrap();
    host.observe(&ui.state.get());
    wait(ui, |app| {
        services
            .results(
                &comments_resource,
                &app.workspace.documents[&comments_file].stamp(comments_file),
            )
            .diagnostics
            .as_ref()
            .is_some_and(|items| {
                items.len() == 1
                    && items[0].message.contains("Trailing comma")
                    && items[0].severity
                        == crate::features::language_service::diagnostics::Severity::Warning
            })
    });
    ui.state.update(|app| {
        app.workspace.close(comments_file);
        app.workspace.close(file);
    });
    host.observe(&ui.state.get());
    for path in [comments_path, path, marker, project] {
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
        "PASS: JSON language WASM -> resource processes -> Linux Node -> diagnostics, schema completion, native formatting, undo and save"
    );
}
