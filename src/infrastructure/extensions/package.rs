//! Importing an extension package (`.loomext`).
//!
//! A package is a gzip-compressed tar archive of the extension folder: the
//! files of the folder sit at the archive root, `manifest.json` among them.
//! Importing checks the archive before anything is installed: size limits,
//! safe paths, regular files only, a manifest that carries everything the
//! extensions page shows, and files that are really there. The folder is built
//! next to its final place and moved in as a whole, so a failure leaves what was
//! installed before untouched.
use crate::features::extensions::manifest::{Manifest, check_package_path};
use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// File extension of a package.
pub const EXTENSION: &str = "loomext";
/// Largest archive accepted, and the most it may expand to.
pub const MAX_ARCHIVE: u64 = 64 * 1024 * 1024;
pub const MAX_UNPACKED: u64 = 128 * 1024 * 1024;
const MAX_ENTRIES: usize = 2048;
const MAX_MANIFEST: u64 = 1024 * 1024;
/// Written into the installed folder: where it came from.
pub const SOURCE_FILE: &str = ".loomext-source.json";

/// What an import did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Imported {
    pub id: String,
    pub name: String,
    pub version: String,
    pub publisher: String,
    pub directory: PathBuf,
    /// SHA-256 of the archive, as hexadecimal text.
    pub sha256: String,
    /// The version that was installed before, when this replaced one.
    pub replaced: Option<String>,
    /// Language packs inside an extension are read at startup.
    pub needs_restart: bool,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

