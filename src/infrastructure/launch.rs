//! Paths passed on the command line: `Loog.exe [--trust] <path>...`, the `loog` shell
//! command and the installer's "Open with Loog" Explorer entries.
//!
//! `main` records them once; the initial app state consumes them after the
//! previous session has been restored (see `AppState::restored`).

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static LAUNCH_PATHS: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();
static TRUSTED_PATHS: OnceLock<Vec<PathBuf>> = OnceLock::new();

/// Record the process arguments (without the program name).
pub fn init(args: impl IntoIterator<Item = OsString>) {
    let launch = parse(args);
    let _ = TRUSTED_PATHS.set(if launch.trust {
        launch.paths.clone()
    } else {
        Vec::new()
    });
    let _ = LAUNCH_PATHS.set(Mutex::new(launch.paths));
}

/// The paths the user asked to trust with `--trust`; empty without that flag.
pub fn trusted_paths() -> Vec<PathBuf> {
    TRUSTED_PATHS.get().cloned().unwrap_or_default()
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

struct Launch {
    paths: Vec<PathBuf>,
    /// `--trust`: the folders named on the command line are trusted, as if the
    /// trust question had been answered yes.
    trust: bool,
}

/// Every argument is a path, except `--option` style flags: `--trust` is
/// understood, any other is reserved and ignored. A bare `--` ends flag handling.
fn parse(args: impl IntoIterator<Item = OsString>) -> Launch {
    let mut launch = Launch {
        paths: Vec::new(),
        trust: false,
    };
    let mut flags_done = false;
    for arg in args {
        if !flags_done {
            if arg == "--" {
                flags_done = true;
                continue;
            }
            if arg == "--trust" {
                launch.trust = true;
                continue;
            }
            if arg.to_string_lossy().starts_with("--") {
                continue;
            }
        }
        if !arg.is_empty() {
            launch.paths.push(PathBuf::from(arg));
        }
    }
    launch
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn arguments_are_paths_and_long_flags_are_ignored() {
        let launch = parse(args(&["src", "--new-window", "README.md", ""]));
        assert_eq!(
            launch.paths,
            vec![PathBuf::from("src"), PathBuf::from("README.md")]
        );
        assert!(!launch.trust);
    }

    #[test]
    fn double_dash_allows_paths_that_look_like_flags() {
        let launch = parse(args(&["--", "--weird-name", "--trust", "file.rs"]));
        assert_eq!(
            launch.paths,
            vec![
                PathBuf::from("--weird-name"),
                PathBuf::from("--trust"),
                PathBuf::from("file.rs")
            ]
        );
        assert!(!launch.trust);
    }

    #[test]
    fn trust_flag_marks_the_launch_and_is_not_a_path() {
        let launch = parse(args(&["--trust", "project"]));
        assert_eq!(launch.paths, vec![PathBuf::from("project")]);
        assert!(launch.trust);
    }
}
