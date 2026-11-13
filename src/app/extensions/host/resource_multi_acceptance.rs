//! Two real provider sessions, including identical target paths and different
//! unsaved buffers. The optional second profile must expose the same test files.
use super::*;

pub(super) fn run(
    ui: &SavingUi,
    host: &ExtensionHost,
    first: &loog_workspace_api::PresentedRoot,
    resource: &loog_workspace_api::ResourceRef,
    path: &ProviderPath,
    language: &str,
) {
    let Ok(profile) = std::env::var("LOOG_REMOTE_SECOND_PROFILE") else {
        return;
    };
    let profile: serde_json::Value = serde_json::from_str(&profile).unwrap();
    println!(
        "CHECK: two SSH environments retain identical paths and independent language contexts"
    );
    host.run_command(&ui.state, "loog.remote.ssh.connect");
    wait(ui, |app| {
        origin(&app.extension_widgets.roots(), "show-1").is_some()
    });
    for field in ["target", "root", "config"] {
        event(
            ui,
            host,
            field,
            Payload::Input(profile[field].as_str().unwrap_or("").into()),
        );
    }
    event(
        ui,
        host,
        "port",
        Payload::Input(profile["port"].as_u64().unwrap_or(22).to_string()),
    );
    event(ui, host, "connect", Payload::Click);
    wait(ui, |app| app.explorer.resources.roots.len() == 2);
    let second = ui
        .state
        .get()
        .explorer
        .resources
        .roots
        .iter()
        .find(|root| root.descriptor.scope != first.scope)
        .unwrap()
        .descriptor
        .clone();
    assert_ne!(first.resource.environment(), second.resource.environment());
    let loog_workspace_api::ResourceRef::Provided {
        path: first_path, ..
    } = &first.resource
    else {
        panic!("first root")
    };
    let loog_workspace_api::ResourceRef::Provided {
        path: second_path, ..
    } = &second.resource
    else {
        panic!("second root")
    };
    assert_eq!(first_path, second_path);
    ui.runtime
        .trust()
        .decide_resource(&second.resource, true)
        .unwrap();
    host.refresh_resource_grants();
    let Answer::Info { file: info } = request(
        ui,
        host,
        second.scope,
        Operation::Stat { path: path.clone() },
    ) else {
        panic!("second file")
    };
    crate::app::resources::open(&ui.state, second.scope, info);
    let other = loog_workspace_api::ResourceRef::provided(
        second.resource.environment().unwrap().clone(),
        path.as_str(),
    )
    .unwrap();
    wait(ui, |app| {
        app.workspace.file_id_for_resource(&other).is_some()
    });
    let left = ui
        .state
        .get()
        .workspace
        .file_id_for_resource(resource)
        .unwrap();
    let right = ui
        .state
        .get()
        .workspace
        .file_id_for_resource(&other)
        .unwrap();
    assert_ne!(left, right);
    let saved = ui.state.get().workspace.documents[&left]
        .buffer
        .text()
        .to_owned();
    let replace = |file, text: &str| {
        ui.state.update(|app| {
            let len = app.workspace.documents[&file].buffer.len();
            app.workspace
                .document_editor_mut(file)
                .unwrap()
                .replace(0..len, text, loog_editor_core::text::EditKind::Other)
                .unwrap();
        });
    };
    replace(left, "{\"message\": }");
    replace(right, "{\"message\":\"环境二😀\",\"enabled\":false}");
    ui.state
        .update(|app| app.search.ui.query.set_text("环境二😀"));
    let searched = super::resource_search::search(ui);
    assert_eq!(searched.errors, 0, "{:?}", searched.first_error);
    assert_eq!(searched.matches, 1);
    assert_eq!(searched.files[0].path, other);
    crate::app::search::clear(&ui.state);
    host.observe(&ui.state.get());
    let services = ui.runtime.language_services().unwrap();
    wait(ui, |app| {
        services
            .results(resource, &app.workspace.documents[&left].stamp(left))
            .diagnostics
            .as_ref()
            .is_some_and(|items| !items.is_empty())
            && services
                .results(&other, &app.workspace.documents[&right].stamp(right))
                .diagnostics
                .as_ref()
                .is_some_and(|items| items.is_empty())
    });
    wait(ui, |_| {
        [resource, &other].iter().all(|current| {
            host.language_services(Some(current))
                .iter()
                .any(|service| service.extension == language && service.note == "Ready")
        })
    });
    assert_eq!(read(ui, host, first.scope, path), saved.as_bytes());
    assert_eq!(read(ui, host, second.scope, path), saved.as_bytes());
    let texts = [
        "{\"message\":\"环境一\",\n  \n}",
        "{\"enabled\":false,\n  \n}",
    ];
    for (file, text) in [(left, texts[0]), (right, texts[1])] {
        replace(file, text);
        ui.state.update(|app| {
            app.workspace
                .document_editor_mut(file)
                .unwrap()
                .set_cursor(text.len() - 2)
        });
    }
    host.observe(&ui.state.get());
    wait(ui, |app| {
        [(resource, left), (&other, right)]
            .iter()
            .all(|(resource, file)| {
                services
                    .results(*resource, &app.workspace.documents[file].stamp(*file))
                    .diagnostics
                    .is_some()
            })
    });
    wait(ui, |_| {
        [resource, &other].iter().all(|current| {
            host.language_services(Some(current))
                .iter()
                .any(|service| service.extension == language && service.note == "Ready")
        })
    });
    // Switch while the first request is in flight. Its late answer must not
    // populate the second buffer's native completion session.
    for file in [left, right] {
        // Tab activation completes its normal observation cycle before the
        // next user command. Otherwise observation deliberately closes the
        // popup created inside the same artificial activation mutation.
        ui.state.update(|app| app.workspace.activate_document(file));
        ui.state.update(|app| {
            crate::app::language_ui::completion::request(&ui.state, app, true, None);
        });
    }
    wait(ui, |app| {
        app.editor
            .language
            .completion
            .as_ref()
            .is_some_and(|session| {
                session.document == right
                    && session.shown.iter().any(|shown| {
                        session.data.entries[shown.entry]
                            .item
                            .label
                            .contains("message")
                    })
            })
    });
    ui.state.update(|app| {
        app.editor.language.completion = None;
        app.workspace.activate_document(left);
    });
    ui.state.update(|app| {
        crate::app::language_ui::completion::request(&ui.state, app, true, None);
    });
    wait(ui, |app| {
        app.editor
            .language
            .completion
            .as_ref()
            .is_some_and(|session| {
                session.document == left
                    && session.shown.iter().any(|shown| {
                        session.data.entries[shown.entry]
                            .item
                            .label
                            .contains("enabled")
                    })
            })
    });
    ui.state.update(|app| {
        app.editor.language.completion = None;
        app.workspace.close(right);
    });
    replace(left, &saved);
    host.observe(&ui.state.get());
    event(ui, host, "disconnect", Payload::Click);
    wait(ui, |app| {
        app.explorer.resources.roots.len() == 1
            && app.explorer.resources.roots[0].descriptor.scope == first.scope
    });
    wait(ui, |app| {
        services
            .results(resource, &app.workspace.documents[&left].stamp(left))
            .diagnostics
            .as_ref()
            .is_some_and(|items| items.is_empty())
    });
    assert_eq!(read(ui, host, first.scope, path), saved.as_bytes());
    event(ui, host, "show", Payload::Click);
    wait(ui, |app| {
        origin(&app.extension_widgets.roots(), "connect").is_some()
    });
    println!(
        "PASS: two SSH sessions, same target paths, distinct buffers/diagnostics/completion contexts, independent disconnect"
    );
}
