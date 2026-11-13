use super::resource_process::worker;
use super::*;
use crate::{
    app::resource_git,
    features::git::{ChangeKind, DiffTarget, path::GitPath},
    infrastructure::git::command::{GitCommand, GitCommandRunner},
};
use loog_workspace_api::ResourceRef;

fn git(ui: &SavingUi, runner: &GitCommandRunner, cwd: &ResourceRef, args: &[&str]) -> String {
    let runner = runner.clone();
    let command = GitCommand::new()
        .cwd(cwd)
        .args(args.iter().map(|arg| (*arg).to_owned()));
    worker(ui, move || runner.run(command))
        .unwrap()
        .stdout_text()
        .unwrap()
}

pub(super) fn run(ui: &SavingUi, host: &ExtensionHost, root: &loog_workspace_api::PresentedRoot) {
    println!(
        "CHECK: resource Git discovery, literal filenames, target worktree encoding, stage, commit and failure"
    );
    ui.runtime
        .trust()
        .decide_resource(&root.resource, true)
        .unwrap();
    host.refresh_resource_grants();
    let ResourceRef::Provided {
        environment,
        path: base,
    } = &root.resource
    else {
        panic!("root")
    };
    let folder = child_path(
        ui,
        host,
        root.scope,
        base.clone(),
        &format!("native-git-{}", std::process::id()),
    );
    request(
        ui,
        host,
        root.scope,
        Operation::CreateDirectory {
            path: folder.clone(),
        },
    );
    let repository = ResourceRef::provided(environment.clone(), folder.as_str()).unwrap();
    let runner = resource_git::runner(ui.state.clone(), root.scope, root.resource.clone());
    git(ui, &runner, &repository, &["init", "-b", "main"]);
    git(
        ui,
        &runner,
        &repository,
        &["config", "user.name", "Loog Acceptance"],
    );
    git(
        ui,
        &runner,
        &repository,
        &["config", "user.email", "acceptance@example.invalid"],
    );
    git(
        ui,
        &runner,
        &repository,
        &["config", "core.autocrlf", "false"],
    );
    let name = "中文\\file[1].txt";
    let file = child_path(ui, host, root.scope, folder.clone(), name);
    let other = child_path(ui, host, root.scope, folder.clone(), "中文\\file1.txt");
    let original = "这是用于验证编码识别和文件差异的中文文本。\n原来的内容。\n";
    let format =
        crate::domain::encoding::TextFormat::new(crate::domain::encoding::Charset::Gbk, false);
    request(
        ui,
        host,
        root.scope,
        Operation::Write {
            path: file.clone(),
            bytes: crate::features::encoding::encode(original, format).unwrap(),
            expected_revision: None,
        },
    );
    request(
        ui,
        host,
        root.scope,
        Operation::Write {
            path: other,
            bytes: b"do not stage through a wildcard".to_vec(),
            expected_revision: None,
        },
    );
    let backend = Arc::new(resource_git::backend(
        ui.state.clone(),
        root.scope,
        root.resource.clone(),
    ));
    let service = backend.clone();
    let target = repository.clone();
    let mut snapshot = worker(ui, move || service.discover(&target, 1))
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.worktree_root, repository);
    let path = GitPath::from_git(name.as_bytes()).unwrap();
    assert_eq!(snapshot.files[&path].worktree, ChangeKind::Untracked);
    let service = backend.clone();
    let target = repository.clone();
    let selected = path.clone();
    worker(ui, move || service.stage(&target, &[selected], false)).unwrap();
    let service = backend.clone();
    snapshot = worker(ui, move || {
        service.refresh(&mut snapshot).unwrap();
        snapshot
    });
    assert_eq!(snapshot.files[&path].index, ChangeKind::Added);
    let service = backend.clone();
    let target = repository.clone();
    let selected = path.clone();
    worker(ui, move || service.stage(&target, &[selected], true)).unwrap();
    assert_eq!(
        read(ui, host, root.scope, &file),
        crate::features::encoding::encode(
            original,
            crate::domain::encoding::TextFormat::new(crate::domain::encoding::Charset::Gbk, false)
        )
        .unwrap()
    );
    let service = backend.clone();
    let target = repository.clone();
    let selected = path.clone();
    worker(ui, move || service.stage(&target, &[selected], false)).unwrap();
    assert_eq!(
        snapshot.files[&GitPath::from_git("中文\\file1.txt".as_bytes()).unwrap()].index,
        ChangeKind::Unmodified
    );
    let service = backend.clone();
    let target = repository.clone();
    worker(ui, move || {
        service.commit(&target, "initial acceptance", false, false)
    })
    .unwrap();
    let first = git(ui, &runner, &repository, &["rev-parse", "HEAD"]);
    let changed = original.replace("原来的内容", "修改后的内容");
    let format =
        crate::domain::encoding::TextFormat::new(crate::domain::encoding::Charset::Utf16Le, true);
    let encoded = crate::features::encoding::encode(&changed, format).unwrap();
    request(
        ui,
        host,
        root.scope,
        Operation::Write {
            path: file.clone(),
            bytes: encoded.clone(),
            expected_revision: None,
        },
    );
    let service = backend.clone();
    let selected = path.clone();
    snapshot = worker(ui, move || {
        service.refresh(&mut snapshot).unwrap();
        let diff = service
            .diff(&snapshot, &selected, DiffTarget::IndexToWorktree)
            .unwrap();
        assert!(!diff.files[0].binary);
        assert!(
            diff.files[0]
                .format_change
                .as_ref()
                .unwrap()
                .contains("UTF-16LE")
        );
        assert!(diff.files[0].hunks.iter().flat_map(|hunk| &hunk.lines).any(|line|
            matches!(line, crate::features::git::diff::DiffLine::Addition(text) if text.contains("修改后的内容"))));
        snapshot
    });
    let service = backend.clone();
    let target = repository.clone();
    let selected = path.clone();
    worker(ui, move || service.stage(&target, &[selected], false)).unwrap();
    git(ui, &runner, &repository, &["config", "user.name", ""]);
    let service = backend.clone();
    let target = repository.clone();
    let error = worker(ui, move || {
        service.commit(&target, "must fail", false, false)
    })
    .unwrap_err();
    assert_eq!(
        error.kind,
        crate::features::git::error::GitErrorKind::IdentityMissing,
        "{error:?}"
    );
    assert_eq!(git(ui, &runner, &repository, &["rev-parse", "HEAD"]), first);
    assert_eq!(read(ui, host, root.scope, &file), encoded);
    git(
        ui,
        &runner,
        &repository,
        &["config", "user.name", "Loog Acceptance"],
    );
    let service = backend.clone();
    let final_target = repository.clone();
    worker(ui, move || {
        service.refresh(&mut snapshot).unwrap();
        assert_eq!(snapshot.files[&path].index, ChangeKind::Modified);
        let diff = service
            .diff(&snapshot, &path, DiffTarget::HeadToIndex)
            .unwrap();
        assert!(diff.files[0].format_change.is_some());
        service
            .commit(&final_target, "convert acceptance encoding", false, false)
            .unwrap();
    });
    native_panel(ui, host, root, &runner, &repository, &file, name);
    let context = ui.state.clone();
    let cwd = root.resource.clone();
    let scope = root.scope;
    let base = base.as_str().to_owned();
    let folder = folder.as_str().to_owned();
    let output = worker(ui, move || crate::app::resource_process::run(&context, scope,
        crate::app::resource_process::Command { program: "python3".into(), args: vec!["-c".into(),
            "import os,sys,shutil; base=os.path.realpath(sys.argv[1]); path=os.path.realpath(sys.argv[2]); assert os.path.dirname(path)==base and os.path.basename(path).startswith('native-git-'); shutil.rmtree(path)".into(), base, folder],
            cwd, env: vec![], input: vec![], timeout: Duration::from_secs(10) }, &|| false)).unwrap();
    assert_eq!(output.code, Some(0));
    ui.runtime
        .trust()
        .decide_resource(&root.resource, false)
        .unwrap();
    host.refresh_resource_grants();
}

