//! Private composition root. Only bootstrap and the registered state bridge escape.
mod bootstrap;
pub(crate) mod editor;
#[cfg(test)]
mod editor_baseline_tests;
pub(crate) mod editor_keys;
#[cfg(test)]
mod editor_performance;
pub(crate) mod editor_view;
pub(crate) mod editor_vim;
#[cfg(test)]
mod editor_vim_integration_tests;
pub(crate) mod encoding;
pub(crate) mod explorer;
pub(crate) mod explorer_view;
pub(crate) mod extensions;
pub(crate) mod git;
mod git_diff;
mod git_poll;
mod git_tasks;
#[cfg(test)]
mod language_packages_tests;
pub(crate) mod language_services;
pub(crate) mod language_ui;
pub(crate) mod log_panel;
pub(crate) mod palette;
#[cfg(test)]
mod phase0_baseline;
mod presentation;
pub(crate) mod runtime;
pub(crate) mod saving;
#[cfg(test)]
mod saving_tests;
mod shell;
pub(crate) mod state;
pub(crate) mod syntax;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod ui_timer;

pub(crate) use bootstrap::run;

#[cfg(test)]
pub(crate) use bootstrap::svg_icons;

mod git_clone;
mod git_conflict;
mod git_peek;
#[cfg(test)]
mod git_performance;
pub(crate) mod git_view;
#[cfg(test)]
mod git_view_tests;
pub(crate) mod search;
mod search_tasks;
pub(crate) mod workspace;
pub(crate) mod workspace_edits;
mod workspace_git;
pub(crate) mod workspace_restore;
pub(crate) mod workspace_session;

pub(crate) mod terminal;
pub(crate) mod terminal_effects;
#[cfg(test)]
mod terminal_profile_tests;
pub(crate) mod terminal_profiles;
pub(crate) mod terminal_runtime;

pub(crate) mod terminal_binding;
#[cfg(test)]
mod terminal_session_tests;

#[cfg(test)]
mod terminal_command_tests;

pub(crate) mod layout_map;
pub(crate) mod terminal_view;
#[cfg(test)]
mod terminal_view_tests;

#[cfg(test)]
mod terminal_performance;

#[cfg(test)]
mod settings_project_tests;

pub(crate) mod workspace_commands;

pub(crate) mod settings;

pub(crate) mod key_actions;
pub(crate) mod keymap_context;

pub(crate) mod keymap_view;

pub(crate) mod extensions_view;

pub(crate) mod file_loading;

pub(crate) mod file_watcher;

pub(crate) mod window;

pub(crate) mod chrome;

pub(crate) mod close_dialog;
pub(crate) mod highlight_menu;
pub(crate) mod language_services_menu;
pub(crate) mod trust_prompt;

pub(crate) mod pane_chrome;
pub(crate) mod tab_menu;

pub(crate) mod tabs;

pub mod diff_view;

pub(crate) mod context;

pub(crate) mod update;

mod gif_playback;
mod image_loading;
pub(crate) mod image_preview;
pub(crate) mod image_service;
#[cfg(test)]
mod image_tests;
mod markdown_assets;
mod markdown_cards;
mod markdown_drag;
mod markdown_render;
pub(crate) mod markdown_service;
mod markdown_view;
mod outline_view;

#[cfg(test)]
mod markdown_tests;
#[cfg(test)]
mod outline_view_tests;

#[cfg(test)]
mod encoding_tests;

#[cfg(test)]
mod render_context_tests;

pub(crate) mod editing;

pub(crate) mod folding;
pub(crate) mod highlight_inspect;

pub(crate) mod multicursor;

pub(crate) mod layout_visibility;
