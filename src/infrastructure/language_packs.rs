// Discover independent language packages at startup; open native modules and
// queries only when the syntax worker requests them. Catalog resources install on demand.
use crate::domain::language::Registry;
use crate::features::syntax::{Resources, TreeIndex};
use libloading::Library;
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};
use tree_sitter::{Language, Parser, WasmStore, wasmtime};
use tree_sitter_language::LanguageFn;

#[path = "language_catalog.rs"]
mod catalog;
#[path = "language_standard.rs"]
mod standard;

const MAX_MANIFEST: u64 = 1024 * 1024;
const MAX_QUERY: u64 = 4 * 1024 * 1024;
const MAX_QUERY_CACHE: usize = 8 * 1024 * 1024;
const MAX_MODULES: usize = 64;
const MAX_WASM_BYTES: u64 = 32 * 1024 * 1024;
const MAX_PACKAGES: usize = 128;
const PLAIN: &str = r#"{"name":"plaintext","display_name":"Plain Text","badge":"TXT","variants":[{"id":"plaintext"}]}"#;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Module {
    module: String,
    #[serde(default)]
    symbol: Option<String>,
}
impl Module {
    fn entry_point(&self, grammar: &str) -> String {
        self.symbol
            .clone()
            .unwrap_or_else(|| format!("tree_sitter_{grammar}"))
    }
    fn wasm_name(&self, grammar: &str) -> Result<String, String> {
        self.entry_point(grammar)
            .strip_prefix("tree_sitter_")
            .filter(|value| name(value))
            .map(str::to_owned)
            .ok_or_else(|| "WASM entry point must be tree_sitter_<name>".to_owned())
    }
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    id: String,
    #[serde(default = "enabled")]
    enabled: bool,
    languages: Vec<Value>,
    /// Older packages may still list servers. They are read and ignored: servers
    /// are defined by the language service, not by language packages.
    #[serde(default)]
    #[allow(dead_code)]
    servers: Vec<Value>,
    grammars: BTreeMap<String, Module>,
}
fn enabled() -> bool {
    true
}

fn name(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}
// Preserve standard WASM/native filenames, including Neovim's .so on macOS.
// For a stem, the native fallback gains its platform extension without lib.
fn module_file(module: &str, platform: &str) -> Result<String, String> {
    if name(module) {
        let extension = match platform {
            "windows" => "dll",
            "macos" => "dylib",
            _ => "so",
        };
        return Ok(format!("{module}.{extension}"));
    }
    if let Some((stem, extension)) = module.rsplit_once('.') {
        let mut chars = stem.chars();
        if chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
            && matches!(extension, "dll" | "so" | "dylib" | "wasm")
        {
            return Ok(module.to_owned());
        }
    }
    Err(format!("invalid language module filename {module}"))
}
fn module_candidates(module: &str, platform: &str) -> Vec<String> {
    let file = module_file(module, platform).expect("validated module filename");
    if name(module) {
        vec![
            format!("tree-sitter-{module}.wasm"),
            format!("{module}.wasm"),
            file,
        ]
    } else {
        vec![file]
    }
}
fn read_text(path: &Path, limit: u64) -> Result<String, String> {
    let file = fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut text = String::new();
    file.take(limit + 1)
        .read_to_string(&mut text)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if text.len() as u64 > limit {
        return Err(format!("{} exceeds {limit} bytes", path.display()));
    }
    Ok(text)
}

#[derive(Clone)]
struct Installed {
    manifest: Manifest,
    root: PathBuf,
    standard: Option<BTreeMap<String, standard::Grammar>>,
}

