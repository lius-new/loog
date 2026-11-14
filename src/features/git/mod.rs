//! Git rules: repository types, status/diff parsing, the UI snapshot and the
//! diff documents built from it. No process, filesystem, thread or GUI access.
pub mod actions;
pub mod conflict;
pub mod diff;
pub mod diff_document;
pub mod diff_selection;
#[cfg(test)]
pub(crate) mod diff_selection_tests;
pub mod diff_text;
pub mod error;
pub mod model;
pub mod operation;
pub mod parser;
pub mod snapshot;
pub mod split_layout;
pub mod types;

pub use self::error::{GitError, GitResult};
pub use self::snapshot::{GitStoreSnapshot, PollTarget};
pub use self::types::*;
pub mod clone_url;
