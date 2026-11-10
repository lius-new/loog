//! What is installed, and which version of it is in use.
//!
//! The user's extensions live in `<root>/<id>/<version>/`, and `<root>/extensions.json`
//! says which version of each id is active, whether it is pinned, where it came
//! from and what the user turned off (bundled extensions included). A version
//! folder is never edited after it is installed: updating adds a folder and
//! changes the record, so the previous version is still there to go back to.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

const FILE: &str = "extensions.json";

/// Where an installed version came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Source {
    /// A `.loogext` file the user chose.
    File {
        sha256: String,
        file: Option<String>,
    },
    /// The language registry.
    Registry { sha256: String, release: String },
    /// The extension registry index: the package at `url`, which hashed to `sha256`.
    Index { sha256: String, url: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The version in use: the folder `<id>/<version>`.
    pub version: String,
    /// Kept at this version: an update is not taken until the pin is lifted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    pub source: Source,
    /// The version in use before this one, kept for going back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub extensions: BTreeMap<String, Entry>,
    /// The ids the user turned off, installed or bundled.
    #[serde(default)]
    pub disabled: BTreeSet<String>,
}

/// The folder of one installed version.
pub fn version_dir(root: &Path, id: &str, version: &str) -> PathBuf {
    root.join(id).join(version)
}

/// The recorded state; empty when the file is missing or unreadable.
pub fn read(root: &Path) -> State {
    fs::read_to_string(root.join(FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Replace the file whole, so a crash leaves the old one or the new one.
pub fn write(root: &Path, state: &State) -> Result<(), String> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let fail = |error: std::io::Error| format!("{FILE}: {error}");
    fs::create_dir_all(root).map_err(fail)?;
    let temporary = root.join(format!(
        ".{FILE}-{}-{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let text = serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?;
    fs::write(&temporary, text).map_err(fail)?;
    fs::rename(&temporary, root.join(FILE)).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        fail(error)
    })
}

/// Read, change and write the state.
pub fn update<T>(root: &Path, change: impl FnOnce(&mut State) -> T) -> Result<T, String> {
    // Downloads and UI actions can update different parts of the same record.
    static UPDATE: Mutex<()> = Mutex::new(());
    let _guard = UPDATE.lock().unwrap();
    let mut state = read(root);
    let result = change(&mut state);
    write(root, &state)?;
    Ok(result)
}

/// The folders in use, one per installed extension that has its folder, by id.
/// Extensions the user turned off are included only when `enabled_only` is false.
pub fn folders(root: &Path, enabled_only: bool) -> Vec<PathBuf> {
    let state = read(root);
    state
        .extensions
        .iter()
        .filter(|(id, _)| !(enabled_only && state.disabled.contains(*id)))
        .map(|(id, entry)| version_dir(root, id, &entry.version))
        .filter(|dir| dir.join("manifest.json").is_file())
        .collect()
}

/// The ids the user turned off.
pub fn disabled(root: &Path) -> BTreeSet<String> {
    read(root).disabled
}

pub fn set_enabled(root: &Path, id: &str, enabled: bool) -> Result<(), String> {
    update(root, |state| {
        if enabled {
            state.disabled.remove(id);
        } else {
            state.disabled.insert(id.to_owned());
        }
    })
}

/// Record that `version` of `id` is now the one in use. Returns the version it
/// replaced. Version folders other than the new one and the one before it are
/// deleted, as is anything left from the old flat layout.
pub fn record_install(
    root: &Path,
    id: &str,
    version: &str,
    source: Source,
) -> Result<Option<String>, String> {
    let replaced = update(root, |state| {
        let old = state.extensions.get(id).map(|entry| entry.version.clone());
        let pinned = state.extensions.get(id).is_some_and(|entry| entry.pinned);
        let previous = old.clone().filter(|old| old != version).or_else(|| {
            state
                .extensions
                .get(id)
                .and_then(|entry| entry.previous.clone())
        });
        state.extensions.insert(
            id.to_owned(),
            Entry {
                version: version.to_owned(),
                pinned,
                source,
                previous,
            },
        );
        old
    })?;
    prune(root, id);
    Ok(replaced)
}

/// Put the record of `id` back as it was in `before` after an installation of
/// `installed` failed further on, and delete the folder that installation made.
pub fn undo_install(
    root: &Path,
    id: &str,
    before: Option<&Entry>,
    installed: &str,
) -> Result<(), String> {
    update(root, |state| match before {
        Some(entry) => {
            let mut entry = entry.clone();
            // Pruning may have deleted the version kept for going back.
            if entry
                .previous
                .as_ref()
                .is_some_and(|previous| !version_dir(root, id, previous).is_dir())
            {
                entry.previous = None;
            }
            state.extensions.insert(id.to_owned(), entry);
        }
        None => {
            state.extensions.remove(id);
        }
    })?;
    if before.is_none_or(|entry| entry.version != installed) {
        let _ = fs::remove_dir_all(version_dir(root, id, installed));
        if before.is_none() {
            let _ = fs::remove_dir(root.join(id));
        }
    }
    Ok(())
}

/// Keep `id` at the version it has (or stop keeping it there).
pub fn set_pinned(root: &Path, id: &str, pinned: bool) -> Result<(), String> {
    update(root, |state| {
        state
            .extensions
            .get_mut(id)
            .map(|entry| entry.pinned = pinned)
            .ok_or_else(|| format!("{id} is not installed"))
    })?
}

/// Go back to the version in use before the current one. The current one becomes
/// the version to go forward to. Returns `(from, to)`.
pub fn roll_back(root: &Path, id: &str) -> Result<(String, String), String> {
    update(root, |state| {
        let entry = state
            .extensions
            .get_mut(id)
            .ok_or_else(|| format!("{id} is not installed"))?;
        let previous = entry
            .previous
            .clone()
            .filter(|previous| {
                version_dir(root, id, previous)
                    .join("manifest.json")
                    .is_file()
            })
            .ok_or_else(|| format!("there is no earlier version of {id} to go back to"))?;
        let from = std::mem::replace(&mut entry.version, previous.clone());
        entry.previous = Some(from.clone());
        Ok((from, previous))
    })?
}

/// Delete the version folders of `id` that the record neither uses nor keeps.
fn prune(root: &Path, id: &str) {
    let Some(entry) = read(root).extensions.remove(id) else {
        return;
    };
    let Ok(entries) = fs::read_dir(root.join(id)) else {
        return;
    };
    for found in entries.flatten() {
        let name = found.file_name().to_string_lossy().into_owned();
        if name != entry.version && Some(&name) != entry.previous.as_ref() {
            let _ = fs::remove_dir_all(found.path());
        }
    }
}

/// Forget `id`, delete everything installed of it and return the folder it was in use from.
pub fn remove(root: &Path, id: &str) -> Result<(), String> {
    update(root, |state| {
        state.extensions.remove(id);
        state.disabled.remove(id);
    })?;
    match fs::remove_dir_all(root.join(id)) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("{}: {error}", root.join(id).display()))
        }
        _ => Ok(()),
    }
}