fn registry(packages: &BTreeMap<String, Installed>) -> Result<Registry, String> {
    let mut languages = BTreeMap::from([("plaintext".to_owned(), PLAIN.to_owned())]);
    let mut grammars = BTreeSet::new();
    for package in packages.values() {
        for grammar in package.manifest.grammars.keys() {
            if !grammars.insert(grammar.as_str()) {
                return Err(format!("grammar {grammar} is defined by multiple packages"));
            }
        }
        for value in &package.manifest.languages {
            let key = value
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| "package descriptor has no name".to_owned())?;
            if languages
                .insert(key.to_owned(), value.to_string())
                .is_some()
            {
                return Err(format!("descriptor {key} is defined by multiple packages"));
            }
        }
    }
    if grammars.len() > 128 || languages.len() > 129 {
        return Err("too many installed language descriptors".into());
    }
    let sources: Vec<_> = languages
        .iter()
        .map(|(n, j)| (n.as_str(), j.as_str()))
        .collect();
    let grammars: Vec<_> = grammars.into_iter().collect();
    Registry::from_sources(&sources, &grammars).map_err(|e| e.to_string())
}

impl Manifest {
    fn validate(&self, directory: &Path) -> Result<(), String> {
        if self.schema != 1 {
            return Err(format!(
                "unsupported language package schema {}",
                self.schema
            ));
        }
        if !name(&self.id) || directory.file_name().is_none_or(|n| n != self.id.as_str()) {
            return Err("package directory must match its canonical id".into());
        }
        for (grammar, module) in &self.grammars {
            if !name(grammar)
                || module_file(&module.module, std::env::consts::OS).is_err()
                || module.symbol.as_deref().is_some_and(|symbol| !name(symbol))
            {
                return Err(format!("invalid language module entry {grammar}"));
            }
        }
        // Each package is self-contained; references cannot depend on scan order.
        registry(&BTreeMap::from([(
            self.id.clone(),
            Installed {
                manifest: self.clone(),
                root: directory.to_owned(),
                standard: None,
            },
        )]))
        .map(|_| ())
    }
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub(crate) struct Activity {
    pub language_loads: usize,
    pub query_reads: usize,
    pub modules: BTreeSet<String>,
    pub wasm_loads: usize,
    pub wasm_modules: BTreeSet<String>,
    pub queries: BTreeSet<(String, String)>,
    pub errors: BTreeSet<String>,
}
type TextResult = Result<Option<Arc<str>>, String>;
#[derive(Default)]
struct Loaded {
    languages: HashMap<String, Result<Language, String>>,
    texts: HashMap<(String, String), TextResult>,
    text_bytes: usize,
    activity: Activity,
}

pub(crate) struct Packages {
    packages: BTreeMap<String, Installed>,
    loaded: Mutex<Loaded>,
    catalog: Option<Arc<catalog::Manager>>,
    disabled: BTreeSet<String>,
}
impl Packages {
    pub(crate) fn discover(bundled: &Path, user: Option<&Path>) -> Self {
        Self::discover_with(bundled, &[], user)
    }

    /// Like [`Self::discover`], with the language packs that extension packages
    /// contribute (see [`extension_packs`]) between the bundled collections and
    /// the user's: they cannot replace a bundled collection, and the user's
    /// copies still win over both.
    pub(crate) fn discover_with(
        bundled: &Path,
        extension_packs: &[PathBuf],
        user: Option<&Path>,
    ) -> Self {
        let mut source = Self {
            packages: BTreeMap::new(),
            loaded: Mutex::default(),
            catalog: None,
            disabled: BTreeSet::new(),
        };
        source.scan(bundled, false);
        source.add_directories(extension_packs.to_vec(), false);
        if let Some(user) = user {
            source.scan(user, true);
        }
        source
    }

