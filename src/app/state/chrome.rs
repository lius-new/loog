//! Presentation of the default terminal region.
#[derive(Clone)]
pub struct TerminalChrome {
    pub show_terminal: bool,
    pub terminal_h: f32,
    pub resizing_terminal: bool,
}
impl Default for TerminalChrome {
    fn default() -> Self {
        Self {
            show_terminal: false,
            terminal_h: crate::ui::theme::TERMINAL_H,
            resizing_terminal: false,
        }
    }
}

use super::DrawerMode;
/// Visibility and width of the left-hand Source Control drawer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SidebarChrome {
    pub visible: bool,
    pub width: f32,
    /// True while the drawer resize handle is being dragged.
    pub resizing: bool,
}

impl Default for SidebarChrome {
    fn default() -> Self {
        Self {
            visible: false,
            width: crate::ui::theme::SIDEBAR_W,
            resizing: false,
        }
    }
}

/// Visibility, page and width of the right-hand drawer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawerChrome {
    pub visible: bool,
    pub mode: DrawerMode,
    pub width: f32,
    /// True while the drawer resize handle is being dragged.
    pub resizing: bool,
}

impl Default for DrawerChrome {
    fn default() -> Self {
        Self {
            visible: true,
            mode: DrawerMode::Files,
            width: crate::ui::theme::SIDEBAR_W,
            resizing: false,
        }
    }
}

#[derive(Clone, Default)]
pub struct ChromeState {
    pub toast: Option<crate::domain::notification::Toast>,
    pub toast_seq: u64,
    pub welcome_hover: Option<usize>,
    pub workspace_home_hover: Option<usize>,
    /// The language server panel opened from the status bar.
    pub language_panel: bool,
    pub encoding: Option<crate::app::encoding::Picker>,
    /// The language server whose details card is open, by instance.
    pub language_details: Option<u64>,
    /// The folder the trust question is about. Set from what is still undecided,
    /// so it ends when the question is answered.
    pub trust_prompt: Option<std::path::PathBuf>,
    /// A declined folder the person asked to decide on again, from the title bar.
    pub trust_review: Option<std::path::PathBuf>,
}

impl ChromeState {
    /// The folder the trust question is showing for: one nobody has decided on
    /// comes before one that is being looked at again.
    pub fn trust_question(&self) -> Option<&std::path::Path> {
        self.trust_prompt
            .as_deref()
            .or(self.trust_review.as_deref())
    }
}
