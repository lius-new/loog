#![allow(unused_imports)]
use crate::app::context::AppContext;
use crate::features::git::actions::*;
use crate::features::git::model::*;
use crate::features::git::*;
use crate::ui::components::input::InputState;
use crate::ui::git::confirm::{GitConfirm, GitConfirmAction};
use crate::ui::git::menu::{GitMenu, GitMenuTarget};
use crate::ui::git::panel::{CONFLICTS, STAGED, UNTRACKED};
use crate::ui::theme;
use lgui::core::*;
use lgui::prelude::*;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
mod panel_virtualization_tests {
    use super::*;
    use crate::app::state::AppState;
    use crate::ui::git::panel::*;
    use lgui::prelude::State;

    /// A repository with 10,000 unstaged files, a stash, a tag, two
    /// worktrees and forty commits.
    fn drawer_fixture() -> (
        crate::features::git::RepositoryId,
        GitStoreSnapshot,
        impl Fn(&mut AppState) + Send + Sync + 'static,
    ) {
        use crate::features::git::RepositoryId;
        use crate::features::git::snapshot::{GitPresentation, RepositoryPresentation};
        let id = RepositoryId(1);
        let paths = (0..10_000)
            .map(|index| PathBuf::from(format!("src/file{index:05}.rs")))
            .collect::<Vec<_>>();
        let repository = RepositorySnapshot {
            id,
            worktree_root: PathBuf::from("loom"),
            git_dir: PathBuf::from("loom/.git"),
            common_dir: PathBuf::from("loom/.git"),
            head: crate::features::git::HeadState::Branch("main".into()),
            upstream: None,
            ahead: 0,
            behind: 0,
            files: Arc::new(
                paths
                    .iter()
                    .cloned()
                    .map(|path| {
                        (
                            path,
                            FileState {
                                worktree: ChangeKind::Modified,
                                ..Default::default()
                            },
                        )
                    })
                    .collect(),
            ),
            ignored: Default::default(),
            repository_state: RepositoryState::Normal,
            operation_step: None,
            head_oid: None,
            tags: Arc::new(vec![TagInfo {
                name: "v1.0.0".into(),
                commit: Some("abc00004".into()),
            }]),
            stashes: Arc::new(vec![StashInfo {
                index: 0,
                oid: "stash".into(),
                message: "WIP: search panel".into(),
            }]),
            worktrees: Arc::new(
                [("loom", "main"), ("loom-search", "feature/search")]
                    .into_iter()
                    .map(|(path, branch)| WorktreeInfo {
                        path: path.into(),
                        oid: "abc".into(),
                        branch: Some(branch.into()),
                        bare: false,
                        detached: false,
                        locked: None,
                    })
                    .collect(),
            ),
            features: Default::default(),
            generation: 0,
        };
        let mut prepared = RepositoryPresentation {
            dirty_count: paths.len(),
            ..Default::default()
        };
        prepared.sections[UNSTAGED as usize] = paths;
        let snapshot = GitStoreSnapshot {
            repositories: Arc::new([(id, repository)].into_iter().collect()),
            active_repository: Some(id),
            presentation: Arc::new(GitPresentation {
                repositories: [(id, prepared)].into_iter().collect(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let history = move |app: &mut AppState| {
            app.git.model.set_history(GitHistory {
                repository: "loom".into(),
                head: None,
                commits: Ok((0..40)
                    .map(|index| CommitSummary {
                        oid: format!("abc{index:05}"),
                        parents: match index {
                            0 => vec!["abc00001".into(), "abc00002".into()],
                            1 => vec!["abc00003".into()],
                            2 => vec!["abc00004".into()],
                            3 | 4 => vec!["abc00005".into()],
                            6 => vec!["abc00007".into(), "abc00008".into()],
                            7 | 8 => vec!["abc00009".into()],
                            39 => Vec::new(),
                            _ => vec![format!("abc{:05}", index + 1)],
                        },
                        author: "lius-new".into(),
                        author_email: "dev@example.com".into(),
                        timestamp: 0,
                        subject: format!("Improve editor behavior {index}"),
                        decorations: match index {
                            0 => vec!["HEAD -> refs/heads/main".into()],
                            2 => vec![
                                "refs/remotes/origin/main".into(),
                                "refs/remotes/origin/HEAD".into(),
                            ],
                            4 => vec!["tag: refs/tags/v1.0.0".into()],
                            6 => vec!["refs/heads/feature/editor-incremental-highlighting".into()],
                            8 => vec![
                                "refs/heads/feature/ui".into(),
                                "refs/remotes/origin/feature/ui".into(),
                            ],
                            _ => Vec::new(),
                        },
                    })
                    .collect()),
            });
        };
        (id, snapshot, history)
    }

    /// The state and store a test view exposes to the test.
    type Exposed = Arc<std::sync::Mutex<Option<(AppContext, State<GitStoreSnapshot>)>>>;

    fn render_view(
        viewport: UiRect,
        panel_rect: Option<UiRect>,
        snapshot: GitStoreSnapshot,
        history: impl Fn(&mut AppState) + Send + Sync + 'static,
        prepare: impl Fn(&mut AppState) + Send + Sync + 'static,
        output: Exposed,
    ) -> lgui::application::AppView {
        Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.git.ui.hovered = true;
                app.git.ui.scroll_hover = Some(ScrollRegion::Details);
                history(&mut app);
                prepare(&mut app);
                app
            }));
            let fixture = snapshot.clone();
            let store = cx.state_with(move || fixture);
            *output.lock().unwrap() = Some((state.clone(), store.clone()));
            let drawer = UiRect::new(
                viewport.left,
                viewport.top,
                viewport.right.min(340.0),
                panel_rect.map_or(viewport.bottom, |panel| panel.top),
            );
            let mut root = lgui::prelude::group(viewport).child(render(
                drawer,
                crate::app::git_view::binding(&state, &store),
                store.clone().into(),
                cx.focus_handle(UiId::new("git-test-editor")),
                cx.focus_handle(UiId::new("git-test-commit")),
                UiId::new("git-test-commit"),
                cx.focus_handle(UiId::new(crate::ui::git::drawer::FILTER_ID)),
            ));
            if let Some(panel) = panel_rect {
                root = root.child(crate::ui::git::workbench::render(
                    panel,
                    crate::app::git_view::binding(&state, &store),
                    store.clone().into(),
                    cx.focus_handle(UiId::new("git-test-editor")),
                ));
            }
            root
        })
    }

    fn text_nodes(session: &lgui::session::UiSession) -> Vec<String> {
        session
            .tree()
            .nodes()
            .iter()
            .filter_map(|node| node.text.as_ref().map(|value| value.to_string()))
            .collect()
    }

    fn named(session: &lgui::session::UiSession, name: &str) -> Option<UiRect> {
        let node = session.tree().nodes().iter().find(|node| {
            node.semantics
                .as_ref()
                .and_then(|semantics| semantics.name.as_deref())
                == Some(name)
        })?;
        Some(session.tree().hit_for_id(&node.id).unwrap().rect)
    }

    fn click(
        session: &mut lgui::session::UiSession,
        view: &lgui::application::AppView,
        viewport: UiRect,
        point: Point,
    ) {
        use lgui::application::ApplicationContext;
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
                &lgui::window::WindowId::new("git-panel-test"),
                |action| session.handle_default_action(action),
                |_| {},
            );
            session.render_view(view, viewport, UiScale::ONE);
        }
    }

    fn center(rect: UiRect) -> Point {
        Point::new(
            (rect.left + rect.right) / 2.0,
            (rect.top + rect.bottom) / 2.0,
        )
    }

    #[test]
    fn the_submit_menu_offers_only_the_way_out_during_an_operation() {
        use crate::ui::git::menu::{Entry, GitMenuTarget, entries};
        use crate::ui::git::state::GitPanelState;
        let (id, mut snapshot, _) = drawer_fixture();
        let labels = |snapshot: &GitStoreSnapshot| {
            entries(&GitMenuTarget::Submit, &GitPanelState::default(), snapshot)
                .into_iter()
                .map(|entry| match entry {
                    Entry::Item { label, .. } => label,
                    Entry::Message(message) => message,
                    Entry::Separator => "-".to_owned(),
                })
                .collect::<Vec<_>>()
        };
        assert!(labels(&snapshot).contains(&"Commit and Push".to_owned()));
        for (state, expected) in [
            (RepositoryState::RebaseInProgress, "Abort Rebase"),
            (RepositoryState::MergeInProgress, "Abort Merge"),
            (RepositoryState::Bisecting, "End Bisect"),
        ] {
            Arc::make_mut(&mut snapshot.repositories)
                .get_mut(&id)
                .unwrap()
                .repository_state = state;
            assert_eq!(labels(&snapshot), [expected]);
        }
    }

    #[test]
    fn an_unfinished_operation_replaces_the_commit_form_with_its_status() {
        use crate::features::git::ConflictKind;
        use lgui::application::ApplicationContext;
        use lgui::session::UiSession;
        let viewport = UiRect::new(0.0, 0.0, 340.0, 600.0);
        let (id, snapshot, history) = drawer_fixture();
        let exposed = Arc::new(std::sync::Mutex::new(None));
        let view = render_view(viewport, None, snapshot, history, |_| {}, exposed.clone());
        let context = ApplicationContext::empty(Default::default());
        context
            .resources()
            .provide(lgui::icons::IconRegistration(Arc::new(
                crate::app::svg_icons(),
            )));
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let mut session = UiSession::new();
        let (_, store) = {
            session.render_view(&view, viewport, UiScale::ONE);
            exposed.lock().unwrap().clone().unwrap()
        };
        let disabled = |session: &UiSession, name: &str| {
            session
                .tree()
                .nodes()
                .iter()
                .find(|node| {
                    node.semantics
                        .as_ref()
                        .and_then(|semantics| semantics.name.as_deref())
                        == Some(name)
                })
                .and_then(|node| node.semantics.as_ref())
                .map(|semantics| semantics.state.disabled)
        };
        let conflicted = PathBuf::from("src/file00000.rs");
        store.update(|snapshot| {
            let repository = Arc::make_mut(&mut snapshot.repositories)
                .get_mut(&id)
                .unwrap();
            repository.repository_state = RepositoryState::RebaseInProgress;
            repository.operation_step = Some((1, 2));
            Arc::make_mut(&mut repository.files)
                .get_mut(&conflicted)
                .unwrap()
                .conflict = Some(ConflictKind::BothModified);
        });
        session.render_view(&view, viewport, UiScale::ONE);

        save_drawer_preview(&session, &context, viewport, "git-operation-conflicts");
        // Nothing at the top; the form's place reports the operation.
        let texts = text_nodes(&session);
        assert!(!texts.iter().any(|value| value == "REBASING 1/2"));
        assert!(texts.contains(&"Rebasing 1/2".to_owned()), "{texts:?}");
        assert!(named(&session, "Commit message").is_none());
        assert!(named(&session, "Commit changes").is_none());
        assert!(named(&session, "Amend last commit").is_none());
        // The button is Continue, waiting for the conflict.
        assert_eq!(disabled(&session, "Continue Rebase"), Some(true));

        store.update(|snapshot| {
            let repository = Arc::make_mut(&mut snapshot.repositories)
                .get_mut(&id)
                .unwrap();
            Arc::make_mut(&mut repository.files)
                .get_mut(&conflicted)
                .unwrap()
                .conflict = None;
        });
        session.render_view(&view, viewport, UiScale::ONE);
        assert_eq!(disabled(&session, "Continue Rebase"), Some(false));
        save_drawer_preview(&session, &context, viewport, "git-operation-ready");

        // A bisect is ended, not continued.
        store.update(|snapshot| {
            Arc::make_mut(&mut snapshot.repositories)
                .get_mut(&id)
                .unwrap()
                .repository_state = RepositoryState::Bisecting;
        });
        session.render_view(&view, viewport, UiScale::ONE);
        assert_eq!(disabled(&session, "End Bisect"), Some(false));
        assert!(named(&session, "Continue Rebase").is_none());
    }

    #[test]
    fn the_drawer_keeps_changes_and_the_commit_form_without_bottom_sections() {
        use lgui::application::ApplicationContext;
        use lgui::session::UiSession;
        let viewport = UiRect::new(0.0, 0.0, 340.0, 600.0);
        let (id, snapshot, history) = drawer_fixture();
        let exposed = Arc::new(std::sync::Mutex::new(None));
        let view = render_view(viewport, None, snapshot, history, |_| {}, exposed.clone());
        let context = ApplicationContext::empty(Default::default());
        context
            .resources()
            .provide(lgui::icons::IconRegistration(Arc::new(
                crate::app::svg_icons(),
            )));
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        for section in ["COMMITS", "TAGS", "STASHES", "WORKTREES"] {
            assert!(
                named(&session, section).is_none(),
                "{section} belongs to the Git panel"
            );
        }
        assert!(named(&session, "UNSTAGED").is_some());
        // The commit form stays at the bottom below a long change list.
        let input = named(&session, "Commit message").unwrap();
        assert!(input.height() >= 70.0);
        assert_eq!(
            named(&session, "Commit changes").unwrap().bottom,
            viewport.bottom - 10.0
        );
        let (state, store) = exposed.lock().unwrap().clone().unwrap();
        store.update(|snapshot| {
            Arc::make_mut(&mut snapshot.presentation)
                .repositories
                .get_mut(&id)
                .unwrap()
                .sections[UNSTAGED as usize]
                .clear();
        });
        session.render_view(&view, viewport, UiScale::ONE);
        // Short and empty lists keep the same bottom-anchored composer.
        assert_eq!(named(&session, "Commit message").unwrap(), input);
        assert!(text_nodes(&session).contains(&"Working tree clean.".to_owned()));
        let _ = state;
        save_drawer_preview(&session, &context, viewport, "git-drawer-no-changes");
    }

    #[test]
    fn drawer_labels_follow_the_compared_side_even_when_hovered_or_selected() {
        use crate::ui::git::commands::{Binding, Frame};
        use crate::ui::git::state::GitPanelState;
        use lgui::application::{AppView, ApplicationContext};
        use lgui::session::UiSession;

        let context = ApplicationContext::empty(Default::default());
        context
            .resources()
            .provide(lgui::icons::IconRegistration(Arc::new(
                crate::app::svg_icons(),
            )));
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        // The same path is added in the index and deleted in the working tree.
        let split = FileState {
            index: ChangeKind::Added,
            worktree: ChangeKind::Deleted,
            ..Default::default()
        };
        let cases = vec![
            (
                "src/new.rs",
                split.clone(),
                STAGED,
                "A",
                theme::c().git.added_text,
                false,
            ),
            (
                "src/new.rs",
                split,
                UNSTAGED,
                "D",
                theme::c().git.deleted,
                true,
            ),
            (
                "src/modified.rs",
                FileState {
                    worktree: ChangeKind::Modified,
                    ..Default::default()
                },
                UNSTAGED,
                "M",
                theme::c().git.modified,
                false,
            ),
            (
                "src/untracked.rs",
                FileState {
                    worktree: ChangeKind::Untracked,
                    ..Default::default()
                },
                UNTRACKED,
                "?",
                theme::c().git.added_text,
                false,
            ),
            (
                "src/conflict.rs",
                FileState {
                    worktree: ChangeKind::Deleted,
                    conflict: Some(ConflictKind::DeletedByUs),
                    ..Default::default()
                },
                CONFLICTS,
                "U",
                theme::c().error_text,
                false,
            ),
        ];
        for (width, nested, hovered, active) in [
            (288.0, false, false, false),
            (288.0, false, true, true),
            (160.0, true, true, true),
        ] {
            let viewport = UiRect::new(0.0, 0.0, width, cases.len() as f32 * CHANGE_ROW_H);
            let fixtures = cases.clone();
            let view: AppView = Arc::new(move |cx| {
                let state = cx.state_with(GitPanelState::default);
                let store = cx.state_with(GitStoreSnapshot::default);
                let binding = Binding::new(state.into(), Frame::default, |_| true);
                let mut root = panel(viewport, VisualStyle::filled(theme::c().sidebar));
                for (index, (path, file, section, _, _, _)) in fixtures.iter().enumerate() {
                    let top = index as f32 * CHANGE_ROW_H;
                    root = root.child(file_row(
                        UiRect::new(0.0, top, width, top + CHANGE_ROW_H),
                        path.into(),
                        nested,
                        file.clone(),
                        *section,
                        if *section == STAGED {
                            DiffTarget::HeadToIndex
                        } else {
                            DiffTarget::IndexToWorktree
                        },
                        active,
                        hovered,
                        "loom".into(),
                        binding.clone(),
                        store.clone().into(),
                        cx.focus_handle(UiId::new("git-label-editor")),
                    ));
                }
                root
            });
            let mut session = UiSession::new();
            session.render_view(&view, viewport, UiScale::ONE);
            for (index, (path, _, _, indicator, color, deleted)) in cases.iter().enumerate() {
                let top = index as f32 * CHANGE_ROW_H;
                let nodes = session
                    .tree()
                    .nodes()
                    .iter()
                    .filter(|node| {
                        node.layout_rect.top >= top && node.layout_rect.bottom <= top + CHANGE_ROW_H
                    })
                    .collect::<Vec<_>>();
                let filename = std::path::Path::new(path)
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap();
                let name = nodes
                    .iter()
                    .find(|node| node.text.as_deref() == Some(filename))
                    .unwrap();
                assert_eq!(name.text_style.unwrap().color, *color);
                let status = nodes
                    .iter()
                    .find(|node| node.text.as_deref() == Some(*indicator))
                    .unwrap();
                assert_eq!(status.text_style.unwrap().color, *color);
                let strikes = nodes
                    .iter()
                    .filter(|node| {
                        node.style.fill == Some(*color) && node.layout_rect.height() == 1.0
                    })
                    .collect::<Vec<_>>();
                assert_eq!(strikes.len(), usize::from(*deleted), "{path}");
                if let Some(strike) = strikes.first() {
                    assert_eq!(strike.layout_rect.left, name.layout_rect.left);
                    assert_eq!(strike.layout_rect.right, name.layout_rect.right - TEXT_PAD);
                }
                for folder in nodes
                    .iter()
                    .filter(|node| node.text.as_deref() == Some("src"))
                {
                    assert_eq!(folder.text_style.unwrap().color, theme::c().text_ghost);
                }
                // Hover actions must not cover the persistent status letter.
                for button in nodes.iter().filter(|node| {
                    node.semantics
                        .as_ref()
                        .is_some_and(|s| s.role == SemanticRole::Button)
                }) {
                    assert!(button.layout_rect.right <= status.layout_rect.left);
                }
            }
            save_drawer_preview(
                &session,
                &context,
                viewport,
                &format!("git-change-labels-{width}-{hovered}"),
            );
        }
    }

    #[test]
    fn drawer_filter_staging_and_multiline_commit_match_the_reference() {
        use crate::ui::git::commands::{Binding, Command, Frame};
        use crate::ui::git::state::GitPanelState;
        use lgui::application::{AppView, ApplicationContext};
        use lgui::session::UiSession;
        use std::sync::Mutex;

        let viewport = UiRect::new(0.0, 0.0, 288.0, 650.0);
        let (id, mut snapshot, _) = drawer_fixture();
        let paths = ["src/ui/panel.rs", "src/ui/state.rs", "docs/git.md"].map(PathBuf::from);
        let repo = Arc::make_mut(&mut snapshot.repositories)
            .get_mut(&id)
            .unwrap();
        repo.files = Arc::new(
            paths
                .iter()
                .enumerate()
                .map(|(index, path)| {
                    (
                        path.clone(),
                        FileState {
                            index: if index == 1 {
                                ChangeKind::Modified
                            } else {
                                ChangeKind::Unmodified
                            },
                            worktree: if index == 1 {
                                ChangeKind::Unmodified
                            } else {
                                ChangeKind::Modified
                            },
                            ..Default::default()
                        },
                    )
                })
                .collect(),
        );
        let prepared = Arc::make_mut(&mut snapshot.presentation)
            .repositories
            .get_mut(&id)
            .unwrap();
        prepared.sections = Default::default();
        prepared.sections[STAGED as usize] = vec![paths[1].clone()];
        prepared.sections[UNSTAGED as usize] = vec![paths[0].clone(), paths[2].clone()];
        prepared.dirty_count = 3;
        let exposed = Arc::new(Mutex::new(None));
        let output = exposed.clone();
        let commands = Arc::new(Mutex::new(Vec::<String>::new()));
        let captured = commands.clone();
        let view: AppView = Arc::new(move |cx| {
            let state = cx.state_with(GitPanelState::default);
            let viewport = cx.viewport();
            let fixture = snapshot.clone();
            let store = cx.state_with(move || fixture);
            *output.lock().unwrap() = Some((state.clone(), store.clone()));
            let requests = captured.clone();
            let binding = Binding::new(state.clone().into(), Frame::default, move |command| {
                let request = match command {
                    Command::StagePaths { paths } => format!("stage:{}", paths[0].display()),
                    Command::UnstagePaths { paths } => format!("unstage:{}", paths[0].display()),
                    Command::Commit => "commit".into(),
                    Command::CommitAndPush => "commit-push".into(),
                    Command::PressChange { .. } => "drag".into(),
                    _ => return true,
                };
                requests.lock().unwrap().push(request);
                true
            });
            let focus = cx.focus_handle(UiId::new("drawer-test-editor"));
            let mut root = render(
                viewport,
                binding.clone(),
                store.clone().into(),
                focus.clone(),
                cx.focus_handle(UiId::new("drawer-test-commit")),
                UiId::new("drawer-test-commit"),
                cx.focus_handle(UiId::new(crate::ui::git::drawer::FILTER_ID)),
            );
            if let Some(menu) = state.get().ui.menu {
                root = root.child(crate::ui::git::menu::render(
                    viewport,
                    &menu,
                    binding,
                    store.into(),
                    focus,
                ));
            }
            root
        });
        let context = ApplicationContext::empty(Default::default());
        context
            .resources()
            .provide(lgui::icons::IconRegistration(Arc::new(
                crate::app::svg_icons(),
            )));
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let (state, store) = exposed.lock().unwrap().clone().unwrap();
        let input_before = named(&session, "Commit message").unwrap();
        assert!(input_before.height() > 70.0);
        let send = |session: &mut UiSession, event| {
            dispatch_runtime_output(
                session.handle_input(event),
                &context,
                &lgui::window::WindowId::new("drawer-design-test"),
                |action| session.handle_default_action(action),
                |_| {},
            );
            session.runtime().run_effects();
            let updates = session.apply_pending_updates();
            let handlers = updates
                .focus_events
                .iter()
                .flat_map(|event| session.tree().handler_events(event))
                .collect::<Vec<_>>();
            for event in handlers {
                dispatch_event_handlers(
                    &event,
                    &context,
                    &lgui::window::WindowId::new("drawer-design-test"),
                );
            }
            session.render_view(&view, viewport, UiScale::ONE);
        };
        let activate = |session: &mut UiSession, name: &str| {
            let point = center(named(session, name).unwrap_or_else(|| panic!("Missing {name}")));
            for event in [
                InputEvent::PointerDown {
                    pointer: PointerData::mouse(point),
                    button: PointerButton::Left,
                },
                InputEvent::PointerUp {
                    pointer: PointerData::mouse(point),
                    button: PointerButton::Left,
                },
            ] {
                send(session, event);
            }
        };
        activate(&mut session, "Stage src/ui/panel.rs");
        activate(&mut session, "Unstage src/ui/state.rs");
        assert_eq!(
            *commands.lock().unwrap(),
            ["stage:src/ui/panel.rs", "unstage:src/ui/state.rs"]
        );
        state.update(|app| app.ui.file_filter.set_text("GIT.MD"));
        session.render_view(&view, viewport, UiScale::ONE);
        assert!(named(&session, "Stage docs/git.md").is_some());
        assert!(named(&session, "Stage src/ui/panel.rs").is_none());
        assert_eq!(named(&session, "Commit message").unwrap(), input_before);
        state.update(|app| app.ui.file_filter.clear());
        session.render_view(&view, viewport, UiScale::ONE);
        activate(&mut session, "Commit message");
        send(
            &mut session,
            InputEvent::TextInput("feat: Git drawer".into()),
        );
        send(
            &mut session,
            InputEvent::Keyboard(KeyboardEvent {
                key: LogicalKey::Named(NamedKey::Enter),
                state: KeyState::Down,
                ..Default::default()
            }),
        );
        send(&mut session, InputEvent::TextInput("正文说明".into()));
        assert_eq!(
            state.get().ui.commit_input.text(),
            "feat: Git drawer\n正文说明"
        );
        assert!(
            !commands
                .lock()
                .unwrap()
                .iter()
                .any(|command| command == "commit")
        );
        let input_node = session
            .tree()
            .nodes()
            .iter()
            .find(|node| {
                node.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some("Commit message")
            })
            .unwrap();
        assert!(input_node.semantics.as_ref().unwrap().state.multiline);
        save_drawer_preview(&session, &context, viewport, "git-drawer-aligned");
        activate(&mut session, "Commit changes");
        assert_eq!(commands.lock().unwrap().last().unwrap(), "commit");
        activate(&mut session, "Commit options");
        activate(&mut session, "Commit and Push");
        assert_eq!(commands.lock().unwrap().last().unwrap(), "commit-push");
        state.update(|app| {
            app.ui.menu = None;
            app.ui.commit_input.clear();
        });
        session.render_view(&view, viewport, UiScale::ONE);
        let button = session
            .tree()
            .nodes()
            .iter()
            .find(|node| {
                node.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some("Commit changes")
            })
            .unwrap();
        assert!(button.semantics.as_ref().unwrap().state.disabled);
        let before = commands.lock().unwrap().len();
        activate(&mut session, "Commit changes");
        assert_eq!(commands.lock().unwrap().len(), before);
        state.update(|app| app.ui.commit_amend = true);
        session.render_view(&view, viewport, UiScale::ONE);
        assert!(crate::ui::git::drawer::commit_reason(&state.get(), &store.get()).is_none());
        save_drawer_preview(&session, &context, viewport, "git-drawer-amend");
        let narrow = UiRect::new(0.0, 0.0, 160.0, 650.0);
        session.render_view(&view, narrow, UiScale::ONE);
        assert!(named(&session, "Commit options").unwrap().right <= narrow.right);
        assert!(named(&session, "Commit message").unwrap().right <= narrow.right);
        save_drawer_preview(&session, &context, narrow, "git-drawer-narrow");
    }

    #[test]
    fn the_git_panel_switches_between_history_stashes_worktrees_and_tags() {
        use lgui::application::ApplicationContext;
        use lgui::session::UiSession;
        let viewport = UiRect::new(0.0, 0.0, 800.0, 600.0);
        let panel = UiRect::new(0.0, 300.0, 800.0, 600.0);
        let (_, snapshot, history) = drawer_fixture();
        let exposed = Arc::new(std::sync::Mutex::new(None));
        let view = render_view(
            viewport,
            Some(panel),
            snapshot,
            history,
            |app| app.git.ui.set_panel(true),
            exposed.clone(),
        );
        let context = ApplicationContext::empty(Default::default());
        context
            .resources()
            .provide(lgui::icons::IconRegistration(Arc::new(
                crate::app::svg_icons(),
            )));
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let (state, _) = exposed.lock().unwrap().clone().unwrap();
        // History is the default tab; opening the panel expands only that section.
        assert_eq!(state.get().git.ui.panel_tab, HISTORY);
        assert!(!state.get().git.ui.collapsed_sections.contains(&HISTORY));
        assert!(state.get().git.ui.collapsed_sections.contains(&TAGS));
        assert!(
            text_nodes(&session)
                .iter()
                .any(|line| line.contains("Improve editor behavior 0"))
        );
        save_drawer_preview(&session, &context, viewport, "git-panel-commits");
        state.update(|app| {
            let oid = "abc00000";
            app.git.model.toggle_commit(oid.into());
            app.git.model.set_commit_files(
                oid,
                Ok(vec![
                    (ChangeKind::Added, "src/added.rs".into()),
                    (ChangeKind::Modified, "src/modified.rs".into()),
                    (ChangeKind::Deleted, "src/deleted.rs".into()),
                ]),
            );
            app.git.model.cache_commit_detail(
                oid.into(),
                Ok(CommitDetails {
                    author: "Ada".into(),
                    author_email: "ada@example.com".into(),
                    date: "2026-10-26".into(),
                    message: "Update source files".into(),
                    files_changed: 3,
                    insertions: 12,
                    deletions: 4,
                }),
            );
        });
        session.render_view(&view, viewport, UiScale::ONE);
        let summary_top = named(&session, "Copy commit hash").unwrap().top;
        for (name, color, deleted) in [
            ("added.rs", theme::c().git.added_text, false),
            ("modified.rs", theme::c().git.modified, false),
            ("deleted.rs", theme::c().git.deleted, true),
        ] {
            let nodes = session.tree().nodes();
            let label = nodes
                .iter()
                .find(|node| node.text.as_deref() == Some(name))
                .unwrap();
            assert_eq!(label.text_style.unwrap().color, color);
            assert!(
                label.layout_rect.bottom < summary_top,
                "changes precede the summary"
            );
            assert_eq!(
                nodes.iter().any(|node| node.style.fill == Some(color)
                    && node.layout_rect.left == label.layout_rect.left
                    && node.layout_rect.height() == 1.0
                    && node.layout_rect.top > label.layout_rect.top
                    && node.layout_rect.bottom < label.layout_rect.bottom),
                deleted,
            );
        }
        save_drawer_preview(&session, &context, viewport, "git-history-change-labels");
        // Ids are hashed, so find a detail-pane thumb by geometry: 8px wide, right of the
        // history list, and below or above the summary's top edge.
        let has_thumb = |session: &UiSession, region: ScrollRegion| {
            session.tree().nodes().iter().any(|node| {
                let rect = node.layout_rect;
                rect.width() == 8.0
                    && rect.left > 700.0
                    && match region {
                        ScrollRegion::CommitSummary => rect.top >= summary_top,
                        _ => rect.bottom < summary_top,
                    }
            })
        };
        assert!(!has_thumb(&session, ScrollRegion::CommitSummary));
        let send = |session: &mut UiSession, input| {
            dispatch_runtime_output(
                session.handle_input(input),
                &context,
                &lgui::window::WindowId::new("history-summary-scroll-test"),
                |action| session.handle_default_action(action),
                |_| {},
            );
            session.render_view(&view, viewport, UiScale::ONE);
        };
        let summary_point = Point::new(700.0, summary_top + 8.0);
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(summary_point)),
        );
        assert_eq!(
            state.get().git.ui.scroll_hover,
            Some(ScrollRegion::CommitSummary)
        );
        assert!(has_thumb(&session, ScrollRegion::CommitSummary));
        send(
            &mut session,
            InputEvent::Wheel {
                point: summary_point,
                delta: WheelDelta::pixels(0.0, -10000.0),
            },
        );
        let summary_scroll = state.get().git.ui.history_summary_scroll;
        assert!(summary_scroll > 0.0);
        assert_eq!(state.get().git.ui.history_files_scroll, 0.0);
        save_drawer_preview(&session, &context, viewport, "git-history-summary-scrolled");
        state.update(|app| {
            app.git.model.set_commit_files(
                "abc00000",
                Ok((0..30)
                    .map(|index| (ChangeKind::Modified, format!("src/file-{index}.rs").into()))
                    .collect()),
            )
        });
        session.render_view(&view, viewport, UiScale::ONE);
        let files_point = Point::new(700.0, panel.top + 80.0);
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(files_point)),
        );
        assert!(!has_thumb(&session, ScrollRegion::CommitSummary));
        assert!(has_thumb(&session, ScrollRegion::CommitFiles));
        send(
            &mut session,
            InputEvent::Wheel {
                point: files_point,
                delta: WheelDelta::pixels(0.0, -60.0),
            },
        );
        assert!(state.get().git.ui.history_files_scroll > 0.0);
        assert_eq!(state.get().git.ui.history_summary_scroll, summary_scroll);
        assert_eq!(state.get().git.ui.details_scroll, 0.0);
        crate::app::git::toggle_commit(&state, "loom".into(), "abc00001".into());
        assert_eq!(state.get().git.ui.history_summary_scroll, 0.0);
        assert_eq!(state.get().git.ui.history_files_scroll, 0.0);
        for (tab, expected) in [
            ("Stashes", "WIP: search panel"),
            ("Tags", "v1.0.0"),
            ("Worktrees", "loom-search"),
        ] {
            let target = named(&session, tab).unwrap_or_else(|| panic!("Missing tab {tab}"));
            click(&mut session, &view, viewport, center(target));
            assert!(
                text_nodes(&session)
                    .iter()
                    .any(|line| line.contains(expected)),
                "{tab} lists {expected}: {:?}",
                text_nodes(&session)
            );
            assert!(
                target.top >= panel.top
                    && target.bottom <= panel.top + crate::ui::git::workbench::HEADER_H
            );
        }
        assert_eq!(state.get().git.ui.panel_tab, WORKTREES);
        assert!(!state.get().git.ui.collapsed_sections.contains(&WORKTREES));
        assert!(state.get().git.ui.collapsed_sections.contains(&HISTORY));
        save_drawer_preview(&session, &context, viewport, "git-panel-worktrees");
        // Collapse the content while leaving every tab available.
        let hide = named(&session, "Collapse Git panel").unwrap();
        click(&mut session, &view, viewport, center(hide));
        assert!(!state.get().git.ui.panel_open);
        for section in [HISTORY, STASHES, WORKTREES, TAGS] {
            assert!(state.get().git.ui.collapsed_sections.contains(&section));
        }
        for tab in ["History", "Stashes", "Worktrees", "Tags"] {
            assert!(named(&session, tab).is_some());
        }
        assert!(named(&session, "Expand Git panel").is_some());
    }

    #[test]
    fn a_wide_git_panel_keeps_only_the_log_tab_for_the_navigator() {
        use lgui::application::ApplicationContext;
        use lgui::session::UiSession;
        let viewport = UiRect::new(0.0, 0.0, 1200.0, 600.0);
        let panel = UiRect::new(0.0, 300.0, 1200.0, 600.0);
        let (_, snapshot, history) = drawer_fixture();
        let exposed = Arc::new(std::sync::Mutex::new(None));
        let view = render_view(
            viewport,
            Some(panel),
            snapshot,
            history,
            |app| app.git.ui.set_panel(true),
            exposed,
        );
        let context = ApplicationContext::empty(Default::default());
        context
            .resources()
            .provide(lgui::icons::IconRegistration(Arc::new(
                crate::app::svg_icons(),
            )));
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        assert!(named(&session, "History").is_some());
        for tab in ["Stashes", "Worktrees", "Tags"] {
            assert!(
                named(&session, tab).is_none(),
                "{tab} moved to the navigator"
            );
        }
    }

    #[test]
    fn narrow_git_panel_tabs_scroll_without_covering_the_collapse_button() {
        use lgui::application::ApplicationContext;
        use lgui::session::UiSession;

        fn hit(session: &UiSession, name: &str) -> Option<UiRect> {
            let node = session.tree().nodes().iter().find(|node| {
                node.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some(name)
            })?;
            let mut rect = session.tree().hit_for_id(&node.id)?.rect;
            let mut parent = node.parent.as_ref();
            while let Some(id) = parent {
                let ancestor = session.tree().node(id)?;
                if let Some(clip) = ancestor.clip_rect {
                    let hit = session.tree().hit_for_id(id)?.rect;
                    let clip = clip.translate(
                        hit.left - ancestor.hit_rect.left,
                        hit.top - ancestor.hit_rect.top,
                    );
                    rect = UiRect::new(
                        rect.left.max(clip.left),
                        rect.top.max(clip.top),
                        rect.right.min(clip.right),
                        rect.bottom.min(clip.bottom),
                    );
                }
                parent = ancestor.parent.as_ref();
            }
            (rect.right > rect.left && rect.bottom > rect.top).then_some(rect)
        }

        for width in [200.0, 300.0, 800.0] {
            let viewport = UiRect::new(0.0, 0.0, 900.0, 600.0);
            let panel = UiRect::new(40.0, 300.0, 40.0 + width, 600.0);
            let (_, snapshot, history) = drawer_fixture();
            let exposed = Arc::new(std::sync::Mutex::new(None));
            let view = render_view(
                viewport,
                Some(panel),
                snapshot,
                history,
                |app| app.git.ui.set_panel(true),
                exposed.clone(),
            );
            let context = ApplicationContext::empty(Default::default());
            let _fonts = lgui::backend::install_text_environment(
                &context,
                lgui::render_skia::skia_text_system_handle(),
            );
            let mut session = UiSession::new();
            session.render_view(&view, viewport, UiScale::ONE);
            let (state, _) = exposed.lock().unwrap().clone().unwrap();
            let hide = hit(&session, "Collapse Git panel").unwrap();
            for tab in ["History", "Stashes", "Worktrees", "Tags"] {
                if let Some(rect) = hit(&session, tab) {
                    assert!(
                        rect.left >= panel.left && rect.right <= hide.left,
                        "{tab} overlaps the collapse button in a {width}px panel: {rect:?}"
                    );
                }
            }
            if width == 800.0 {
                assert!(hit(&session, "Next Git tabs").is_none());
            } else {
                // Both wheel axes scroll the header, never the list below it.
                let point = Point::new(panel.left + 60.0, panel.top + 15.0);
                for (delta, expected) in [
                    (WheelDelta::pixels(-10_000.0, 0.0), "Tags"),
                    (WheelDelta::pixels(0.0, 10_000.0), "History"),
                ] {
                    dispatch_runtime_output(
                        session.handle_input(InputEvent::Wheel { point, delta }),
                        &context,
                        &lgui::window::WindowId::new("git-panel-test"),
                        |action| session.handle_default_action(action),
                        |_| {},
                    );
                    session.render_view(&view, viewport, UiScale::ONE);
                    assert!(
                        hit(&session, expected).is_some(),
                        "Wheel scroll must reveal {expected}"
                    );
                    assert_eq!(state.get().git.ui.details_scroll, 0.0);
                }
                for _ in 0..4 {
                    if hit(&session, "Tags").is_some() {
                        break;
                    }
                    let next = hit(&session, "Next Git tabs").unwrap();
                    click(&mut session, &view, viewport, center(next));
                }
            }
            let tags = hit(&session, "Tags").expect("Tags must be reachable");
            click(&mut session, &view, viewport, center(tags));
            assert_eq!(state.get().git.ui.panel_tab, TAGS);
            assert!(text_nodes(&session).iter().any(|line| line == "v1.0.0"));
            let hide_after_scroll = hit(&session, "Collapse Git panel").unwrap();
            assert_eq!(hide_after_scroll, hide);
            click(&mut session, &view, viewport, center(hide_after_scroll));
            assert!(!state.get().git.ui.panel_open);
        }
    }

    #[test]
    fn the_git_panel_splits_the_editor_region_and_drags_to_resize() {
        use crate::ui::git::workbench::{HEADER_H, split};
        let region = UiRect::new(0.0, 32.0, 1000.0, 700.0);
        let (collapsed_editor, collapsed) = split(region, false, None);
        assert_eq!(collapsed.unwrap().height(), HEADER_H);
        assert_eq!(collapsed_editor.bottom, region.bottom - HEADER_H);
        let (editor, panel) = split(region, true, None);
        let panel = panel.unwrap();
        assert_eq!(editor.bottom, panel.top);
        assert_eq!(panel.bottom, region.bottom);
        assert_eq!(panel.height(), 340.0);
        let (_, tall) = split(region, true, Some(10_000.0));
        assert_eq!(tall.unwrap().top, region.top + 160.0);
        let (_, short) = split(region, true, Some(1.0));
        assert_eq!(short.unwrap().height(), 120.0);
        // If expanded content does not fit, its header still stays available.
        let small = UiRect::new(0.0, 0.0, 800.0, 250.0);
        let (editor, panel) = split(small, true, None);
        assert_eq!(panel.unwrap().height(), HEADER_H);
        assert_eq!(editor.height(), small.height() - HEADER_H);
        let tiny = UiRect::new(0.0, 0.0, 800.0, HEADER_H - 1.0);
        assert_eq!(split(tiny, true, None), (tiny, None));
    }

    #[test]
    fn bottom_categories_host_history_and_preserve_terminal_sessions() {
        use crate::app::terminal_binding::TerminalTabs;
        use crate::app::terminal_runtime::TerminalDisplay;
        use crate::domain::application_layout::ApplicationLayout;
        use crate::ui::terminal::state::PanelTab;
        use lgui::application::{AppView, ApplicationContext};
        use lgui::session::UiSession;
        let viewport = UiRect::new(0.0, 0.0, 1000.0, 700.0);
        let dock = UiRect::new(0.0, 300.0, 1000.0, 700.0);
        let (_, snapshot, history) = drawer_fixture();
        let exposed = Arc::new(std::sync::Mutex::new(None));
        let output = exposed.clone();
        let application = Arc::new(lgui::ApplicationHandle::new(|task| task(), || {}));
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                history(&mut app);
                app.terminal_chrome.show_terminal = true;
                app.layout.application_layout = Some(ApplicationLayout::initial(
                    viewport.into(),
                    100.0,
                    100.0,
                    300.0,
                ));
                app.workspace
                    .open_path("loom/main.rs".into(), "fn main() {}".into());
                app
            }));
            let tabs = crate::app::terminal_binding::test_store(cx, Some(state.clone()), || {
                let mut tabs = TerminalTabs::new();
                tabs.split(1);
                tabs
            });
            let current = tabs.get();
            state.try_update(|app| {
                let layout = app.layout.application_layout.as_mut().unwrap();
                let before = layout.version;
                current.sync_layout(layout);
                before != layout.version
            });
            let fixture = snapshot.clone();
            let store = cx.state_with(move || fixture);
            *output.lock().unwrap() = Some((state.clone(), tabs.clone(), store.clone()));
            let focus = cx.focus_handle(UiId::new("bottom-test-terminal"));
            let editor_focus = cx.focus_handle(UiId::new("bottom-test-editor"));
            let mut root = Element::new(move |cx| {
                UiElement::panel(
                    UiId::new("bottom-test-editor"),
                    UiRect::new(0.0, 0.0, 1000.0, 300.0),
                    VisualStyle::default(),
                )
                .children(cx.children)
            })
            .event_policy(EventPolicy::INTERACTIVE);
            let app = state.get();
            if !app.default_terminal_open() {
                return root;
            }
            let region = app
                .layout
                .application_layout
                .as_ref()
                .unwrap()
                .default_terminal_region();
            root = root.child(if app.git_panel_visible() {
                crate::app::terminal_view::render_commits(
                    dock,
                    state,
                    tabs,
                    store,
                    editor_focus,
                    focus,
                    application.clone(),
                    region,
                    600.0,
                    cx.focus_handle(UiId::new(crate::ui::git::workbench::HISTORY_QUERY_ID)),
                )
            } else {
                crate::app::terminal_view::render_region(
                    dock,
                    state,
                    crate::app::layout_map::MapStore::default(),
                    editor_focus,
                    focus,
                    UiId::new("bottom-test-terminal"),
                    tabs,
                    application.clone(),
                    true,
                    region.unwrap(),
                )
            });
            root
        });
        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let (state, tabs, store) = exposed.lock().unwrap().clone().unwrap();
        let handle = tabs.get().tabs()[0].controller.test_controller();
        let generation = handle.test_generation();
        handle.test_parser().process(b"preserved output\r\n");
        let terminal_button = session
            .tree()
            .node(&UiId::new("bottom-panel-terminal"))
            .unwrap()
            .layout_rect;
        let commits_button = named(&session, "Commits").unwrap();
        assert!(named(&session, "New terminal").is_some());
        save_drawer_preview(&session, &context, viewport, "bottom-panel-terminal");
        click(&mut session, &view, viewport, center(commits_button));
        assert_eq!(state.get().terminal.ui.panel_tab, PanelTab::Commits);
        assert!(state.get().git_panel_visible());
        assert!(!state.get().terminal.ui.terminal_focused);
        assert_eq!(
            session
                .tree()
                .node(&UiId::new("bottom-panel-terminal"))
                .unwrap()
                .layout_rect,
            terminal_button
        );
        assert_eq!(named(&session, "Commits"), Some(commits_button));
        assert!(named(&session, "New terminal").is_none());
        assert!(
            named(&session, "History").is_none(),
            "no nested legacy header"
        );
        assert!(named(&session, "Search commits").is_some());
        assert!(named(&session, "Branch").is_some());
        save_drawer_preview(&session, &context, viewport, "bottom-panel-commits");
        assert!(
            text_nodes(&session)
                .iter()
                .any(|line| line.contains("Improve editor behavior"))
        );
        assert!(crate::app::git_poll::history_key(&state.get(), &store.get()).is_some());
        state.update(|app| {
            app.git.ui.details_scroll = 44.0;
        });
        handle.test_parser().process(b"background output\r\n");
        click(&mut session, &view, viewport, center(terminal_button));
        assert_eq!(state.get().terminal.ui.panel_tab, PanelTab::Terminal);
        assert!(crate::app::git_poll::history_key(&state.get(), &store.get()).is_none());
        assert!(named(&session, "New terminal").is_some());
        assert_eq!(tabs.get().saved_groups(), vec![0, 0]);
        assert_eq!(handle.test_generation(), generation);
        assert!(
            handle
                .test_parser()
                .screen()
                .contents()
                .contains("background output")
        );
        click(&mut session, &view, viewport, center(commits_button));
        assert_eq!(state.get().git.ui.details_scroll, 44.0);
        let collapse = center(named(&session, "Hide bottom panel").unwrap());
        click(&mut session, &view, viewport, collapse);
        assert!(!state.get().default_terminal_open());
        assert_eq!(tabs.get().tabs().len(), 2);
        crate::app::terminal::close(
            &state,
            &tabs,
            1,
            crate::features::terminal::actions::MenuAction::All,
            &Arc::new(lgui::ApplicationHandle::new(|task| task(), || {})),
        );
        crate::app::terminal::select_panel(&state, &tabs, PanelTab::Commits);
        session.render_view(&view, viewport, UiScale::ONE);
        assert!(tabs.get().is_empty());
        assert!(
            text_nodes(&session)
                .iter()
                .any(|line| line.contains("Improve editor behavior"))
        );
        let terminal = center(
            session
                .tree()
                .node(&UiId::new("bottom-panel-terminal"))
                .unwrap()
                .layout_rect,
        );
        click(&mut session, &view, viewport, terminal);
        assert_eq!(tabs.get().tabs().len(), 1);
        assert!(named(&session, "New terminal").is_some());
    }

    #[test]
    fn history_header_expanded_bottom_rule_and_underline_are_crisp() {
        check_history_header_bottom(true);
    }

    #[test]
    fn history_header_collapsed_does_not_double_the_next_regions_border() {
        check_history_header_bottom(false);
    }

    fn check_history_header_bottom(expanded: bool) {
        use lgui::application::{AppView, ApplicationContext};
        use lgui::renderer::{FrameInfo, FrameReason};
        use lgui::session::UiSession;

        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        for factor in [0.75_f32, 1.0, 1.25, 1.5, 2.0] {
            let scale = UiScale::new(factor);
            for fraction in [0.0, 0.25, 0.5, 0.75] {
                let top = (80.0 + fraction) / factor;
                let header_bottom = top + crate::ui::git::workbench::HEADER_H;
                let bottom = if expanded {
                    header_bottom + 140.0
                } else {
                    header_bottom
                };
                let viewport = UiRect::new(0.0, 0.0, 640.0 / factor, bottom + theme::STATUS_H);
                let history = UiRect::new(0.0, top, viewport.right, bottom);
                let status = UiRect::new(0.0, bottom, viewport.right, viewport.bottom);
                let view: AppView = Arc::new(move |cx| {
                    let state = AppContext::detached(cx.state_with(|| {
                        let mut app = AppState::new();
                        app.git.ui.panel_open = expanded;
                        app
                    }));
                    let store = cx.state_with(GitStoreSnapshot::default);
                    panel(viewport, VisualStyle::filled(theme::c().bg))
                        .child(crate::ui::git::workbench::render(
                            history,
                            crate::app::git_view::binding(&state, &store),
                            store.into(),
                            cx.focus_handle(UiId::new("header-rule-editor")),
                        ))
                        .child(crate::app::chrome::status(status, state))
                });
                let mut session = UiSession::new();
                session.render_view(&view, viewport, scale);
                if factor == 1.0 && fraction == 0.0 {
                    save_drawer_preview(
                        &session,
                        &context,
                        viewport,
                        if expanded {
                            "history-header-expanded"
                        } else {
                            "history-header-collapsed"
                        },
                    );
                }
                // At higher zoom the tabs overflow; sample inside the clipped underline.
                let underline_x = if expanded {
                    let underline = session
                        .tree()
                        .node(&UiId::owned(format!("git-panel-tab-underline-{}", HISTORY)))
                        .unwrap();
                    scale.physical_value(underline.layout_rect.left) as usize + 4
                } else {
                    4
                };
                if expanded {
                    let point = Point::new(
                        scale.logical_value(underline_x as i32),
                        header_bottom - 0.25 / factor,
                    );
                    let tab = session
                        .tree()
                        .nodes()
                        .iter()
                        .find(|node| {
                            node.semantics.as_ref().and_then(|s| s.name.as_deref())
                                == Some("History")
                        })
                        .unwrap();
                    assert_eq!(session.tree().hit_test(point).unwrap().id, tab.id);
                    assert_eq!(session.tree().cursor_at(point), Some(CursorIcon::Pointer));
                }
                let physical = scale.physical_rect(viewport);
                let edge = scale.physical_value(header_bottom);
                let rule_width = scale.physical_length(1.0);
                let underline_width = scale.physical_length(2.0);
                let dirty = [PhysicalRect::new(0, edge - 5, physical.right, edge + 5)];
                let mut surface = lgui::render_skia::SkiaSoftwareSurface::new(4 * 1024 * 1024);
                for full in [true, false] {
                    let frame =
                        FrameInfo::new(physical, &dirty, scale, FrameReason::SceneChange, full);
                    lgui::assets::backend::with_render_resources(
                        &context,
                        lgui::RenderResources::new(),
                        || surface.draw(&session.tree().scene().project_to_physical(scale), &frame),
                    )
                    .unwrap();
                    let check = |x: usize, y: i32, color: Color| {
                        let offset = (y as usize * physical.width() as usize + x) * 4;
                        let expected = [
                            color.0 as u8,
                            (color.0 >> 8) as u8,
                            (color.0 >> 16) as u8,
                            255,
                        ];
                        let actual = &surface.pixels()[offset..offset + 4];
                        // Blending adjacent identical fills can round an RGB channel by one.
                        let tolerance = u8::from(color == theme::c().sidebar);
                        assert!(
                            actual[3] == 255
                                && actual[..3]
                                    .iter()
                                    .zip(&expected[..3])
                                    .all(|(&a, &b)| a.abs_diff(b) <= tolerance),
                            "expanded={expanded}, scale={factor}, fraction={fraction}, full={full}, x={x}, y={y}, actual={actual:?}, expected={expected:?}"
                        );
                    };
                    if expanded {
                        for y in edge - rule_width..edge {
                            check(400, y, theme::c().border);
                        }
                        for y in edge - underline_width..edge {
                            check(underline_x, y, theme::c().accent);
                        }
                        check(underline_x, edge - underline_width - 1, theme::c().sidebar);
                        check(underline_x, edge, theme::c().sidebar);
                    } else {
                        // The next region owns this edge. A collapsed header has no body to divide.
                        check(400, edge - 1, theme::c().sidebar);
                        for y in edge..edge + rule_width {
                            check(400, y, theme::c().border);
                        }
                        check(400, edge + rule_width, theme::c().sidebar);
                    }
                }
            }
        }
    }
    fn save_drawer_preview(
        session: &lgui::session::UiSession,
        context: &lgui::application::ApplicationContext,
        rect: UiRect,
        name: &str,
    ) {
        let Some(directory) = std::env::var_os("LOOM_GIT_PREVIEW_DIR") else {
            return;
        };
        use lgui::core::{PhysicalRect, UiScale};
        use lgui::renderer::{FrameInfo, FrameReason};
        let physical = PhysicalRect::new(0, 0, rect.width() as i32, rect.height() as i32);
        let dirty = [physical];
        let frame = FrameInfo::new(
            physical,
            &dirty,
            UiScale::ONE,
            FrameReason::SceneChange,
            true,
        );
        let mut surface = lgui::render_skia::SkiaSoftwareSurface::new(16 * 1024 * 1024);
        lgui::assets::backend::with_render_resources(
            context,
            lgui::assets::backend::render_resources(),
            || {
                surface.draw(
                    &session.tree().scene().project_to_physical(UiScale::ONE),
                    &frame,
                )
            },
        )
        .unwrap();
        let mut rgba = surface.pixels().to_vec();
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        image::save_buffer(
            std::path::Path::new(&directory).join(format!("{name}.png")),
            &rgba,
            rect.width() as u32,
            rect.height() as u32,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }

    #[test]
    fn worktree_delete_button_only_requests_confirmation() {
        use crate::ui::git::confirm::GitConfirmAction;
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{InputEvent, Point, PointerData, UiScale, dispatch_runtime_output};
        use lgui::session::UiSession;
        use std::sync::{Arc, Mutex};

        let exposed = Arc::new(Mutex::new(None::<AppContext>));
        let output = exposed.clone();
        let viewport = UiRect::new(0.0, 0.0, 300.0, 100.0);
        let source = PathBuf::from("worktree-ui-test-source");
        let target = PathBuf::from("worktree-ui-test-linked");
        let repository = RepositorySnapshot {
            id: crate::features::git::RepositoryId(1),
            worktree_root: source.clone(),
            git_dir: source.join(".git"),
            common_dir: source.join(".git"),
            head: crate::features::git::HeadState::Branch("main".into()),
            upstream: None,
            ahead: 0,
            behind: 0,
            files: Default::default(),
            ignored: Default::default(),
            repository_state: RepositoryState::Normal,
            operation_step: None,
            head_oid: None,
            stashes: Default::default(),
            tags: Default::default(),
            worktrees: Default::default(),
            features: Default::default(),
            generation: 0,
        };
        let worktree = WorktreeInfo {
            path: target.clone(),
            oid: "abc".into(),
            branch: Some("feature/x".into()),
            bare: false,
            detached: false,
            locked: None,
        };
        let view: AppView = Arc::new(move |cx| {
            let source = repository.worktree_root.clone();
            let state = AppContext::detached(cx.state_with(move || {
                let mut app = AppState::new();
                app.explorer.model.restore(vec![source], Vec::new());
                app
            }));
            let store = cx.state_with(GitStoreSnapshot::default);
            *output.lock().unwrap() = Some(state.clone());
            panel(viewport, VisualStyle::default()).child(worktree_row(
                UiRect::new(0.0, 0.0, 300.0, ROW_H * 2.0),
                &worktree,
                &repository,
                false,
                true,
                true,
                crate::app::git_view::binding(&state, &store),
                store.clone().into(),
            ))
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let state = exposed.lock().unwrap().clone().unwrap();
        let context = ApplicationContext::empty(Default::default());
        let point = Point::new(282.0, ROW_H);
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
            let events = session.handle_input(input);
            dispatch_runtime_output(
                events,
                &context,
                &lgui::window::WindowId::new("worktree-actions-test"),
                |action| session.handle_default_action(action),
                |_| {},
            );
            session.render_view(&view, viewport, UiScale::ONE);
        }
        let app = state.get();
        assert_eq!(app.explorer.model.roots(), std::slice::from_ref(&source));
        assert!(!app.git.model.has_pinned_repository());
        assert!(app.chrome.toast.is_none()); // The click did not also open the row.
        let confirm = app.git.ui.confirm.unwrap();
        assert!(confirm.destructive);
        assert_eq!(
            confirm.action,
            GitConfirmAction::RemoveWorktree {
                repository: source,
                path: target,
                force: false
            }
        );
    }

    #[test]
    fn scrollbar_is_separate_from_resize_handle_and_drags_outside_its_strip() {
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{InputEvent, Point, PointerData, UiScale, dispatch_runtime_output};
        use lgui::session::UiSession;
        use std::sync::{Arc, Mutex};

        for region in [
            ScrollRegion::Changes,
            ScrollRegion::Details,
            ScrollRegion::CommitSummary,
        ] {
            let exposed = Arc::new(Mutex::new(None::<AppContext>));
            let output = exposed.clone();
            let viewport = UiRect::new(0.0, 0.0, 300.0, 300.0);
            let view: AppView = Arc::new(move |cx| {
                let state = AppContext::detached(cx.state_with(AppState::new));
                let store = cx.state(GitStoreSnapshot::default());
                *output.lock().unwrap() = Some(state.clone());
                let scroll = region.scroll(&state.get().git);
                panel(viewport, VisualStyle::default())
                    .child(git_vertical_scrollbar(
                        viewport,
                        1200.0,
                        scroll,
                        crate::app::git_view::binding(&state, &store),
                        region,
                    ))
                    .child(git_resize_handle(
                        viewport,
                        crate::app::git_view::binding(&state, &store),
                        false,
                    ))
            });
            let mut session = UiSession::new();
            session.render_view(&view, viewport, UiScale::ONE);
            let state = exposed.lock().unwrap().clone().unwrap();
            // Grab the thumb near the right edge without entering the resize strip.
            let point = Point::new(viewport.right - 10.0, 12.0);
            assert_eq!(session.tree().cursor_at(point), None);
            assert_eq!(
                session
                    .tree()
                    .cursor_at(Point::new(viewport.right - 4.0, 12.0)),
                Some(CursorIcon::ResizeHorizontal)
            );
            let context = ApplicationContext::empty(Default::default());
            let mut send = |input| {
                let events = session.handle_input(input);
                dispatch_runtime_output(
                    events,
                    &context,
                    &lgui::window::WindowId::new("git-drawer-scrollbar-test"),
                    |action| session.handle_default_action(action),
                    |_| {},
                );
                session.render_view(&view, viewport, UiScale::ONE);
            };
            send(InputEvent::PointerDown {
                pointer: PointerData::mouse(point),
                button: PointerButton::Left,
            });
            assert!(region.drag(&state.get().git).0);
            assert!(!state.get().source_control.resizing);
            let outside = PointerData::mouse(Point::new(30.0, 260.0));
            send(InputEvent::PointerMove(outside));
            assert!(region.scroll(&state.get().git) > 0.0);
            let other = match region {
                ScrollRegion::Changes => ScrollRegion::Details,
                ScrollRegion::Details
                | ScrollRegion::Navigator
                | ScrollRegion::CommitFiles
                | ScrollRegion::CommitSummary => ScrollRegion::Changes,
            };
            assert_eq!(other.scroll(&state.get().git), 0.0);
            send(InputEvent::PointerUp {
                pointer: outside,
                button: PointerButton::Left,
            });
            assert!(!region.drag(&state.get().git).0);
            let released = region.scroll(&state.get().git);
            send(InputEvent::PointerMove(PointerData::mouse(point)));
            assert_eq!(region.scroll(&state.get().git), released);
        }
    }

    #[test]
    fn a_hundred_thousand_changes_only_build_the_viewport_rows() {
        let first = visible_rows(0.0, 0.0, 480.0, 100_000);
        let middle = visible_rows(0.0, 50_000.0 * ROW_H, 50_000.0 * ROW_H + 480.0, 100_000);
        let viewport_rows = (480.0 / ROW_H).ceil() as usize + 2;
        assert!(first.len() <= viewport_rows);
        assert!(middle.len() <= viewport_rows);
        assert!(middle.start > 100); // Entries after the old 100-file cap are reachable.
        let end = visible_rows(0.0, 100_000.0 * ROW_H, 100_000.0 * ROW_H + 480.0, 100_000);
        assert_eq!(end.end, 100_000);
    }

    #[test]
    fn tag_hover_card_shows_metadata_and_stays_inside_the_viewport() {
        use crate::ui::git::{
            commands::{Binding, Command, Frame},
            state::GitPanelState,
            tag_card,
        };
        use lgui::application::{AppView, ApplicationContext};
        use lgui::session::UiSession;
        use std::sync::Mutex;
        let viewport = UiRect::new(0.0, 0.0, 900.0, 600.0);
        let drawer = UiRect::new(600.0, 0.0, 900.0, 300.0);
        let panel_rect = UiRect::new(600.0, 300.0, 900.0, 600.0);
        let tag = TagInfo {
            name: "releases/v2.0".into(),
            commit: Some("abc0123456789".into()),
        };
        let repository = RepositorySnapshot {
            id: RepositoryId(1),
            worktree_root: "repo".into(),
            git_dir: "repo/.git".into(),
            common_dir: "repo/.git".into(),
            head: HeadState::Branch("main".into()),
            head_oid: Some("abc0123456789".into()),
            upstream: None,
            ahead: 0,
            behind: 0,
            files: Default::default(),
            ignored: Default::default(),
            repository_state: RepositoryState::Normal,
            operation_step: None,
            stashes: Default::default(),
            worktrees: Default::default(),
            tags: Arc::new(vec![tag.clone()]),
            features: Default::default(),
            generation: 0,
        };
        let details = TagDetails {
            target: "abc0123456789".into(),
            target_kind: "commit".into(),
            annotation: Some(TagAnnotation {
                tagger: "Release Author".into(),
                email: "release@example.com".into(),
                date: "2026-10-15 13:00".into(),
                message: "Release editor improvements\n包含标签面板和提交轨道。".into(),
            }),
            commit: Some(CommitDetails {
                author: "Commit Author".into(),
                author_email: "dev@example.com".into(),
                date: "2026-10-15 12:00".into(),
                message: "Improve Git drawer".into(),
                files_changed: 3,
                insertions: 21,
                deletions: 4,
            }),
        };
        let exposed = Arc::new(Mutex::new(None::<State<GitPanelState>>));
        let output = exposed.clone();
        let source_tag = tag.clone();
        let view: AppView = Arc::new(move |cx| {
            let viewport = cx.viewport();
            let state = cx.state_with(|| {
                let mut app = GitPanelState::default();
                app.ui.select_panel_tab(TAGS);
                app
            });
            *output.lock().unwrap() = Some(state.clone());
            let store = cx.state_with(|| GitStoreSnapshot {
                active_repository: Some(repository.id),
                repositories: Arc::new([(repository.id, repository.clone())].into_iter().collect()),
                ..Default::default()
            });
            let target = state.clone();
            let tag = source_tag.clone();
            let details = details.clone();
            let binding = Binding::new(state.clone().into(), Frame::default, move |command| {
                if let Command::HoverTag { root, name, anchor } = command {
                    assert_eq!(root, PathBuf::from("repo"));
                    assert_eq!(name, tag.name);
                    target.update(|app| {
                        app.ui.tag_card = Some(TagCard {
                            repository: root,
                            tag: tag.clone(),
                            anchor,
                            details: Ok(details.clone()),
                        })
                    });
                }
                true
            });
            let mut root = panel(viewport, VisualStyle::filled(theme::c().bg)).child(render(
                drawer,
                binding.clone(),
                store.clone().into(),
                cx.focus_handle(UiId::new("tag-hover-editor")),
                cx.focus_handle(UiId::new("tag-hover-commit")),
                UiId::new("tag-hover-commit"),
                cx.focus_handle(UiId::new(crate::ui::git::drawer::FILTER_ID)),
            ));
            root = root.child(crate::ui::git::workbench::render(
                panel_rect,
                binding.clone(),
                store.clone().into(),
                cx.focus_handle(UiId::new("tag-hover-editor")),
            ));
            if let Some(card) = tag_card::render(viewport, &state.get(), &repository) {
                root = root.child(card);
            }
            root
        });
        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        context
            .resources()
            .provide(lgui::icons::IconRegistration(Arc::new(
                crate::app::svg_icons(),
            )));
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let state = exposed.lock().unwrap().clone().unwrap();
        assert!(state.get().ui.tag_card.is_none());
        let row = session
            .tree()
            .nodes()
            .iter()
            .find(|node| {
                node.semantics.as_ref().and_then(|s| s.name.as_deref())
                    == Some("Tag: releases/v2.0")
            })
            .unwrap();
        let rect = session.tree().hit_for_id(&row.id).unwrap().rect;
        dispatch_runtime_output(
            session.handle_input(InputEvent::PointerMove(PointerData::mouse(Point::new(
                rect.left + 40.0,
                rect.top + ROW_H / 2.0,
            )))),
            &context,
            &lgui::window::WindowId::new("tag-hover-test"),
            |action| session.handle_default_action(action),
            |_| {},
        );
        session.render_view(&view, viewport, UiScale::ONE);
        for expected in [
            "releases/v2.0",
            "Annotated tag",
            "Release Author",
            "Commit Author",
            "Improve Git drawer",
        ] {
            assert!(
                session
                    .tree()
                    .nodes()
                    .iter()
                    .any(|node| node.text.as_deref() == Some(expected)),
                "Missing {expected}"
            );
        }
        let card_bounds = |session: &UiSession| {
            session
                .tree()
                .nodes()
                .iter()
                .find(|node| {
                    node.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some("Tag details")
                })
                .unwrap()
                .layout_rect
        };
        let bounds = card_bounds(&session);
        assert!(
            bounds.left >= viewport.left
                && bounds.right <= viewport.right
                && bounds.top >= viewport.top
                && bounds.bottom <= viewport.bottom
        );
        save_drawer_preview(&session, &context, viewport, "git-tag-hover-annotated");
        state.update(|app| {
            let details = app.ui.tag_card.as_mut().unwrap().details.as_mut().unwrap();
            details.annotation = None;
            details.commit.as_mut().unwrap().message =
                "A long commit message with many lines.\n".repeat(60);
        });
        session.render_view(&view, UiRect::new(0.0, 0.0, 900.0, 280.0), UiScale::ONE);
        assert!(card_bounds(&session).bottom <= 280.0);
        assert!(
            session
                .tree()
                .nodes()
                .iter()
                .any(|node| node.text.as_deref() == Some("Lightweight tag"))
        );
        state.update(|app| ScrollRegion::Details.set_scroll(app, 10.0));
        assert!(state.get().ui.tag_card.is_none());
        session.render_view(&view, viewport, UiScale::ONE);
        assert!(!session.tree().nodes().iter().any(|node| {
            node.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some("Tag details")
        }));
    }

    #[test]
    fn branch_and_commit_actions_create_tags_and_tag_menu_deletes_through_the_dialogs() {
        use crate::ui::components::state_access::StateAccess;
        use crate::ui::git::{
            commands::{Binding, Command, Frame},
            confirm, menu,
        };
        use lgui::application::{AppView, ApplicationContext};
        use lgui::session::UiSession;
        use std::sync::Mutex;
        let viewport = UiRect::new(0.0, 0.0, 640.0, 480.0);
        let requests = Arc::new(Mutex::new(Vec::<(String, PathBuf, String)>::new()));
        let captured = requests.clone();
        let exposed = Arc::new(Mutex::new(None::<AppContext>));
        let output = exposed.clone();
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.git.ui.select_panel_tab(TAGS);
                app.git.model.set_history(GitHistory {
                    repository: "repo".into(),
                    head: Some("abc00000".into()),
                    commits: Ok(vec![CommitSummary {
                        oid: "fed00000".into(),
                        parents: Vec::new(),
                        author: "Author".into(),
                        author_email: "author@example.com".into(),
                        timestamp: 0,
                        subject: "Earlier change".into(),
                        decorations: Vec::new(),
                    }]),
                });
                app
            }));
            let store = cx.state_with(|| {
                let repository = RepositorySnapshot {
                    id: RepositoryId(1),
                    worktree_root: "repo".into(),
                    git_dir: "repo/.git".into(),
                    common_dir: "repo/.git".into(),
                    head: HeadState::Branch("main".into()),
                    head_oid: Some("abc00000".into()),
                    upstream: None,
                    ahead: 0,
                    behind: 0,
                    files: Default::default(),
                    ignored: Default::default(),
                    repository_state: RepositoryState::Normal,
                    operation_step: None,
                    stashes: Default::default(),
                    worktrees: Default::default(),
                    tags: Arc::new(vec![TagInfo {
                        name: "v1.0".into(),
                        commit: Some("abc00000".into()),
                    }]),
                    features: Default::default(),
                    generation: 0,
                };
                GitStoreSnapshot {
                    repositories: Arc::new([(repository.id, repository)].into_iter().collect()),
                    active_repository: Some(RepositoryId(1)),
                    ..Default::default()
                }
            });
            *output.lock().unwrap() = Some(state.clone());
            let dispatch_state = state.clone();
            let dispatch_store = store.clone();
            let captured = captured.clone();
            let binding = Binding::new(
                StateAccess::project(state.model(), |app| &app.git, |app| &mut app.git),
                Frame::default,
                move |command| match command {
                    Command::CreateTag { root, name, target } => {
                        assert_eq!(
                            target,
                            if name == "releases/previous" {
                                "fed00000"
                            } else {
                                "def00000"
                            },
                            "Use the selected source commit instead of HEAD"
                        );
                        captured.lock().unwrap().push(("create".into(), root, name));
                        true
                    }
                    Command::DeleteTag { root, name } => {
                        captured.lock().unwrap().push(("delete".into(), root, name));
                        true
                    }
                    Command::OpenCommitChanges { .. } => {
                        panic!("Right-click must not open commit changes")
                    }
                    Command::Checkout { .. } => panic!("Creating a tag must not switch branches"),
                    command => crate::app::git_view::run(&dispatch_state, &dispatch_store, command),
                },
            );
            let focus = cx.focus_handle(UiId::new("tag-menu-editor"));
            let input_focus = cx.focus_handle(UiId::new(confirm::TAG_INPUT_ID));
            let create_open = state.get().git.ui.confirm.as_ref().is_some_and(|confirm| {
                matches!(confirm.action, GitConfirmAction::CreateTag { .. })
            });
            let mounted = input_focus.clone();
            cx.use_effect(create_open, move || {
                if create_open {
                    mounted.focus();
                }
            });
            let mut root = render(
                UiRect::new(0.0, 0.0, viewport.right, viewport.height() / 2.0),
                binding.clone(),
                store.clone().into(),
                focus.clone(),
                cx.focus_handle(UiId::new("tag-menu-commit")),
                UiId::new("tag-menu-commit"),
                cx.focus_handle(UiId::new(crate::ui::git::drawer::FILTER_ID)),
            )
            .child(crate::ui::git::workbench::render(
                UiRect::new(
                    0.0,
                    viewport.height() / 2.0,
                    viewport.right,
                    viewport.bottom,
                ),
                binding.clone(),
                store.clone().into(),
                focus.clone(),
            ));
            if let Some(picker) = state.get().git.ui.branch_picker {
                root = root.child(crate::ui::git::branch_picker::render(
                    viewport,
                    24.0,
                    &picker,
                    state.get().git.ui.branch_picker_input.text(),
                    binding.clone(),
                    store.clone().into(),
                    cx.focus_handle(UiId::new("tag-test-branch-input")),
                    UiId::new("tag-test-branch-input"),
                ));
            }
            if let Some(menu) = state.get().git.ui.menu {
                root = root.child(menu::render(
                    viewport,
                    &menu,
                    binding.clone(),
                    store.clone().into(),
                    focus,
                ));
            }
            if let Some(confirm) = state.get().git.ui.confirm {
                root = root.child(confirm::render(
                    viewport,
                    &confirm,
                    binding,
                    store.clone().into(),
                    UiId::new("tag-menu-dialog"),
                    input_focus,
                ));
            }
            root
        });
        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        context
            .resources()
            .provide(lgui::icons::IconRegistration(Arc::new(
                crate::app::svg_icons(),
            )));
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let state = exposed.lock().unwrap().clone().unwrap();
        let rect = |session: &UiSession, name: &str| {
            let node = session
                .tree()
                .nodes()
                .iter()
                .find(|node| {
                    node.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some(name)
                        || node.text.as_deref() == Some(name)
                })
                .unwrap_or_else(|| panic!("Missing {name}"));
            session.tree().hit_for_id(&node.id).unwrap().rect
        };
        let send = |session: &mut UiSession, input| {
            dispatch_runtime_output(
                session.handle_input(input),
                &context,
                &lgui::window::WindowId::new("tag-menu-test"),
                |action| session.handle_default_action(action),
                |_| {},
            );
            session.render_view(&view, viewport, UiScale::ONE);
        };
        let click = |session: &mut UiSession, name: &str, button| {
            let rect = rect(session, name);
            let point =
                PointerData::mouse(Point::new(rect.left + 12.0, (rect.top + rect.bottom) / 2.0));
            send(
                session,
                InputEvent::PointerDown {
                    pointer: point,
                    button,
                },
            );
            send(
                session,
                InputEvent::PointerUp {
                    pointer: point,
                    button,
                },
            );
        };
        assert!(
            !session
                .tree()
                .nodes()
                .iter()
                .any(|node| node.text.as_deref() == Some("New Tag")),
            "Tag creation belongs to the branch picker"
        );
        state.update(|app| {
            app.git.ui.branch_picker = Some(crate::ui::git::branch_picker::BranchPicker {
                repository: "repo".into(),
                repository_name: "repo".into(),
                anchor_x: 8.0,
                listing: BranchListing::Ready {
                    branches: vec![BranchInfo {
                        name: "feature/release".into(),
                        remote: false,
                        current: false,
                        oid: "def00000".into(),
                        upstream: None,
                        worktree_path: None,
                    }]
                    .into(),
                    worktrees: Arc::from([]),
                },
                collapsed_worktrees: Default::default(),
                selected: 0,
                scroll: 0.0,
                query: String::new(),
                sequence: 1,
                restore_terminal: false,
            });
        });
        session.render_view(&view, viewport, UiScale::ONE);
        save_drawer_preview(&session, &context, viewport, "git-branch-new-tag-action");
        click(&mut session, "New Tag", PointerButton::Left);
        assert!(!state.get().git.ui.collapsed_sections.contains(&TAGS));
        assert!(state.get().git.ui.branch_picker.is_none());
        assert!(
            matches!(
                state.get().git.ui.confirm.map(|confirm| confirm.action),
                Some(GitConfirmAction::CreateTag { .. })
            ),
            "The branch action must open its dialog"
        );
        assert!(
            session
                .tree()
                .nodes()
                .iter()
                .any(|node| node.text.as_deref() == Some("feature/release (def00000)"))
        );
        let focus_input = |session: &mut UiSession| {
            session.runtime().run_effects();
            let updates = session.apply_pending_updates();
            let handlers = updates
                .focus_events
                .iter()
                .flat_map(|event| session.tree().handler_events(event))
                .collect::<Vec<_>>();
            for event in handlers {
                dispatch_event_handlers(
                    &event,
                    &context,
                    &lgui::window::WindowId::new("tag-menu-test"),
                );
            }
            session.render_view(&view, viewport, UiScale::ONE);
        };
        focus_input(&mut session);
        assert!(
            state.get().git.ui.tag_input.focused,
            "Focused node: {:?}",
            session.runtime().interaction_state().focused
        );
        // Invalid names keep the form open and never issue a write request.
        send(&mut session, InputEvent::TextInput("bad name".into()));
        click(&mut session, "Create", PointerButton::Left);
        assert!(state.get().git.ui.confirm.is_some());
        assert!(requests.lock().unwrap().is_empty());
        state.update(|app| app.git.ui.tag_input.set_text("releases/v2.0"));
        session.render_view(&view, viewport, UiScale::ONE);
        save_drawer_preview(&session, &context, viewport, "git-tag-create-dialog");
        click(&mut session, "Create", PointerButton::Left);
        assert!(state.get().git.ui.confirm.is_none());
        assert_eq!(
            *requests.lock().unwrap(),
            vec![("create".into(), "repo".into(), "releases/v2.0".into())]
        );
        state.update(|app| app.git.ui.select_panel_tab(HISTORY));
        session.render_view(&view, viewport, UiScale::ONE);
        click(&mut session, "Earlier change", PointerButton::Right);
        assert!(
            matches!(state.get().git.ui.menu.unwrap().target, GitMenuTarget::HistoryCommit { oid, .. } if oid == "fed00000")
        );
        save_drawer_preview(&session, &context, viewport, "git-commit-new-tag-menu");
        click(&mut session, "New Tag…", PointerButton::Left);
        assert!(
            matches!(state.get().git.ui.confirm.unwrap().action, GitConfirmAction::CreateTag { branch: None, target, .. } if target == "fed00000")
        );
        assert!(
            session
                .tree()
                .nodes()
                .iter()
                .any(|node| node.text.as_deref() == Some("Commit"))
        );
        assert!(
            session
                .tree()
                .nodes()
                .iter()
                .any(|node| node.text.as_deref() == Some("fed00000"))
        );
        focus_input(&mut session);
        send(
            &mut session,
            InputEvent::TextInput("releases/previous".into()),
        );
        save_drawer_preview(&session, &context, viewport, "git-commit-new-tag-dialog");
        click(&mut session, "Create", PointerButton::Left);
        assert!(state.get().git.ui.confirm.is_none());
        assert_eq!(
            requests.lock().unwrap().last().unwrap(),
            &("create".into(), "repo".into(), "releases/previous".into())
        );
        assert_eq!(requests.lock().unwrap().len(), 2);
        state.update(|app| app.git.ui.select_panel_tab(TAGS));
        session.render_view(&view, viewport, UiScale::ONE);
        click(&mut session, "Tag: v1.0", PointerButton::Right);
        assert!(
            matches!(state.get().git.ui.menu.unwrap().target, GitMenuTarget::Tags { name, .. } if name == "v1.0")
        );
        let entries = menu::entries(
            &state.get().git.ui.menu.unwrap().target,
            &state.get().git,
            &GitStoreSnapshot::default(),
        );
        assert_eq!(entries.len(), 3);
        assert!(
            matches!(&entries[0], menu::Entry::Item { label, .. } if label == "Push Tag to Remote…")
        );
        assert!(matches!(&entries[2], menu::Entry::Item { label, .. } if label == "Delete Tag…"));
        save_drawer_preview(&session, &context, viewport, "git-tag-context-menu");
        click(&mut session, "Delete Tag…", PointerButton::Left);
        click(&mut session, "Cancel", PointerButton::Left);
        assert_eq!(requests.lock().unwrap().len(), 2);
        click(&mut session, "Tag: v1.0", PointerButton::Right);
        click(&mut session, "Delete Tag…", PointerButton::Left);
        save_drawer_preview(&session, &context, viewport, "git-tag-delete-dialog");
        click(&mut session, "Delete", PointerButton::Left);
        assert_eq!(
            requests.lock().unwrap().last().unwrap(),
            &("delete".into(), "repo".into(), "v1.0".into())
        );
    }

    #[test]
    fn tag_push_menu_selects_the_exact_remote_and_tag() {
        use crate::ui::git::{
            commands::{Binding, Command, Frame},
            menu,
            state::GitPanelState,
        };
        use lgui::application::{AppView, ApplicationContext};
        use lgui::session::UiSession;
        use std::sync::Mutex;
        let viewport = UiRect::new(0.0, 0.0, 420.0, 260.0);
        let requests = Arc::new(Mutex::new(Vec::<(PathBuf, String, String)>::new()));
        let captured = requests.clone();
        let exposed = Arc::new(Mutex::new(None::<State<GitPanelState>>));
        let output = exposed.clone();
        let view: AppView = Arc::new(move |cx| {
            let state = cx.state_with(|| {
                let mut app = GitPanelState::default();
                app.ui.menu = Some(GitMenu {
                    pos: (24.0, 24.0),
                    target: GitMenuTarget::Tags {
                        repository: "repo".into(),
                        name: "releases/v1".into(),
                    },
                    hover: None,
                });
                app
            });
            *output.lock().unwrap() = Some(state.clone());
            let target = state.clone();
            let captured = captured.clone();
            let binding = Binding::new(state.clone().into(), Frame::default, move |command| {
                match command {
                    Command::RequestPushTag { root, name, pos } => target.update(|app| {
                        app.ui.menu = Some(GitMenu {
                            pos,
                            target: GitMenuTarget::TagPushRemotes {
                                repository: root,
                                name,
                                request: 7,
                                remotes: None,
                            },
                            hover: None,
                        });
                    }),
                    Command::PushTag { root, name, remote } => {
                        captured.lock().unwrap().push((root, name, remote))
                    }
                    _ => panic!("Unexpected tag push menu command"),
                }
                true
            });
            let store = cx.state_with(GitStoreSnapshot::default);
            let mut root = panel(viewport, VisualStyle::filled(theme::c().bg));
            if let Some(menu) = state.get().ui.menu {
                root = root.child(menu::render(
                    viewport,
                    &menu,
                    binding,
                    store.into(),
                    cx.focus_handle(UiId::new("tag-push-editor")),
                ));
            }
            root
        });
        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let state = exposed.lock().unwrap().clone().unwrap();
        let click = |session: &mut UiSession, label: &str| {
            let node = session
                .tree()
                .nodes()
                .iter()
                .find(|node| node.text.as_deref() == Some(label))
                .unwrap_or_else(|| panic!("Missing menu entry {label}"));
            let rect = session.tree().hit_for_id(&node.id).unwrap().rect;
            let pointer =
                PointerData::mouse(Point::new(rect.left + 12.0, (rect.top + rect.bottom) / 2.0));
            for input in [
                InputEvent::PointerDown {
                    pointer,
                    button: PointerButton::Left,
                },
                InputEvent::PointerUp {
                    pointer,
                    button: PointerButton::Left,
                },
            ] {
                dispatch_runtime_output(
                    session.handle_input(input),
                    &context,
                    &lgui::window::WindowId::new("tag-push-menu-test"),
                    |action| session.handle_default_action(action),
                    |_| {},
                );
                session.render_view(&view, viewport, UiScale::ONE);
            }
        };
        click(&mut session, "Push Tag to Remote…");
        assert!(
            session
                .tree()
                .nodes()
                .iter()
                .any(|node| node.text.as_deref() == Some("Loading remotes…"))
        );
        assert!(requests.lock().unwrap().is_empty());
        state.update(|app| {
            let menu = app.ui.menu.as_mut().unwrap();
            assert!(!menu.finish_tag_remotes(6, Ok(vec!["stale".into()])));
            assert!(menu.finish_tag_remotes(7, Ok(vec!["origin".into(), "upstream".into()])));
        });
        session.render_view(&view, viewport, UiScale::ONE);
        assert!(
            session
                .tree()
                .nodes()
                .iter()
                .any(|node| node.text.as_deref() == Some("Push to origin"))
        );
        save_drawer_preview(&session, &context, viewport, "git-tag-push-remotes");
        click(&mut session, "Push to upstream");
        assert!(state.get().ui.menu.is_none());
        assert_eq!(
            *requests.lock().unwrap(),
            vec![("repo".into(), "releases/v1".into(), "upstream".into())]
        );
    }

    #[test]
    fn tag_left_click_does_not_open_commit_changes() {
        use crate::ui::git::commands::{Binding, Command, Frame};
        use crate::ui::git::state::GitPanelState;
        use lgui::application::{AppView, ApplicationContext};
        use lgui::session::UiSession;
        use std::sync::Mutex;

        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let viewport = UiRect::new(0.0, 0.0, 180.0, 80.0);
        let view: AppView = Arc::new(move |cx| {
            let state = cx.state_with(GitPanelState::default);
            let captured = captured.clone();
            let binding = Binding::new(state.into(), Frame::default, move |command| {
                if let Command::OpenCommitChanges { root, oid, focus } = command {
                    captured.lock().unwrap().push((root, oid, focus));
                }
                true
            });
            let mut rows = panel(viewport, VisualStyle::default());
            for (index, tag) in [
                TagInfo {
                    name: "releases/very-long-release-name".into(),
                    commit: Some("resolved-commit".into()),
                },
                TagInfo {
                    name: "tree".into(),
                    commit: None,
                },
            ]
            .iter()
            .enumerate()
            {
                rows = rows.child(tag_row(
                    UiRect::new(
                        0.0,
                        index as f32 * ROW_H,
                        viewport.right,
                        (index + 1) as f32 * ROW_H,
                    ),
                    tag,
                    false,
                    std::path::Path::new("repo"),
                    &binding,
                ));
            }
            rows
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let tag = session
            .tree()
            .nodes()
            .iter()
            .find(|node| {
                node.semantics.as_ref().is_some_and(|semantics| {
                    semantics.name.as_deref() == Some("Tag: releases/very-long-release-name")
                })
            })
            .unwrap();
        assert_eq!(tag.semantics.as_ref().unwrap().role, SemanticRole::Label);
        let tree_tag = session
            .tree()
            .nodes()
            .iter()
            .find(|node| {
                node.semantics
                    .as_ref()
                    .is_some_and(|semantics| semantics.name.as_deref() == Some("Tag: tree"))
            })
            .unwrap();
        assert_eq!(
            tree_tag.semantics.as_ref().unwrap().role,
            SemanticRole::Label
        );
        let context = ApplicationContext::empty(Default::default());
        for y in [ROW_H / 2.0, ROW_H * 1.5] {
            let point = PointerData::mouse(Point::new(40.0, y));
            for input in [
                InputEvent::PointerDown {
                    pointer: point,
                    button: PointerButton::Left,
                },
                InputEvent::PointerUp {
                    pointer: point,
                    button: PointerButton::Left,
                },
            ] {
                dispatch_runtime_output(
                    session.handle_input(input),
                    &context,
                    &lgui::window::WindowId::new("tag-row-test"),
                    |action| session.handle_default_action(action),
                    |_| {},
                );
            }
        }
        assert!(requests.lock().unwrap().is_empty());
    }

    #[test]
    fn history_loads_at_the_bottom_without_a_loading_row_or_scroll_jump() {
        use crate::features::git::model::HistoryRequest;
        use crate::ui::git::commands::{Binding, Command, Frame};
        use crate::ui::git::state::GitPanelState;
        use lgui::application::{AppView, ApplicationContext};
        use lgui::session::UiSession;
        use std::sync::Mutex;

        fn page(start: usize, end: usize) -> Vec<CommitSummary> {
            (start..end)
                .map(|index| CommitSummary {
                    oid: format!("commit-{index}"),
                    parents: Vec::new(),
                    author: "author".into(),
                    author_email: "author@example.com".into(),
                    timestamp: 0,
                    subject: format!("Commit {index}"),
                    decorations: Vec::new(),
                })
                .collect()
        }

        for drag_scrollbar in [false, true] {
            let viewport = UiRect::new(0.0, 0.0, 300.0, 180.0);
            let requests = Arc::new(Mutex::new(Vec::<HistoryRequest>::new()));
            let captured = requests.clone();
            let exposed = Arc::new(Mutex::new(None::<State<GitPanelState>>));
            let output = exposed.clone();
            let view: AppView = Arc::new(move |cx| {
                let state = cx.state_with(|| {
                    let mut app = GitPanelState::default();
                    app.ui.hovered = true;
                    app.ui.scroll_hover = Some(ScrollRegion::Details);
                    app.ui.collapsed_sections.remove(&HISTORY);
                    let request = app
                        .model
                        .begin_history("repo".into(), Some("head".into()))
                        .unwrap();
                    app.model.finish_history_page(&request, Ok(page(0, 51)));
                    app
                });
                *output.lock().unwrap() = Some(state.clone());
                let target = state.clone();
                let captured = captured.clone();
                let binding = Binding::new(state.clone().into(), Frame::default, move |command| {
                    if let Command::LoadMoreHistory { root } = command {
                        target.try_update(|app| {
                            if let Some(request) = app.model.begin_history_page(&root) {
                                captured.lock().unwrap().push(request);
                            }
                            false
                        });
                    }
                    true
                });
                let app = state.get();
                let commits = app.model.history().unwrap().commits.as_ref().unwrap();
                let content = commits
                    .iter()
                    .enumerate()
                    .map(|(index, commit)| {
                        Element::from(lgui::prelude::text(
                            UiRect::new(
                                0.0,
                                index as f32 * ROW_H,
                                280.0,
                                (index + 1) as f32 * ROW_H,
                            ),
                            commit.subject.clone(),
                            theme::mono(theme::c().text, theme::SMALL),
                        ))
                        .key(commit.oid.clone())
                    })
                    .collect();
                let bottom = commits.len() as f32 * ROW_H;
                scroll_region(
                    viewport,
                    content,
                    bottom,
                    app.ui.details_scroll,
                    bottom - viewport.height(),
                    binding,
                    ScrollRegion::Details,
                )
            });
            let mut session = UiSession::new();
            session.render_view(&view, viewport, UiScale::ONE);
            let state = exposed.lock().unwrap().clone().unwrap();
            let context = ApplicationContext::empty(Default::default());
            let send = |session: &mut UiSession, input| {
                dispatch_runtime_output(
                    session.handle_input(input),
                    &context,
                    &lgui::window::WindowId::new("history-pagination-test"),
                    |action| session.handle_default_action(action),
                    |_| {},
                );
                session.render_view(&view, viewport, UiScale::ONE);
            };
            let wheel = |y| InputEvent::Wheel {
                point: Point::new(100.0, 100.0),
                delta: WheelDelta::pixels(0.0, y),
            };
            send(&mut session, wheel(-20.0));
            assert!(requests.lock().unwrap().is_empty());
            if drag_scrollbar {
                let point = PointerData::mouse(Point::new(viewport.right - 6.0, 12.0));
                send(
                    &mut session,
                    InputEvent::PointerDown {
                        pointer: point,
                        button: PointerButton::Left,
                    },
                );
                let bottom = PointerData::mouse(Point::new(viewport.right - 6.0, 175.0));
                send(&mut session, InputEvent::PointerMove(bottom));
                send(
                    &mut session,
                    InputEvent::PointerUp {
                        pointer: bottom,
                        button: PointerButton::Left,
                    },
                );
            } else {
                send(&mut session, wheel(-10_000.0));
            }
            assert_eq!(requests.lock().unwrap().len(), 1);
            let scroll = state.get().ui.details_scroll;
            let last_rect = |session: &UiSession| {
                let node = session
                    .tree()
                    .nodes()
                    .iter()
                    .find(|node| node.text.as_deref() == Some("Commit 49"))
                    .unwrap();
                session.tree().hit_for_id(&node.id).unwrap().rect
            };
            let before = last_rect(&session);
            let node_count = session.tree().nodes().len();
            send(&mut session, wheel(-10_000.0));
            assert_eq!(requests.lock().unwrap().len(), 1);
            assert_eq!(session.tree().nodes().len(), node_count);
            let request = requests.lock().unwrap()[0].clone();
            state.update(|app| {
                app.model.finish_history_page(&request, Ok(page(50, 101)));
            });
            session.render_view(&view, viewport, UiScale::ONE);
            assert_eq!(state.get().ui.details_scroll, scroll);
            assert_eq!(last_rect(&session), before);
            state.update(|app| {
                app.ui.collapsed_sections.insert(HISTORY);
            });
            send(&mut session, wheel(-10_000.0));
            assert_eq!(requests.lock().unwrap().len(), 1);
            state.update(|app| {
                app.ui.collapsed_sections.remove(&HISTORY);
            });
            send(&mut session, wheel(-10_000.0));
            assert_eq!(requests.lock().unwrap().len(), 2);
            let request = requests.lock().unwrap()[1].clone();
            state.update(|app| {
                app.model.finish_history_page(&request, Ok(page(100, 103)));
            });
            send(&mut session, wheel(-10_000.0));
            assert_eq!(requests.lock().unwrap().len(), 2);
            assert_eq!(
                state
                    .get()
                    .model
                    .history()
                    .unwrap()
                    .commits
                    .as_ref()
                    .unwrap()
                    .len(),
                103
            );
            assert!(!session.tree().nodes().iter().any(|node| {
                node.text
                    .as_deref()
                    .is_some_and(|label| label.contains("Loading"))
            }));
        }
    }
}
mod panel_action_tests {
    use super::*;
    use crate::app::state::AppState;
    use crate::ui::git::panel::*;
    use lgui::prelude::State;

    #[test]
    fn change_rows_drag_to_editor_splits_and_buttons_do_not_arm_file_drags() {
        use crate::app::state::{FileDragSource, TabDrop};
        use crate::app::tabs;
        use crate::model::pane_layout::Direction;
        use crate::ui::tabs::PaneRegion;
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{InputEvent, Point, PointerData, UiScale, dispatch_runtime_output};
        use lgui::session::UiSession;
        use std::sync::{Arc, Mutex};

        for section in [STAGED, UNSTAGED, UNTRACKED] {
            let target = if section == STAGED {
                DiffTarget::HeadToIndex
            } else {
                DiffTarget::IndexToWorktree
            };
            let exposed = Arc::new(Mutex::new(None::<AppContext>));
            let output = exposed.clone();
            let viewport = UiRect::new(0.0, 0.0, 900.0, 400.0);
            let view: AppView = Arc::new(move |cx| {
                let state = AppContext::detached(cx.state_with(|| {
                    let mut app = AppState::new();
                    app.workspace.open_path("current.txt".into(), "keep".into());
                    app
                }));
                *output.lock().unwrap() = Some(state.clone());
                let pane = state.get().workspace.active_pane();
                let store = cx.state_with(GitStoreSnapshot::default);
                let id = cx.use_stable_id();
                let region = PaneRegion {
                    pane,
                    strip: UiRect::new(0.0, 0.0, 600.0, 30.0),
                    body: UiRect::new(0.0, 30.0, 600.0, 400.0),
                };
                let clear = state.clone();
                let moving = state.clone();
                let release = state.clone();
                panel(viewport, VisualStyle::default())
                    .on_event_capture(UiEventKind::PointerDown, move |_cx, _payload| {
                        clear.try_update(|app| app.editor.tabs.file_drag.take().is_some());
                    })
                    .on_event_capture(UiEventKind::PointerDrag, move |_cx, payload| {
                        if let UiEventPayload::PointerDrag { pointer } = payload {
                            moving.try_update(|app| {
                                tabs::update_file_drag(
                                    app,
                                    &[region],
                                    pointer.point.x,
                                    pointer.point.y,
                                )
                            });
                        }
                    })
                    .on_event_capture(UiEventKind::PointerUp, move |_cx, _payload| {
                        crate::app::explorer::finish_drawer_file_drag(
                            &release,
                            GitStoreSnapshot::default(),
                        );
                    })
                    .child(file_row(
                        UiRect::new(600.0, 0.0, 900.0, ROW_H),
                        "src/a.rs".into(),
                        false,
                        FileState::default(),
                        section,
                        target,
                        false,
                        true,
                        "repo".into(),
                        crate::app::git_view::binding(&state, &store),
                        store.clone().into(),
                        cx.focus_handle(id),
                    ))
            });
            let context = ApplicationContext::empty(Default::default());
            let mut session = UiSession::new();
            session.render_view(&view, viewport, UiScale::ONE);
            let state = exposed.lock().unwrap().clone().unwrap();
            let send = |session: &mut UiSession, input| {
                let events = session.handle_input(input);
                dispatch_runtime_output(
                    events,
                    &context,
                    &lgui::window::WindowId::new("change-drag-test"),
                    |action| session.handle_default_action(action),
                    |_| {},
                );
                session.render_view(&view, viewport, UiScale::ONE);
            };
            let down = |x, y| InputEvent::PointerDown {
                pointer: PointerData::mouse(Point::new(x, y)),
                button: PointerButton::Left,
            };
            send(&mut session, down(650.0, 10.0));
            let drag = state.get().editor.tabs.file_drag.unwrap();
            assert!(!drag.active);
            assert_eq!(
                drag.source,
                FileDragSource::Changes {
                    request: crate::features::git::DiffRequest {
                        repository_root: "repo".into(),
                        path: "src/a.rs".into(),
                        target
                    },
                    untracked: section == UNTRACKED,
                }
            );
            send(
                &mut session,
                InputEvent::PointerMove(PointerData::mouse(Point::new(649.0, 10.0))),
            );
            assert!(!state.get().editor.tabs.file_drag.unwrap().active);
            send(
                &mut session,
                InputEvent::PointerMove(PointerData::mouse(Point::new(590.0, 150.0))),
            );
            let drag = state.get().editor.tabs.file_drag.unwrap();
            assert!(drag.active);
            assert_eq!(
                drag.drop,
                Some(TabDrop::Pane {
                    pane: state.get().workspace.active_pane(),
                    split: Some(Direction::Right)
                })
            );
            // Release outside the editor cancels the drag without opening Changes.
            send(
                &mut session,
                InputEvent::PointerMove(PointerData::mouse(Point::new(800.0, 100.0))),
            );
            send(
                &mut session,
                InputEvent::PointerUp {
                    pointer: PointerData::mouse(Point::new(800.0, 100.0)),
                    button: PointerButton::Left,
                },
            );
            assert!(state.get().editor.tabs.file_drag.is_none());
            assert_eq!(state.get().workspace.pane_ids().len(), 1);
            assert_eq!(
                state.get().workspace.active_editor().unwrap().text(),
                "keep"
            );
            let action = session
                .tree()
                .nodes()
                .iter()
                .find(|node| {
                    node.semantics
                        .as_ref()
                        .is_some_and(|s| s.role == SemanticRole::Button)
                })
                .unwrap()
                .layout_rect;
            send(
                &mut session,
                down(
                    (action.left + action.right) / 2.0,
                    (action.top + action.bottom) / 2.0,
                ),
            );
            assert!(
                state.get().editor.tabs.file_drag.is_none(),
                "action buttons must not start a file drag"
            );
        }
    }

    #[test]
    fn commit_card_messages_wrap_at_spaces_or_between_cjk_characters() {
        let width = crate::ui::tabs::measure("aaaa bbbb", theme::UI_SIZE, 400);
        assert_eq!(
            wrap_text("aaaa bbbb cccc\n\ndddd", width),
            ["aaaa bbbb", "cccc", "", "dddd"]
        );
        let cjk = "提交信息悬停卡片显示完整内容";
        let lines = wrap_text(cjk, width);
        assert!(lines.len() > 1);
        assert_eq!(lines.concat(), cjk);
        assert!(
            lines
                .iter()
                .all(|line| crate::ui::tabs::measure(line, theme::UI_SIZE, 400) <= width)
        );
        let long = "x".repeat(40);
        assert_eq!(wrap_text(&long, width).concat(), long);
    }

    #[test]
    fn grouping_lists_root_files_then_each_folder_with_its_files() {
        let files = ["README.md", "src/a.rs", "src/ui/b.rs", "src/c.rs"].map(PathBuf::from);
        let describe = |rows: Vec<ChangeRow>| {
            rows.into_iter()
                .map(|row| match row {
                    ChangeRow::Folder { path, files } => {
                        format!("[{}] {}", path.display(), files.len())
                    }
                    ChangeRow::File { path, nested } => {
                        format!("{}{}", if nested { "  " } else { "" }, path.display())
                    }
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(describe(change_rows(&files, false)).len(), 4);
        assert_eq!(
            describe(change_rows(&files, true)),
            [
                "README.md",
                "[src] 2",
                "  src/a.rs",
                "  src/c.rs",
                "[src/ui] 1",
                "  src/ui/b.rs"
            ]
        );
    }

    #[test]
    fn paths_lead_with_the_file_name() {
        assert_eq!(
            split_path(std::path::Path::new("src/api/client.ts")),
            ("client.ts".to_owned(), Some("src/api".to_owned()))
        );
        assert_eq!(
            split_path(std::path::Path::new("README.md")),
            ("README.md".to_owned(), None)
        );
    }

    #[test]
    fn history_rows_show_state_and_the_expanded_commit_files() {
        let commit = |oid: &str| CommitSummary {
            oid: oid.into(),
            parents: Vec::new(),
            author: "a".into(),
            author_email: "a@b".into(),
            timestamp: 0,
            subject: oid.into(),
            decorations: Vec::new(),
        };
        assert_eq!(
            history_rows(None, None),
            [HistoryRow::Message("Loading commits…".into())]
        );
        let history = GitHistory {
            repository: PathBuf::from("repo"),
            head: Some("b".into()),
            commits: Ok(vec![commit("b"), commit("a")]),
        };
        let expanded = ExpandedCommit {
            oid: "b".into(),
            files: Some(Ok(vec![(ChangeKind::Modified, PathBuf::from("f"))])),
        };
        assert_eq!(
            history_rows(Some(&history), Some(&expanded)),
            [
                HistoryRow::Commit(commit("b"), true),
                HistoryRow::File {
                    oid: "b".into(),
                    kind: ChangeKind::Modified,
                    path: PathBuf::from("f"),
                },
                HistoryRow::Commit(commit("a"), false),
            ]
        );
    }

    #[test]
    fn history_search_matches_every_word_and_draws_without_lanes() {
        let commit = |oid: &str, subject: &str, author: &str| CommitSummary {
            oid: oid.into(),
            parents: vec!["p".into()],
            author: author.into(),
            author_email: "a@b".into(),
            timestamp: 0,
            subject: subject.into(),
            decorations: Vec::new(),
        };
        let history = GitHistory {
            repository: PathBuf::from("repo"),
            head: Some("b".into()),
            commits: Ok(vec![
                commit("bee1", "fix terminal tabs", "Ana"),
                commit("aaa2", "perf text batches", "Bo"),
            ]),
        };
        let rows = history_rows_matching(Some(&history), None, "TERMINAL ana", true);
        assert_eq!(rows.len(), 1);
        assert!(matches!(&rows[0], HistoryRow::Commit(c, false) if c.parents.is_empty()));
        let chained = history_rows_matching(Some(&history), None, "", true);
        assert!(
            matches!(&chained[0], HistoryRow::Commit(c, _) if c.parents == ["aaa2".to_owned()])
        );
        assert!(matches!(&chained[1], HistoryRow::Commit(c, _) if c.parents.is_empty()));
        assert_eq!(
            history_rows_matching(Some(&history), None, "aaa", false).len(),
            1
        );
        assert_eq!(
            history_rows_matching(Some(&history), None, "nothing", false),
            [HistoryRow::Message("No commits match.".into())]
        );
    }

    #[test]
    fn the_banner_names_the_operation_and_rebase_progress() {
        let mut repository = RepositorySnapshot {
            id: crate::features::git::RepositoryId(1),
            worktree_root: PathBuf::from("repo"),
            git_dir: PathBuf::from("repo/.git"),
            common_dir: PathBuf::from("repo/.git"),
            head: crate::features::git::HeadState::Branch("main".into()),
            upstream: None,
            ahead: 0,
            behind: 0,
            files: Default::default(),
            ignored: Default::default(),
            repository_state: RepositoryState::Normal,
            operation_step: None,
            head_oid: None,
            stashes: Default::default(),
            tags: Default::default(),
            worktrees: Default::default(),
            features: Default::default(),
            generation: 0,
        };
        assert_eq!(operation_label(&repository), None);
        repository.repository_state = RepositoryState::MergeInProgress;
        assert_eq!(operation_label(&repository).as_deref(), Some("MERGING"));
        repository.repository_state = RepositoryState::RebaseInProgress;
        repository.operation_step = Some((2, 5));
        assert_eq!(
            operation_label(&repository).as_deref(),
            Some("REBASING 2/5")
        );
    }

    #[test]
    fn each_section_offers_its_actions() {
        assert_eq!(section_actions(STAGED), [RowAction::Unstage]);
        assert_eq!(
            section_actions(UNSTAGED),
            [RowAction::Discard, RowAction::Stage]
        );
        assert!(section_actions(CONFLICTS).is_empty());
        assert_eq!(
            file_actions(CONFLICTS),
            [
                RowAction::OpenFile,
                RowAction::KeepCurrent,
                RowAction::TakeIncoming,
                RowAction::Stage
            ]
        );
        assert_eq!(RowAction::Stage.file_name(CONFLICTS), "Mark as Resolved");
        assert_eq!(RowAction::Discard.file_name(UNTRACKED), "Delete File");
        assert_eq!(
            RowAction::Discard.section_name(UNSTAGED),
            "Discard All Changes"
        );
    }
}
mod branch_picker_tests {
    use super::*;
    use crate::app::state::AppState;
    use crate::ui::git::branch_picker::*;
    use lgui::prelude::State;
    use std::sync::Arc;

    #[test]
    fn inline_worktree_action_targets_its_branch_without_switching() {
        use crate::ui::git::confirm::GitConfirmAction;
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{
            InputEvent, Point, PointerButton, PointerData, UiScale, dispatch_runtime_output,
        };
        use lgui::session::UiSession;
        use std::sync::Mutex;

        let exposed = Arc::new(Mutex::new(None::<AppContext>));
        let output = exposed.clone();
        let viewport = UiRect::new(0.0, 0.0, 460.0, 420.0);
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                let mut branches = sample();
                let mut occupied = branch("fix/login", false, false, None);
                occupied.worktree_path = Some(PathBuf::from("projects/loom-fix-login"));
                branches.insert(2, occupied);
                open_picker(&mut app, branches);
                let picker = app.git.ui.branch_picker.as_mut().unwrap();
                picker.repository = PathBuf::from("projects/loom");
                picker.repository_name = "loom".into();
                picker.anchor_x = 8.0;
                picker.selected = 2;
                if let BranchListing::Ready { worktrees, .. } = &mut picker.listing {
                    *worktrees = vec![
                        worktree("projects/loom", "main"),
                        worktree("projects/loom-fix-login", "fix/login"),
                        worktree("projects/loom-fix-login-review", "fix/login"),
                    ]
                    .into();
                }
                app
            }));
            let store = cx.state_with(GitStoreSnapshot::default);
            *output.lock().unwrap() = Some(state.clone());
            let app = state.get();
            let input_id = cx.use_stable_id();
            let input_focus = cx.focus_handle(input_id.clone());
            let root = panel(viewport, VisualStyle::filled(theme::c().bg));
            if let Some(picker) = app.git.ui.branch_picker.as_ref() {
                root.child(render(
                    viewport,
                    24.0,
                    picker,
                    app.git.ui.branch_picker_input.text(),
                    crate::app::git_view::binding(&state, &store),
                    store.clone().into(),
                    input_focus,
                    input_id,
                ))
            } else {
                root
            }
        });
        let context = ApplicationContext::empty(Default::default());
        let _text = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let state = exposed.lock().unwrap().clone().unwrap();
        let node = session
            .tree()
            .nodes()
            .iter()
            .find(|node| {
                node.semantics
                    .as_ref()
                    .and_then(|semantics| semantics.name.as_deref())
                    == Some("Check out feature in a new worktree")
            })
            .expect("Selected branch has an inline worktree action");
        let rect = node.layout_rect;
        let point = Point::new(
            (rect.left + rect.right) / 2.0,
            (rect.top + rect.bottom) / 2.0,
        );
        assert!(rect.height() < ROW_H);

        if let Some(path) = std::env::var_os("LOOM_BRANCH_PICKER_PREVIEW") {
            use lgui::core::PhysicalRect;
            use lgui::icons::IconRegistration;
            use lgui::renderer::{FrameInfo, FrameReason};
            let icons = crate::app::svg_icons();
            context
                .resources()
                .provide(IconRegistration(Arc::new(icons)));
            let scale = UiScale::new(2.0);
            let mut surface = lgui::render_skia::SkiaSoftwareSurface::new(4 * 1024 * 1024);
            let frame = FrameInfo::new(
                PhysicalRect::new(0, 0, 920, 840),
                &[],
                scale,
                FrameReason::SceneChange,
                true,
            );
            lgui::assets::backend::with_render_resources(
                &context,
                lgui::RenderResources::new(),
                || {
                    surface
                        .draw(&session.tree().scene().project_to_physical(scale), &frame)
                        .unwrap();
                },
            );
            let mut pixels = surface.pixels().to_vec();
            for pixel in pixels.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
            image::save_buffer(path, &pixels, 920, 840, image::ColorType::Rgba8).unwrap();
            state.update(|app| app.git.ui.branch_picker.as_mut().unwrap().selected = 2);
            session.render_view(&view, viewport, UiScale::ONE);
        }

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
            let events = session.handle_input(input);
            dispatch_runtime_output(
                events,
                &context,
                &lgui::window::WindowId::new("branch-worktree-action-test"),
                |action| session.handle_default_action(action),
                |_| {},
            );
            session.render_view(&view, viewport, UiScale::ONE);
        }
        let app = state.get();
        assert!(app.git.ui.branch_picker.is_none());
        assert_eq!(
            app.git.ui.confirm.unwrap().action,
            GitConfirmAction::CreateWorktree {
                repository: PathBuf::from("projects/loom"),
                path: PathBuf::from("projects/loom-feature"),
                checkout: Checkout::Switch("feature".into()),
            }
        );
    }

    fn worktree(path: &str, branch: &str) -> WorktreeInfo {
        WorktreeInfo {
            path: path.into(),
            oid: "abc".into(),
            branch: Some(branch.into()),
            bare: false,
            detached: false,
            locked: None,
        }
    }

    #[test]
    fn every_worktree_of_a_branch_gets_its_own_child_row() {
        let mut app = AppState::new();
        open_picker(&mut app, sample());
        let picker = app.git.ui.branch_picker.as_mut().unwrap();
        if let BranchListing::Ready { worktrees, .. } = &mut picker.listing {
            *worktrees = vec![
                worktree("/repo-feature", "feature"),
                worktree("/repo-feature-review", "feature"),
            ]
            .into();
        }
        let expanded = picker_rows(picker, "feature");
        assert_eq!(
            names(&expanded),
            [
                "[LOCAL]",
                "feature",
                "@/repo-feature",
                "@/repo-feature-review"
            ]
        );
        assert_eq!(selectable(&expanded).len(), 3);
        picker.collapsed_worktrees.insert("feature".into());
        assert_eq!(
            names(&picker_rows(picker, "feature")),
            ["[LOCAL]", "feature"]
        );
    }

    fn branch(name: &str, remote: bool, current: bool, upstream: Option<&str>) -> BranchInfo {
        BranchInfo {
            name: name.into(),
            remote,
            current,
            oid: "0".into(),
            upstream: upstream.map(Into::into),
            worktree_path: None,
        }
    }

    fn sample() -> Vec<BranchInfo> {
        vec![
            branch("feature", false, false, None),
            branch("main", false, true, Some("origin/main")),
            branch("origin/main", true, false, None),
            branch("origin/release/1.0", true, false, None),
        ]
    }

    fn names(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::Header(label) => format!("[{label}]"),
                Row::Create(name) => format!("+{name}"),
                Row::Branch(branch) => branch.name.clone(),
                Row::Worktree(worktree) => format!("@{}", worktree.path.display()),
            })
            .collect()
    }

    #[test]
    fn lists_current_branch_first_then_remotes() {
        assert_eq!(
            names(&rows(&sample(), "")),
            [
                "[LOCAL]",
                "main",
                "feature",
                "[REMOTE]",
                "origin/main",
                "origin/release/1.0"
            ]
        );
    }

    #[test]
    fn filters_case_insensitively_and_offers_to_create() {
        assert_eq!(
            names(&rows(&sample(), "REL")),
            ["+REL", "[REMOTE]", "origin/release/1.0"]
        );
        // An existing name, local or remote, is not offered for creation.
        assert_eq!(names(&rows(&sample(), "feature")), ["[LOCAL]", "feature"]);
        assert_eq!(
            names(&rows(&sample(), "origin/main")),
            ["[REMOTE]", "origin/main"]
        );
        assert_eq!(names(&rows(&sample(), "no such")), Vec::<String>::new());
    }

    #[test]
    fn rejects_invalid_branch_names() {
        for name in [
            "", "-x", "a..b", "a b", "a~1", "x.lock", "a/", "/a", "@", "a@{1",
        ] {
            assert!(!valid_branch_name(name), "{name:?}");
        }
        for name in ["topic", "feature/login", "fix-1.2"] {
            assert!(valid_branch_name(name), "{name:?}");
        }
    }

    #[test]
    fn remote_rows_reuse_matching_local_branches() {
        let branches = sample();
        let remote = |name: &str| Row::Branch(branch(name, true, false, None));
        // Tracked by the current branch: nothing to do.
        assert_eq!(checkout_for(&remote("origin/main"), &branches), None);
        assert_eq!(
            checkout_for(&remote("origin/release/1.0"), &branches),
            Some(Checkout::Track("origin/release/1.0".into()))
        );
        assert_eq!(
            checkout_for(&remote("origin/feature"), &branches),
            Some(Checkout::Switch("feature".into()))
        );
        assert_eq!(
            checkout_for(&Row::Branch(branches[0].clone()), &branches),
            Some(Checkout::Switch("feature".into()))
        );
        assert_eq!(
            checkout_for(&Row::Branch(branches[1].clone()), &branches),
            None
        );
        assert_eq!(
            checkout_for(&Row::Create("topic".into()), &branches),
            Some(Checkout::Create("topic".into()))
        );
    }

    #[test]
    fn occupied_local_and_tracking_branches_open_their_worktree() {
        let mut branches = sample();
        let path = PathBuf::from("../repo-feature");
        branches[0].worktree_path = Some(path.clone());
        branches[0].upstream = Some("origin/feature".into());
        let expected = Some(Checkout::OpenWorktree {
            branch: "feature".into(),
            path,
        });
        assert_eq!(
            checkout_for(&Row::Branch(branches[0].clone()), &branches),
            expected
        );
        let remote = Row::Branch(branch("origin/feature", true, false, None));
        assert_eq!(checkout_for(&remote, &branches), expected);
        assert_eq!(worktree_checkout_for(&remote, &branches), None);
        assert_eq!(
            worktree_checkout_for(&Row::Branch(branches[1].clone()), &branches),
            None
        );
        // A remote with no local branch is available for a new tracking worktree.
        let remote = Row::Branch(branch("origin/release/1.0", true, false, None));
        assert_eq!(
            worktree_checkout_for(&remote, &branches),
            Some(Checkout::Track("origin/release/1.0".into()))
        );
    }

    fn open_picker(app: &mut AppState, branches: Vec<BranchInfo>) {
        app.git.ui.branch_picker = Some(BranchPicker {
            repository: PathBuf::from("repo"),
            repository_name: "repo".into(),
            anchor_x: 0.0,
            listing: BranchListing::Ready {
                branches: branches.into(),
                worktrees: Arc::from([]),
            },
            collapsed_worktrees: BTreeSet::new(),
            selected: 0,
            scroll: 0.0,
            query: String::new(),
            sequence: 1,
            restore_terminal: false,
        });
    }

    #[test]
    fn selection_wraps_and_resets_when_the_filter_changes() {
        let mut app = AppState::new();
        open_picker(&mut app, sample());
        move_selection(&mut app.git, -1);
        assert_eq!(app.git.ui.branch_picker.as_ref().unwrap().selected, 3);
        move_selection(&mut app.git, 1);
        assert_eq!(app.git.ui.branch_picker.as_ref().unwrap().selected, 0);
        move_selection(&mut app.git, 1);
        app.git.ui.branch_picker_input.set_text("rel");
        let picker = app.git.ui.branch_picker.as_ref().unwrap();
        assert_eq!(picker.view("rel"), (0, 0.0));
    }

    #[test]
    fn keyboard_selection_scrolls_into_view() {
        let mut app = AppState::new();
        let branches = (0..30)
            .map(|index| branch(&format!("b{index:02}"), false, index == 0, None))
            .collect();
        open_picker(&mut app, branches);
        move_selection(&mut app.git, -1);
        let picker = app.git.ui.branch_picker.as_ref().unwrap();
        assert_eq!(picker.selected, 29);
        // Header plus 30 rows, scrolled so the last row ends at the bottom.
        assert_eq!(picker.scroll, HEADER_H + 30.0 * ROW_H - MAX_LIST_H);
        move_selection(&mut app.git, 1);
        assert_eq!(app.git.ui.branch_picker.as_ref().unwrap().scroll, 0.0);
    }
}
mod confirm_tests {
    use super::*;
    use crate::app::state::AppState;
    use crate::ui::git::confirm::render;
    use crate::ui::git::confirm::*;
    use lgui::application::{AppView, ApplicationContext};
    use lgui::core::{
        InputEvent, Point, PointerButton, PointerData, UiScale, dispatch_runtime_output,
    };
    use lgui::prelude::State;
    use lgui::session::UiSession;
    use std::sync::{Arc, Mutex};