    fn scan(&mut self, root: &Path, replace: bool) {
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                Self::report(
                    self.loaded.get_mut().unwrap(),
                    &format!("{}: {e}", root.display()),
                );
                return;
            }
        };
        let mut directories = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => {
                    if entry.file_name() == ".managed" {
                        continue;
                    }
                    if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                        directories.push(entry.path());
                        if directories.len() > MAX_PACKAGES {
                            Self::report(
                                self.loaded.get_mut().unwrap(),
                                &format!("{}: too many language packages", root.display()),
                            );
                            return;
                        }
                    }
                }
                Err(e) => Self::report(
                    self.loaded.get_mut().unwrap(),
                    &format!("{}: {e}", root.display()),
                ),
            }
        }
        self.add_directories(directories, replace);
    }

    fn add_directories(&mut self, mut directories: Vec<PathBuf>, replace: bool) {
        directories.sort();
        for directory in directories {
            // Ignore unrelated folders and partially downloaded packages.
            if !directory.join("tree-sitter.json").is_file()
                && !directory.join("package.json").is_file()
            {
                continue;
            }
            let result = (|| {
                let installed = if directory.join("tree-sitter.json").is_file() {
                    standard::read(&directory)?
                } else {
                    let path = directory.join("package.json");
                    let manifest: Manifest = serde_json::from_str(&read_text(&path, MAX_MANIFEST)?)
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    manifest.validate(&directory)?;
                    Installed {
                        manifest,
                        root: directory.clone(),
                        standard: None,
                    }
                };
                let manifest = &installed.manifest;
                let enabled = manifest.enabled;
                let affected: Vec<_> = manifest.grammars.keys().cloned().collect();
                let mut candidate = self.packages.clone();
                if replace && installed.standard.is_some() {
                    // A user copy replaces the same complete grammar collection,
                    // even when the upstream directory name is different.
                    candidate.retain(|_, old| {
                        !old.manifest.grammars.keys().eq(manifest.grammars.keys())
                    });
                } else if !replace && candidate.contains_key(&manifest.id) {
                    return Err(format!("duplicate language collection {}", manifest.id));
                }
                if manifest.enabled {
                    candidate.insert(manifest.id.clone(), installed);
                } else {
                    candidate.remove(&manifest.id);
                }
                if candidate.len() > MAX_PACKAGES {
                    return Err("too many installed language packages".into());
                }
                registry(&candidate)?;
                for grammar in affected {
                    if enabled {
                        self.disabled.remove(&grammar);
                    } else {
                        self.disabled.insert(grammar);
                    }
                }
                self.packages = candidate;
                Ok::<_, String>(())
            })();
            if let Err(error) = result {
                Self::report(
                    self.loaded.get_mut().unwrap(),
                    &format!("{}: {error}", directory.display()),
                );
            }
        }
    }

    pub(crate) fn registry(&self) -> Result<Registry, String> {
        let mut packages = self.packages.clone();
        if let Some(catalog) = &self.catalog {
            for (id, description) in &catalog.descriptions {
                packages.insert(format!("catalog:{id}"), description.clone());
            }
        }
        registry(&packages)
    }
    fn enable_catalog(&mut self, file: &Path, cache: Option<PathBuf>) -> Result<(), String> {
        if !file.exists() {
            return Ok(());
        }
        let catalog = catalog::Catalog::read(file)?;
        let mut descriptions = BTreeMap::new();
        let mut combined = self.packages.clone();
        for (id, description) in catalog.descriptions()? {
            if description
                .manifest
                .grammars
                .keys()
                .any(|g| self.disabled.contains(g))
            {
                continue;
            }
            // Complete installed collections, including user overrides, stay authoritative.
            if self.packages.values().any(|p| {
                p.manifest
                    .grammars
                    .keys()
                    .any(|g| description.manifest.grammars.contains_key(g))
            }) {
                continue;
            }
            let mut candidate = combined.clone();
            candidate.insert(format!("catalog:{id}"), description.clone());
            match registry(&candidate) {
                Ok(_) => {
                    combined = candidate;
                    descriptions.insert(id, description);
                }
                Err(error) => Self::report(
                    self.loaded.get_mut().unwrap(),
                    &format!("catalog {id}: {error}"),
                ),
            }
        }
        self.catalog = Some(catalog::Manager::new(catalog, descriptions, cache));
        Ok(())
    }
    fn available(&self, grammar: &str) -> Result<(Installed, Module), String> {
        if let Some((installed, module)) = self.grammar(grammar) {
            return Ok((installed.clone(), module.clone()));
        }
        let installed = self
            .catalog
            .as_ref()
            .ok_or_else(|| format!("language package {grammar} is not installed"))?
            .request(grammar)?;
        let module = installed
            .manifest
            .grammars
            .get(grammar)
            .ok_or("downloaded grammar is missing")?
            .clone();
        Ok((installed, module))
    }
    pub(crate) fn download_infos(&self) -> Vec<catalog::Info> {
        self.catalog.as_ref().map(|c| c.infos()).unwrap_or_default()
    }
    pub(crate) fn retry_downloads(&self) {
        if let Some(catalog) = &self.catalog {
            catalog.retry();
        }
    }
    pub(crate) fn activity(&self) -> Activity {
        self.loaded.lock().unwrap().activity.clone()
    }
    pub(crate) fn grammar_names(&self) -> Vec<String> {
        self.packages
            .values()
            .flat_map(|p| p.manifest.grammars.keys().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    fn grammar(&self, grammar: &str) -> Option<(&Installed, &Module)> {
        self.packages
            .values()
            .find_map(|p| p.manifest.grammars.get(grammar).map(|m| (p, m)))
    }
    fn module_path(package: &Installed, module: &str) -> PathBuf {
        if let Some(grammars) = &package.standard {
            return standard::module_path(&package.root, module, grammars[module].directory());
        }
        // A stem accepts an upstream WASM download without renaming. Explicit
        // filenames select exactly one format; never borrow another package.
        let root = package.root.join("modules");
        let candidates = module_candidates(module, std::env::consts::OS);
        candidates
            .iter()
            .map(|file| root.join(file))
            .find(|file| file.is_file())
            .unwrap_or_else(|| root.join(candidates.last().unwrap()))
    }
    fn report(loaded: &mut Loaded, error: &str) {
        if loaded.activity.errors.insert(error.to_owned()) {
            eprintln!("language package: {error}");
        }
    }
}
// One lazy, immutable engine; every parser owns a separate mutable WASM store.
// A loaded Tree-sitter Language is reference-counted and independent of the
// temporary store that compiled it. All execution stores use this same engine.
static WASM_ENGINE: OnceLock<Result<wasmtime::Engine, String>> = OnceLock::new();
fn wasm_engine() -> Result<&'static wasmtime::Engine, String> {
    WASM_ENGINE
        .get_or_init(|| wasmtime::Engine::new(&wasmtime::Config::new()).map_err(|e| e.to_string()))
        .as_ref()
        .map_err(Clone::clone)
}
fn load_wasm(path: &Path, name: &str) -> Result<Language, String> {
    let file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if file.metadata().map_err(|e| e.to_string())?.len() > MAX_WASM_BYTES {
        return Err(format!("{} exceeds {MAX_WASM_BYTES} bytes", path.display()));
    }
    let mut bytes = Vec::new();
    file.take(MAX_WASM_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_WASM_BYTES {
        return Err(format!("{} exceeds {MAX_WASM_BYTES} bytes", path.display()));
    }
    if !bytes.starts_with(b"\0asm\x01\0\0\0") {
        return Err(format!(
            "{}: invalid WebAssembly module header",
            path.display()
        ));
    }
    let mut store = WasmStore::new(wasm_engine()?).map_err(|e| e.to_string())?;
    let language = store
        .load_language(name, &bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    validate_abi(path, &language)?;
    Ok(language)
}
fn validate_abi(path: &Path, language: &Language) -> Result<(), String> {
    let version = language.abi_version();
    if !(tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION..=tree_sitter::LANGUAGE_VERSION)
        .contains(&version)
    {
        return Err(format!(
            "{} uses unsupported Tree-sitter ABI {version}",
            path.display()
        ));
    }
    Ok(())
}
// Tree-sitter's Language, Tree and Query all contain pointers into native
// modules. Keep handles until process exit, including after a worker is dropped.
// Catalog/package updates take effect on restart; this cache has a fixed cap.
fn load_native(path: &Path, symbol: &str) -> Result<Language, String> {
    static MODULES: OnceLock<Mutex<HashMap<PathBuf, Library>>> = OnceLock::new();
    let mut modules = MODULES.get_or_init(Mutex::default).lock().unwrap();
    let key = path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if !modules.contains_key(&key) {
        if modules.len() >= MAX_MODULES {
            return Err("native language module limit reached; restart to release modules".into());
        }
        // SAFETY: packages are installed application resources, loaded through
        // an explicit absolute path, with an agreed Tree-sitter C entry point.
        let library =
            unsafe { Library::new(&key) }.map_err(|e| format!("{}: {e}", path.display()))?;
        modules.insert(key.clone(), library);
    }
    let library = &modules[&key];
    // SAFETY: standard Tree-sitter entry points use this C signature. Keep the
    // owning Library in MODULES for the entire lifetime of every Language.
    let function = unsafe { library.get::<unsafe extern "C" fn() -> *const ()>(symbol.as_bytes()) }
        .map_err(|e| format!("{} ({symbol}): {e}", path.display()))?;
    // Reject an invalid null entry point before Tree-sitter dereferences it.
    if unsafe { function() }.is_null() {
        return Err(format!(
            "{} ({symbol}) returned a null grammar",
            path.display()
        ));
    }
    let language = Language::new(unsafe { LanguageFn::from_raw(*function) });
    validate_abi(path, &language)?;
    Ok(language)
}

impl Resources for Packages {
    fn generation(&self) -> u64 {
        self.catalog.as_ref().map_or(0, |c| c.generation())
    }
    fn pending(&self, grammar: &str) -> bool {
        self.catalog.as_ref().is_some_and(|c| c.pending(grammar))
    }
    fn subscribe(&self, changed: Arc<dyn Fn() + Send + Sync>) {
        if let Some(catalog) = &self.catalog {
            catalog.subscribe(changed);
        }
    }
    fn language(&self, grammar: &str) -> Result<Language, String> {
        // Waiting for an installation is temporary, never a cached module failure.
        let (package, module) = self.available(grammar)?;
        let mut loaded = self.loaded.lock().unwrap();
        if let Some(language) = loaded.languages.get(grammar) {
            return language.clone();
        }
        loaded.activity.language_loads += 1;
        let result = {
            loaded.activity.modules.insert(module.module.clone());
            let path = Self::module_path(&package, &module.module);
            if let Err(error) = standard::contained(&package.root, &path) {
                Self::report(&mut loaded, &error);
                loaded
                    .languages
                    .insert(grammar.to_owned(), Err(error.clone()));
                return Err(error);
            }
            if path
                .extension()
                .is_some_and(|extension| extension == "wasm")
            {
                loaded.activity.wasm_loads += 1;
                loaded
                    .activity
                    .wasm_modules
                    .insert(path.file_name().unwrap().to_string_lossy().into_owned());
                if loaded
                    .languages
                    .values()
                    .filter_map(|language| language.as_ref().ok())
                    .filter(|language| language.is_wasm())
                    .count()
                    >= MAX_MODULES
                {
                    Err("WASM language limit reached; restart to release modules".into())
                } else {
                    module
                        .wasm_name(grammar)
                        .and_then(|name| load_wasm(&path, &name))
                }
            } else {
                load_native(&path, &module.entry_point(grammar))
            }
        };
        if let Err(error) = &result {
            Self::report(&mut loaded, error);
        }
        loaded.languages.insert(grammar.to_owned(), result.clone());
        result
    }
    fn parser(&self, grammar: &str) -> Result<Parser, String> {
        let language = self.language(grammar)?;
        let mut parser = Parser::new();
        if language.is_wasm() {
            let store = WasmStore::new(wasm_engine()?).map_err(|e| e.to_string())?;
            parser.set_wasm_store(store).map_err(|e| e.to_string())?;
        }
        parser.set_language(&language).map_err(|e| e.to_string())?;
        Ok(parser)
    }
    fn text(&self, grammar: &str, file: &str) -> TextResult {
        let (package, _) = self.available(grammar)?;
        if !["highlights.scm", "injections.scm"].contains(&file) {
            return Err(format!("unknown language resource {grammar}/{file}"));
        }
        let mut loaded = self.loaded.lock().unwrap();
        let key = (grammar.to_owned(), file.to_owned());
        if let Some(text) = loaded.texts.get(&key) {
            return text.clone();
        }
        loaded.activity.queries.insert(key.clone());
        loaded.activity.query_reads += 1;
        let paths = if let Some(grammars) = &package.standard {
            grammars[grammar].queries(file)
        } else {
            vec![format!("grammars/{grammar}/{file}")]
        };
        let optional = file == "injections.scm";
        let result = (|| {
            let mut combined = String::new();
            for relative in paths {
                let path = package.root.join(relative);
                if optional && !path.exists() {
                    continue;
                }
                standard::contained(&package.root, &path)?;
                let text = read_text(&path, MAX_QUERY)?;
                if combined.len() + text.len() + 1 > MAX_QUERY as usize {
                    return Err("combined query exceeds byte limit".to_owned());
                }
                combined.push_str(&text);
                combined.push('\n');
            }
            if optional && combined.is_empty() {
                return Ok(None);
            }
            if loaded.text_bytes + combined.len() > MAX_QUERY_CACHE {
                return Err("language query cache budget exceeded".to_owned());
            }
            loaded.text_bytes += combined.len();
            Ok(Some(Arc::from(combined)))
        })();
        if let Err(error) = &result {
            Self::report(&mut loaded, error);
        }
        loaded.texts.insert(key, result.clone());
        result
    }
    fn report(&self, error: &str) {
        Self::report(&mut self.loaded.lock().unwrap(), error);
    }
}
/// The language packs extension packages contribute: for every package folder
/// in `extensions` whose manifest lists a `contributes.languages` resource named
/// `tree-sitter.json` (or `package.json`), the folder that file is in. The
/// manifest is read with the extension loader's own rules and a package whose
/// manifest it rejects contributes nothing; nothing here runs or loads a module.
pub(crate) fn extension_packs(extensions: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(extensions) else {
        return Vec::new();
    };
    let disabled = super::extensions::disabled::read(extensions);
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let package = entry.path();
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) || entry.file_name() == ".cache" {
            continue;
        }
        let Ok(manifest) =
            read_text(&package.join("manifest.json"), MAX_MANIFEST).and_then(|text| {
                crate::features::extensions::manifest::Manifest::parse(&text)
                    .map_err(|error| error.to_string())
            })
        else {
            continue;
        };
        if disabled.contains(&manifest.id) {
            continue;
        }
        for resource in &manifest.contributes.languages {
            if let Some(directory) = pack_directory(&package, &resource.path) {
                found.push(directory);
            }
        }
    }
    found.sort();
    found.dedup();
    found.truncate(MAX_PACKAGES);
    found
}

