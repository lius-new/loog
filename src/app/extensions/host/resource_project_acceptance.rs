//! Root project settings through the actual provider and native consumers.
use super::*;

pub(super) fn run(ui: &SavingUi, host: &ExtensionHost, root: &loog_workspace_api::PresentedRoot) {
    use crate::domain::encoding::{Charset, TextFormat};
    use crate::features::settings::editing::EditorOverrides;
    let loog_workspace_api::ResourceRef::Provided {
        path: base,
        environment,
    } = &root.resource
    else {
        panic!("provided root")
    };
    let scope = root.scope;
    assert!(crate::app::key_actions::open_project_settings(&ui.state));
    wait(ui, |app| {
        app.workspace
            .active_document()
            .and_then(|id| app.workspace.documents.get(&id))
            .is_some_and(|doc| {
                doc.meta.name == "settings.json"
                    && doc.meta.provider_root.as_ref() == Some(&root.resource)
            })
    });
    ui.state.update(|app| {
        let id = app.workspace.active_document().unwrap();
        assert!(
            app.workspace.documents[&id]
                .buffer
                .text()
                .contains("Loog project settings")
        );
        app.workspace.close(id);
    });
    let directory = child_path(ui, host, scope, base.clone(), ".loog");
    let settings = child_path(ui, host, scope, directory.clone(), "settings.json");
    let project = child_path(
        ui,
        host,
        scope,
        base.clone(),
        &format!("native-project-{}", std::process::id()),
    );
    request(
        ui,
        host,
        scope,
        Operation::CreateDirectory {
            path: project.clone(),
        },
    );
    let source = child_path(ui, host, scope, project.clone(), "src");
    request(
        ui,
        host,
        scope,
        Operation::CreateDirectory {
            path: source.clone(),
        },
    );
    let name = "中文\\配置.cfg";
    let path = child_path(ui, host, scope, source.clone(), name);
    let encoding_path = child_path(ui, host, scope, source.clone(), "encoded.txt");
    let editorconfig = child_path(ui, host, scope, project.clone(), ".editorconfig");
    let write = |path: &ProviderPath, bytes: Vec<u8>| {
        request(
            ui,
            host,
            scope,
            Operation::Write {
                path: path.clone(),
                bytes,
                expected_revision: None,
            },
        );
    };
    let configuration = |language: &str, indent: usize| {
        serde_json::to_vec(&serde_json::json!({
            "default_shell": "bash",
            "encoding": { "auto_detect": false },
            "editor": { "indent_size": 4 },
            "editor_languages": {(language): {"indent_size": indent}},
            "file_associations": [{"pattern":"native-project-*/src/*.cfg", "language": language}]
        }))
        .unwrap()
    };
    write(&settings, configuration("json", 6));
    write(&path, b"{}\n".to_vec());
    write(&editorconfig, b"root=true\n[*]\nindent_size=2\n".to_vec());
    let original = "中文编码测试，保留原始字节。".repeat(30);
    let bytes =
        crate::features::encoding::encode(&original, TextFormat::new(Charset::Gbk, false)).unwrap();
    write(&encoding_path, bytes.clone());
    let resource =
        loog_workspace_api::ResourceRef::provided(environment.clone(), path.as_str()).unwrap();
    let open = |path: &ProviderPath| {
        let Answer::Info { file } =
            request(ui, host, scope, Operation::Stat { path: path.clone() })
        else {
            panic!("file metadata")
        };
        crate::app::resources::open(&ui.state, scope, file);
    };
    open(&path);
    wait(ui, |app| {
        app.workspace.file_id_for_resource(&resource).is_some()
    });
    let id = ui
        .state
        .get()
        .workspace
        .file_id_for_resource(&resource)
        .unwrap();
    ui.state.update(|app| {
        let doc = &app.workspace.documents[&id];
        assert_eq!(doc.meta.lang.name(), "json");
        assert_eq!(
            doc.meta.provider_relative.as_ref().unwrap().last().unwrap(),
            name
        );
        assert_eq!(doc.buffer.file_options().indent_size, Some(2));
        assert_eq!(crate::app::editing::effective(app, id).indent_size, 6);
        app.editor.editing.manual.insert(
            id,
            EditorOverrides {
                indent_size: Some(8),
                ..Default::default()
            },
        );
        app.workspace
            .document_editor_mut(id)
            .unwrap()
            .replace(0..2, "unsaved", loog_editor_core::text::EditKind::Other)
            .unwrap();
    });
    write(&settings, configuration("rust", 7));
    wait(ui, |app| {
        app.workspace.documents[&id].meta.lang.name() == "rust"
    });
    ui.state.update(|app| {
        assert_eq!(crate::app::editing::effective(app, id).indent_size, 8);
        app.editor.editing.manual.remove(&id);
        assert_eq!(crate::app::editing::effective(app, id).indent_size, 7);
        assert_eq!(app.workspace.documents[&id].buffer.text(), "unsaved\n");
        app.workspace.document_editor_mut(id).unwrap().undo();
        assert_eq!(app.workspace.documents[&id].buffer.text(), "{}\n");
    });
    // Invalid syntax retains the last valid settings and remains editable.
    write(&settings, b"{ invalid json".to_vec());
    wait(ui, |app| {
        app.chrome
            .toast
            .as_ref()
            .is_some_and(|toast| toast.message.contains("project settings"))
    });
    assert_eq!(
        ui.state.get().workspace.documents[&id].meta.lang.name(),
        "rust"
    );
    let settings_resource =
        loog_workspace_api::ResourceRef::provided(environment.clone(), settings.as_str()).unwrap();
    assert!(crate::app::key_actions::open_project_settings(&ui.state));
    wait(ui, |app| {
        app.workspace
            .file_id_for_resource(&settings_resource)
            .is_some()
    });
    ui.state.update(|app| {
        let config_id = app
            .workspace
            .file_id_for_resource(&settings_resource)
            .unwrap();
        assert_eq!(
            app.workspace.documents[&config_id].buffer.text(),
            "{ invalid json"
        );
        app.workspace.close(config_id);
    });
    write(&settings, configuration("json", 6));
    wait(ui, |app| {
        app.workspace.documents[&id].meta.lang.name() == "json"
    });
    open(&encoding_path);
    let encoded =
        loog_workspace_api::ResourceRef::provided(environment.clone(), encoding_path.as_str())
            .unwrap();
    wait(ui, |app| {
        app.workspace.file_id_for_resource(&encoded).is_some()
    });
    ui.state.update(|app| {
        let encoded_id = app.workspace.file_id_for_resource(&encoded).unwrap();
        assert_eq!(
            app.workspace.documents[&encoded_id].format().charset,
            Charset::Utf8
        );
        assert_ne!(app.workspace.documents[&encoded_id].buffer.text(), original);
        app.workspace.close(encoded_id);
    });
    assert_eq!(read(ui, host, scope, &encoding_path), bytes);
    terminal(ui, host, root);
    request(
        ui,
        host,
        scope,
        Operation::Remove {
            path: settings.clone(),
            recursive: false,
        },
    );
    wait(ui, |app| {
        app.workspace.documents[&id].meta.lang.name() != "json"
    });
    assert_eq!(
        crate::app::editing::effective(&ui.state.get(), id).indent_size,
        2
    );
    write(&settings, configuration("json", 6));
    wait(ui, |app| {
        app.workspace.documents[&id].meta.lang.name() == "json"
    });
    assert_eq!(
        crate::app::editing::effective(&ui.state.get(), id).indent_size,
        6
    );
    ui.state.update(|app| {
        app.workspace.close(id);
    });
    for path in [
        path,
        encoding_path,
        editorconfig,
        source,
        project,
        settings,
        directory,
    ] {
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
        "CHECK: target project JSONC settings drive native language/encoding/editing/Shell; change, invalid syntax, delete and recreation preserve text and undo"
    );
}