    #[test]
    fn worktree_form_shows_destination_and_cancels_at_normal_and_narrow_widths() {
        for width in [640.0, 360.0] {
            let exposed = Arc::new(Mutex::new(None::<AppContext>));
            let output = exposed.clone();
            let viewport = UiRect::new(0.0, 0.0, width, 420.0);
            let destination = PathBuf::from(r"D:\Documents\cprojects\loom-feature-login");
            let expected_path = crate::ui::display_path(&destination);
            let view: AppView = Arc::new(move |cx| {
                let state = AppContext::detached(cx.state_with(|| {
                    let mut app = AppState::new();
                    app.git.ui.confirm = Some(GitConfirm {
                        title: "New Worktree".into(),
                        message: String::new(),
                        confirm_label: "Create",
                        destructive: false,
                        action: GitConfirmAction::CreateWorktree {
                            repository: PathBuf::from(r"D:\Documents\cprojects\loom"),
                            path: destination.clone(),
                            checkout: Checkout::Switch("feature/login".into()),
                        },
                        restore_terminal: false,
                    });
                    app
                }));
                let store = cx.state_with(GitStoreSnapshot::default);
                let dialog_id = cx.use_stable_id();
                *output.lock().unwrap() = Some(state.clone());
                let root = panel(viewport, VisualStyle::filled(theme::c().bg));
                if let Some(confirm) = state.get().git.ui.confirm {
                    root.child(render(
                        viewport,
                        &confirm,
                        crate::app::git_view::binding(&state, &store),
                        store.clone().into(),
                        dialog_id,
                        cx.focus_handle(UiId::new(crate::ui::git::confirm::TAG_INPUT_ID)),
                    ))
                } else {
                    root
                }
            });
            let context = ApplicationContext::empty(Default::default());
            let _text = lgui::backend::install_text_environment(
                &context,
                lgui::render_skia::skia_text_system_handle(),
            );
            let mut session = UiSession::new();
            session.render_view(&view, viewport, UiScale::ONE);
            let find = |name: &str| {
                session
                    .tree()
                    .nodes()
                    .iter()
                    .find(|node| {
                        node.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some(name)
                    })
                    .unwrap_or_else(|| panic!("Missing dialog control: {name}"))
            };
            let location = find("Worktree location");
            assert_eq!(
                location.semantics.as_ref().unwrap().value.as_deref(),
                Some(expected_path.as_str())
            );
            assert!(location.layout_rect.left >= viewport.left);
            assert!(
                location.layout_rect.right <= find("Choose worktree location").layout_rect.left
            );
            let cancel = find("Cancel").layout_rect;
            assert!(find("Create").layout_rect.right <= viewport.right);

            if width == 640.0
                && let Some(path) = std::env::var_os("LOOM_WORKTREE_DIALOG_PREVIEW")
            {
                use lgui::core::PhysicalRect;
                use lgui::icons::IconRegistration;
                use lgui::renderer::{FrameInfo, FrameReason};
                context
                    .resources()
                    .provide(IconRegistration(Arc::new(crate::app::svg_icons())));
                let scale = UiScale::new(2.0);
                let mut surface = lgui::render_skia::SkiaSoftwareSurface::new(8 * 1024 * 1024);
                let frame = FrameInfo::new(
                    PhysicalRect::new(0, 0, 1280, 840),
                    &[],
                    scale,
                    FrameReason::SceneChange,
                    true,
                );
                lgui::assets::backend::with_render_resources(
                    &context,
                    lgui::RenderResources::new(),
                    || {
                        surface
                            .draw(&session.tree().scene().project_to_physical(scale), &frame)
                            .unwrap();
                    },
                );
                let mut pixels = surface.pixels().to_vec();
                for pixel in pixels.as_chunks_mut::<4>().0 {
                    pixel.swap(0, 2);
                }
                image::save_buffer(path, &pixels, 1280, 840, image::ColorType::Rgba8).unwrap();
            }

            let point = Point::new(
                (cancel.left + cancel.right) / 2.0,
                (cancel.top + cancel.bottom) / 2.0,
            );
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
                let events = session.handle_input(input);
                dispatch_runtime_output(
                    events,
                    &context,
                    &lgui::window::WindowId::new("worktree-dialog-test"),
                    |action| session.handle_default_action(action),
                    |_| {},
                );
                session.render_view(&view, viewport, UiScale::ONE);
            }
            assert!(
                exposed
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .get()
                    .git
                    .ui
                    .confirm
                    .is_none()
            );
        }
    }
}
mod menu_tests {
    use super::*;
    use crate::app::state::AppState;
    use crate::ui::git::menu::*;
    use crate::ui::git::panel::UNSTAGED;
    use lgui::prelude::State;

