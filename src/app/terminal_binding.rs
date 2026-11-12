//! Application model store and weak runtime projection; UI consumes plain frames.
//! State contains only TerminalModel; projections and operation handles are weak.
#[cfg(test)]
use crate::app::context::AppContext;
use crate::features::terminal::model::{TerminalMetadata, TerminalModel};
use crate::features::terminal::{SessionId, ShellKind};
use crate::infrastructure::terminal::runtime::{TerminalHandle, TerminalRuntime};
use lgui::prelude::State;
use std::path::PathBuf;
use std::sync::{Arc, Weak};

#[derive(Clone)]
pub(crate) struct TerminalStore {
    model: State<TerminalModel>,
    runtime: Weak<TerminalRuntime>,
}
impl TerminalStore {
    pub fn new(model: State<TerminalModel>, runtime: &Arc<TerminalRuntime>) -> Self {
        runtime.synchronize(&model.get());
        Self {
            model,
            runtime: Arc::downgrade(runtime),
        }
    }
    pub fn notify(&self) {
        self.model.update(|_| {});
    }
    pub fn get(&self) -> TerminalTabs {
        TerminalTabs {
            model: self.model.get(),
            runtime: self.runtime.clone(),
        }
    }
    pub fn update(&self, update: impl FnOnce(&mut TerminalTabs)) {
        let directories = self
            .get()
            .tabs()
            .iter()
            .map(|tab| (tab.id, tab.working_directory()))
            .collect::<Vec<_>>();
        self.model.update(|model| {
            let mut projection = TerminalTabs {
                model: model.clone(),
                runtime: self.runtime.clone(),
            };
            projection.model.update_directories(&directories);
            update(&mut projection);
            *model = projection.model;
        });
        if let Some(runtime) = self.runtime.upgrade() {
            runtime.synchronize(&self.model.get());
        }
    }
}
#[derive(Clone)]
pub(crate) struct TerminalTabs {
    model: TerminalModel,
    runtime: Weak<TerminalRuntime>,
}
#[derive(Clone)]
pub(crate) struct TerminalTab {
    pub id: SessionId,
    pub group_id: u64,
    pub width_weight: f32,
    pub controller: TerminalHandle,
    pub cwd: Option<PathBuf>,
    pub title: Option<String>,
    pub purpose: Option<String>,
    pub running_program: Option<String>,
}
/// The tab text: the user's own name or the shell's short label, followed by
/// the purpose, or else the program that is running.
pub(crate) fn display_title(
    short_label: &str,
    title: Option<&str>,
    purpose: Option<&str>,
    running_program: Option<&str>,
) -> String {
    let name = title.unwrap_or(short_label);
    match (purpose, running_program) {
        (Some(purpose), _) => format!("{name} · {purpose}"),
        // A program running in an unnamed terminal is all the tab says; the shell
        // label would only repeat what the program already tells.
        (None, Some(program)) if title.is_none() => program.to_owned(),
        (None, Some(program)) => format!("{name} · {program}"),
        (None, None) => name.to_owned(),
    }
}
impl TerminalTab {
    pub fn working_directory(&self) -> Option<PathBuf> {
        self.controller
            .working_directory()
            .or_else(|| self.cwd.clone())
    }
    pub fn label(&self) -> String {
        display_title(
            &self.controller.short_label(),
            self.title.as_deref(),
            self.purpose.as_deref(),
            self.running_program.as_deref(),
        )
    }
}
impl TerminalTabs {
    pub fn frame_metadata(
        &self,
    ) -> std::collections::HashMap<
        SessionId,
        (
            String,
            Option<PathBuf>,
            crate::features::terminal::activity::CommandActivity,
        ),
    > {
        let ids = self
            .model
            .tabs()
            .iter()
            .map(|tab| tab.id())
            .collect::<Vec<_>>();
        self.runtime
            .upgrade()
            .map(|runtime| runtime.metadata(&ids))
            .unwrap_or_default()
    }
    pub fn model(&self) -> &TerminalModel {
        &self.model
    }
    pub fn into_model(self) -> TerminalModel {
        self.model
    }

