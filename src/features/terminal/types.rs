//! Pure terminal identity, configuration and display data.
use serde::{Deserialize, Serialize};

pub type SessionId = u64;

pub const TERMINAL_PURPOSES: &[&str] = &[
    "Development",
    "Server",
    "Testing",
    "Build",
    "Debugging",
    "Other",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalSize {
    pub rows: u16,
    pub cols: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

/// How a search went: the number of matches and the selected one, counted from 1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchStatus {
    pub total: usize,
    /// `0` when there is no match.
    pub current: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellKind {
    PowerShell,
    Cmd,
    Bash,
    System,
    /// Stable fingerprint of a registered WSL distribution's GUID.
    Wsl(u64),
    Custom(u64),
}

impl Default for ShellKind {
    fn default() -> Self {
        if cfg!(windows) {
            Self::PowerShell
        } else {
            Self::System
        }
    }
}

impl ShellKind {
    pub const ALL: [Self; 3] = [Self::PowerShell, Self::Cmd, Self::Bash];

    pub const fn label(self) -> &'static str {
        match self {
            Self::PowerShell => "PowerShell",
            Self::Cmd => "Command Prompt",
            Self::Bash => "Bash",
            Self::System => "System default",
            Self::Wsl(_) => "WSL",
            Self::Custom(_) => "Custom terminal",
        }
    }

    pub const fn short_label(self) -> &'static str {
        match self {
            Self::PowerShell => "pwsh",
            Self::Cmd => "cmd",
            Self::Bash => "bash",
            Self::System => "shell",
            Self::Wsl(_) => "wsl",
            Self::Custom(_) => "custom",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalStatus {
    Idle,
    Starting,
    Running,
    Exited(u32),
    Stopped,
    Failed(String),
}

impl TerminalStatus {
    pub fn message(&self) -> Option<String> {
        match self {
            Self::Idle | Self::Running => None,
            Self::Starting => Some("Starting terminal…".to_owned()),
            Self::Exited(code) => Some(format!("Process exited with code {code}")),
            Self::Stopped => Some("Terminal stopped — press + to start it again".to_owned()),
            Self::Failed(error) => Some(format!("Unable to start terminal: {error}")),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalCellStyle {
    pub foreground: Option<u32>,
    pub background: Option<u32>,
    pub bold: bool,
    pub dim: bool,
    pub underline: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalRun {
    pub start_col: u16,
    pub columns: u16,
    pub text: String,
    pub style: TerminalCellStyle,
}

#[derive(Clone, Debug)]
pub struct TerminalSnapshot {
    pub shell: ShellKind,
    pub shell_name: String,
    pub notice: Option<String>,
    pub status: TerminalStatus,
    pub rows: u16,
    pub cols: u16,
    pub lines: Vec<Vec<TerminalRun>>,
    /// View offset from the live screen; zero is the bottom of the history.
    pub scrollback: usize,
    pub scrollback_rows: usize,
    pub cursor: (u16, u16),
    pub cursor_visible: bool,
    pub application_cursor: bool,
    /// Selected cells per visible row as `(row, start_col, end_col)`, end exclusive.
    pub selection: Vec<(u16, u16, u16)>,
    /// Link under the pointer while Ctrl is held, as `(row, start_col, end_col)`.
    pub link: Option<(u16, u16, u16)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shell_labels_are_stable() {
        assert_eq!(
            ShellKind::ALL.map(ShellKind::short_label),
            ["pwsh", "cmd", "bash"]
        );
    }
}
