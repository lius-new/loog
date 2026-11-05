//! Terminal effect installation, generation-aware observation and persistence.
use crate::app::context::AppContext;
use crate::app::runtime::AppRuntime;
use crate::app::state::AppState;
use crate::app::terminal_binding::{TerminalStore, TerminalTabs};
use crate::features::terminal::model::TerminalModel;
use crate::features::terminal::program::{ProgramCandidate, SustainedProgram};
use crate::features::terminal::{SessionId, TerminalSize};
use crate::infrastructure::terminal::notification::FrameNotifier;
use crate::infrastructure::terminal::runtime::TerminalHandle;
use lgui::ApplicationHandle;
use lgui::prelude::{RenderCx, State};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::{self, RecvTimeoutError},
};
use std::time::{Duration, Instant};

pub(crate) const PROBE_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Default)]
pub(crate) struct Mounts(AtomicU64);
impl Mounts {
    pub fn begin(&self) -> u64 {
        self.0.fetch_add(1, Ordering::AcqRel) + 1
    }
    pub fn end(&self, token: u64) -> bool {
        self.0
            .compare_exchange(token, token + 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

pub(crate) fn restored(session: crate::app::workspace_session::WorkspaceSession) -> TerminalModel {
    let mut tabs = TerminalTabs::restored_with_groups(
        session.terminal_tabs,
        session.active_terminal,
        &session.terminal_groups,
    );
    tabs.restore_widths(&session.terminal_widths);
    tabs.restore_purposes(&session.terminal_purposes);
    tabs.restore_titles(&session.terminal_titles);
    tabs.restore_directories(&session.terminal_directories);
    tabs.into_model()
}
pub(crate) fn bind(
    model: State<TerminalModel>,
    runtime: &Arc<AppRuntime>,
    state: &AppContext,
) -> TerminalStore {
    let source = state.clone();
    runtime
        .terminals()
        .set_profile_source(move || source.get().preferences.values().terminal_profiles.clone());
    TerminalStore::new(model, runtime.terminals())
}
pub(crate) fn install_owner(
    cx: &mut RenderCx<'_, '_>,
    runtime: &Arc<AppRuntime>,
    state: &AppContext,
    tabs: &TerminalStore,
) {
    let mounts = runtime.terminal_mounts().clone();
    let owner = Arc::downgrade(runtime.terminals());
    cx.use_effect((), move || {
        let token = mounts.begin();
        move || {
            if mounts.end(token)
                && let Some(owner) = owner.upgrade()
            {
                owner.shutdown();
            }
        }
    });
    let profiles = state.get().preferences.values().terminal_profiles.clone();
    let owner = Arc::downgrade(runtime.terminals());
    let tabs = tabs.clone();
    cx.use_effect(profiles, move || {
        if let Some(owner) = owner.upgrade() {
            owner.refresh_profiles(tabs.get().model());
            tabs.notify();
        }
    });
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Observation {
    pub id: SessionId,
    pub generation: u64,
    pub program: Option<String>,
    pub cwd: Option<PathBuf>,
}
#[derive(Clone, Default)]
pub(crate) struct Lease(Arc<AtomicBool>);
impl Lease {
    pub fn stop(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn active(&self) -> bool {
        !self.0.load(Ordering::Acquire)
    }
}
/// Applied on the UI queue. Generation queries happen before the model lock.
pub(crate) fn apply(tabs: &TerminalStore, lease: &Lease, observations: Vec<Observation>) {
    if !lease.active() {
        return;
    }
    let current = tabs.get();
    let accepted = observations
        .into_iter()
        .filter(|observation| {
            current.tabs().iter().any(|tab| {
                tab.id == observation.id
                    && tab.controller.generation() == Some(observation.generation)
            })
        })
        .collect::<Vec<_>>();
    if accepted.is_empty() {
        return;
    }
    let programs = accepted
        .iter()
        .map(|o| (o.id, o.program.clone()))
        .collect::<Vec<_>>();
    let directories = accepted
        .iter()
        .map(|o| (o.id, o.cwd.clone()))
        .collect::<Vec<_>>();
    tabs.update(|tabs| {
        tabs.update_programs(&programs);
        tabs.update_directories(&directories);
    });
}

struct Tracked {
    id: SessionId,
    handle: TerminalHandle,
    cwd: Option<PathBuf>,
    generation: Option<u64>,
    program: SustainedProgram,
}
pub(crate) struct Poller {
    sessions: Vec<Tracked>,
    previous: Vec<Observation>,
    last: Instant,
}
impl Poller {
    pub fn new(tabs: &TerminalTabs, now: Instant) -> Self {
        let sessions = tabs
            .tabs()
            .into_iter()
            .map(|tab| Tracked {
                id: tab.id,
                generation: tab.controller.generation(),
                cwd: tab.working_directory(),
                handle: tab.controller,
                program: SustainedProgram::default(),
            })
            .collect::<Vec<_>>();
        let previous = tabs
            .tabs()
            .into_iter()
            .filter_map(|tab| {
                Some(Observation {
                    id: tab.id,
                    generation: tab.controller.generation()?,
                    program: tab.running_program.clone(),
                    cwd: tab.working_directory(),
                })
            })
            .collect();
        Self {
            sessions,
            previous,
            last: now,
        }
    }
    pub fn due(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.last) < PROBE_INTERVAL {
            return false;
        }
        self.last = now;
        true
    }
    pub fn sample(
        &mut self,
        now: Instant,
        mut probe: impl FnMut(&TerminalHandle, Option<u32>) -> Option<ProgramCandidate>,
    ) -> Option<Vec<Observation>> {
        let next = self
            .sessions
            .iter_mut()
            .filter_map(|session| {
                let generation = session.handle.generation()?;
                if session.generation != Some(generation) {
                    session.program = SustainedProgram::default();
                    session.generation = Some(generation);
                }
                let candidate = probe(&session.handle, session.program.sustained_pid(now));
                let cwd = session
                    .handle
                    .working_directory()
                    .or_else(|| session.cwd.clone());
                if session.handle.generation() != Some(generation) {
                    return None;
                }
                Some(Observation {
                    id: session.id,
                    generation,
                    program: session.program.observe(candidate, now),
                    cwd,
                })
            })
            .collect::<Vec<_>>();
        if next == self.previous {
            return None;
        }
        self.previous = next.clone();
        Some(next)
    }
}
pub(crate) fn start_programs(tabs: TerminalStore, application: ApplicationHandle) -> impl FnOnce() {
    start_programs_with_clock(tabs, application, Arc::new(Instant::now))
}
fn start_programs_with_clock(
    tabs: TerminalStore,
    application: ApplicationHandle,
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
) -> impl FnOnce() {
    let lease = Lease::default();
    let applied_lease = lease.clone();
    let mut poller = Poller::new(&tabs.get(), clock());
    let notifications =
        crate::infrastructure::ui_dispatch::LatestNotification::new(application, move |updates| {
            apply(&tabs, &applied_lease, updates)
        });
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::Builder::new()
        .name("loom-terminal-programs".into())
        .spawn(move || {
            while let Err(RecvTimeoutError::Timeout) = receiver.recv_timeout(PROBE_INTERVAL) {
                let now = clock();
                if poller.sessions.is_empty() || !poller.due(now) {
                    continue;
                }
                let probe = crate::infrastructure::terminal::program::ProgramProbe::new();
                if let Some(updates) = poller.sample(now, |handle, preferred| {
                    handle.program_candidate(&probe, preferred)
                }) {
                    notifications.send(updates);
                }
            }
        })
        .ok();
    move || {
        lease.stop();
        let _ = sender.send(());
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

pub(crate) type Start = (SessionId, TerminalHandle, Option<PathBuf>, TerminalSize);
pub(crate) fn visible_starts(
    layout: &crate::domain::application_layout::ApplicationLayout,
    snapshot: &crate::domain::application_layout::Snapshot,
    tabs: &TerminalTabs,
    cwd: &Option<PathBuf>,
) -> Vec<Start> {
    snapshot
        .regions
        .iter()
        .flat_map(|(region, content, rect)| {
            if !matches!(
                content,
                crate::domain::application_layout::Content::Terminal { .. }
            ) {
                return Vec::new();
            }
            let (ids, active) = layout.terminal_view(*region).unwrap();
            let sessions = tabs.region_view(&ids, active).active_group();
            let frames =
                crate::app::terminal_view::project(tabs.region_view(&ids, active)).active_group();
            let rects = crate::ui::terminal::view::session_rects((*rect).into(), &frames);
            sessions
                .into_iter()
                .zip(rects)
                .map(|(session, rect)| {
                    (
                        session.id,
                        session.controller.clone(),
                        session.working_directory().or_else(|| cwd.clone()),
                        crate::ui::terminal::view::pty_size_for_body(rect),
                    )
                })
                .collect()
        })
        .collect()
}
fn notifier(handle: TerminalHandle, application: Arc<ApplicationHandle>) -> FrameNotifier {
    FrameNotifier::for_generation(move |generation| {
        if handle.generation() == Some(generation) {
            application.request_frame();
        }
    })
}
pub(crate) fn start_visible(
    starts: Vec<Start>,
    fallback: Option<PathBuf>,
    application: Arc<ApplicationHandle>,
) {
    for (_, handle, cwd, size) in starts {
        let cwd = crate::infrastructure::terminal::integration::starting_directory(
            cwd.as_deref(),
            fallback.as_deref(),
        );
        let notifier = notifier(handle.clone(), application.clone());
        handle.ensure_started(cwd.as_deref(), size, notifier);
    }
}
pub(crate) fn save(tabs: &TerminalStore, visible: bool, height: f32) {
    let tabs = tabs.get();
    let _ = crate::app::workspace_session::save_terminal_state(
        &tabs.shells(),
        &tabs.saved_groups(),
        &tabs.widths(),
        &tabs.purposes(),
        &tabs.titles(),
        &tabs.directories(),
        tabs.active_index(),
        visible,
        height,
    );
}
pub(crate) fn install_save(cx: &mut RenderCx<'_, '_>, tabs: &TerminalStore, app: &AppState) {
    let workspace_root = app.terminal.ui.terminal_workspace_root.clone();
    cx.use_effect(workspace_root.clone(), move || {
        let _ = crate::app::workspace_session::save_terminal_workspace_root(workspace_root);
    });
    let snapshot = tabs.get();
    let visible = app.default_terminal_open();
    let height = app.terminal_chrome.terminal_h;
    let key = (
        app.terminal_chrome.resizing_terminal,
        snapshot.shells(),
        snapshot.saved_groups(),
        snapshot.widths(),
        snapshot.purposes(),
        snapshot.titles(),
        snapshot.directories(),
        app.terminal.ui.terminal_split_drag,
        snapshot.active_index(),
        visible,
        height,
    );
    let tabs = tabs.clone();
    // The height is rewritten to disk on every drag event otherwise.
    let dragging =
        app.terminal.ui.terminal_split_drag.is_some() || app.terminal_chrome.resizing_terminal;
    cx.use_effect(key, move || {
        if !dragging {
            save(&tabs, visible, height);
        }
    });
}
pub(crate) fn blink(state: State<bool>, visible: bool, focused: bool) -> impl FnOnce() {
    state.set(true);
    let (sender, receiver) = mpsc::channel();
    let worker = if visible && focused {
        Some(
            std::thread::Builder::new()
                .name("loom-terminal-cursor-blink".into())
                .spawn(move || {
                    while let Err(RecvTimeoutError::Timeout) =
                        receiver.recv_timeout(Duration::from_millis(500))
                    {
                        state.update(|visible| *visible = !*visible);
                    }
                })
                .ok(),
        )
        .flatten()
    } else {
        None
    };
    move || {
        let _ = sender.send(());
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::terminal_command_tests::fixture;
    use crate::app::test_support::{ManualClock, UiTasks};
    use crate::features::terminal::ShellKind;

    #[test]
    fn queued_probe_and_cwd_reject_old_run_closed_id_and_cancelled_effect() {
        let (_session, state, tabs) = fixture();
        let handle = tabs.get().active().unwrap().controller;
        let controller = handle.test_controller();
        let queue = UiTasks::default();
        let app = queue.application();
        let lease = Lease::default();
        let old = lease.clone();
        let target = tabs.clone();
        let stale = Observation {
            id: handle.id(),
            generation: handle.generation().unwrap(),
            program: Some("old run".into()),
            cwd: Some("old cwd".into()),
        };
        app.post(move || apply(&target, &old, vec![stale]));
        let generation = controller.test_advance_generation();
        let current = Observation {
            id: handle.id(),
            generation,
            program: Some("new run".into()),
            cwd: Some("new cwd".into()),
        };
        apply(&tabs, &lease, vec![current.clone()]);
        queue.run_all();
        assert_eq!(
            tabs.get().active().unwrap().running_program.as_deref(),
            Some("new run")
        );
        assert_eq!(tabs.get().active().unwrap().cwd, Some("new cwd".into()));
        let target = tabs.clone();
        let old = lease.clone();
        let pending = current.clone();
        app.post(move || apply(&target, &old, vec![pending]));
        lease.stop();
        let replacement = Lease::default();
        apply(
            &tabs,
            &replacement,
            vec![Observation {
                program: Some("new effect".into()),
                ..current.clone()
            }],
        );
        queue.run_all();
        assert_eq!(
            tabs.get().active().unwrap().running_program.as_deref(),
            Some("new effect")
        );
        let application = Arc::new(ApplicationHandle::new(|task| task(), || {}));
        crate::app::terminal::close(
            &state,
            &tabs,
            handle.id(),
            crate::features::terminal::actions::MenuAction::Close,
            &application,
        );
        let new_id = crate::app::terminal::new(&state, &tabs, ShellKind::Cmd, None);
        apply(&tabs, &replacement, vec![current]);
        assert_eq!(tabs.get().active_id(), Some(new_id));
        assert!(tabs.get().active().unwrap().running_program.is_none());
    }
    #[test]
    fn polling_clock_boundaries_and_restart_reset_the_sustained_label() {
        let (_session, _state, tabs) = fixture();
        let mut clock = ManualClock::default();
        let start = Instant::now();
        let mut poller = Poller::new(&tabs.get(), start);
        clock.advance(Duration::from_millis(249));
        assert!(!poller.due(start + clock.elapsed()));
        clock.advance(Duration::from_millis(1));
        assert!(poller.due(start + clock.elapsed()));
        let candidate = || {
            Some(ProgramCandidate {
                pid: 42,
                name: "long task".into(),
            })
        };
        poller.sample(start, |_, _| candidate());
        let before = poller.sample(start + Duration::from_millis(999), |_, _| candidate());
        assert!(before.is_none_or(|v| v[0].program.is_none()));
        let sustained = poller
            .sample(start + Duration::from_millis(1000), |_, preferred| {
                assert!(preferred.is_some());
                candidate()
            })
            .unwrap();
        assert_eq!(sustained[0].program.as_deref(), Some("long task"));
        tabs.get()
            .active()
            .unwrap()
            .controller
            .test_controller()
            .test_advance_generation();
        let reset = poller
            .sample(start + Duration::from_millis(1250), |_, preferred| {
                assert!(preferred.is_none());
                candidate()
            })
            .unwrap();
        assert!(reset[0].program.is_none());
        assert_eq!(reset[0].generation, sustained[0].generation + 1);
    }
    #[test]
    fn mount_cleanup_is_idempotent_and_old_cleanup_cannot_stop_replacement() {
        let mounts = Mounts::default();
        let old = mounts.begin();
        let current = mounts.begin();
        assert!(!mounts.end(old));
        assert!(mounts.end(current));
        assert!(!mounts.end(current));
        let another = mounts.begin();
        assert!(!mounts.end(old));
        assert!(mounts.end(another));
    }
    #[test]
    fn hidden_default_regions_generate_no_start_or_resize_requests() {
        let (_session, state, tabs) = fixture();
        let app = state.get();
        let layout = app.layout.application_layout.unwrap();
        let bounds = lgui::prelude::UiRect::new(0.0, 0.0, 800.0, 600.0);
        let hidden = layout.snapshot(
            bounds.into(),
            crate::domain::application_layout::Visibility {
                git: false,
                files: false,
                terminal: false,
            },
        );
        assert!(visible_starts(&layout, &hidden, &tabs.get(), &None).is_empty());
        let visible = layout.snapshot(
            bounds.into(),
            crate::domain::application_layout::Visibility {
                git: false,
                files: false,
                terminal: true,
            },
        );
        let starts = visible_starts(&layout, &visible, &tabs.get(), &None);
        assert_eq!(starts.iter().map(|s| s.0).collect::<Vec<_>>(), vec![1]);
        assert!(matches!(
            {
                use crate::app::terminal_runtime::TerminalDisplay;
                tabs.get().active().unwrap().controller.snapshot()
            }
            .status,
            crate::features::terminal::TerminalStatus::Idle
        ));
    }
    #[test]
    fn restored_terminal_json_retains_legacy_fields_groups_widths_and_directories() {
        let empty: crate::app::workspace_session::WorkspaceSession =
            serde_json::from_str("{}").unwrap();
        assert!(restored(empty).is_empty());
        let original = crate::app::workspace_session::WorkspaceSession {
            terminal_tabs: vec![ShellKind::Cmd, ShellKind::Custom(42)],
            active_terminal: Some(99),
            terminal_groups: vec![0, 0],
            terminal_widths: vec![-1.0, f32::INFINITY],
            terminal_purposes: vec![Some("deprecated".into()), Some(" Build ".into())],
            terminal_directories: vec![Some("C:/folder with spaces/中文".into()), None],
            ..Default::default()
        };
        let model = restored(original.clone());
        assert_eq!(model.shells(), original.terminal_tabs);
        assert_eq!(model.saved_groups(), vec![0, 0]);
        assert!(model.widths().iter().all(|w| w.is_finite() && *w > 0.0));
        assert_eq!(model.active_index(), Some(1));
        assert_eq!(model.tabs()[0].cwd(), original.terminal_directories[0]);
        assert_eq!(model.tabs()[0].purpose().as_deref(), Some("Other"));
        let json = serde_json::to_value(crate::app::workspace_session::WorkspaceSession {
            terminal_tabs: original.terminal_tabs.clone(),
            ..Default::default()
        })
        .unwrap();
        let roundtrip: crate::app::workspace_session::WorkspaceSession =
            serde_json::from_value(json.clone()).unwrap();
        assert_eq!(roundtrip.terminal_tabs, original.terminal_tabs);
        for field in [
            "terminal_tabs",
            "terminal_groups",
            "terminal_widths",
            "terminal_purposes",
            "terminal_directories",
            "active_terminal",
            "show_terminal",
            "terminal_height",
        ] {
            assert!(json.get(field).is_some());
        }
        assert!(json.get("running_program").is_none());
        assert!(json.get("application_layout").is_none());
    }
}

#[cfg(test)]
mod adapter_tests {
    use super::*;
    use crate::app::terminal_command_tests::fixture;
    use crate::features::terminal::{ShellKind, profiles::ShellProfile};
    use lgui::application::AppView;
    use lgui::core::UiScale;
    use lgui::prelude::{UiRect, VisualStyle, panel};
    use lgui::session::UiSession;
    use std::sync::{Mutex, atomic::AtomicUsize};
    #[test]
    fn frame_notifications_recheck_session_identity_and_generation() {
        let (_session, state, tabs) = fixture();
        let handle = tabs.get().active().unwrap().controller;
        let counter = Arc::new(AtomicUsize::new(0));
        let counted = counter.clone();
        let application = Arc::new(ApplicationHandle::new(
            |task| task(),
            move || {
                counted.fetch_add(1, Ordering::AcqRel);
            },
        ));
        let notify = notifier(handle.clone(), application.clone());
        let old = handle.generation().unwrap();
        notify.request_frame_for(old);
        let new = handle.test_controller().test_advance_generation();
        notify.request_frame_for(old);
        notify.request_frame_for(new);
        assert_eq!(counter.load(Ordering::Acquire), 2);
        crate::app::terminal::close(
            &state,
            &tabs,
            handle.id(),
            crate::features::terminal::actions::MenuAction::Close,
            &application,
        );
        let after_close = counter.load(Ordering::Acquire);
        notify.request_frame_for(new);
        assert_eq!(counter.load(Ordering::Acquire), after_close);
    }
    #[test]
    fn restored_custom_session_receives_profile_before_first_projection() {
        let exposed = Arc::new(Mutex::new(None));
        let output = exposed.clone();
        let view: AppView = Arc::new(move |cx| {
            let runtime = cx.state_with(|| Arc::new(AppRuntime::default())).get();
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.preferences
                    .values_mut()
                    .terminal_profiles
                    .push(ShellProfile {
                        id: 42,
                        name: "first projection".into(),
                        program: "custom.exe".into(),
                        args: vec!["--literal".into()],
                    });
                app
            }));
            let model =
                cx.state_with(|| TerminalModel::restored(vec![ShellKind::Custom(42)], None));
            let tabs = bind(model, &runtime, &state);
            *output.lock().unwrap() = Some(tabs.get().active().unwrap().controller.short_label());
            panel(cx.viewport(), VisualStyle::default())
        });
        let mut session = UiSession::new();
        session.render_view(&view, UiRect::new(0.0, 0.0, 100.0, 100.0), UiScale::ONE);
        assert_eq!(exposed.lock().unwrap().as_deref(), Some("first projection"));
    }
}