    fn labels(target: &GitMenuTarget, app: &AppState) -> Vec<String> {
        entries(target, &app.git, &GitStoreSnapshot::default())
            .into_iter()
            .filter_map(|entry| match entry {
                Entry::Item { label, .. } => Some(label),
                Entry::Separator | Entry::Message(_) => None,
            })
            .collect()
    }

    fn file(section: u8) -> GitMenuTarget {
        GitMenuTarget::File {
            repository: PathBuf::from("repo"),
            path: PathBuf::from("src/main.rs"),
            section,
        }
    }

    #[test]
    fn file_menus_offer_the_section_actions() {
        let app = AppState::new();
        assert!(
            labels(&file(STAGED), &app)
                .iter()
                .any(|label| label == "Unstage Changes")
        );
        assert!(
            !labels(&file(STAGED), &app)
                .iter()
                .any(|label| label == "Discard Changes")
        );
        let unstaged = labels(&file(UNSTAGED), &app);
        assert!(
            unstaged.iter().any(|label| label == "Stage Changes")
                && unstaged.iter().any(|label| label == "Discard Changes")
        );
        assert!(
            labels(&file(UNTRACKED), &app)
                .iter()
                .any(|label| label == "Delete File")
        );
        let conflicts = labels(&file(CONFLICTS), &app);
        assert!(
            conflicts.iter().any(|label| label == "Keep Current (Ours)")
                && conflicts.iter().any(|label| label == "Mark as Resolved")
        );
    }

