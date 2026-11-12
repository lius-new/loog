//! SessionId registry and weak access handles. Only the registry owns sessions.
use crate::features::terminal::model::TerminalModel;
use crate::features::terminal::profiles::ShellProfile;
use crate::features::terminal::screen::Palette;
use crate::features::terminal::{
    SessionId, ShellKind, TerminalSize, TerminalSnapshot, TerminalStatus,
};
use crate::infrastructure::terminal::notification::FrameNotifier;
use crate::infrastructure::terminal::registry::{Registry, Resource};
use crate::infrastructure::terminal::session::TerminalController;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
type Profiles = Arc<dyn Fn() -> Vec<ShellProfile> + Send + Sync>;
pub(crate) struct TerminalRuntime {
    sessions: Registry<TerminalController>,
    profiles: Mutex<Profiles>,
}
impl Default for TerminalRuntime {
    fn default() -> Self {
        Self {
            sessions: Registry::default(),
            profiles: Mutex::new(Arc::new(Vec::new)),
        }
    }
}
impl Resource for TerminalController {
    fn stop(&self) {
        self.dispose();
    }
}
impl TerminalRuntime {
    pub fn set_profile_source(
        &self,
        source: impl Fn() -> Vec<ShellProfile> + Send + Sync + 'static,
    ) {
        *self.profiles.lock().unwrap() = Arc::new(source);
    }
    pub fn profiles(&self) -> Vec<ShellProfile> {
        let source = self.profiles.lock().unwrap().clone();
        source()
    }
    pub fn synchronize(&self, model: &TerminalModel) {
        let profiles = self.profiles();
        for tab in model.tabs() {
            self.sessions.install(tab.id(), || {
                let session = TerminalController::with_shell(tab.shell());
                session.set_profile(
                    profiles
                        .iter()
                        .find(|p| tab.shell() == ShellKind::Custom(p.id))
                        .cloned(),
                );
                session
            });
        }
    }
    pub fn refresh_profiles(&self, model: &TerminalModel) {
        let profiles = self.profiles();
        for tab in model.tabs() {
            if let Some(session) = self.sessions.get(tab.id()) {
                session.set_profile(
                    profiles
                        .iter()
                        .find(|p| tab.shell() == ShellKind::Custom(p.id))
                        .cloned(),
                );
            }
        }
    }
    pub fn handle(self: &Arc<Self>, id: SessionId, shell: ShellKind) -> TerminalHandle {
        TerminalHandle {
            runtime: Arc::downgrade(self),
            id,
            shell,
        }
    }
    pub fn metadata(
        &self,
        ids: &[SessionId],
    ) -> std::collections::HashMap<
        SessionId,
        (
            String,
            Option<PathBuf>,
            crate::features::terminal::activity::CommandActivity,
        ),
    > {
        self.sessions
            .get_many(ids)
            .into_iter()
            .map(|(id, controller)| {
                (
                    id,
                    (
                        controller.short_label(),
                        controller.working_directory(),
                        controller.command_activity(),
                    ),
                )
            })
            .collect()
    }
    pub fn shutdown(&self) {
        self.sessions.shutdown();
    }
    fn with<T>(&self, id: SessionId, f: impl FnOnce(&TerminalController) -> T) -> Option<T> {
        self.sessions.get(id).map(|session| f(&session))
    }
    #[cfg(test)]
    pub fn test_len(&self) -> usize {
        self.sessions.len()
    }
}
#[derive(Clone)]
pub(crate) struct TerminalHandle {
    runtime: Weak<TerminalRuntime>,
    id: SessionId,
    shell: ShellKind,
}
impl TerminalHandle {
    pub fn detached(id: SessionId, shell: ShellKind) -> Self {
        Self {
            runtime: Weak::new(),
            id,
            shell,
        }
    }
    fn with<T>(&self, f: impl FnOnce(&TerminalController) -> T) -> Option<T> {
        self.runtime.upgrade()?.with(self.id, f)
    }
    pub fn generation(&self) -> Option<u64> {
        self.with(TerminalController::generation)
    }
    pub fn id(&self) -> SessionId {
        self.id
    }
    pub fn shell(&self) -> ShellKind {
        self.shell
    }
    pub fn short_label(&self) -> String {
        self.with(TerminalController::short_label)
            .unwrap_or_else(|| self.shell.short_label().into())
    }
    pub fn command_activity(&self) -> crate::features::terminal::activity::CommandActivity {
        self.with(TerminalController::command_activity)
            .unwrap_or_default()
    }
    pub fn working_directory(&self) -> Option<PathBuf> {
        self.with(TerminalController::working_directory).flatten()
    }
    pub fn ensure_started(
        &self,
        cwd: Option<&Path>,
        size: TerminalSize,
        application: impl Into<FrameNotifier>,
    ) {
        self.with(|c| c.ensure_started(cwd, size, application));
    }
    pub fn restart(
        &self,
        shell: ShellKind,
        cwd: Option<&Path>,
        size: TerminalSize,
        application: impl Into<FrameNotifier>,
    ) {
        self.with(|c| c.restart(shell, cwd, size, application));
    }
    pub fn terminate(&self, application: impl Into<FrameNotifier>) {
        if self
            .runtime
            .upgrade()
            .is_some_and(|r| r.sessions.remove(self.id))
        {
            application.into().request_frame();
        }
    }
    pub fn snapshot_with_palette(&self, palette: &Palette) -> TerminalSnapshot {
        self.with(|c| c.snapshot_with_palette(palette))
            .unwrap_or_else(|| TerminalSnapshot {
                shell: self.shell,
                shell_name: self.shell.label().into(),
                notice: None,
                status: TerminalStatus::Stopped,
                rows: 24,
                cols: 80,
                lines: vec![vec![]; 24],
                scrollback: 0,
                scrollback_rows: 0,
                cursor: (0, 0),
                cursor_visible: false,
                application_cursor: false,
                win32_input_mode: false,
                selection: vec![],
                link: None,
            })
    }
    /// Sets the cell whose link is underlined; returns whether it changed.
    pub fn set_link_hover(&self, cell: Option<(u16, u16)>) -> bool {
        self.with(|c| c.set_link_hover(cell)).unwrap_or(false)
    }
    pub fn write(&self, bytes: &[u8]) {
        self.with(|c| c.write(bytes));
    }
    pub fn write_protocol(&self, bytes: &[u8]) {
        self.with(|c| c.write_protocol(bytes));
    }
    pub fn paste(&self, text: &str) {
        self.with(|c| c.paste(text));
    }
    pub fn resize(&self, size: TerminalSize) {
        self.with(|c| c.resize(size));
    }
    pub fn scroll(&self, rows: i32) {
        self.with(|c| c.scroll(rows));
    }
    pub fn set_scrollback(&self, rows: usize) {
        self.with(|c| c.set_scrollback(rows));
    }
    pub fn clear_output(&self) {
        self.with(|c| c.clear_output());
    }
    pub fn search(&self, query: &str) -> crate::features::terminal::SearchStatus {
        self.with(|c| c.search(query)).unwrap_or_default()
    }
    pub fn search_step(&self, delta: i32) -> crate::features::terminal::SearchStatus {
        self.with(|c| c.search_step(delta)).unwrap_or_default()
    }
    pub fn search_clear(&self) {
        self.with(|c| c.search_clear());
    }
    pub fn set_mouse_position(&self, row: u16, col: u16) {
        self.with(|c| c.set_mouse_position(row, col));
    }
    pub fn wheel(&self, delta: f32, modifiers: crate::features::terminal::input::Modifiers) {
        self.with(|c| c.wheel(delta, modifiers));
    }
    pub fn begin_selection(&self, row: u16, col: u16) {
        self.with(|c| c.begin_selection(row, col));
    }
    pub fn url_at(&self, row: u16, cell: u16) -> Option<String> {
        self.with(|c| c.url_at(row, cell)).flatten()
    }
    pub fn press_selection(&self, row: u16, col: u16, cell: u16, extend: bool) {
        self.with(|c| c.press_selection(row, col, cell, extend));
    }
    pub fn extend_selection(&self, row: u16, col: u16) {
        self.with(|c| c.extend_selection(row, col));
    }
    pub fn clear_selection(&self) -> bool {
        self.with(TerminalController::clear_selection)
            .unwrap_or(false)
    }
    pub fn selected_text(&self) -> Option<String> {
        self.with(TerminalController::selected_text).flatten()
    }
    pub fn program_candidate(
        &self,
        probe: &crate::infrastructure::terminal::program::ProgramProbe,
        preferred: Option<u32>,
    ) -> Option<crate::features::terminal::program::ProgramCandidate> {
        self.with(|c| c.program_candidate(probe, preferred))
            .flatten()
    }
    #[cfg(test)]
    pub fn test_controller(&self) -> TerminalController {
        self.with(Clone::clone).expect("test session registered")
    }
}