/// The pack folder a package's language resource `path` names: its parent, when
/// the file is a pack manifest and stays inside the package after resolving links.
fn pack_directory(package: &Path, path: &str) -> Option<PathBuf> {
    let file = Path::new(path).file_name()?.to_str()?;
    if !matches!(file, "tree-sitter.json" | "package.json") {
        return None;
    }
    let base = package.canonicalize().ok()?;
    let directory = package.join(path).parent()?.canonicalize().ok()?;
    directory.starts_with(&base).then_some(directory)
}

/// Why a language resource of an extension package cannot be used, checked the
/// way startup discovery would: the pack's manifest, its grammars and file types.
pub(crate) fn check_extension_pack(package: &Path, path: &str) -> Result<(), String> {
    let directory = pack_directory(package, path)
        .ok_or("a language resource must be a tree-sitter.json inside the package")?;
    let installed = if directory.join("tree-sitter.json").is_file() {
        standard::read(&directory)?
    } else {
        let text = read_text(&directory.join("package.json"), MAX_MANIFEST)?;
        let manifest: Manifest = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        manifest.validate(&directory)?;
        Installed {
            manifest,
            root: directory.clone(),
            standard: None,
        }
    };
    registry(&BTreeMap::from([(
        installed.manifest.id.clone(),
        installed,
    )]))
    .map(|_| ())
}

