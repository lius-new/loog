//! Explorer receives disk and shell access through these injected interfaces.
use super::{DirEntry, ExplorerTargetKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub trait DirectoryPort {
    /// One directory's entries sorted by `tree::sort_entries`; an unreadable
    /// directory lists as empty.
    fn read_directory(&self, directory: &Path) -> Arc<Vec<DirEntry>>;
    fn is_dir(&self, path: &Path) -> bool;
    fn exists(&self, path: &Path) -> bool;
    /// Canonical form of `path`, or `None` when it cannot be resolved.
    fn canonicalize(&self, path: &Path) -> Option<PathBuf>;
}

pub trait FileOpsPort {
    /// Create an empty file, creating missing parent folders. Fails if the
    /// file already exists.
    fn create_file(&self, path: &Path) -> std::io::Result<()>;
    fn create_folder(&self, path: &Path) -> std::io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()>;
    /// Returns `Ok(false)` when the user cancelled the platform operation.
    fn move_to_trash(&self, path: &Path) -> Result<bool, String>;
    fn set_file_clipboard(&self, paths: &[&Path], cut: bool) -> Result<(), String>;
    fn reveal(&self, path: &Path, kind: ExplorerTargetKind) -> std::io::Result<()>;
}