#[inline(never)]
fn native_panel(
    ui: &SavingUi,
    host: &ExtensionHost,
    root: &loog_workspace_api::PresentedRoot,
    runner: &GitCommandRunner,
    repository: &ResourceRef,
    file: &ProviderPath,
    name: &str,
) {
    println!(
        "CHECK: native Source Control discovery, diff identity, stage, commit and failed-message retention"
    );
    request(
        ui,
        host,
        root.scope,
        Operation::Write {
            path: file.clone(),
            bytes: "原生界面提交的中文内容。\n".as_bytes().to_vec(),
            expected_revision: None,
        },
    );
    let Answer::Info { file: info } =
        request(ui, host, root.scope, Operation::Stat { path: file.clone() })
    else {
        panic!("file")
    };
    crate::app::resources::open(&ui.state, root.scope, info);
    wait(ui, |app| {
        app.workspace.active_document().and_then(|id| app.workspace.documents.get(&id))
        .is_some_and(|document| document.meta.resource.environment() == root.resource.environment() && matches!(&document.meta.resource, ResourceRef::Provided { path, .. } if path == file))
    });
    ui.state.update(|app| app.source_control.visible = true);
    crate::app::resource_git_tasks::tick(&ui.state);
    wait(ui, |app| {
        app.git
            .resources
            .model
            .entries
            .get(&root.scope)
            .is_some_and(|entry| !entry.busy && entry.repository.is_some())
    });
    assert_eq!(
        ui.state.get().git.resources.model.entries[&root.scope]
            .repository
            .as_ref()
            .unwrap()
            .worktree_root,
        *repository
    );
    click(ui, &format!("Open diff {name}"));
    wait(ui, |app| {
        app.workspace
            .active_diff()
            .and_then(|diff| diff.provided.as_ref())
            .is_some_and(|identity| identity.repository == *repository)
    });
    let app = ui.state.get();
    let diff = app.workspace.active_diff().unwrap();
    assert!(diff.repository_root.as_os_str().is_empty());
    assert_eq!(diff.path_label(), name);
    assert!(
        diff.rows
            .iter()
            .any(|row| row.text.contains("原生界面提交"))
    );
    assert_eq!(
        app.workspace.active_meta().unwrap().resource,
        diff.resource()
    );
    assert!(
        app.workspace
            .git_poll_target()
            .diffs
            .iter()
            .all(|diff| !diff.repository_root.as_os_str().is_empty())
    );
    drop(app);
    click(ui, "Stage");
    wait(ui, |app| {
        app.git
            .resources
            .model
            .entries
            .get(&root.scope)
            .is_some_and(|entry| {
                !entry.busy
                    && entry.repository.as_ref().is_some_and(|repo| {
                        repo.files[&GitPath::from_git(name.as_bytes()).unwrap()].index
                            == ChangeKind::Modified
                    })
            })
    });
    git(ui, runner, repository, &["config", "user.name", ""]);
    assert!(
        ui.state
            .get()
            .workspace
            .active_diff()
            .unwrap()
            .rows
            .is_empty(),
        "Staging must refresh the open worktree diff"
    );
    let before = git(ui, runner, repository, &["rev-parse", "HEAD"]);
    ui.state.update(|app| {
        app.git.resources.messages.insert(
            root.resource.clone(),
            crate::ui::components::input::InputState::new("native UI acceptance"),
        );
    });
    click(ui, "Commit");
    wait(ui, |app| {
        app.git
            .resources
            .model
            .entries
            .get(&root.scope)
            .is_some_and(|entry| !entry.busy && entry.error.is_some())
    });
    assert_eq!(
        ui.state.get().git.resources.messages[&root.resource].text(),
        "native UI acceptance"
    );
    assert_eq!(git(ui, runner, repository, &["rev-parse", "HEAD"]), before);
    git(
        ui,
        runner,
        repository,
        &["config", "user.name", "Loog Acceptance"],
    );
    click(ui, "Commit");
    wait(ui, |app| {
        app.git
            .resources
            .model
            .entries
            .get(&root.scope)
            .is_some_and(|entry| !entry.busy && entry.error.is_none())
            && app.git.resources.messages[&root.resource].text().is_empty()
    });
    assert_ne!(git(ui, runner, repository, &["rev-parse", "HEAD"]), before);
    ui.state.update(|app| {
        let diffs = app
            .workspace
            .diffs
            .values()
            .filter(|record| {
                record
                    .content
                    .provided
                    .as_ref()
                    .is_some_and(|identity| identity.repository == *repository)
            })
            .map(|record| record.view)
            .collect::<Vec<_>>();
        for view in diffs {
            if let Some(pane) = app.workspace.pane_of(view) {
                app.workspace.close_item(pane, view);
            }
        }
        let resource =
            ResourceRef::provided(root.resource.environment().unwrap().clone(), file.as_str())
                .unwrap();
        if let Some(id) = app.workspace.file_id_for_resource(&resource) {
            app.workspace.close(id);
        }
        app.source_control.visible = false;
        app.git.resources.model.entries.remove(&root.scope);
    });
}