fn terminal(ui: &SavingUi, host: &ExtensionHost, root: &loog_workspace_api::PresentedRoot) {
    use crate::features::terminal::{
        TerminalSize, TerminalStatus, model::ProvidedTerminal, screen::Palette,
    };
    use crate::infrastructure::terminal::{
        notification::FrameNotifier, session::TerminalController,
    };
    let loog_workspace_api::ResourceRef::Provided { path, .. } = &root.resource else {
        unreachable!()
    };
    // The preceding terminal fixture deliberately revoked execution trust.
    ui.runtime
        .trust()
        .decide_resource(&root.resource, true)
        .unwrap();
    host.refresh_resource_grants();
    wait(ui, |app| {
        app.explorer.resources.root(root.scope).is_some_and(|root| {
            root.descriptor
                .capabilities
                .contains(&loog_workspace_api::Capability::Terminal)
        })
    });
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
    controller.ensure_started(
        None,
        TerminalSize {
            cols: 80,
            rows: 24,
            pixel_width: 0,
            pixel_height: 0,
        },
        FrameNotifier::new(|| {}),
    );
    wait(ui, |_| {
        let status = controller.snapshot_with_palette(&palette).status;
        assert!(!matches!(status, TerminalStatus::Failed(_)), "{status:?}");
        status == TerminalStatus::Running
    });
    controller
        .write(b"stty -echo; printf '\\033c'; printf 'PROJECT-SHELL:%s\\n' \"$BASH_VERSION\"\r");
    wait(ui, |_| {
        controller
            .test_parser()
            .screen()
            .contents()
            .lines()
            .any(|line| {
                line.strip_prefix("PROJECT-SHELL:").is_some_and(|version| {
                    version.chars().next().is_some_and(|c| c.is_ascii_digit())
                })
            })
    });
    controller.write(b"exit\r");
    wait(ui, |_| {
        matches!(
            controller.snapshot_with_palette(&palette).status,
            TerminalStatus::Exited(_)
        )
    });
    ui.runtime
        .trust()
        .decide_resource(&root.resource, false)
        .unwrap();
    host.refresh_resource_grants();
}
