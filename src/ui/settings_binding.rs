//! Settings value frame, projected drafts and semantic command port.
use super::settings::{FontField, FontTarget, Setting};
use crate::features::settings::rules::Settings;
use crate::ui::components::{input::InputState, state_access::StateAccess};
use std::sync::Arc;
#[derive(Clone, Default)]
pub struct SettingsUiState {
    pub editor_font_input: InputState,
    pub terminal_font_input: InputState,
}
#[derive(Clone)]
pub struct Frame {
    pub prefs: Settings,
    pub scroll: (f32, f32),
    pub profile: crate::ui::terminal::settings_binding::Frame,
    pub terminal_ui: crate::ui::terminal::state::TerminalUiState,
    pub about: crate::features::update::About,
}
pub enum Command {
    Toggle(Setting),
    UiScaleStep(bool),
    UiScaleReset,
    FontPreset(FontTarget, String),
    FontStep(FontTarget, FontField, bool),
    FontCommit(FontTarget),
    FontCancel(FontTarget),
    FontFocus,
    Scroll { delta: f32, max: f32 },
    OpenUserFile,
    OpenKeymap,
}
#[derive(Clone)]
pub struct Binding {
    pub ui: StateAccess<SettingsUiState>,
    pub profile: crate::ui::terminal::settings_binding::Binding,
    read: Arc<dyn Fn() -> Frame + Send + Sync>,
    run: Arc<dyn Fn(Command) + Send + Sync>,
    update: Arc<dyn Fn() -> bool + Send + Sync>,
}
impl Binding {
    pub fn new(
        ui: StateAccess<SettingsUiState>,
        profile: crate::ui::terminal::settings_binding::Binding,
        read: impl Fn() -> Frame + Send + Sync + 'static,
        run: impl Fn(Command) + Send + Sync + 'static,
        update: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            ui,
            profile,
            read: Arc::new(read),
            run: Arc::new(run),
            update: Arc::new(update),
        }
    }
    pub fn frame(&self) -> Frame {
        (self.read)()
    }
    pub fn run(&self, c: Command) {
        (self.run)(c);
    }
    /// Run the Version row's update action; true asks to close the window
    /// so the installer can run.
    pub fn activate_update(&self) -> bool {
        (self.update)()
    }
}
