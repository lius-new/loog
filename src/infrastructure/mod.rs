//! Platform/framework adapters, composed by app.
pub(crate) mod ui_dispatch;

pub(crate) mod edit_journal;
pub(crate) mod explorer_filesystem;
pub(crate) mod extensions;
pub(crate) mod git;
pub(crate) mod search_filesystem;
pub(crate) mod session_filesystem;
pub(crate) mod user_paths;
pub(crate) mod workspace_filesystem;

pub(crate) mod terminal;

pub(crate) mod render_cache_policy;

pub mod config_watcher;
pub(crate) mod process_job;
pub mod project_settings;
pub mod settings_filesystem;

pub mod keymap_filesystem;

pub mod file_loader;
pub(crate) mod file_saver;

pub mod file_watcher;

pub mod document_filesystem;
pub mod file_dialogs;
pub mod file_drop;
pub(crate) mod language_packs;
pub mod query_watcher;
pub(crate) mod syntax_worker;

pub mod launch;
pub(crate) mod update;
pub mod window_geometry;

pub(crate) mod markdown_worker;

pub(crate) mod gif_stream;
pub(crate) mod image_filesystem;
pub mod markdown_resources;

pub(crate) mod editorconfig;
