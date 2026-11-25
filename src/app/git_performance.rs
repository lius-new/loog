//! Reproducible synthetic large-repository preparation and panel frame benchmark.
use crate::app::context::AppContext;
use crate::app::state::AppState;
use crate::features::git::*;
use lgui::application::{AppView, ApplicationContext};
use lgui::core::{UiId, UiScale};
use lgui::prelude::UiRect;
use lgui::session::UiSession;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

#[test]
#[ignore = "opt-in R6 large Git snapshot preparation and panel frame benchmark"]
fn large_repository_prepare_and_panel_frame() {
    let id = RepositoryId(1);
    let repository = RepositorySnapshot {
        id,
        worktree_root: "repo".into(),
        git_dir: "repo/.git".into(),
        common_dir: "repo/.git".into(),
        head: HeadState::Branch("main".into()),
        upstream: None,
        ahead: 0,
        behind: 0,
        files: Arc::new(
            (0..10_000)
                .map(|i| {
                    (
                        PathBuf::from(format!("src/dir{:03}/file{i:05}.rs", i % 100)),
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
        stashes: Arc::default(),
        tags: Arc::default(),
        worktrees: Arc::default(),
        features: Default::default(),
        generation: 1,
    };
    let source = GitStoreSnapshot {
        repositories: Arc::new([(id, repository)].into_iter().collect()),
        active_repository: Some(id),
        initializing: false,
        ..Default::default()
    };
    let mut prepared = source.clone();
    prepared.prepare_for_ui(None);
    let mut cold = Vec::new();
    let mut reuse = Vec::new();
    for round in 0..35 {
        let mut next = source.clone();
        let start = Instant::now();
        next.prepare_for_ui(None);
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        std::hint::black_box(&next);
        if round >= 3 {
            cold.push(elapsed);
        }
        let mut next = source.clone();
        let start = Instant::now();
        next.prepare_for_ui(Some(&prepared));
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        std::hint::black_box(&next);
        if round >= 3 {
            reuse.push(elapsed);
        }
    }
    let viewport = UiRect::new(0.0, 0.0, 340.0, 900.0);
    let view: AppView = Arc::new(move |cx| {
        let state = AppContext::detached(cx.state_with(AppState::new));
        let fixture = prepared.clone();
        let store = cx.state_with(move || fixture);
        crate::ui::git::panel::render(
            viewport,
            super::git_view::binding(&state, &store),
            store.into(),
            cx.focus_handle(UiId::new("bench-editor")),
            cx.focus_handle(UiId::new("bench-commit")),
            UiId::new("bench-commit"),
        )
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
    let mut frames = Vec::new();
    for round in 0..105 {
        let start = Instant::now();
        session.render_view(&view, viewport, UiScale::ONE);
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        std::hint::black_box(session.tree());
        if round >= 5 {
            frames.push(elapsed);
        }
    }
    let nodes = session.tree().nodes().len();
    assert!(nodes < 1000, "Git rows must remain virtualized");
    fn median(mut values: Vec<f64>) -> f64 {
        values.sort_by(f64::total_cmp);
        let m = values.len() / 2;
        (values[m - 1] + values[m]) / 2.0
    }
    println!(
        "R6_BENCH {}",
        serde_json::json!({"files":10000,"cold_prepare_ms":median(cold),"reused_prepare_ms":median(reuse),"panel_frame_ms":median(frames),"panel_nodes":nodes})
    );
}