    #[test]
    fn tag_remote_menu_handles_empty_errors_and_replaced_requests() {
        let app = AppState::new();
        let mut menu = GitMenu {
            pos: (0.0, 0.0),
            target: GitMenuTarget::TagPushRemotes {
                repository: "repo".into(),
                name: "v1.0".into(),
                request: 2,
                remotes: None,
            },
            hover: None,
        };
        assert!(!menu.finish_tag_remotes(1, Ok(vec!["stale".into()])));
        assert!(
            matches!(&entries(&menu.target, &app.git, &GitStoreSnapshot::default())[0], Entry::Message(message) if message == "Loading remotes…")
        );
        assert!(menu.finish_tag_remotes(2, Ok(Vec::new())));
        assert!(
            matches!(&entries(&menu.target, &app.git, &GitStoreSnapshot::default())[0], Entry::Message(message) if message == "No remotes configured.")
        );
        assert!(menu.finish_tag_remotes(2, Err("Could not load remotes.".into())));
        assert!(
            matches!(&entries(&menu.target, &app.git, &GitStoreSnapshot::default())[0], Entry::Message(message) if message == "Could not load remotes.")
        );
        menu.target = GitMenuTarget::Tags {
            repository: "other".into(),
            name: "v2.0".into(),
        };
        assert!(!menu.finish_tag_remotes(2, Ok(vec!["origin".into()])));
    }

