//! Session JSON and platform paths; no application or terminal state dependency.
use serde::{Serialize, de::DeserializeOwned};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::{env, fs, io};
pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let contents = fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}
pub(crate) fn write_session(path: &Path, session: &impl Serialize) -> io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing session directory"))?;
    fs::create_dir_all(parent)?;
    let contents = serde_json::to_vec_pretty(session).map_err(io::Error::other)?;
    let temporary = parent.join(format!(
        ".loog-session-{}-{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(&contents)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let contents = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    fs::write(path, contents)
}

pub(crate) fn session_path() -> Option<PathBuf> {
    config_path("workspace.json")
}

pub(crate) fn config_path(file_name: &str) -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        super::user_paths::windows_root(|key| env::var_os(key)).map(|path| path.join(file_name))
    }

    #[cfg(target_os = "macos")]
    {
        env::var_os("HOME").map(PathBuf::from).map(|path| {
            path.join("Library")
                .join("Application Support")
                .join("Loog")
                .join(file_name)
        })
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
            return Some(PathBuf::from(path).join("loog").join(file_name));
        }
        env::var_os("HOME")
            .map(PathBuf::from)
            .map(|path| path.join(".config").join("loog").join(file_name))
    }
}

/// Read for a subsequent update. Invalid/future data is preserved, not replaced.
pub(crate) fn read_session<T: DeserializeOwned + Default>(path: &Path) -> io::Result<T> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error),
    }
}