/// Where the application keeps its bundled resource directory `name`: beside the
/// executable, or in the bundle's `Resources` on macOS.
pub(crate) fn bundled_directory(executable: &Path, name: &str) -> PathBuf {
    let parent = executable.parent().expect("executable has a directory");
    if cfg!(target_os = "macos")
        && parent.file_name().is_some_and(|name| name == "MacOS")
        && parent
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "Contents")
    {
        parent.parent().unwrap().join("Resources").join(name)
    } else {
        parent.join(name)
    }
}

fn locations(executable: &Path) -> PathBuf {
    bundled_directory(executable, "language-packs")
}

fn user_directory_from(
    platform: &str,
    get: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    user_resource_directory_from(platform, get, "language-packs")
}

/// The directory `name` in Loom's per-user data location.
pub(crate) fn user_resource_directory(name: &str) -> Option<PathBuf> {
    user_resource_directory_from(std::env::consts::OS, |key| std::env::var_os(key), name)
}

fn user_resource_directory_from(
    platform: &str,
    get: impl Fn(&str) -> Option<std::ffi::OsString>,
    name: &str,
) -> Option<PathBuf> {
    let home = || get("HOME").map(PathBuf::from);
    let base = match platform {
        "windows" => super::user_paths::windows_root(&get),
        "macos" => home().map(|p| p.join("Library/Application Support/Loom")),
        _ => get("XDG_DATA_HOME")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home().map(|p| p.join(".local/share")))
            .map(|p| p.join("loom")),
    };
    base.filter(|p| p.is_absolute()).map(|p| p.join(name))
}
fn user_directory() -> Option<PathBuf> {
    user_directory_from(std::env::consts::OS, |name| std::env::var_os(name))
}

