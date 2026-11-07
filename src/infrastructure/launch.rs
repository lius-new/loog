//! Paths passed on the command line: `Loog.exe <path>...`, the `loog` shell
//! command and the installer's "Open with Loog" Explorer entries.
//!
//! `main` records them once; the initial app state consumes them after the
//! previous session has been restored (see `AppState::restored`).

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static LAUNCH_PATHS: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();

/// Record the process arguments (without the program name).
pub fn init(args: impl IntoIterator<Item = OsString>) {
    let _ = LAUNCH_PATHS.set(Mutex::new(parse(args)));
}

/// The recorded paths, handed out once so a remounted root component never
/// reopens them.
pub fn take_paths() -> Vec<PathBuf> {
    LAUNCH_PATHS
        .get()
        .and_then(|paths| {
            paths
                .lock()
                .ok()
                .map(|mut paths| std::mem::take(&mut *paths))
        })
        .unwrap_or_default()
}

/// Every argument is a path, except `--option` style flags, which are
/// reserved and ignored. A bare `--` ends flag handling.
fn parse(args: impl IntoIterator<Item = OsString>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut flags_done = false;
    for arg in args {
        if !flags_done {
            if arg == "--" {
                flags_done = true;
                continue;
            }
            if arg.to_string_lossy().starts_with("--") {
                continue;
            }
        }
        if !arg.is_empty() {
            paths.push(PathBuf::from(arg));
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn arguments_are_paths_and_long_flags_are_ignored() {
        assert_eq!(
            parse(args(&["src", "--new-window", "README.md", ""])),
            vec![PathBuf::from("src"), PathBuf::from("README.md")]
        );
    }

    #[test]
    fn double_dash_allows_paths_that_look_like_flags() {
        assert_eq!(
            parse(args(&["--", "--weird-name", "file.rs"])),
            vec![PathBuf::from("--weird-name"), PathBuf::from("file.rs")]
        );
    }
}
