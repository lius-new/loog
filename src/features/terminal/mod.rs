//! Terminal rules and data without GUI, PTY or filesystem access.
pub(crate) mod directory;
pub(crate) mod input;
pub(crate) mod mouse;
pub(crate) mod profiles;
pub(crate) mod program;
pub(crate) mod screen;
mod types;
pub(crate) use self::types::{
    SearchStatus, SessionId, ShellKind, TERMINAL_PURPOSES, TerminalCellStyle, TerminalRun,
    TerminalSize, TerminalSnapshot, TerminalStatus,
};

pub mod actions;
pub(crate) mod activity;
pub mod availability;
pub(crate) mod model;
