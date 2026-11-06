//! Feature entry points. Keymap internals retain the crate::input bridge until R8.
pub(crate) mod encoding;
pub(crate) mod explorer;
pub(crate) mod extensions;
pub(crate) mod git;
pub(crate) mod highlight_menu;
pub(crate) mod highlighting;
pub(crate) mod keymap;
pub(crate) mod language_service;
pub(crate) mod language_services_menu;
pub(crate) mod markdown;
pub(crate) mod outline;
pub(crate) mod output;
pub(crate) mod palette;
pub(crate) mod problems;
pub(crate) mod process_framing;
pub(crate) mod providers;

pub(crate) mod search;

pub(crate) mod terminal;

pub mod settings;
pub(crate) mod syntax;
pub(crate) mod update;
