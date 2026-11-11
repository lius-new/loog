//! Read-only frames, projected interaction state and semantic command ports.
use super::state::{TerminalTabMenuKind, TerminalUiState};
use crate::features::terminal::actions::MenuAction;
use crate::features::terminal::input::Modifiers;
use crate::features::terminal::model::TerminalModel;
use crate::features::terminal::{SessionId, ShellKind, TerminalSnapshot};
use crate::ui::components::state_access::StateAccess;
use lgui::services::{Clipboard, ClipboardError};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
pub struct SessionFrame {
    pub id: SessionId,
    pub group_id: u64,
    pub width_weight: f32,
    pub shell: ShellKind,
    pub short_label: String,
    pub title: String,
    pub cwd: Option<PathBuf>,
    /// The user's own name for the session, when there is one.
    pub title_override: Option<String>,
    pub purpose: Option<String>,
    pub running_program: Option<String>,
}
impl SessionFrame {
    pub fn label(&self) -> String {
        self.title.clone()
    }
    pub fn working_directory(&self) -> Option<PathBuf> {
        self.cwd.clone()
    }
}
#[derive(Clone)]
pub struct View {
    model: TerminalModel,
    tabs: Vec<SessionFrame>,
}
impl View {
    pub fn new(model: TerminalModel, tabs: Vec<SessionFrame>) -> Self {
        Self { model, tabs }
    }
    pub fn model(&self) -> &TerminalModel {
        &self.model
    }
    pub fn tabs(&self) -> Vec<SessionFrame> {
        self.tabs.clone()
    }
    pub fn active_id(&self) -> Option<SessionId> {
        self.model.active_id()
    }
    pub fn active(&self) -> Option<SessionFrame> {
        self.tabs
            .iter()
            .find(|t| Some(t.id) == self.active_id())
            .cloned()
    }
    pub fn active_group(&self) -> Vec<SessionFrame> {
        let Some(active) = self.active() else {
            return Vec::new();
        };
        self.tabs
            .iter()
            .filter(|t| t.group_id == active.group_id)
            .cloned()
            .collect()
    }
    pub fn is_empty(&self) -> bool {
        self.model.is_empty()
    }
    pub fn region_view(&self, ids: &[SessionId], active: Option<SessionId>) -> Self {
        let model = self.model.region_view(ids, active);
        let tabs = model
            .tabs()
            .iter()
            .filter_map(|meta| {
                let mut tab = self.tabs.iter().find(|t| t.id == meta.id())?.clone();
                tab.group_id = meta.group_id();
                Some(tab)
            })
            .collect();
        Self { model, tabs }
    }
}
#[derive(Clone)]
pub struct Chrome {
    pub resizing_terminal: bool,
    pub terminal_h: f32,
}
#[derive(Clone)]
pub struct Frame {
    pub workspace_roots: Vec<PathBuf>,
    pub ui: TerminalUiState,
    pub chrome: Chrome,
    pub modifiers: lgui::core::KeyModifiers,
    pub shell_options: Vec<crate::features::terminal::profiles::ShellOption>,
    /// The default bottom terminal region fills its split.
    pub maximized: bool,
    /// The region that owns the panel tools (maximize, collapse). Every other
    /// terminal region is a detached group of tabs.
    pub default_region: Option<u64>,
    /// The tab strip of every terminal region, in display order.
    pub regions: Vec<Vec<u64>>,
}
#[derive(Clone)]
pub struct Binding {
    pub ui: StateAccess<TerminalUiState>,
    read: Arc<dyn Fn() -> Frame + Send + Sync>,
    run: Arc<dyn Fn(Command) -> Outcome + Send + Sync>,
}
impl Binding {
    pub fn new(
        ui: StateAccess<TerminalUiState>,
        read: impl Fn() -> Frame + Send + Sync + 'static,
        run: impl Fn(Command) -> Outcome + Send + Sync + 'static,
    ) -> Self {
        Self {
            ui,
            read: Arc::new(read),
            run: Arc::new(run),
        }
    }
    pub fn get(&self) -> Frame {
        (self.read)()
    }
    pub fn run(&self, command: Command) -> Outcome {
        (self.run)(command)
    }
}
type Clip = dyn Fn(SessionId, ClipboardShortcut, &dyn Clipboard) -> Result<(), ClipboardError>
    + Send
    + Sync;