pub fn sha256(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

static TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// A folder next to the installed extensions, removed when dropped.
struct Scratch(PathBuf);
impl Scratch {
    fn new(root: &Path, kind: &str) -> Result<Self, String> {
        for _ in 0..100 {
            let path = root.join(format!(
                ".import-{kind}-{}-{}",
                std::process::id(),
                TEMP_ID.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        Err("could not allocate a folder for the import".into())
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A path inside the archive: relative, `/`-separated, and a name every
/// platform can create.
fn archive_path(raw: &str) -> Result<String, String> {
    let path = raw.strip_prefix("./").unwrap_or(raw).trim_end_matches('/');
    let invalid = || format!("the archive has an unusable path: {raw}");
    if path.is_empty() || path.len() > 240 || check_package_path(path).is_err() {
        return Err(invalid());
    }
    for part in path.split('/') {
        if part.is_empty()
            || part == "."
            || part.ends_with(['.', ' '])
            || part.contains([':', '*', '?', '"', '<', '>', '|', '\\'])
            || part.chars().any(|c| c.is_control())
        {
            return Err(invalid());
        }
    }
    Ok(path.to_owned())
}

/// Unpack `bytes` into `root`, which must be empty and new.
fn unpack(bytes: &[u8], root: &Path) -> Result<(), String> {
    let reader = flate2::read::GzDecoder::new(bytes).take(MAX_UNPACKED + 1);
    let mut archive = tar::Archive::new(reader);
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    for entry in archive
        .entries()
        .map_err(|e| format!("not a package: {e}"))?
    {
        let mut entry = entry.map_err(|e| format!("damaged package: {e}"))?;
        let raw = entry
            .path()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned();
        let kind = entry.header().entry_type();
        if raw == "." || raw == "./" {
            continue;
        }
        let path = archive_path(&raw)?;
        if seen.len() >= MAX_ENTRIES || !seen.insert(path.to_ascii_lowercase()) {
            return Err("the archive has duplicate paths or too many entries".into());
        }
        let output = root.join(&path);
        if kind.is_dir() {
            fs::create_dir_all(&output).map_err(|e| e.to_string())?;
            continue;
        }
        if !kind.is_file() {
            return Err(format!(
                "{raw}: a package may hold only regular files and folders"
            ));
        }
        let size = entry.size();
        total = total.checked_add(size).ok_or("the archive is too large")?;
        if total > MAX_UNPACKED {
            return Err("the archive expands to more than the allowed size".into());
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)
            .map_err(|e| format!("{raw}: {e}"))?;
        let copied = std::io::copy(&mut entry, &mut file).map_err(|e| e.to_string())?;
        if copied != size {
            return Err("the archive is truncated".into());
        }
        file.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn read_limited(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err(format!("{} is larger than {limit} bytes", path.display()));
    }
    Ok(bytes)
}

/// Check an unpacked package folder and return its manifest.
pub fn check_folder(folder: &Path, loom_version: &str) -> Result<Manifest, String> {
    let manifest_text = read_limited(&folder.join("manifest.json"), MAX_MANIFEST)
        .map_err(|_| "the package has no manifest.json".to_owned())?;
    let manifest = Manifest::parse(&String::from_utf8_lossy(&manifest_text))
        .map_err(|error| format!("manifest.json: {error}"))?;
    let size_of = |path: &str| {
        fs::metadata(folder.join(path))
            .ok()
            .filter(|meta| meta.is_file())
            .map(|meta| meta.len())
    };
    let mut problems = manifest.publishing_problems(size_of);
    if let Some(component) = &manifest.component
        && size_of(component).is_none()
    {
        problems.push(format!("`component`: {component} is not in the package"));
    }
    if !problems.is_empty() {
        return Err(format!(
            "the manifest is incomplete:\n- {}",
            problems.join("\n- ")
        ));
    }
    if !manifest.runs_on(loom_version) {
        return Err(format!(
            "{} needs Loom {} or newer; this is Loom {loom_version}",
            manifest.name.as_deref().unwrap_or(&manifest.id),
            manifest.engines.as_ref().map_or("?", |e| e.loom.as_str())
        ));
    }
    Ok(manifest)
}

/// Install the package `archive` into `extensions` (the folder that holds one
/// folder per extension). An installed extension with the same id is replaced.
pub fn import(archive: &Path, extensions: &Path, loom_version: &str) -> Result<Imported, String> {
    let bytes = read_limited(archive, MAX_ARCHIVE)?;
    fs::create_dir_all(extensions).map_err(|e| e.to_string())?;
    let staging = Scratch::new(extensions, "new")?;
    let folder = staging.0.join("package");
    fs::create_dir(&folder).map_err(|e| e.to_string())?;
    unpack(&bytes, &folder)?;
    let manifest = check_folder(&folder, loom_version)?;
    let digest = sha256(&bytes);
    let name = manifest.name.clone().unwrap_or_default();
    let publisher = manifest.publisher.clone().unwrap_or_default();
    let needs_restart = !manifest.contributes.languages.is_empty();
    let source = serde_json::json!({
        "sha256": digest,
        "file": archive.file_name().map(|n| n.to_string_lossy().into_owned()),
        "version": manifest.version,
    });
    fs::write(folder.join(SOURCE_FILE), source.to_string()).map_err(|e| e.to_string())?;

    let target = extensions.join(&manifest.id);
    let replaced = fs::read_to_string(target.join("manifest.json"))
        .ok()
        .and_then(|text| Manifest::parse(&text).ok())
        .map(|old| old.version);
    if target.exists() {
        // Move the old folder aside first, so a failure puts it back.
        let aside = Scratch::new(extensions, "old")?;
        let kept = aside.0.join("previous");
        fs::rename(&target, &kept)
            .map_err(|e| format!("could not replace the installed extension: {e}"))?;
        if let Err(error) = fs::rename(&folder, &target) {
            let _ = fs::rename(&kept, &target);
            return Err(format!("could not install the extension: {error}"));
        }
    } else {
        fs::rename(&folder, &target)
            .map_err(|e| format!("could not install the extension: {e}"))?;
    }
    Ok(Imported {
        id: manifest.id,
        name,
        version: manifest.version,
        publisher,
        directory: target,
        sha256: digest,
        replaced,
        needs_restart,
    })
}

#[cfg(test)]
#[path = "package_tests.rs"]
mod tests;