    #[test]
    fn history_commit_menu_offers_open_tag_creation_and_revert() {
        let target = GitMenuTarget::HistoryCommit {
            repository: PathBuf::from("repo"),
            oid: "abc".into(),
        };
        assert_eq!(
            labels(&target, &AppState::new()),
            ["Open Commit Changes", "New Tag…", "Revert Commit"]
        );
    }

    #[test]
    fn commit_menu_reflects_the_options() {
        let mut app = AppState::new();
        app.git.ui.commit_amend = true;
        let entries = entries(
            &GitMenuTarget::Commit,
            &app.git,
            &GitStoreSnapshot::default(),
        );
        let checked = entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Item {
                    label,
                    checked: Some(checked),
                    ..
                } => Some((label.as_str(), *checked)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(checked, [("Amend Last Commit", true), ("Sign Off", false)]);
    }
}
mod stash_switch_tests {
    use super::*;
    use crate::app::state::AppState;
    use crate::ui::git::stash_switch::*;
    use lgui::prelude::State;

    fn files(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn names_a_few_files_and_counts_the_rest() {
        assert_eq!(describe_files(&[]), "your files");
        assert_eq!(describe_files(&files(&["a"])), "a");
        assert_eq!(describe_files(&files(&["a", "b"])), "a and b");
        assert_eq!(describe_files(&files(&["a", "b", "c"])), "a, b and c");
        assert_eq!(
            describe_files(&files(&["a", "b", "c", "d", "e"])),
            "a, b, c and 2 more files"
        );
    }
}
mod clone_tests {
    use super::*;
    use crate::app::state::AppState;
    use crate::ui::git::clone::*;
    use lgui::prelude::State;

    #[test]
    fn long_repository_url_keeps_its_editing_tail_visible() {
        assert_eq!(visible_tail("abcdefgh", 5), "…efgh");
        assert_eq!(visible_tail("repo", 8), "repo");
    }
}
