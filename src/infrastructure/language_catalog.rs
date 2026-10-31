//! Consume the application-pinned catalog. Downloads never run on a parser/UI thread.
use super::*;
use ring::digest::{SHA256, digest};
use std::{
    collections::VecDeque,
    io::Write,
    sync::{
        Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_ARCHIVE: u64 = 64 * 1024 * 1024;
const MAX_UNPACKED: u64 = 128 * 1024 * 1024;
const RETRY_AFTER: Duration = Duration::from_secs(30);
type Listener = dyn Fn() + Send + Sync;
type Download = dyn Fn(&str, u64) -> Result<Vec<u8>, String> + Send + Sync;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Catalog {
    schema: u32,
    distribution: Distribution,
    packages: BTreeMap<String, Package>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Distribution {
    repository: String,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Package {
    name: String,
    asset: String,
    release: String,
    sha256: String,
    size: u64,
    grammars: BTreeMap<String, CatalogGrammar>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogGrammar {
    #[serde(rename = "file-types")]
    file_types: Vec<String>,
}

fn slug(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
}
fn sha(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
impl Catalog {
    pub(super) fn read(file: &Path) -> Result<Self, String> {
        let value: Self =
            toml::from_str(&read_text(file, MAX_MANIFEST)?).map_err(|e| e.to_string())?;
        let repository: Vec<_> = value.distribution.repository.split('/').collect();
        if value.schema != 1
            || repository.len() != 2
            || !repository.iter().all(|part| {
                !part.is_empty()
                    && *part != "."
                    && *part != ".."
                    && part
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
            })
            || value.packages.len() > MAX_PACKAGES
        {
            return Err("invalid language package catalog".into());
        }
        let mut grammars = BTreeSet::new();
        let mut suffixes = BTreeSet::new();
        for (id, p) in &value.packages {
            let release = p
                .release
                .strip_prefix("language-packs-")
                .unwrap_or_default();
            if !slug(id)
                || !id.starts_with(|c: char| c.is_ascii_lowercase())
                || p.name.is_empty()
                || p.asset != format!("{id}.tar.gz")
                || release.is_empty()
                || !release
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
                || p.sha256.len() != 64
                || !p
                    .sha256
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                || p.size == 0
                || p.size > MAX_ARCHIVE
                || p.grammars.is_empty()
            {
                return Err(format!("invalid language catalog entry {id}"));
            }
            for (grammar, g) in &p.grammars {
                if !name(grammar)
                    || grammar == "plaintext"
                    || !grammars.insert(grammar)
                    || g.file_types.len() > 512
                {
                    return Err(format!("invalid or duplicate catalog grammar {grammar}"));
                }
                for suffix in &g.file_types {
                    if suffix.is_empty()
                        || suffix.contains(['/', '\\', '\0'])
                        || !suffixes.insert(suffix.to_ascii_lowercase())
                    {
                        return Err(format!("invalid or conflicting catalog suffix {suffix}"));
                    }
                }
            }
        }
        if grammars.len() > 128 {
            return Err("too many catalog grammars".into());
        }
        Ok(value)
    }
    pub(super) fn descriptions(&self) -> Result<BTreeMap<String, Installed>, String> {
        self.packages
            .iter()
            .map(|(id, p)| {
                let grammars: Vec<_> = p
                    .grammars
                    .iter()
                    .map(|(name, g)| {
                        serde_json::json!({
                            "name": name, "title": p.name, "file-types": g.file_types,
                        })
                    })
                    .collect();
                let installed = standard::from_json(
                    Path::new("."),
                    &serde_json::json!({"grammars": grammars}).to_string(),
                )?;
                Ok((id.clone(), installed))
            })
            .collect()
    }
}

enum State {
    Queued,
    Downloading,
    Ready(Installed),
    Failed { error: String, retry: Instant },
}
#[derive(Default)]
struct Work {
    states: BTreeMap<String, State>,
    queue: VecDeque<String>,
    running: usize,
}
pub(crate) struct Info {
    pub name: String,
    pub status: &'static str,
    pub error: Option<String>,
}
pub(super) struct Manager {
    catalog: Catalog,
    pub(super) descriptions: BTreeMap<String, Installed>,
    /// Downloaded archives, kept so a restart installs without the network.
    cache: Option<PathBuf>,
    /// The folder extensions are installed into: a language becomes the extension
    /// `loom.lang.<name>` there.
    extensions: Option<PathBuf>,
    work: Mutex<Work>,
    revision: AtomicU64,
    listeners: Mutex<Vec<Weak<Listener>>>,
    download: Arc<Download>,
}
impl Manager {
    pub(super) fn new(
        catalog: Catalog,
        descriptions: BTreeMap<String, Installed>,
        cache: Option<PathBuf>,
        extensions: Option<PathBuf>,
    ) -> Arc<Self> {
        Self::with_download(catalog, descriptions, cache, extensions, Arc::new(download))
    }
    fn with_download(
        catalog: Catalog,
        descriptions: BTreeMap<String, Installed>,
        cache: Option<PathBuf>,
        extensions: Option<PathBuf>,
        download: Arc<Download>,
    ) -> Arc<Self> {
        Arc::new(Self {
            catalog,
            descriptions,
            cache,
            extensions,
            work: Mutex::default(),
            revision: AtomicU64::new(0),
            listeners: Mutex::default(),
            download,
        })
    }
    pub(super) fn generation(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }
    pub(super) fn pending(&self, grammar: &str) -> bool {
        let work = self.work.lock().unwrap();
        self.descriptions.iter().any(|(id, p)| {
            p.manifest.grammars.contains_key(grammar)
                && matches!(
                    work.states.get(id),
                    Some(State::Queued | State::Downloading)
                )
        })
    }
    pub(super) fn subscribe(&self, changed: Arc<Listener>) {
        self.listeners
            .lock()
            .unwrap()
            .push(Arc::downgrade(&changed));
    }
    fn notify(&self) {
        let listeners = {
            let mut refs = self.listeners.lock().unwrap();
            let listeners: Vec<_> = refs.iter().filter_map(Weak::upgrade).collect();
            refs.retain(|r| r.strong_count() > 0);
            listeners
        };
        for listener in listeners {
            listener();
        }
    }
    pub(super) fn request(self: &Arc<Self>, grammar: &str) -> Result<Installed, String> {
        let id = self
            .descriptions
            .iter()
            .find(|(_, p)| p.manifest.grammars.contains_key(grammar))
            .map(|(id, _)| id.clone())
            .ok_or_else(|| format!("language package {grammar} is not installed"))?;
        let mut work = self.work.lock().unwrap();
        match work.states.get(&id) {
            Some(State::Ready(package)) => return Ok(package.clone()),
            Some(State::Queued | State::Downloading) => {
                return Err(format!("language package {id} is downloading"));
            }
            Some(State::Failed { error, retry }) if Instant::now() < *retry => {
                return Err(error.clone());
            }
            _ => {}
        }
        work.states.insert(id.clone(), State::Queued);
        work.queue.push_back(id.clone());
        // Short-lived workers share a deduplicated queue; at most two run at once.
        if work.running < 2 {
            work.running += 1;
            let manager = self.clone();
            if let Err(error) = std::thread::Builder::new()
                .name("loom-language-download".into())
                .spawn(move || manager.run())
            {
                work.running -= 1;
                work.queue.retain(|queued| queued != &id);
                work.states.insert(
                    id.clone(),
                    State::Failed {
                        error: error.to_string(),
                        retry: Instant::now() + RETRY_AFTER,
                    },
                );
            }
        }
        Err(format!("language package {id} is downloading"))
    }
    fn run(self: Arc<Self>) {
        loop {
            let id = {
                let mut work = self.work.lock().unwrap();
                let Some(id) = work.queue.pop_front() else {
                    work.running -= 1;
                    return;
                };
                work.states.insert(id.clone(), State::Downloading);
                id
            };
            self.notify();
            let result = self.install(&id);
            {
                let mut work = self.work.lock().unwrap();
                let state = match result {
                    Ok(package) => {
                        self.revision.fetch_add(1, Ordering::Release);
                        State::Ready(package)
                    }
                    Err(error) => State::Failed {
                        error,
                        retry: Instant::now() + RETRY_AFTER,
                    },
                };
                work.states.insert(id, state);
            }
            self.notify();
        }
    }
    pub(super) fn infos(&self) -> Vec<Info> {
        self.work
            .lock()
            .unwrap()
            .states
            .iter()
            .map(|(id, state)| Info {
                name: self.catalog.packages[id].name.clone(),
                status: match state {
                    State::Queued => "queued",
                    State::Downloading => "downloading",
                    State::Ready(_) => "ready",
                    State::Failed { .. } => "download failed",
                },
                error: match state {
                    State::Failed { error, .. } => Some(error.clone()),
                    _ => None,
                },
            })
            .collect()
    }
    pub(super) fn retry(self: &Arc<Self>) {
        let grammars: Vec<_> = {
            let mut work = self.work.lock().unwrap();
            let ids: Vec<_> = work
                .states
                .iter()
                .filter(|(_, s)| matches!(s, State::Failed { .. }))
                .map(|(id, _)| id.clone())
                .collect();
            ids.iter()
                .map(|id| {
                    work.states.remove(id);
                    self.catalog.packages[id]
                        .grammars
                        .keys()
                        .next()
                        .unwrap()
                        .clone()
                })
                .collect()
        };
        for grammar in grammars {
            let _ = self.request(&grammar);
        }
    }
    fn install(&self, id: &str) -> Result<Installed, String> {
        let cache = self
            .cache
            .as_ref()
            .ok_or("No user directory is available for language packages")?;
        fs::create_dir_all(cache).map_err(|e| e.to_string())?;
        let p = &self.catalog.packages[id];
        let archive = cache.join(format!("{}.tar.gz", p.sha256));
        let bytes = match fs::File::open(&archive) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(p.size + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| e.to_string())?;
                if bytes.len() as u64 == p.size && sha(&bytes) == p.sha256 {
                    bytes
                } else {
                    self.fetch(p, &archive)?
                }
            }
            Err(_) => self.fetch(p, &archive)?,
        };
        let staging = Temporary::new(cache)?;
        unpack(&bytes, &staging.0, id)?;
        let directory = staging.0.join(id);
        let installed = standard::read(&directory)?;
        let expected: BTreeSet<_> = p.grammars.keys().cloned().collect();
        if installed
            .manifest
            .grammars
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
            != expected
        {
            return Err(format!("{id}: downloaded grammars differ from the catalog"));
        }
        let metadata: Value = serde_json::from_str(&read_text(
            &directory.join("tree-sitter.json"),
            MAX_MANIFEST,
        )?)
        .map_err(|e| e.to_string())?;
        for grammar in metadata["grammars"].as_array().ok_or("missing grammars")? {
            let name = grammar["name"].as_str().ok_or("missing grammar name")?;
            let mut actual: Vec<String> =
                serde_json::from_value(grammar["file-types"].clone()).map_err(|e| e.to_string())?;
            let mut wanted = p.grammars[name].file_types.clone();
            actual.sort();
            wanted.sort();
            if actual != wanted {
                return Err(format!("{id}: file associations differ from the catalog"));
            }
            let module = Packages::module_path(&installed, name);
            if module.extension().is_none_or(|ext| ext != "wasm") {
                return Err("Downloaded packages must contain WASM modules".into());
            }
        }
        let validation = Packages::discover(&staging.0, None);
        if !validation.activity().errors.is_empty() {
            return Err("invalid downloaded package metadata".into());
        }
        for grammar in &expected {
            crate::features::syntax::validate_resources(&validation, grammar)?;
        }
        // The language is installed as an ordinary extension: a manifest names its
        // grammar, and a marker records that the registry put it there.
        let extensions = self
            .extensions
            .as_ref()
            .ok_or("No user directory is available for extensions")?;
        let extension = extension_id(id);
        fs::write(
            directory.join("manifest.json"),
            registry_manifest(&extension, p),
        )
        .map_err(|e| e.to_string())?;
        fs::write(
            directory.join(REGISTRY_MARKER),
            serde_json::json!({"sha256": p.sha256, "release": p.release}).to_string(),
        )
        .map_err(|e| e.to_string())?;
        fs::create_dir_all(extensions).map_err(|e| e.to_string())?;
        let destination = extensions.join(&extension);
        let quarantine = Temporary::new(cache)?;
        // Existing files are reused only if every extracted byte matches the pinned archive.
        if destination.exists() {
            // An extension of the same name that the user installed is theirs.
            if !destination.join(REGISTRY_MARKER).is_file() {
                return Err(format!(
                    "{extension}: an extension with this name is already installed"
                ));
            }
            if inventory(&directory)? == inventory(&destination).unwrap_or_default() {
                return standard::read(&destination);
            }
            // Move only what the registry installed aside before installing verified bytes.
            match fs::rename(&destination, quarantine.0.join("corrupt")) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("repair language extension {id}: {e}")),
            }
        }
        match fs::rename(&directory, &destination) {
            Ok(()) => {}
            Err(_)
                if destination.exists() && inventory(&directory)? == inventory(&destination)? => {}
            Err(e) => return Err(format!("activate language package {id}: {e}")),
        }
        standard::read(&destination)
    }
    fn fetch(&self, package: &Package, archive: &Path) -> Result<Vec<u8>, String> {
        let url = format!(
            "https://github.com/{}/releases/download/{}/{}",
            self.catalog.distribution.repository, package.release, package.asset
        );
        let bytes = (self.download)(&url, package.size)?;
        if bytes.len() as u64 != package.size || sha(&bytes) != package.sha256 {
            return Err("Language archive size or SHA-256 mismatch".into());
        }
        let temporary = Temporary::new(archive.parent().unwrap())?;
        let file = temporary.0.join("archive");
        fs::write(&file, &bytes).map_err(|e| e.to_string())?;
        // Atomic replacement also repairs a corrupt archive left in the cache.
        fs::rename(file, archive).map_err(|e| format!("cache language archive: {e}"))?;
        Ok(bytes)
    }
}

/// Present in a folder the registry installed, so it is never taken for one the
/// user installed.
const REGISTRY_MARKER: &str = ".registry.json";

/// The extension a registry language is installed as.
fn extension_id(id: &str) -> String {
    format!("loom.lang.{}", id.replace('_', "-"))
}

/// The manifest of a registry language: a resource-only extension that
/// contributes its grammar.
fn registry_manifest(extension: &str, package: &Package) -> String {
    let release = package
        .release
        .strip_prefix("language-packs-")
        .unwrap_or("");
    let parts: Vec<&str> = release.split('.').take(3).collect();
    let version = if parts.len() == 3 && parts.iter().all(|p| p.parse::<u32>().is_ok()) {
        parts.join(".")
    } else {
        "0.0.0".to_owned()
    };
    serde_json::json!({
        "schema": 1,
        "id": extension,
        "version": version,
        "name": format!("{} Language", package.name),
        "publisher": "Loom",
        "description": format!("Syntax highlighting for {}.", package.name),
        "categories": ["Languages"],
        "contributes": {
            "languages": [{ "id": format!("{extension}.grammar"), "path": "tree-sitter.json" }]
        }
    })
    .to_string()
}

fn download(url: &str, size: u64) -> Result<Vec<u8>, String> {
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(60))
        .build()
        .get(url)
        .call()
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(size + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}
static TEMP_ID: AtomicU64 = AtomicU64::new(0);
struct Temporary(PathBuf);
impl Temporary {
    fn new(root: &Path) -> Result<Self, String> {
        for _ in 0..100 {
            let path = root.join(format!(
                ".install-{}-{}",
                std::process::id(),
                TEMP_ID.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        Err("could not allocate a language installation directory".into())
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn unpack(bytes: &[u8], root: &Path, id: &str) -> Result<(), String> {
    let reader = flate2::read::GzDecoder::new(bytes).take(MAX_UNPACKED + 1);
    let mut archive = tar::Archive::new(reader);
    let mut total = 0u64;
    let mut seen = BTreeSet::new();
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry
            .path()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned();
        let path = path.trim_end_matches('/');
        standard::relative(path)?;
        if path != id && !path.starts_with(&format!("{id}/")) {
            return Err("Language archive has an unexpected root directory".into());
        }
        if !seen.insert(path.to_ascii_lowercase()) || seen.len() > 2048 {
            return Err("Duplicate paths or too many archive entries".into());
        }
        if path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part.ends_with(['.', ' ']))
        {
            return Err("Invalid language archive path".into());
        }
        let output = root.join(path);
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(output).map_err(|e| e.to_string())?;
            continue;
        }
        if !kind.is_file() {
            return Err("Language archives may contain only regular files and directories".into());
        }
        let size = entry.size();
        total = total.checked_add(size).ok_or("archive too large")?;
        if size > MAX_WASM_BYTES || total > MAX_UNPACKED {
            return Err("Language archive exceeds extraction limits".into());
        }
        fs::create_dir_all(output.parent().unwrap()).map_err(|e| e.to_string())?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)
            .map_err(|e| e.to_string())?;
        let copied = std::io::copy(&mut entry, &mut file).map_err(|e| e.to_string())?;
        if copied != size {
            return Err("truncated language archive".into());
        }
        file.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}
fn inventory(root: &Path) -> Result<BTreeMap<PathBuf, String>, String> {
    fn walk(
        root: &Path,
        directory: &Path,
        result: &mut BTreeMap<PathBuf, String>,
        total: &mut u64,
    ) -> Result<(), String> {
        if fs::symlink_metadata(directory)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("Language cache contains a symlink".into());
        }
        for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() {
                walk(root, &entry.path(), result, total)?;
            } else if kind.is_file() {
                let file = fs::File::open(entry.path()).map_err(|e| e.to_string())?;
                let mut bytes = Vec::new();
                file.take(MAX_WASM_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| e.to_string())?;
                *total += bytes.len() as u64;
                if bytes.len() as u64 > MAX_WASM_BYTES
                    || *total > MAX_UNPACKED
                    || result.len() >= 2048
                {
                    return Err("Language cache exceeds limits".into());
                }
                result.insert(
                    entry.path().strip_prefix(root).unwrap().to_owned(),
                    sha(&bytes),
                );
            } else {
                return Err("Language cache contains a link or special file".into());
            }
        }
        Ok(())
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result, &mut 0)?;
    Ok(result)
}

#[cfg(test)]
#[path = "language_catalog_tests.rs"]
mod tests;