#[inline(never)]
fn click(ui: &SavingUi, name: &str) {
    use lgui::{
        application::{AppView, ApplicationContext},
        core::*,
        session::UiSession,
    };
    let state = ui.state.clone();
    let view: AppView = Arc::new(move |cx| {
        let store = cx.state_with(crate::features::git::GitStoreSnapshot::default);
        crate::ui::git::panel::render(
            cx.viewport(),
            crate::app::git_view::binding(&state, &store),
            store.into(),
            cx.focus_handle(UiId::new("resource-git-editor")),
            cx.focus_handle(UiId::new("resource-git-message")),
            UiId::new("resource-git-message"),
            cx.focus_handle(UiId::new("resource-git-filter")),
        )
    });
    let viewport = UiRect::new(0.0, 0.0, 380.0, 700.0);
    let mut session = UiSession::new();
    session.render_view(&view, viewport, UiScale::ONE);
    let node = session
        .tree()
        .nodes()
        .iter()
        .find(|node| {
            node.semantics
                .as_ref()
                .and_then(|semantics| semantics.name.as_deref())
                == Some(name)
        })
        .unwrap_or_else(|| panic!("missing control {name}"));
    let rect = session.tree().hit_for_id(&node.id).unwrap().rect;
    let point = Point::new(
        (rect.left + rect.right) / 2.0,
        (rect.top + rect.bottom) / 2.0,
    );
    let context = ApplicationContext::empty(Default::default());
    for input in [
        InputEvent::PointerDown {
            pointer: PointerData::mouse(point),
            button: PointerButton::Left,
        },
        InputEvent::PointerUp {
            pointer: PointerData::mouse(point),
            button: PointerButton::Left,
        },
    ] {
        dispatch_runtime_output(
            session.handle_input(input),
            &context,
            &lgui::window::WindowId::new("resource-git-test"),
            |action| session.handle_default_action(action),
            |_| {},
        );
        session.render_view(&view, viewport, UiScale::ONE);
    }
}
