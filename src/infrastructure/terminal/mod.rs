//! Terminal process adapters; composed by the application.
pub(crate) mod integration;
pub(crate) mod notification;
pub(crate) mod profiles;
pub(crate) mod program;
pub(crate) mod pty;
pub(crate) mod session;
pub(crate) mod transport;
#[cfg(windows)]
mod wsl;

mod registry;
pub(crate) mod runtime;
