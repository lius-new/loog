//! Command-palette rows an extension asked the application to keep while it is not running.
//!
//! One small file per extension, `<dir>/<id>.json`, holding the rows exactly as the extension
//! gave them and the version of the package they came from. Nothing here interprets a row:
//! whether a file is shown at all is decided by the caller from what is installed and enabled.
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The most rows kept for one extension, the same bound as published rows.
pub const MAX_ROWS: usize = 64;
/// The longest line of a row, the same bound as published rows.
const MAX_LINE: usize = 200;
/// A file larger than this is not read: 64 rows of four short lines fit far inside it.
const MAX_FILE: u64 = 128 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub detail: Option<String>,
    pub category: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Retained {
    /// The version of the package that gave these rows; rows of another version are not shown.
    pub version: String,
    pub rows: Vec<Row>,
}

fn line(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_LINE && !text.contains(['\0', '\n', '\r'])
}

impl Row {
    fn valid(&self) -> bool {
        line(&self.id)
            && line(&self.title)
            && line(&self.category)
            && self.detail.as_ref().is_none_or(|detail| {
                detail.len() <= MAX_LINE && !detail.contains(['\0', '\n', '\r'])
            })
    }
}

impl Retained {
    fn valid(&self) -> bool {
        line(&self.version) && self.rows.len() <= MAX_ROWS && self.rows.iter().all(Row::valid)
    }
}

/// The file for `owner`, or none when the id is not a plain file name.
fn file_of(dir: &Path, owner: &str) -> Option<PathBuf> {
    let plain = !owner.is_empty()
        && owner.len() <= 128
        && !owner.starts_with('.')
        && owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'));
    plain.then(|| dir.join(format!("{owner}.json")))
}

/// Keep `kept` for `owner`. The file is replaced whole, never half written.
pub fn save(dir: &Path, owner: &str, kept: &Retained) -> io::Result<()> {
    let path = file_of(dir, owner)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not an extension id"))?;
    if !kept.valid() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "retained rows exceed their budget",
        ));
    }
    fs::create_dir_all(dir)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec(kept)?)?;
    fs::rename(&temporary, &path).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })
}

/// Forget what is kept for `owner`.
pub fn forget(dir: &Path, owner: &str) {
    if let Some(path) = file_of(dir, owner) {
        let _ = fs::remove_file(path);
    }
}

/// Everything that is kept. A file that is too large, cannot be read, does not parse or holds
/// a row outside the bounds is skipped: it was not written by this application.
pub fn load(dir: &Path) -> Vec<(String, Retained)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut kept = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(owner) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".json"))
        else {
            continue;
        };
        if file_of(dir, owner).as_deref() != Some(path.as_path())
            || entry.metadata().map_or(true, |meta| meta.len() > MAX_FILE)
        {
            continue;
        }
        let parsed = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Retained>(&bytes).ok());
        if let Some(retained) = parsed.filter(Retained::valid) {
            kept.push((owner.to_owned(), retained));
        }
    }
    kept.sort_by(|a, b| a.0.cmp(&b.0));
    kept
}

/// Remove the files of extensions `installed` does not know, and leftovers of an interrupted
/// write. Only a tidy-up: what is shown never depends on it.
pub fn prune(dir: &Path, installed: impl Fn(&str) -> bool) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let orphan = match name.strip_suffix(".json") {
            Some(owner) => !installed(owner),
            None => name.ends_with(".json.tmp"),
        };
        if orphan {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str) -> Row {
        Row {
            id: id.into(),
            title: format!("Title {id}"),
            detail: Some("detail".into()),
            category: "Saved".into(),
        }
    }

    fn kept(version: &str, ids: &[&str]) -> Retained {
        Retained {
            version: version.into(),
            rows: ids.iter().map(|id| row(id)).collect(),
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("loog-retained-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn rows_come_back_exactly_as_they_were_given() {
        let dir = scratch("roundtrip");
        save(&dir, "example.tool", &kept("1.2.0", &["a", "b"])).unwrap();
        save(&dir, "example.other", &kept("0.1.0", &[])).unwrap();
        let all = load(&dir);
        assert_eq!(all.len(), 2);
        assert_eq!(all[1], ("example.tool".into(), kept("1.2.0", &["a", "b"])));
        // Saving again replaces the file; nothing is merged.
        save(&dir, "example.tool", &kept("1.3.0", &["c"])).unwrap();
        assert_eq!(
            load(&dir)
                .into_iter()
                .find(|(owner, _)| owner == "example.tool")
                .unwrap()
                .1,
            kept("1.3.0", &["c"])
        );
        assert!(!dir.join("example.tool.json.tmp").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn forgetting_removes_the_file_and_an_empty_list_is_still_a_list() {
        let dir = scratch("forget");
        save(&dir, "example.tool", &kept("1.0.0", &["a"])).unwrap();
        forget(&dir, "example.tool");
        assert!(load(&dir).is_empty());
        forget(&dir, "example.tool");
        forget(&dir, "../escape");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rows_outside_the_bounds_are_refused_and_files_that_are_not_ours_are_skipped() {
        let dir = scratch("bounds");
        let too_many: Vec<String> = (0..=MAX_ROWS).map(|n| n.to_string()).collect();
        let ids: Vec<&str> = too_many.iter().map(String::as_str).collect();
        assert!(save(&dir, "example.tool", &kept("1.0.0", &ids)).is_err());
        assert!(save(&dir, "example.tool", &kept("1.0.0", &["two\nlines"])).is_err());
        assert!(save(&dir, "../escape", &kept("1.0.0", &["a"])).is_err());
        assert!(save(&dir, "", &kept("1.0.0", &["a"])).is_err());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("broken.json"), "{ nope").unwrap();
        fs::write(dir.join("huge.json"), vec![b' '; MAX_FILE as usize + 1]).unwrap();
        fs::write(
            dir.join("forged.json"),
            r#"{"version":"1","rows":[{"id":"a\nb","title":"t","category":"c"}]}"#,
        )
        .unwrap();
        fs::write(dir.join("notes.txt"), "x").unwrap();
        save(&dir, "example.tool", &kept("1.0.0", &["a"])).unwrap();
        let all = load(&dir);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, "example.tool");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pruning_removes_the_files_of_extensions_that_are_gone_and_interrupted_writes() {
        let dir = scratch("prune");
        save(&dir, "example.here", &kept("1.0.0", &["a"])).unwrap();
        save(&dir, "example.gone", &kept("1.0.0", &["a"])).unwrap();
        fs::write(dir.join("example.here.json.tmp"), "half").unwrap();
        fs::write(dir.join("notes.txt"), "x").unwrap();
        prune(&dir, |owner| owner == "example.here");
        let names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"example.here.json".to_owned()));
        assert!(
            names.contains(&"notes.txt".to_owned()),
            "only our own files are touched"
        );
        assert!(!names.contains(&"example.gone.json".to_owned()));
        assert!(!names.contains(&"example.here.json.tmp".to_owned()));
        let _ = fs::remove_dir_all(&dir);
    }
}
