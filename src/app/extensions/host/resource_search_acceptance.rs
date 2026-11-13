//! Real component, target disk scan, native result model and editor navigation.
use super::*;

pub(super) fn search(ui: &SavingUi) -> Arc<crate::features::search::Results> {
    let mut expected = crate::app::search::key(&ui.state.get());
    let mut stop = Box::new(crate::app::search::start(
        ui.state.clone(),
        ui.tasks.application(),
        expected.clone(),
    )) as Box<dyn FnOnce()>;
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        ui.tasks.run_all();
        let app = ui.state.get();
        let current = crate::app::search::key(&app);
        if expected != current {
            stop();
            expected = current;
            stop = Box::new(crate::app::search::start(
                ui.state.clone(),
                ui.tasks.application(),
                expected.clone(),
            ));
        } else if !app.search.model.status().running {
            let status = app.search.model.status();
            assert!(status.error.is_none(), "{:?}", status.error);
            assert!(status.results.complete, "search did not complete");
            let results = status.results.clone();
            stop();
            return results;
        }
        assert!(Instant::now() < deadline, "native search timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn focus() -> lgui::core::UiFocusHandle {
    use lgui::{
        application::AppView,
        core::{UiId, UiScale},
        prelude::{UiRect, VisualStyle, panel},
        session::UiSession,
    };
    let output = Arc::new(Mutex::new(None));
    let captured = output.clone();
    let view: AppView = Arc::new(move |cx| {
        *captured.lock().unwrap() = Some(cx.focus_handle(UiId::new("resource-search-acceptance")));
        panel(UiRect::new(0.0, 0.0, 400.0, 300.0), VisualStyle::default())
    });
    UiSession::new().render_view(&view, UiRect::new(0.0, 0.0, 400.0, 300.0), UiScale::ONE);
    output.lock().unwrap().take().unwrap()
}

pub(super) fn run(ui: &SavingUi, host: &ExtensionHost, root: &loog_workspace_api::PresentedRoot) {
    use crate::domain::encoding::{Charset, TextFormat};
    use loog_workspace_api::ResourceRef;
    println!("CHECK: native target search, dirty buffers, UTF-16 and result navigation");
    let ResourceRef::Provided {
        path: base,
        environment,
    } = &root.resource
    else {
        panic!("provided root")
    };
    let folder = format!("native-search-{}", std::process::id());
    let directory = child_path(ui, host, root.scope, base.clone(), &folder);
    request(
        ui,
        host,
        root.scope,
        Operation::CreateDirectory {
            path: directory.clone(),
        },
    );
    let mut files = Vec::new();
    for (name, text, format) in [
        (
            "中文\\file.txt",
            "中文 needle\r\nnext",
            TextFormat::default(),
        ),
        (
            "utf16.txt",
            "needle 中文\r\n",
            TextFormat::new(Charset::Utf16Le, true),
        ),
        ("ignored.txt", "needle", TextFormat::default()),
        (".gitignore", "ignored.txt\n", TextFormat::default()),
    ] {
        let path = child_path(ui, host, root.scope, directory.clone(), name);
        request(
            ui,
            host,
            root.scope,
            Operation::Write {
                path: path.clone(),
                bytes: crate::features::encoding::encode(text, format).unwrap(),
                expected_revision: None,
            },
        );
        files.push(path);
    }
    ui.state.update(|app| {
        app.search.ui.query.set_text("needle");
        app.search.ui.include.set_text(format!("{folder}/*.txt"));
    });
    let results = search(ui);
    assert_eq!(results.errors, 0, "{:?}", results.first_error);
    assert_eq!(results.matches, 2);
    let file = results
        .files
        .iter()
        .find(|f| f.name() == "中文\\file.txt")
        .unwrap()
        .clone();
    assert_eq!(
        file.provider.as_ref().unwrap().relative,
        vec![folder.clone(), "中文\\file.txt".into()]
    );
    crate::app::search::navigate(
        &ui.state,
        ui.tasks.application(),
        lgui::prelude::UiRect::new(0.0, 0.0, 600.0, 400.0),
        focus(),
        file.clone(),
        0,
        false,
        false,
    );
    // A real watch event from an unrelated ignored file must not cancel the
    // explicit navigation while its target bytes/configuration are in flight.
    request(
        ui,
        host,
        root.scope,
        Operation::Write {
            path: files[2].clone(),
            bytes: b"unrelated change".to_vec(),
            expected_revision: None,
        },
    );
    wait(ui, |app| {
        app.workspace
            .active_document()
            .is_some_and(|id| app.workspace.documents[&id].meta.resource == file.path)
    });
    let id = ui.state.get().workspace.active_document().unwrap();
    assert_eq!(
        ui.state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .selected_text(),
        Some("needle")
    );
    ui.state.update(|app| {
        let len = app.workspace.documents[&id].buffer.len();
        app.workspace
            .document_editor_mut(id)
            .unwrap()
            .replace(
                0..len,
                "dirty needle",
                loog_editor_core::text::EditKind::Other,
            )
            .unwrap();
        app.search.ui.query.set_text("dirty");
    });
    let results = search(ui);
    assert_eq!(results.matches, 1);
    assert_eq!(results.files[0].path, file.path);
    assert_eq!(
        read(ui, host, root.scope, &files[0]),
        "中文 needle\r\nnext".as_bytes()
    );
    crate::app::search::navigate(
        &ui.state,
        ui.tasks.application(),
        lgui::prelude::UiRect::new(0.0, 0.0, 600.0, 400.0),
        focus(),
        results.files[0].clone(),
        0,
        true,
        false,
    );
    wait(ui, |app| {
        app.workspace
            .active_editor()
            .is_some_and(|editor| editor.selected_text() == Some("dirty"))
    });
    ui.state
        .update(|app| app.search.ui.query.set_text("needle"));
    let results = search(ui);
    let encoded = results
        .files
        .iter()
        .find(|f| f.name() == "utf16.txt")
        .unwrap()
        .clone();
    crate::app::search::navigate(
        &ui.state,
        ui.tasks.application(),
        lgui::prelude::UiRect::new(0.0, 0.0, 600.0, 400.0),
        focus(),
        encoded.clone(),
        0,
        true,
        false,
    );
    wait(ui, |app| {
        app.workspace
            .active_document()
            .is_some_and(|id| app.workspace.documents[&id].meta.resource == encoded.path)
    });
    let encoded_id = ui.state.get().workspace.active_document().unwrap();
    assert_eq!(
        ui.state.get().workspace.documents[&encoded_id].format(),
        TextFormat::new(Charset::Utf16Le, true)
    );
    assert_eq!(
        ui.state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .selected_text(),
        Some("needle")
    );
    // A replacement query invalidates both old publications and navigation.
    ui.state
        .update(|app| app.search.ui.query.set_text("needle"));
    let old = crate::app::search::start(
        ui.state.clone(),
        ui.tasks.application(),
        crate::app::search::key(&ui.state.get()),
    );
    old();
    ui.state
        .update(|app| app.search.ui.query.set_text("no-such-native-search-value"));
    assert_eq!(search(ui).matches, 0);
    crate::app::search::clear(&ui.state);
    ui.state.update(|app| {
        app.workspace.close(id);
        app.workspace.close(encoded_id);
        app.search.ui.include.clear();
    });
    for path in files {
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
    request(
        ui,
        host,
        root.scope,
        Operation::Remove {
            path: directory,
            recursive: false,
        },
    );
    assert_eq!(environment, file.path.environment().unwrap());
}
