//! Target path and EditorConfig acceptance through the native file consumer.
use super::*;

pub(super) fn run(ui: &SavingUi, host: &ExtensionHost, root: &loog_workspace_api::PresentedRoot) {
    let loog_workspace_api::ResourceRef::Provided {
        path: base,
        environment,
    } = &root.resource
    else {
        panic!("provided root")
    };
    let scope = root.scope;
    let project = child_path(
        ui,
        host,
        scope,
        base.clone(),
        &format!("native-options-{}", std::process::id()),
    );
    request(
        ui,
        host,
        scope,
        Operation::CreateDirectory {
            path: project.clone(),
        },
    );
    let directory = child_path(ui, host, scope, project.clone(), "src");
    request(
        ui,
        host,
        scope,
        Operation::CreateDirectory {
            path: directory.clone(),
        },
    );
    let parent_config = child_path(ui, host, scope, project.clone(), ".editorconfig");
    let child_config = child_path(ui, host, scope, directory.clone(), ".editorconfig");
    let name = "name-中文\\file.txt";
    let path = child_path(ui, host, scope, directory.clone(), name);
    for (path, text) in [
        (
            &parent_config,
            "\u{feff}root=true\n[*]\nindent_style=space\nindent_size=4\nend_of_line=crlf\n[src/name-中文\\\\file.txt]\ntab_width=7\n",
        ),
        (&child_config, "[*.txt]\nindent_size=2\n"),
        (&path, "one\n"),
    ] {
        request(
            ui,
            host,
            scope,
            Operation::Write {
                path: path.clone(),
                bytes: text.as_bytes().to_vec(),
                expected_revision: None,
            },
        );
    }
    let Answer::Path { path: resolved } = request(
        ui,
        host,
        scope,
        Operation::ResolvePath {
            base: directory.clone(),
            relative: format!("../src/{name}"),
        },
    ) else {
        panic!("resolved target path")
    };
    assert_eq!(resolved, path);
    let Answer::RelativePath { components } = request(
        ui,
        host,
        scope,
        Operation::RelativePath {
            base: project.clone(),
            path: path.clone(),
        },
    ) else {
        panic!("relative target path")
    };
    assert_eq!(components, Some(vec!["src".into(), name.into()]));
    let Answer::Info { file } = request(ui, host, scope, Operation::Stat { path: path.clone() })
    else {
        panic!("file")
    };
    crate::app::resources::open(&ui.state, scope, file);
    let resource =
        loog_workspace_api::ResourceRef::provided(environment.clone(), path.as_str()).unwrap();
    wait(ui, |app| {
        app.workspace.file_id_for_resource(&resource).is_some()
    });
    ui.state.update(|app| {
        let id = app.workspace.file_id_for_resource(&resource).unwrap();
        let buffer = &app.workspace.documents[&id].buffer;
        let options = buffer.file_options();
        assert_eq!(options.indent_size, Some(2));
        assert_eq!(options.tab_width, Some(7));
        assert_eq!(options.use_tabs, Some(false));
        assert_eq!(
            options.line_ending,
            Some(loog_editor_core::text::LineEnding::Crlf)
        );
        assert_eq!(buffer.text().to_string(), "one\n");
        assert!(!app.workspace.is_dirty(id));
        app.editor.editing.manual.insert(
            id,
            crate::features::settings::editing::EditorOverrides {
                indent_size: Some(6),
                ..Default::default()
            },
        );
        app.workspace
            .document_editor_mut(id)
            .unwrap()
            .replace(0..3, "unsaved", loog_editor_core::text::EditKind::Other)
            .unwrap();
    });
    request(
        ui,
        host,
        scope,
        Operation::Write {
            path: child_config.clone(),
            bytes: b"[*.txt]\nindent_size=3\n".to_vec(),
            expected_revision: None,
        },
    );
    wait(ui, |app| {
        app.workspace
            .file_id_for_resource(&resource)
            .is_some_and(|id| {
                app.workspace.documents[&id]
                    .buffer
                    .file_options()
                    .indent_size
                    == Some(3)
            })
    });
    ui.state.update(|app| {
        let id = app.workspace.file_id_for_resource(&resource).unwrap();
        assert_eq!(crate::app::editing::effective(app, id).indent_size, 6);
        assert_eq!(app.workspace.documents[&id].buffer.text(), "unsaved\n");
        assert!(app.workspace.is_dirty(id));
        app.workspace.document_editor_mut(id).unwrap().undo();
        assert_eq!(app.workspace.documents[&id].buffer.text(), "one\n");
        assert!(!app.workspace.is_dirty(id));
    });
    request(
        ui,
        host,
        scope,
        Operation::Remove {
            path: child_config.clone(),
            recursive: false,
        },
    );
    wait(ui, |app| {
        app.workspace
            .file_id_for_resource(&resource)
            .is_some_and(|id| {
                app.workspace.documents[&id]
                    .buffer
                    .file_options()
                    .indent_size
                    == Some(4)
            })
    });
    request(
        ui,
        host,
        scope,
        Operation::Write {
            path: child_config.clone(),
            bytes: b"[*.txt]\nindent_size=5\n".to_vec(),
            expected_revision: None,
        },
    );
    wait(ui, |app| {
        app.workspace
            .file_id_for_resource(&resource)
            .is_some_and(|id| {
                app.workspace.documents[&id]
                    .buffer
                    .file_options()
                    .indent_size
                    == Some(5)
            })
    });
    ui.state.update(|app| {
        let id = app.workspace.file_id_for_resource(&resource).unwrap();
        app.workspace.close(id);
    });
    assert_eq!(read(ui, host, scope, &path), b"one\n");
    for path in [path, child_config, parent_config, directory, project] {
        request(
            ui,
            host,
            scope,
            Operation::Remove {
                path,
                recursive: false,
            },
        );
    }
    println!(
        "CHECK: native EditorConfig uses target names and watches change/delete/create while preserving dirty buffers, undo and manual overrides"
    );
}