#[derive(Clone)]
pub struct Sessions {
    view: Arc<dyn Fn() -> View + Send + Sync>,
    screen: Arc<dyn Fn(SessionId) -> TerminalSnapshot + Send + Sync>,
    run: Arc<dyn Fn(Command) -> Outcome + Send + Sync>,
    clipboard: Arc<Clip>,
}
impl Sessions {
    pub fn new(
        view: impl Fn() -> View + Send + Sync + 'static,
        screen: impl Fn(SessionId) -> TerminalSnapshot + Send + Sync + 'static,
        run: impl Fn(Command) -> Outcome + Send + Sync + 'static,
        clipboard: impl Fn(SessionId, ClipboardShortcut, &dyn Clipboard) -> Result<(), ClipboardError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            view: Arc::new(view),
            screen: Arc::new(screen),
            run: Arc::new(run),
            clipboard: Arc::new(clipboard),
        }
    }
    pub fn get(&self) -> View {
        (self.view)()
    }
    pub fn screen(&self, id: SessionId) -> TerminalSnapshot {
        (self.screen)(id)
    }
    pub fn short_label(&self, id: SessionId) -> String {
        self.get()
            .tabs()
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.short_label.clone())
            .unwrap_or_default()
    }
    pub fn run(&self, command: Command) -> Outcome {
        (self.run)(command)
    }
    pub fn clipboard(
        &self,
        id: SessionId,
        shortcut: ClipboardShortcut,
        clipboard: &dyn Clipboard,
    ) -> Result<(), ClipboardError> {
        (self.clipboard)(id, shortcut, clipboard)
    }
}
#[derive(Clone)]
pub struct FrameRequest(Arc<dyn Fn() + Send + Sync>);
impl FrameRequest {
    pub fn new(callback: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(callback))
    }
    pub fn request_frame(&self) {
        (self.0)()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardShortcut {
    Copy,
    CopyOrInterrupt,
    Paste,
    CopyOrPaste,
}
pub enum Command {
    SelectPanel(super::state::PanelTab),
    ChooseWorkspace(PathBuf),
    Select(SessionId),
    New {
        target: SessionId,
        shell: Option<ShellKind>,
    },
    Split(SessionId),
    Menu {
        target: SessionId,
        action: MenuAction,
    },
    Close {
        target: SessionId,
        action: MenuAction,
    },
    Purpose {
        target: SessionId,
        purpose: Option<String>,
    },
    ShellMenu(SessionId),
    BeginRename(SessionId),
    CommitRename,
    CancelRename,
    ClearOutput(SessionId),
    ToggleMaximize,
    /// Hides the default bottom terminal panel.
    CollapsePanel,
    /// Opens the find box over a terminal, or closes it when already open there.
    ToggleSearch(SessionId),
    /// Searches for the text now in the find box.
    Search,
    /// Moves to the next (`+1`) or previous (`-1`) match.
    SearchStep(i32),
    CloseSearch,
    ProtocolInput {
        target: SessionId,
        bytes: Vec<u8>,
    },
    Write {
        target: SessionId,
        bytes: Vec<u8>,
    },
    ResizePair {
        left: SessionId,
        right: SessionId,
        fraction: f32,
    },
    Mouse {
        target: SessionId,
        row: u16,
        col: u16,
    },
    /// Pointer cell to underline a link at while Ctrl is held; `None` clears.
    /// `applied` reports whether the underline changed.
    LinkHover {
        target: SessionId,
        cell: Option<(u16, u16)>,
    },
    /// Ctrl-click: opens the link at the cell; `applied` is false without one.
    OpenLink {
        target: SessionId,
        row: u16,
        cell: u16,
    },
    /// Left press: starts, extends (shift) or widens (double/triple click) the
    /// selection. `col` is the nearest cell boundary, `cell` the cell hit.
    PressSelection {
        target: SessionId,
        row: u16,
        col: u16,
        cell: u16,
        extend: bool,
    },
    ExtendSelection {
        target: SessionId,
        row: u16,
        col: u16,
    },
    Wheel {
        target: SessionId,
        delta: f32,
        modifiers: Modifiers,
    },
    Scrollback {
        target: SessionId,
        rows: usize,
    },
    Focus,
    ResizeStart,
    Resize(f32),
    ResizeEnd,
    OpenMenu {
        target: SessionId,
        position: (f32, f32),
        kind: TerminalTabMenuKind,
    },
    ClipboardError(String),
}
#[derive(Default)]
pub struct Outcome {
    pub applied: bool,
    pub created: Option<SessionId>,
}

pub fn select(_state: &Binding, tabs: &Sessions, id: SessionId) -> bool {
    tabs.run(Command::Select(id)).applied
}
pub fn new_from_target(
    _state: &Binding,
    tabs: &Sessions,
    target: SessionId,
    shell: Option<ShellKind>,
) -> Option<SessionId> {
    tabs.run(Command::New { target, shell }).created
}
pub fn split(tabs: &Sessions, target: SessionId) -> Option<SessionId> {
    tabs.run(Command::Split(target)).created
}
pub fn shell_menu(state: &Binding, _tabs: &Sessions, id: SessionId) -> bool {
    state.run(Command::ShellMenu(id)).applied
}
pub fn resize_pair(tabs: &Sessions, left: SessionId, right: SessionId, fraction: f32) {
    tabs.run(Command::ResizePair {
        left,
        right,
        fraction,
    });
}
pub fn close(
    _state: &Binding,
    tabs: &Sessions,
    target: SessionId,
    action: MenuAction,
    _request: &FrameRequest,
) -> bool {
    tabs.run(Command::Close { target, action }).applied
}
pub fn protocol_input(tabs: &Sessions, target: SessionId, bytes: &[u8]) {
    tabs.run(Command::ProtocolInput {
        target,
        bytes: bytes.to_vec(),
    });
}
pub fn write(tabs: &Sessions, target: SessionId, bytes: &[u8]) {
    tabs.run(Command::Write {
        target,
        bytes: bytes.to_vec(),
    });
}
pub fn mouse_position(tabs: &Sessions, target: SessionId, row: u16, col: u16) {
    tabs.run(Command::Mouse { target, row, col });
}
/// Returns whether the underlined link changed.
pub fn link_hover(tabs: &Sessions, target: SessionId, cell: Option<(u16, u16)>) -> bool {
    tabs.run(Command::LinkHover { target, cell }).applied
}
/// Opens the link under the pointer; returns whether there was one.
pub fn open_link(tabs: &Sessions, target: SessionId, row: u16, cell: u16) -> bool {
    tabs.run(Command::OpenLink { target, row, cell }).applied
}
pub fn press_selection(
    tabs: &Sessions,
    target: SessionId,
    row: u16,
    col: u16,
    cell: u16,
    extend: bool,
) {
    tabs.run(Command::PressSelection {
        target,
        row,
        col,
        cell,
        extend,
    });
}
pub fn extend_selection(tabs: &Sessions, target: SessionId, row: u16, col: u16) {
    tabs.run(Command::ExtendSelection { target, row, col });
}
pub fn wheel(tabs: &Sessions, target: SessionId, delta: f32, modifiers: Modifiers) {
    tabs.run(Command::Wheel {
        target,
        delta,
        modifiers,
    });
}
pub fn clipboard(
    tabs: &Sessions,
    target: SessionId,
    shortcut: ClipboardShortcut,
    clipboard: &dyn Clipboard,
) -> Result<(), ClipboardError> {
    tabs.clipboard(target, shortcut, clipboard)
}
pub fn report_clipboard_error(state: &Binding, result: Result<(), ClipboardError>) {
    if let Err(error) = result {
        state.run(Command::ClipboardError(error.to_string()));
    }
}
pub fn menu_action(
    _state: &Binding,
    tabs: &Sessions,
    target: SessionId,
    action: MenuAction,
    _request: &FrameRequest,
) -> bool {
    tabs.run(Command::Menu { target, action }).applied
}
pub fn purpose(_state: &Binding, tabs: &Sessions, target: SessionId, purpose: Option<&str>) {
    tabs.run(Command::Purpose {
        target,
        purpose: purpose.map(str::to_owned),
    });
}