    pub fn new() -> Self {
        Self {
            model: TerminalModel::new(),
            runtime: Weak::new(),
        }
    }
    pub fn restored(shells: Vec<ShellKind>, active: Option<usize>) -> Self {
        Self {
            model: TerminalModel::restored(shells, active),
            runtime: Weak::new(),
        }
    }
    pub fn restored_with_groups(
        shells: Vec<ShellKind>,
        active: Option<usize>,
        groups: &[usize],
    ) -> Self {
        Self {
            model: TerminalModel::restored_with_groups(shells, active, groups),
            runtime: Weak::new(),
        }
    }
    pub fn with_runtime(mut self, runtime: &Arc<TerminalRuntime>) -> Self {
        runtime.synchronize(&self.model);
        self.runtime = Arc::downgrade(runtime);
        self
    }
    fn project(&self, tab: &TerminalMetadata) -> TerminalTab {
        TerminalTab {
            id: tab.id(),
            group_id: tab.group_id(),
            width_weight: tab.width_weight(),
            controller: self
                .runtime
                .upgrade()
                .map(|r| r.handle(tab.id(), tab.shell()))
                .unwrap_or_else(|| TerminalHandle::detached(tab.id(), tab.shell())),
            cwd: tab.cwd(),
            title: tab.title(),
            purpose: tab.purpose(),
            running_program: tab.running_program(),
        }
    }
    pub fn tabs(&self) -> Vec<TerminalTab> {
        self.model.tabs().iter().map(|t| self.project(t)).collect()
    }
    pub fn active(&self) -> Option<TerminalTab> {
        self.model.active().map(|t| self.project(t))
    }
    pub fn active_group(&self) -> Vec<TerminalTab> {
        self.model
            .active_group()
            .iter()
            .map(|t| self.project(t))
            .collect()
    }
    pub fn active_id(&self) -> Option<u64> {
        self.model.active_id()
    }
    pub fn active_index(&self) -> Option<usize> {
        self.model.active_index()
    }
    pub fn region_view(&self, ids: &[u64], active: Option<u64>) -> Self {
        Self {
            model: self.model.region_view(ids, active),
            runtime: self.runtime.clone(),
        }
    }
    pub fn detach_group(&mut self, id: u64) {
        self.model.detach_group(id);
    }
    pub fn saved_groups(&self) -> Vec<usize> {
        self.model.saved_groups()
    }
    pub fn widths(&self) -> Vec<f32> {
        self.model.widths()
    }
    pub fn purposes(&self) -> Vec<Option<String>> {
        self.model.purposes()
    }
    pub fn titles(&self) -> Vec<Option<String>> {
        self.model.titles()
    }
    pub fn directories(&self) -> Vec<Option<PathBuf>> {
        self.tabs()
            .iter()
            .map(TerminalTab::working_directory)
            .collect()
    }
    pub fn restore_directories(&mut self, values: &[Option<PathBuf>]) {
        self.model.restore_directories(values);
    }
    pub fn restore_widths(&mut self, values: &[f32]) {
        self.model.restore_widths(values);
    }
    pub fn restore_purposes(&mut self, values: &[Option<String>]) {
        self.model.restore_purposes(values);
    }
    pub fn restore_titles(&mut self, values: &[Option<String>]) {
        self.model.restore_titles(values);
    }
    pub fn set_title(&mut self, id: u64, title: Option<&str>) {
        self.model.set_title(id, title);
    }
    pub fn update_directories(&mut self, values: &[(u64, Option<PathBuf>)]) {
        self.model.update_directories(values);
    }
    pub fn update_programs(&mut self, values: &[(u64, Option<String>)]) {
        self.model.update_programs(values);
    }
    pub fn set_purpose(&mut self, id: u64, purpose: Option<&str>) {
        self.model.set_purpose(id, purpose);
    }
    pub fn shells(&self) -> Vec<ShellKind> {
        self.model.shells()
    }
    pub fn is_empty(&self) -> bool {
        self.model.is_empty()
    }
    pub fn add(&mut self, shell: ShellKind) -> u64 {
        self.model.add(shell)
    }
    pub fn add_at(&mut self, shell: ShellKind, cwd: Option<PathBuf>) -> u64 {
        self.model.add_at(shell, cwd)
    }
    pub fn split(&mut self, id: u64) -> Option<u64> {
        self.model.split(id)
    }
    pub fn select(&mut self, id: u64) {
        self.model.select(id);
    }
    pub fn resize_pair(&mut self, left: u64, right: u64, fraction: f32) {
        self.model.resize_pair(left, right, fraction);
    }
    pub fn close(&mut self, id: u64) -> Option<TerminalHandle> {
        let tab = self.model.close(id)?;
        Some(self.project(&tab).controller)
    }
    pub fn sync_layout(&self, layout: &mut crate::domain::application_layout::ApplicationLayout) {
        layout.sync(
            &self
                .model
                .tabs()
                .iter()
                .map(|t| (t.id(), t.group_id()))
                .collect::<Vec<_>>(),
            self.active_id(),
        );
    }
    #[cfg(test)]
    pub fn set_profiles(&mut self, profiles: &[crate::features::terminal::profiles::ShellProfile]) {
        if let Some(runtime) = self.runtime.upgrade() {
            let source = profiles.to_vec();
            runtime.set_profile_source(move || source.clone());
            runtime.synchronize(&self.model);
            runtime.refresh_profiles(&self.model);
        }
    }
    #[cfg(test)]
    pub fn test_profiles(&self) -> Vec<crate::features::terminal::profiles::ShellProfile> {
        self.runtime
            .upgrade()
            .map(|r| r.profiles())
            .unwrap_or_default()
    }
}
#[cfg(test)]
pub(crate) fn test_store(
    cx: &mut lgui::prelude::RenderCx<'_, '_>,
    state: Option<AppContext>,
    initial: impl FnOnce() -> TerminalTabs,
) -> TerminalStore {
    let runtime = cx.state_with(|| Arc::new(TerminalRuntime::default())).get();
    if let Some(state) = state {
        runtime
            .set_profile_source(move || state.get().preferences.values().terminal_profiles.clone());
    }
    let model = cx.state_with(|| initial().model);
    TerminalStore::new(model, &runtime)
}