pub(crate) fn resources() -> Arc<Packages> {
    static RESOURCES: OnceLock<Arc<Packages>> = OnceLock::new();
    RESOURCES
        .get_or_init(|| {
            #[cfg(not(test))]
            let executable = std::env::current_exe().expect("locate Loom language resources");
            #[cfg(test)]
            {
                let root = std::env::var_os("LOOM_TEST_LANGUAGE_PACKS");
                let source = match root {
                    Some(path) => Packages::discover(Path::new(&path), None),
                    None => Packages {
                        packages: BTreeMap::new(),
                        loaded: Mutex::default(),
                        catalog: None,
                        disabled: BTreeSet::new(),
                    },
                };
                Arc::new(source)
            }
            #[cfg(not(test))]
            {
                let bundled = locations(&executable);
                let user = user_directory();
                // Packs inside extension packages are found now, like the others; a
                // pack added later is seen after a restart, as for any language pack.
                let contributed = user_resource_directory("extensions")
                    .map(|directory| extension_packs(&directory))
                    .unwrap_or_default();
                let mut source = Packages::discover_with(&bundled, &contributed, user.as_deref());
                if let Err(error) = source.enable_catalog(
                    &bundled.join("language-packs.toml"),
                    user.map(|p| p.join(".managed")),
                ) {
                    Packages::report(source.loaded.get_mut().unwrap(), &error);
                }
                Arc::new(source)
            }
        })
        .clone()
}
pub(crate) fn initialize() {
    let source = resources();
    if let Ok(registry) = source.registry()
        && let Err(error) = Registry::install(registry)
    {
        eprintln!("language packages: {error}");
    }
}
pub(crate) fn index() -> TreeIndex {
    TreeIndex::new(resources())
}

#[cfg(test)]
#[path = "language_packs_unit_tests.rs"]
mod unit_tests;
