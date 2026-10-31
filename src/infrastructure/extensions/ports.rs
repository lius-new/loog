//! Disk-backed storage and bounded file access for extensions (design §6, §9).
use crate::features::extensions::manifest::check_package_path;
use loom_extension_api::{ErrorKind, ExtensionError};
use loom_extension_host::{FilePort, StoragePort};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Stored values of one extension, one JSON file per scope under `dir`.
pub struct FsStorage {
    dir: PathBuf,
}

impl FsStorage {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Scope names come from the host, but the file name is still restricted
    /// to harmless characters so a scope can never name another directory.
    fn file(&self, scope: &str) -> Option<PathBuf> {
        let safe = !scope.is_empty()
            && scope
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            && !scope.starts_with('.');
        safe.then(|| self.dir.join(format!("{scope}.json")))
    }
}

impl StoragePort for FsStorage {
    fn load(&mut self, scope: &str) -> BTreeMap<String, String> {
        // A missing or damaged file is an empty scope, never an error.
        self.file(scope)
            .and_then(|file| std::fs::read_to_string(file).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save(&mut self, scope: &str, values: &BTreeMap<String, String>) -> Result<(), String> {
        let file = self.file(scope).ok_or("invalid storage scope")?;
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let partial = file.with_extension("json.partial");
        let text = serde_json::to_string(values).map_err(|e| e.to_string())?;
        std::fs::write(&partial, text).map_err(|e| e.to_string())?;
        std::fs::rename(&partial, &file).map_err(|e| e.to_string())
    }
}

/// Reads and searches files inside a folder.
pub struct FsFiles;

/// Files looked at by one search before it gives up.
const SCAN_LIMIT: usize = 200_000;

fn refuse(kind: ErrorKind, message: &str) -> ExtensionError {
    ExtensionError::new(kind, message)
}

impl FilePort for FsFiles {
    fn read(
        &self,
        root: &Path,
        relative: &str,
        max: usize,
    ) -> Result<(String, bool), ExtensionError> {
        if check_package_path(relative).is_err() {
            return Err(refuse(
                ErrorKind::PermissionDenied,
                "the path must stay inside the folder",
            ));
        }
        let base = root
            .canonicalize()
            .map_err(|_| refuse(ErrorKind::ExpiredHandle, "the folder cannot be read"))?;
        // Resolving links first means a link out of the folder is caught here.
        let file = base
            .join(relative)
            .canonicalize()
            .map_err(|_| refuse(ErrorKind::InvalidEdit, "the file does not exist"))?;
        if !file.starts_with(&base) {
            return Err(refuse(
                ErrorKind::PermissionDenied,
                "the path must stay inside the folder",
            ));
        }
        if !file.is_file() {
            return Err(refuse(ErrorKind::InvalidEdit, "not a file"));
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&file)
            .and_then(|handle| handle.take(max as u64 + 1).read_to_end(&mut bytes))
            .map_err(|e| refuse(ErrorKind::ExtensionFailed, &e.to_string()))?;
        let truncated = bytes.len() > max;
        bytes.truncate(max);
        if bytes[..bytes.len().min(8192)].contains(&0) {
            return Err(refuse(ErrorKind::InvalidEdit, "not a text file"));
        }
        match String::from_utf8(bytes) {
            Ok(text) => Ok((text, truncated)),
            Err(error) => {
                // Cutting at the limit can split a character; anything else is not UTF-8.
                let valid = error.utf8_error().valid_up_to();
                if truncated && error.utf8_error().error_len().is_none() {
                    let bytes = error.into_bytes();
                    Ok((String::from_utf8_lossy(&bytes[..valid]).into_owned(), true))
                } else {
                    Err(refuse(
                        ErrorKind::InvalidEdit,
                        "the file is not valid UTF-8 text",
                    ))
                }
            }
        }
    }

    fn find(
        &self,
        root: &Path,
        pattern: &str,
        limit: usize,
    ) -> Result<Vec<String>, ExtensionError> {
        let glob = globset::Glob::new(pattern)
            .map_err(|e| refuse(ErrorKind::InvalidEdit, &format!("invalid pattern: {e}")))?
            .compile_matcher();
        let base = root
            .canonicalize()
            .map_err(|_| refuse(ErrorKind::ExpiredHandle, "the folder cannot be read"))?;
        let mut found = Vec::new();
        for (scanned, entry) in ignore::WalkBuilder::new(&base)
            .follow_links(false)
            .build()
            .flatten()
            .filter(|entry| entry.file_type().is_some_and(|t| t.is_file()))
            .enumerate()
        {
            if scanned >= SCAN_LIMIT || found.len() >= limit {
                break;
            }
            let Ok(relative) = entry.path().strip_prefix(&base) else {
                continue;
            };
            let relative = relative.to_string_lossy().replace('\\', "/");
            if glob.is_match(&relative) {
                found.push(relative);
            }
        }
        found.sort();
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("loom-ports-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "fn main() {}\n中文").unwrap();
        std::fs::write(dir.join("README.md"), "hello").unwrap();
        std::fs::write(dir.join("blob.bin"), [0u8, 1, 2, 3]).unwrap();
        dir
    }

    #[test]
    fn files_inside_the_folder_are_read_and_long_ones_are_cut_on_a_character() {
        let dir = folder("read");
        let (text, truncated) = FsFiles.read(&dir, "src/lib.rs", 1000).unwrap();
        assert_eq!((text.as_str(), truncated), ("fn main() {}\n中文", false));
        // 13 bytes is the middle of the first CJK character.
        let (cut, truncated) = FsFiles.read(&dir, "src/lib.rs", 14).unwrap();
        assert!(truncated && cut == "fn main() {}\n", "{cut:?}");
    }

    #[test]
    fn paths_that_leave_the_folder_binary_files_and_missing_files_are_refused() {
        let dir = folder("refuse");
        for path in ["../x", "/etc/passwd", "src/../../x", "C:/x", "src\\..\\x"] {
            assert_eq!(
                FsFiles.read(&dir, path, 10).unwrap_err().kind,
                ErrorKind::PermissionDenied,
                "{path}"
            );
        }
        assert_eq!(
            FsFiles.read(&dir, "blob.bin", 10).unwrap_err().kind,
            ErrorKind::InvalidEdit
        );
        assert_eq!(
            FsFiles.read(&dir, "nope.txt", 10).unwrap_err().kind,
            ErrorKind::InvalidEdit
        );
        assert_eq!(
            FsFiles.read(&dir, "src", 10).unwrap_err().kind,
            ErrorKind::InvalidEdit
        );
    }

    #[test]
    fn a_link_out_of_the_folder_is_not_followed() {
        let dir = folder("link");
        let outside =
            std::env::temp_dir().join(format!("loom-ports-outside-{}", std::process::id()));
        std::fs::write(&outside, "secret").unwrap();
        let link = dir.join("escape.txt");
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(&outside, &link);
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&outside, &link);
        if made.is_err() {
            return; // creating links needs a privilege on some systems
        }
        assert_eq!(
            FsFiles.read(&dir, "escape.txt", 100).unwrap_err().kind,
            ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn find_matches_globs_relative_to_the_folder_and_respects_the_limit() {
        let dir = folder("find");
        std::fs::write(dir.join("src/more.rs"), "").unwrap();
        assert_eq!(
            FsFiles.find(&dir, "**/*.rs", 10).unwrap(),
            ["src/lib.rs", "src/more.rs"]
        );
        assert_eq!(FsFiles.find(&dir, "*.md", 10).unwrap(), ["README.md"]);
        assert_eq!(FsFiles.find(&dir, "**/*.rs", 1).unwrap().len(), 1);
        assert_eq!(
            FsFiles.find(&dir, "[", 10).unwrap_err().kind,
            ErrorKind::InvalidEdit
        );
    }

    #[test]
    fn storage_round_trips_per_scope_and_survives_damage() {
        let dir = std::env::temp_dir().join(format!("loom-storage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut storage = FsStorage::new(dir.clone());
        assert!(storage.load("user").is_empty());
        storage
            .save("user", &BTreeMap::from([("k".into(), "v".into())]))
            .unwrap();
        storage
            .save("root-abc", &BTreeMap::from([("a".into(), "b".into())]))
            .unwrap();
        assert_eq!(FsStorage::new(dir.clone()).load("user")["k"], "v");
        assert_eq!(FsStorage::new(dir.clone()).load("root-abc")["a"], "b");
        std::fs::write(dir.join("user.json"), "not json").unwrap();
        assert!(FsStorage::new(dir.clone()).load("user").is_empty());
    }

    #[test]
    fn a_scope_name_cannot_name_another_place() {
        let dir = std::env::temp_dir().join(format!("loom-storage-names-{}", std::process::id()));
        let mut storage = FsStorage::new(dir);
        for scope in ["../x", "a/b", "a\\b", ".hidden", ""] {
            assert!(storage.save(scope, &BTreeMap::new()).is_err(), "{scope}");
        }
    }
}