/// Whether `dir` is the folder of an installed version under `root`, and its id.
pub fn installed_id(root: &Path, dir: &Path) -> Option<String> {
    let id = dir.parent()?.file_name()?.to_str()?;
    (dir.parent()?.parent()? == root && read(root).extensions.contains_key(id))
        .then(|| id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(sha: &str) -> Source {
        Source::File {
            sha256: sha.into(),
            file: None,
        }
    }

    fn root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("loog-state-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn make(root: &Path, id: &str, version: &str) {
        let dir = version_dir(root, id, version);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("manifest.json"), "{}").unwrap();
    }

    #[test]
    fn a_missing_or_damaged_file_is_an_empty_state() {
        let dir = root("empty");
        assert_eq!(read(&dir), State::default());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(FILE), "{ nope").unwrap();
        assert_eq!(read(&dir), State::default());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn updating_keeps_the_previous_version_and_deletes_older_ones() {
        let dir = root("update");
        for version in ["1.0.0", "1.1.0", "1.2.0"] {
            make(&dir, "a.one", version);
            let replaced = record_install(&dir, "a.one", version, file(version)).unwrap();
            assert_eq!(replaced.is_some(), version != "1.0.0");
        }
        let entry = &read(&dir).extensions["a.one"];
        assert_eq!(
            (entry.version.as_str(), entry.previous.as_deref()),
            ("1.2.0", Some("1.1.0"))
        );
        assert!(!version_dir(&dir, "a.one", "1.0.0").exists());
        assert!(version_dir(&dir, "a.one", "1.1.0").exists());
        assert_eq!(folders(&dir, true), [version_dir(&dir, "a.one", "1.2.0")]);
        // Installing the version in use again does not make it its own previous.
        record_install(&dir, "a.one", "1.2.0", file("again")).unwrap();
        assert_eq!(
            read(&dir).extensions["a.one"].previous.as_deref(),
            Some("1.1.0")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn turning_off_and_removing_are_remembered() {
        let dir = root("off");
        make(&dir, "a.one", "1.0.0");
        make(&dir, "b.two", "2.0.0");
        record_install(&dir, "a.one", "1.0.0", file("a")).unwrap();
        record_install(&dir, "b.two", "2.0.0", file("b")).unwrap();
        set_enabled(&dir, "a.one", false).unwrap();
        set_enabled(&dir, "loog.bundled", false).unwrap();
        assert_eq!(folders(&dir, true), [version_dir(&dir, "b.two", "2.0.0")]);
        assert_eq!(folders(&dir, false).len(), 2);
        assert!(disabled(&dir).contains("loog.bundled"));
        assert_eq!(
            installed_id(&dir, &version_dir(&dir, "a.one", "1.0.0")).as_deref(),
            Some("a.one")
        );
        assert_eq!(installed_id(&dir, &dir.join("a.one")), None);
        remove(&dir, "a.one").unwrap();
        assert!(!dir.join("a.one").exists());
        assert!(!read(&dir).disabled.contains("a.one"));
        assert_eq!(folders(&dir, false), [version_dir(&dir, "b.two", "2.0.0")]);
        let _ = fs::remove_dir_all(&dir);
    }
}