// Independent service owners are explicit in legacy cross-layer test fixtures.
// Production TerminalTabs projections contain only Weak service references.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    #[derive(Clone)]
    pub(crate) struct Sessions {
        tabs: TerminalTabs,
        owner: Arc<TerminalRuntime>,
    }
    impl std::ops::Deref for Sessions {
        type Target = TerminalTabs;
        fn deref(&self) -> &Self::Target {
            &self.tabs
        }
    }
    impl std::ops::DerefMut for Sessions {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.tabs
        }
    }
    impl Sessions {
        fn bind(tabs: TerminalTabs) -> Self {
            let owner = Arc::new(TerminalRuntime::default());
            Self {
                tabs: tabs.with_runtime(&owner),
                owner,
            }
        }
        pub fn new() -> Self {
            Self::bind(TerminalTabs::new())
        }
        pub fn restored(shells: Vec<ShellKind>, active: Option<usize>) -> Self {
            Self::bind(TerminalTabs::restored(shells, active))
        }
        pub fn restored_with_groups(
            shells: Vec<ShellKind>,
            active: Option<usize>,
            groups: &[usize],
        ) -> Self {
            Self::bind(TerminalTabs::restored_with_groups(shells, active, groups))
        }
        pub fn add(&mut self, shell: ShellKind) -> u64 {
            self.add_at(shell, None)
        }
        pub fn add_at(&mut self, shell: ShellKind, cwd: Option<PathBuf>) -> u64 {
            let id = self.tabs.add_at(shell, cwd);
            self.owner.synchronize(&self.tabs.model);
            id
        }
        pub fn split(&mut self, id: u64) -> Option<u64> {
            let current = self
                .tabs
                .tabs()
                .iter()
                .map(|tab| (tab.id, tab.working_directory()))
                .collect::<Vec<_>>();
            self.tabs.model.update_directories(&current);
            let result = self.tabs.split(id);
            self.owner.synchronize(&self.tabs.model);
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::runtime::AppRuntime;
    use crate::app::terminal_runtime::TerminalDisplay;
    #[cfg(windows)]
    use crate::features::terminal::TerminalSize;
    use crate::features::terminal::TerminalStatus;
    use crate::infrastructure::terminal::notification::FrameNotifier;

    #[test]
    fn model_region_and_operation_snapshots_do_not_keep_app_runtime_or_sessions_alive() {
        let owner = AppRuntime::default();
        let view = TerminalTabs::new().with_runtime(owner.terminals());
        let model = view.clone().into_model();
        let region = view.region_view(&[1], Some(1));
        let handle = region.active().unwrap().controller;
        let weak = Arc::downgrade(owner.terminals());
        assert_eq!(Arc::strong_count(owner.terminals()), 1);
        assert_eq!(owner.terminals().test_len(), 1);
        drop(owner);
        assert!(weak.upgrade().is_none());
        assert_eq!(model.active_id(), Some(1));
        assert_eq!(view.active_id(), Some(1));
        assert!(matches!(handle.snapshot().status, TerminalStatus::Stopped));
        handle.write(b"stale input");
        assert!(handle.selected_text().is_none());
    }
    #[test]
    fn close_removes_model_and_layout_before_release_and_stale_snapshots_cannot_recreate_it() {
        let owner = AppRuntime::default();
        let mut view = TerminalTabs::new().with_runtime(owner.terminals());
        let stale = view.clone();
        let handle = stale.active().unwrap().controller;
        let inspected = handle.test_controller();
        let mut layout = crate::domain::application_layout::ApplicationLayout::initial(
            crate::domain::geometry::Rect::new(0.0, 0.0, 800.0, 600.0),
            100.0,
            100.0,
            200.0,
        );
        view.sync_layout(&mut layout);
        assert!(layout.terminal_region(1).is_some());
        let closing = view.close(1).unwrap();
        assert!(view.close(1).is_none());
        view.sync_layout(&mut layout);
        assert!(layout.terminal_region(1).is_none());
        assert_eq!(owner.terminals().test_len(), 1);
        let closed = view.clone();
        let closed_layout = layout.clone();
        let notifications = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = notifications.clone();
        let notify = FrameNotifier::new(move || {
            assert!(closed.is_empty());
            assert!(closed_layout.terminal_region(1).is_none());
            observed.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        });
        closing.terminate(&notify);
        closing.terminate(&notify);
        assert_eq!(notifications.load(std::sync::atomic::Ordering::Acquire), 1);
        assert_eq!(inspected.test_generation(), 1);
        assert!(matches!(
            inspected.snapshot().status,
            TerminalStatus::Stopped
        ));
        assert_eq!(owner.terminals().test_len(), 0);
        owner.terminals().synchronize(&stale.into_model());
        assert_eq!(owner.terminals().test_len(), 0);
        owner.terminals().shutdown();
        owner.terminals().shutdown();
    }
    #[test]
    fn move_reuses_session_and_split_allocates_one_with_latest_directory() {
        let owner = AppRuntime::default();
        let mut view = TerminalTabs::new().with_runtime(owner.terminals());
        let original = view.active().unwrap().controller.test_controller();
        original.test_parser().process(
            b"preserved screen
",
        );
        original.begin_selection(0, 0);
        original.extend_selection(0, 9);
        let cwd = PathBuf::from("latest runtime cwd");
        *original.test_cwd() = Some(cwd.clone());
        let generation = original.test_generation();
        view.detach_group(1);
        owner.terminals().synchronize(&view.clone().into_model());
        let region = view.region_view(&[1], Some(1));
        let moved = region.active().unwrap().controller.test_controller();
        assert!(original.test_same_runtime(&moved));
        assert_eq!(moved.test_generation(), generation);
        assert_eq!(moved.selected_text(), Some("preserved".into()));
        assert_eq!(moved.working_directory(), Some(cwd.clone()));
        assert_eq!(owner.terminals().test_len(), 1);
        view.update_directories(&[(1, moved.working_directory())]);
        let second = view.split(1).unwrap();
        owner.terminals().synchronize(&view.clone().into_model());
        assert_eq!(owner.terminals().test_len(), 2);
        assert_eq!(view.active_id(), Some(second));
        assert_eq!(view.active().unwrap().cwd, Some(cwd));
        let split = view.active().unwrap().controller.test_controller();
        assert!(!original.test_same_runtime(&split));
        assert!(matches!(split.snapshot().status, TerminalStatus::Idle));
    }
    #[cfg(windows)]
    #[test]
    fn real_pty_moves_with_same_pid_and_owner_teardown_kills_it_with_snapshots_retained() {
        let owner = AppRuntime::default();
        let mut view = TerminalTabs::new().with_runtime(owner.terminals());
        let handle = view.active().unwrap().controller;
        let size = TerminalSize {
            rows: 12,
            cols: 80,
            pixel_width: 560,
            pixel_height: 228,
        };
        handle.ensure_started(Some(&std::env::temp_dir()), size, FrameNotifier::new(|| {}));
        let inspected = handle.test_controller();
        assert!(inspected.test_process_alive());
        struct ObservedProcess(windows_sys::Win32::Foundation::HANDLE);
        impl Drop for ObservedProcess {
            fn drop(&mut self) {
                unsafe {
                    windows_sys::Win32::Foundation::CloseHandle(self.0);
                }
            }
        }
        let pid = inspected.test_pid();
        let processes = [pid].map(|pid| {
            let handle = unsafe {
                windows_sys::Win32::System::Threading::OpenProcess(
                    windows_sys::Win32::System::Threading::PROCESS_SYNCHRONIZE,
                    0,
                    pid,
                )
            };
            assert!(
                !handle.is_null(),
                "could not observe test-created PID {pid}"
            );
            ObservedProcess(handle)
        });
        let generation = inspected.test_generation();
        view.update_directories(&[(1, handle.working_directory())]);
        let sibling = view.split(1).unwrap();
        owner.terminals().synchronize(&view.clone().into_model());
        view.select(1);
        let snapshot = view.clone().into_model();
        let rect = crate::domain::geometry::Rect::new(0.0, 0.0, 1200.0, 800.0);
        let visible = crate::domain::application_layout::Visibility {
            git: true,
            files: true,
            terminal: true,
        };
        let mut layout = crate::domain::application_layout::ApplicationLayout::initial(
            rect, 200.0, 200.0, 240.0,
        );
        view.sync_layout(&mut layout);
        let default_region = layout.terminal_region(1).unwrap();
        let plan = layout
            .plan(
                1,
                &crate::domain::application_layout::Placement::Side(
                    1,
                    crate::domain::pane_layout::Direction::Right,
                ),
                rect,
                visible,
                vec![1, sibling],
            )
            .unwrap();
        let destination = plan.destination;
        assert!(layout.commit(plan, rect, visible, &[1, sibling]));
        assert_ne!(destination, default_region);
        view.detach_group(1);
        owner.terminals().synchronize(&view.clone().into_model());
        let (sessions, active) = layout.terminal_view(destination).unwrap();
        let region = view.region_view(&sessions, active);
        assert_eq!(owner.terminals().test_len(), 2);
        let moved = region.active().unwrap().controller;
        assert_eq!(moved.test_controller().test_pid(), pid);
        assert_eq!(moved.test_controller().test_generation(), generation);
        let weak = Arc::downgrade(owner.terminals());
        assert_eq!(Arc::strong_count(owner.terminals()), 1);
        drop(owner);
        assert!(weak.upgrade().is_none());
        for process in &processes {
            assert_eq!(
                unsafe {
                    windows_sys::Win32::System::Threading::WaitForSingleObject(process.0, 5000)
                },
                windows_sys::Win32::Foundation::WAIT_OBJECT_0,
                "test-created shell did not exit"
            );
        }
        assert!(!inspected.test_process_alive());
        assert_eq!(snapshot.active_id(), Some(1));
        assert_eq!(region.active_id(), Some(1));
        let disposed_generation = inspected.test_generation();
        moved.restart(
            ShellKind::default(),
            None,
            size,
            FrameNotifier::new(|| panic!("stale handle restarted a process")),
        );
        inspected.restart(
            ShellKind::default(),
            None,
            size,
            FrameNotifier::new(|| panic!("disposed resource restarted a process")),
        );
        assert_eq!(inspected.test_generation(), disposed_generation);
        assert!(!inspected.test_process_alive());
        assert!(matches!(moved.snapshot().status, TerminalStatus::Stopped));
    }

    #[cfg(windows)]
    #[test]
    fn startup_notification_can_close_its_own_real_session_without_deadlocking() {
        let owner = AppRuntime::default();
        let view = TerminalTabs::new().with_runtime(owner.terminals());
        let handle = view.active().unwrap().controller;
        let inspected = handle.test_controller();
        let closing = handle.clone();
        let size = TerminalSize {
            rows: 12,
            cols: 80,
            pixel_width: 560,
            pixel_height: 228,
        };
        handle.ensure_started(
            None,
            size,
            FrameNotifier::new(move || closing.terminate(FrameNotifier::new(|| {}))),
        );
        assert_eq!(owner.terminals().test_len(), 0);
        assert!(!inspected.test_process_alive());
        assert!(matches!(
            inspected.snapshot().status,
            TerminalStatus::Stopped
        ));
        assert!(matches!(handle.snapshot().status, TerminalStatus::Stopped));
    }

    #[test]
    fn configuration_source_refreshes_session_profiles_without_a_second_model_copy() {
        let owner = AppRuntime::default();
        let profile = crate::features::terminal::profiles::ShellProfile {
            id: 42,
            name: "Original".into(),
            program: PathBuf::from("missing-shell"),
            args: vec!["--original".into()],
        };
        let source = Arc::new(std::sync::Mutex::new(vec![profile.clone()]));
        let config = source.clone();
        owner
            .terminals()
            .set_profile_source(move || config.lock().unwrap().clone());
        let view = TerminalTabs::restored(vec![ShellKind::Custom(42)], Some(0))
            .with_runtime(owner.terminals());
        let snapshot = view.clone().into_model();
        let handle = view.active().unwrap().controller;
        assert_eq!(handle.short_label(), "Original");
        let mut repaired = profile;
        repaired.name = "Repaired".into();
        repaired.args = vec!["--repaired".into()];
        *source.lock().unwrap() = vec![repaired.clone()];
        owner.terminals().refresh_profiles(&snapshot);
        assert_eq!(handle.short_label(), "Repaired");
        assert_eq!(*handle.test_controller().test_profile(), Some(repaired));
        assert_eq!(snapshot.shells(), vec![ShellKind::Custom(42)]);
        assert_eq!(snapshot.active_id(), Some(1));
        assert_eq!(owner.terminals().test_len(), 1);
    }
}
