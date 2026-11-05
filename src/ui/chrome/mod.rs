//! Narrow chrome value frames and semantic command ports.
use std::path::PathBuf;
use std::sync::Arc;
#[derive(Clone)]
pub struct HomeFrame {
    pub paths: Vec<PathBuf>,
    pub hover: Option<usize>,
}
pub enum HomeCommand {
    Hover(Option<usize>),
    Focus(bool),
    OpenFile,
    OpenFolder,
    OpenRecent(PathBuf),
    ToggleTerminal,
    OpenSourceControl,
}
#[derive(Clone)]
pub struct HomeBinding {
    read: Arc<dyn Fn() -> HomeFrame + Send + Sync>,
    run: Arc<dyn Fn(HomeCommand) -> bool + Send + Sync>,
}
impl HomeBinding {
    pub fn new(
        read: impl Fn() -> HomeFrame + Send + Sync + 'static,
        run: impl Fn(HomeCommand) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            read: Arc::new(read),
            run: Arc::new(run),
        }
    }
    pub fn frame(&self) -> HomeFrame {
        (self.read)()
    }
    pub fn run(&self, c: HomeCommand) -> bool {
        (self.run)(c)
    }
}
#[derive(Clone)]
pub struct TitleFrame {
    pub settings_open: bool,
    pub extensions_open: bool,
    /// An extension failed to start: the title bar button carries a dot.
    pub extensions_attention: bool,
    pub maximized: bool,
    pub update: Option<crate::features::update::Indicator>,
    /// Open folders were declined, so extensions may not run programs there.
    pub restricted: Option<Restricted>,
    pub drawer_open: bool,
    pub source_control_open: bool,
    pub terminal_open: bool,
}
/// The title bar's standing note that open folders are not trusted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restricted {
    /// How many open folders were declined.
    pub count: usize,
    /// What the control is called to assistive technology, and what clicking it does.
    pub label: String,
}
/// A workspace panel the title bar shows or hides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelToggle {
    Drawer,
    SourceControl,
    Terminal,
}
#[derive(Clone)]
pub struct TitleBinding {
    read: Arc<dyn Fn() -> TitleFrame + Send + Sync>,
    open: Arc<dyn Fn() + Send + Sync>,
    open_extensions: Arc<dyn Fn() + Send + Sync>,
    update: Arc<dyn Fn() -> bool + Send + Sync>,
    toggle: Arc<dyn Fn(PanelToggle) + Send + Sync>,
    review_trust: Arc<dyn Fn() + Send + Sync>,
}
impl TitleBinding {
    pub fn new(
        read: impl Fn() -> TitleFrame + Send + Sync + 'static,
        open: impl Fn() + Send + Sync + 'static,
        open_extensions: impl Fn() + Send + Sync + 'static,
        update: impl Fn() -> bool + Send + Sync + 'static,
        toggle: impl Fn(PanelToggle) + Send + Sync + 'static,
        review_trust: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self {
            read: Arc::new(read),
            open: Arc::new(open),
            open_extensions: Arc::new(open_extensions),
            update: Arc::new(update),
            toggle: Arc::new(toggle),
            review_trust: Arc::new(review_trust),
        }
    }
    /// Ask again about a folder that was declined.
    pub fn review_trust(&self) {
        (self.review_trust)();
    }
    pub fn frame(&self) -> TitleFrame {
        (self.read)()
    }
    pub fn toggle(&self, panel: PanelToggle) {
        (self.toggle)(panel);
    }
    pub fn open_settings(&self) {
        (self.open)();
    }
    pub fn open_extensions(&self) {
        (self.open_extensions)();
    }
    /// Run the update button's action; true asks to close the window so the
    /// installer can run.
    pub fn activate_update(&self) -> bool {
        (self.update)()
    }
}

#[derive(Clone)]
pub struct FileStatus {
    pub line_ending: String,
    pub indent: String,
    pub encoding: String,
    pub lang: crate::domain::document::ResolvedLanguage,
    pub missing: bool,
    pub conflict: bool,
    pub cursor: Option<(usize, usize)>,
    pub is_diff: bool,
    pub is_file: bool,
}
#[derive(Clone)]
pub struct StatusFrame {
    pub extension_widgets: Vec<Arc<crate::features::extensions::ui::Node>>,
    pub pending: Vec<crate::features::keymap::keystroke::Keystroke>,
    pub file: Option<FileStatus>,
    pub vim: Option<(loom_editor_core::vim::Mode, loom_editor_core::vim::Status)>,
    /// What the active file's diagnostics and syntax downloads say.
    pub language: Vec<StatusNote>,
    /// How the language services that answer for the active file are doing together;
    /// none when no service answers for it.
    pub language_services: Option<crate::features::language_services_menu::State>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteLevel {
    Quiet,
    Warning,
    Error,
}
/// One short phrase for the right side of the status bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusNote {
    pub text: String,
    pub level: NoteLevel,
}
#[derive(Clone)]
pub enum StatusCommand {
    ExtensionWidget {
        origin: crate::features::extensions::ui::Origin,
        payload: crate::features::extensions::ui::Payload,
    },
    /// The language badge: which languages are highlighted and which can be installed.
    HighlightMenu,
    /// The language services icon: how each service is doing, and what to install.
    LanguageServices,
    Encoding,
    LineEnding,
    Indent,
}
#[derive(Clone)]
pub struct StatusBinding {
    read: Arc<dyn Fn() -> StatusFrame + Send + Sync>,
    run: Arc<dyn Fn(StatusCommand) + Send + Sync>,
}
impl StatusBinding {
    pub fn new(
        read: impl Fn() -> StatusFrame + Send + Sync + 'static,
        run: impl Fn(StatusCommand) + Send + Sync + 'static,
    ) -> Self {
        Self {
            read: Arc::new(read),
            run: Arc::new(run),
        }
    }
    pub fn frame(&self) -> StatusFrame {
        (self.read)()
    }
    pub fn run(&self, c: StatusCommand) {
        (self.run)(c);
    }
}
pub enum CloseCommand {
    Save,
    Discard,
    Cancel,
}
#[derive(Clone, Copy)]
pub struct CloseOutcome {
    pub exit: bool,
    pub resolved: bool,
}
#[derive(Clone)]
pub struct CloseBinding {
    busy: Arc<dyn Fn() -> bool + Send + Sync>,
    read: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    run: Arc<dyn Fn(CloseCommand) -> CloseOutcome + Send + Sync>,
}
impl CloseBinding {
    pub fn new(
        read: impl Fn() -> Option<String> + Send + Sync + 'static,
        busy: impl Fn() -> bool + Send + Sync + 'static,
        run: impl Fn(CloseCommand) -> CloseOutcome + Send + Sync + 'static,
    ) -> Self {
        Self {
            read: Arc::new(read),
            busy: Arc::new(busy),
            run: Arc::new(run),
        }
    }
    pub fn busy(&self) -> bool {
        (self.busy)()
    }
    pub fn message(&self) -> Option<String> {
        (self.read)()
    }
    pub fn run(&self, c: CloseCommand) -> CloseOutcome {
        (self.run)(c)
    }
}

pub(crate) mod menu_binding;
pub(crate) mod sash_binding;

pub(crate) mod tab_state;
pub(crate) mod tabs_binding;

pub mod diff_binding;

pub(crate) mod encoding_binding;
