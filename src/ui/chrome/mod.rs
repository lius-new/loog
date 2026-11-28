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
    hover_signal: Option<lgui::core::Signal<Option<usize>>>,
}
impl HomeBinding {
    pub fn new(
        read: impl Fn() -> HomeFrame + Send + Sync + 'static,
        run: impl Fn(HomeCommand) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            read: Arc::new(read),
            run: Arc::new(run),
            hover_signal: None,
        }
    }
    /// Lets the page repaint hovered rows in place instead of re-rendering.
    pub fn with_hover_signal(mut self, signal: lgui::core::Signal<Option<usize>>) -> Self {
        self.hover_signal = Some(signal);
        self
    }
    pub fn hover_signal(&self) -> Option<&lgui::core::Signal<Option<usize>>> {
        self.hover_signal.as_ref()
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
    pub maximized: bool,
    pub update: Option<crate::features::update::Indicator>,
    /// Open folders were declined, so extensions may not run programs there.
    pub restricted: Option<Restricted>,
    pub layout_open: bool,
    pub project: Project,
    /// The active file below its project folder, one entry per folder and then the file.
    pub path: Vec<PathSegment>,
}
/// The project the title bar names: the open folder the active file is in, else the first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    /// Empty when no folder is open.
    pub name: String,
    pub initial: char,
    pub menu_open: bool,
}
/// One part of the active file's path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathSegment {
    pub label: String,
    /// The folder a click shows in the file tree; none for the file itself.
    pub folder: Option<PathBuf>,
}
/// The title bar's standing note that open folders are not trusted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restricted {
    /// How many open folders were declined.
    pub count: usize,
    /// What the control is called to assistive technology, and what clicking it does.
    pub label: String,
}
#[derive(Clone)]
pub struct TitleBinding {
    read: Arc<dyn Fn() -> TitleFrame + Send + Sync>,
    open_project: Arc<dyn Fn() + Send + Sync>,
    reveal: Arc<dyn Fn(PathBuf) + Send + Sync>,
    update: Arc<dyn Fn() -> bool + Send + Sync>,
    open_layout: Arc<dyn Fn() + Send + Sync>,
    review_trust: Arc<dyn Fn() + Send + Sync>,
}
impl TitleBinding {
    pub fn new(
        read: impl Fn() -> TitleFrame + Send + Sync + 'static,
        open_project: impl Fn() + Send + Sync + 'static,
        reveal: impl Fn(PathBuf) + Send + Sync + 'static,
        update: impl Fn() -> bool + Send + Sync + 'static,
        open_layout: impl Fn() + Send + Sync + 'static,
        review_trust: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self {
            read: Arc::new(read),
            open_project: Arc::new(open_project),
            reveal: Arc::new(reveal),
            update: Arc::new(update),
            open_layout: Arc::new(open_layout),
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
    pub fn open_layout(&self) {
        (self.open_layout)();
    }
    /// Open the project menu, or close it when it is open.
    pub fn open_project(&self) {
        (self.open_project)();
    }
    /// Show a folder of the active file's path in the file tree.
    pub fn reveal(&self, folder: PathBuf) {
        (self.reveal)(folder);
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
    pub vim: Option<(loog_editor_core::vim::Mode, loog_editor_core::vim::Status)>,
    /// What the active file's diagnostics and syntax downloads say.
    pub language: Vec<StatusNote>,
    /// How the language services that answer for the active file are doing together;
    /// none when no service answers for it.
    pub language_services: Option<crate::features::language_services_menu::State>,
    /// Errors, warnings and information in the open files; none hides the item.
    pub problems: Option<ProblemCounts>,
}
/// How many findings of each severity the status bar shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProblemCounts {
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
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
    /// The problem counts: shows or hides the Problems panel.
    Problems,
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
