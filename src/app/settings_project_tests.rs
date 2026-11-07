//! Application/project configuration integration regression.
use crate::features::settings::rules::Settings;
use crate::features::terminal::ShellKind;
use crate::infrastructure::project_settings::*;
use std::fs;
#[test]
fn workspace_folders_apply_their_own_project_settings() {
    let base = std::env::temp_dir().join(format!(
        "loog-project-settings-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let (with, without) = (base.join("with"), base.join("without"));
    fs::create_dir_all(with.join(DIR_NAME)).unwrap();
    fs::create_dir_all(&without).unwrap();
    fs::write(
        path(&with),
        "// team shell\n{ \"default_shell\": \"cmd\", \"theme\": \"ember\" }",
    )
    .unwrap();

    let mut app = crate::app::state::AppState::new();
    app.explorer
        .model
        .restore(vec![with.clone(), without.clone()], Vec::new());
    app.refresh_project_settings();
    assert_eq!(app.preferences.values().project_settings.len(), 1);
    assert_eq!(
        app.preferences.values().project_settings[0].1.errors.len(),
        1
    );
    assert!(app.chrome.toast.is_some(), "the ignored theme is reported");

    let inside = app.settings_at(Some(&with.join("src")));
    assert_eq!(inside.default_shell, ShellKind::Cmd);
    assert_eq!(
        inside.theme,
        app.preferences.values().theme,
        "personal settings stay the user's"
    );
    assert_eq!(
        app.settings_at(Some(&without)).default_shell,
        app.preferences.values().default_shell
    );
    assert_eq!(
        app.settings_at(None).default_shell,
        app.preferences.values().default_shell
    );

    fs::remove_dir_all(&base).unwrap();
}

#[test]
fn custom_default_keeps_its_id_across_settings_and_workspace_json() {
    let settings = Settings {
        default_shell: ShellKind::Custom(7),
        ..Settings::default()
    };
    let json = serde_json::to_string(&settings).unwrap();
    assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), settings);
    let session = crate::app::workspace_session::WorkspaceSession {
        terminal_tabs: vec![
            ShellKind::PowerShell,
            ShellKind::System,
            ShellKind::Custom(7),
        ],
        ..Default::default()
    };
    assert_eq!(
        serde_json::from_str::<crate::app::workspace_session::WorkspaceSession>(
            &serde_json::to_string(&session).unwrap()
        )
        .unwrap(),
        session
    );
}

#[test]
fn file_associations_from_user_and_project_settings_choose_the_language() {
    use crate::features::settings::rules::FileAssociation;
    let base = std::env::temp_dir().join(format!(
        "loog-associations-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let (project, other) = (base.join("project"), base.join("other"));
    fs::create_dir_all(project.join(DIR_NAME)).unwrap();
    fs::create_dir_all(&other).unwrap();
    fs::write(
        path(&project),
        r#"{ "file_associations": [{ "pattern": "**/*.cfg", "language": "typescript" }] }"#,
    )
    .unwrap();

    let mut app = crate::app::state::AppState::new();
    app.explorer
        .model
        .restore(vec![project.clone(), other.clone()], Vec::new());
    let language = |app: &crate::app::state::AppState, file: &std::path::Path| {
        let id = app.workspace.file_id_for_path(file).unwrap();
        app.workspace.documents[&id].meta.lang.name()
    };
    let (in_project, in_other) = (project.join("a.cfg"), other.join("a.cfg"));
    app.workspace.open_path(in_project.clone(), "x".into());
    app.workspace.open_path(in_other.clone(), "x".into());
    assert_eq!(language(&app, &in_project), "plaintext");
    let before = {
        let id = app.workspace.file_id_for_path(&in_project).unwrap();
        app.workspace.documents[&id].stamp(id)
    };

    // The project file is read: its rule reaches the open file in its folder only.
    app.refresh_project_settings();
    assert_eq!(language(&app, &in_project), "typescript");
    assert_eq!(language(&app, &in_other), "plaintext");
    let id = app.workspace.file_id_for_path(&in_project).unwrap();
    assert_ne!(app.workspace.documents[&id].stamp(id), before);

    // A user rule applies everywhere the project has none, and loses to the project.
    let mut settings = app.settings();
    settings.file_associations = vec![
        FileAssociation {
            pattern: "*.cfg".into(),
            language: "rust".into(),
        },
        FileAssociation {
            pattern: "*.zzz".into(),
            language: "csharp".into(),
        },
    ];
    assert!(app.apply_settings(settings));
    assert_eq!(
        language(&app, &in_project),
        "typescript",
        "project beats user"
    );
    assert_eq!(language(&app, &in_other), "rust");
    let zzz = other.join("b.zzz");
    app.workspace.open_path(zzz.clone(), "x".into());
    assert_eq!(
        language(&app, &zzz),
        "csharp",
        "files opened later are recognised too"
    );

    // Removing the user rules reverts the file that only they covered.
    let mut settings = app.settings();
    settings.file_associations.clear();
    assert!(app.apply_settings(settings));
    assert_eq!(language(&app, &in_other), "plaintext");
    assert_eq!(language(&app, &in_project), "typescript");

    // Removing the project folder drops its rules.
    app.explorer.model.restore(vec![other.clone()], Vec::new());
    app.refresh_project_settings();
    assert_eq!(language(&app, &in_project), "plaintext");

    fs::remove_dir_all(&base).unwrap();
}
