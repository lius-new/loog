// Discover independent language packages at startup; open native modules and
// queries only when the syntax worker requests them. Catalog resources install on demand.
use crate::domain::language::Registry;
use crate::features::syntax::query_stack::{self, Layer, Origin, Rejected, Stacked};
use crate::features::syntax::{Resources, TreeIndex};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};
use tree_sitter::{Language, Parser, WasmStore, wasmtime};

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
// Grammars are WebAssembly modules: they run in a sandbox, so a downloaded or
// extension-supplied language cannot execute native code inside Loom. A stem
// names the upstream download without renaming it.
fn module_file(module: &str) -> Result<String, String> {
    if name(module) {
        return Ok(format!("{module}.wasm"));
    }
    if let Some((stem, "wasm")) = module.rsplit_once('.') {
        let mut chars = stem.chars();
        if chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        {
            return Ok(module.to_owned());
        }
    }
    Err(format!(
        "invalid language module filename {module}; only WebAssembly (.wasm) grammars are supported"
    ))
}
fn module_candidates(module: &str) -> Vec<String> {
    let file = module_file(module).expect("validated module filename");
    if name(module) {
        vec![format!("tree-sitter-{module}.wasm"), file]
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
                || module_file(&module.module).is_err()
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
    /// Which file supplied each part of a text above, by the same key.
    origins: HashMap<(String, String), Vec<Origin>>,
    text_bytes: usize,
    activity: Activity,
}

type Listener = Arc<dyn Fn() + Send + Sync>;

/// Query files stacked on the grammars' own (see `query_stack`), and what to tell
/// the syntax worker when they change.
#[derive(Default)]
struct Layering {
    /// The user's `queries` folder: `<grammar>/<file>` replaces or extends.
    user: Mutex<Option<PathBuf>>,
    extensions: Mutex<Vec<ExtensionQuery>>,
    /// Advances every time the stacked queries are read again.
    generation: AtomicU64,
    listeners: Mutex<Vec<Listener>>,
    /// Layers that could not be used, by `(grammar, file)`.
    rejected: Mutex<BTreeMap<(String, String), Vec<Rejected>>>,
}

pub(crate) struct Packages {
    packages: BTreeMap<String, Installed>,
    loaded: Mutex<Loaded>,
    catalog: Option<Arc<catalog::Manager>>,
    disabled: BTreeSet<String>,
    layering: Layering,
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
        let mut source = Self::empty();
        source.scan(bundled, false);
        source.add_directories(extension_packs.to_vec(), false);
        if let Some(user) = user {
            source.scan(user, true);
        }
        source
    }

    fn empty() -> Self {
        Self {
            packages: BTreeMap::new(),
            loaded: Mutex::default(),
            catalog: None,
            disabled: BTreeSet::new(),
            layering: Layering::default(),
        }
    }

    /// The grammars of the language packs in these folders: the folders enabled
    /// extensions name in their `contributes.languages`.
    pub(crate) fn from_directories(directories: Vec<PathBuf>) -> Self {
        let mut source = Self::empty();
        source.add_directories(directories, false);
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
    /// Offer the languages of the registry `file` for download on demand. Archives
    /// are kept in `cache`; an installed language becomes an extension in
    /// `extensions`.
    fn enable_catalog(
        &mut self,
        file: &Path,
        cache: Option<PathBuf>,
        extensions: Option<PathBuf>,
    ) -> Result<(), String> {
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
        self.catalog = Some(catalog::Manager::new(
            catalog,
            descriptions,
            cache,
            extensions,
        ));
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
        let candidates = module_candidates(module);
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

    /// The grammar's own `file` query with the extensions' and the user's layers
    /// stacked on it. `None` when an optional query exists nowhere.
    fn stacked_text(
        &self,
        package: &Installed,
        grammar: &str,
        file: &str,
    ) -> Result<Option<Stacked>, String> {
        let paths = if let Some(grammars) = &package.standard {
            grammars[grammar].queries(file)
        } else {
            vec![format!("grammars/{grammar}/{file}")]
        };
        let optional = file == "injections.scm";
        let mut base = Vec::new();
        let mut size = 0;
        for relative in paths {
            let path = package.root.join(&relative);
            if optional && !path.exists() {
                continue;
            }
            standard::contained(&package.root, &path)?;
            let text = read_text(&path, MAX_QUERY)?;
            size += text.len() + 1;
            if size > MAX_QUERY as usize {
                return Err("combined query exceeds byte limit".to_owned());
            }
            base.push((path.display().to_string(), text));
        }
        let (layers, mut rejected) = self.layers_for(grammar, file);
        // A layer is checked against the grammar; without one it cannot be.
        let language = if layers.is_empty() {
            None
        } else {
            self.language(grammar).ok()
        };
        let mut stacked = query_stack::stack(base, &layers, |candidate| match &language {
            Some(language) => crate::features::syntax::check_query(language, file, candidate),
            None => Ok(()),
        });
        if stacked.text.len() > MAX_QUERY as usize {
            return Err("combined query exceeds byte limit".to_owned());
        }
        rejected.append(&mut stacked.rejected);
        for problem in &rejected {
            eprintln!(
                "query {grammar}/{file}: {}: {}",
                problem.source, problem.error
            );
        }
        self.layering
            .rejected
            .lock()
            .unwrap()
            .insert((grammar.to_owned(), file.to_owned()), rejected);
        Ok((!(optional && stacked.text.trim().is_empty())).then_some(stacked))
    }

    /// The layers above the grammar's query: extensions in id order, then the
    /// user's file. A file that cannot be read is reported, not stacked.
    fn layers_for(&self, grammar: &str, file: &str) -> (Vec<Layer>, Vec<Rejected>) {
        let mut layers = Vec::new();
        let mut rejected = Vec::new();
        let mut add = |source: String, path: &Path| match read_text(path, MAX_QUERY) {
            Ok(text) => layers.push(Layer { source, text }),
            Err(error) => rejected.push(Rejected { source, error }),
        };
        for query in self.layering.extensions.lock().unwrap().iter() {
            if query.grammar == grammar && query.file == file {
                add(
                    format!("{} ({})", query.extension, query.path.display()),
                    &query.path,
                );
            }
        }
        if let Some(user) = self.layering.user.lock().unwrap().as_ref() {
            let path = user.join(grammar).join(file);
            if path.is_file() {
                add(path.display().to_string(), &path);
            }
        }
        (layers, rejected)
    }

    /// Read every stacked query again and tell the syntax worker, so open
    /// documents are coloured with the new rules.
    pub(crate) fn reload_queries(&self) {
        {
            let mut loaded = self.loaded.lock().unwrap();
            loaded.texts.clear();
            loaded.origins.clear();
            loaded.text_bytes = 0;
        }
        self.layering.rejected.lock().unwrap().clear();
        self.layering.generation.fetch_add(1, Ordering::AcqRel);
        let listeners = self.layering.listeners.lock().unwrap().clone();
        for listener in listeners {
            listener();
        }
    }

    /// Set where layers come from. The queries are read again when that changed,
    /// or when `force` says a file under the same names was edited.
    pub(crate) fn set_query_sources(
        &self,
        user: Option<PathBuf>,
        extensions: Vec<ExtensionQuery>,
        force: bool,
    ) {
        let changed = {
            let mut current_user = self.layering.user.lock().unwrap();
            let mut current = self.layering.extensions.lock().unwrap();
            let changed = *current_user != user || *current != extensions;
            *current_user = user;
            *current = extensions;
            changed
        };
        if changed || force {
            self.reload_queries();
        }
    }

    /// How many layers were left out of the queries read so far.
    pub(crate) fn query_problem_count(&self) -> usize {
        self.layering
            .rejected
            .lock()
            .unwrap()
            .values()
            .map(Vec::len)
            .sum()
    }

    /// Layers left out of the queries read so far, one line each, for display.
    pub(crate) fn query_problems(&self) -> Vec<String> {
        self.layering
            .rejected
            .lock()
            .unwrap()
            .iter()
            .flat_map(|((grammar, file), problems)| {
                problems.iter().map(move |problem| {
                    format!("{grammar}/{file}: {}: {}", problem.source, problem.error)
                })
            })
            .collect()
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
impl Resources for Packages {
    fn generation(&self) -> u64 {
        self.catalog.as_ref().map_or(0, |c| c.generation())
            + self.layering.generation.load(Ordering::Acquire)
    }
    fn pending(&self, grammar: &str) -> bool {
        self.catalog.as_ref().is_some_and(|c| c.pending(grammar))
    }
    fn subscribe(&self, changed: Arc<dyn Fn() + Send + Sync>) {
        self.layering
            .listeners
            .lock()
            .unwrap()
            .push(changed.clone());
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
            if path.extension().is_none_or(|extension| extension != "wasm") {
                Err(format!(
                    "{}: only WebAssembly (.wasm) grammars are supported",
                    path.display()
                ))
            } else {
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
        let key = (grammar.to_owned(), file.to_owned());
        if let Some(text) = self.loaded.lock().unwrap().texts.get(&key) {
            return text.clone();
        }
        // Reading and checking layers can load the grammar, which takes the lock.
        let result = self.stacked_text(&package, grammar, file);
        let mut loaded = self.loaded.lock().unwrap();
        loaded.activity.queries.insert(key.clone());
        loaded.activity.query_reads += 1;
        let result = result.and_then(|stacked| match stacked {
            Some(stacked) if loaded.text_bytes + stacked.text.len() > MAX_QUERY_CACHE => {
                Err("language query cache budget exceeded".to_owned())
            }
            Some(stacked) => {
                loaded.text_bytes += stacked.text.len();
                loaded.origins.insert(key.clone(), stacked.origins);
                Ok(Some(Arc::from(stacked.text)))
            }
            None => Ok(None),
        });
        if let Err(error) = &result {
            Self::report(&mut loaded, error);
        }
        loaded.texts.insert(key, result.clone());
        result
    }
    fn origin(&self, grammar: &str, file: &str, at: usize) -> Option<(String, usize)> {
        let loaded = self.loaded.lock().unwrap();
        let key = (grammar.to_owned(), file.to_owned());
        let text = loaded.texts.get(&key)?.as_ref().ok()?.as_ref()?;
        query_stack::locate(text, loaded.origins.get(&key)?, at)
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
    packs_in(
        super::extensions::state::folders(extensions, true),
        &super::extensions::state::disabled(extensions),
    )
}

/// The language packs of the extension `folders`, except those of the `disabled` ids.
pub(crate) fn packs_in(folders: Vec<PathBuf>, disabled: &BTreeSet<String>) -> Vec<PathBuf> {
    packs_of(enabled_extensions(folders, disabled))
}

type Enabled = Vec<(PathBuf, crate::features::extensions::manifest::Manifest)>;

/// The folders extensions are loaded from: the ones that ship with Loom, then the
/// version of each the user installed that is in use. What the user turned off
/// is recorded in the user's folder, bundled extensions included.
fn extension_folders() -> (Vec<PathBuf>, BTreeSet<String>) {
    let user = user_resource_directory("extensions");
    let disabled = user
        .as_deref()
        .map(super::extensions::state::disabled)
        .unwrap_or_default();
    let mut folders = bundled_extensions_directory()
        .map(|root| bundled_folders(&root))
        .unwrap_or_default();
    if let Some(user) = user {
        folders.extend(super::extensions::state::folders(&user, true));
    }
    (folders, disabled)
}

/// The language packs of every enabled extension, bundled and installed.
pub(crate) fn installed_extension_packs() -> Vec<PathBuf> {
    let (folders, disabled) = extension_folders();
    packs_of(enabled_extensions(folders, &disabled))
}

/// The queries of every enabled extension, bundled and installed.
pub(crate) fn installed_extension_queries() -> Vec<ExtensionQuery> {
    let (folders, disabled) = extension_folders();
    queries_of(enabled_extensions(folders, &disabled))
}

/// Where the extensions that ship with Loom are kept: `extensions` beside the
/// executable (in the bundle's `Resources` on macOS). Tests use the source tree.
pub(crate) fn bundled_extensions_directory() -> Option<PathBuf> {
    #[cfg(test)]
    {
        Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/extensions"))
    }
    #[cfg(not(test))]
    {
        let executable = std::env::current_exe().ok()?;
        Some(bundled_directory(&executable, "extensions"))
    }
}

fn packs_of(enabled: Enabled) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for (package, manifest) in enabled {
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

/// The extension folders directly under `root`, which is where the ones that
/// ship with Loom are (they have no version folder).
pub(crate) fn bundled_folders(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<_> = entries
        .flatten()
        .filter(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_dir())
                && !entry.file_name().to_string_lossy().starts_with('.')
        })
        .map(|entry| entry.path())
        .collect();
    found.sort();
    found
}

/// Of the extension `folders`, those that are not disabled and whose manifest
/// is valid.
fn enabled_extensions(folders: Vec<PathBuf>, disabled: &BTreeSet<String>) -> Enabled {
    folders
        .into_iter()
        .filter_map(|package| {
            let manifest =
                read_text(&package.join("manifest.json"), MAX_MANIFEST).and_then(|text| {
                    crate::features::extensions::manifest::Manifest::parse(&text)
                        .map_err(|error| error.to_string())
                });
            manifest.ok().map(|manifest| (package, manifest))
        })
        .filter(|(_, manifest)| !disabled.contains(&manifest.id))
        .collect()
}

/// A query file an extension package stacks on a grammar's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExtensionQuery {
    pub extension: String,
    pub grammar: String,
    pub file: String,
    pub path: PathBuf,
}

/// The queries enabled extension packages contribute, in extension id order.
fn queries_of(enabled: Enabled) -> Vec<ExtensionQuery> {
    let mut found = Vec::new();
    for (package, manifest) in enabled {
        let Ok(base) = package.canonicalize() else {
            continue;
        };
        for query in &manifest.contributes.queries {
            let Ok(path) = package.join(&query.path).canonicalize() else {
                continue;
            };
            if path.starts_with(&base) {
                found.push(ExtensionQuery {
                    extension: manifest.id.clone(),
                    grammar: query.grammar.clone(),
                    file: query.file.clone(),
                    path,
                });
            }
        }
    }
    found.sort_by(|a, b| a.extension.cmp(&b.extension).then(a.path.cmp(&b.path)));
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

/// The directory `name` in Loom's per-user data location.
pub(crate) fn user_resource_directory(name: &str) -> Option<PathBuf> {
    user_resource_directory_from(std::env::consts::OS, |key| std::env::var_os(key), name)
}

fn user_resource_directory_from(
    platform: &str,
    get: impl Fn(&str) -> Option<std::ffi::OsString>,
    name: &str,
) -> Option<PathBuf> {
    // A development override: `loom-ext dev --to <dir>` stages extensions there.
    if name == "extensions"
        && let Some(dir) = get("LOOM_EXTENSIONS_DIR")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
    {
        return Some(dir);
    }
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

pub(crate) fn resources() -> Arc<Packages> {
    static RESOURCES: OnceLock<Arc<Packages>> = OnceLock::new();
    RESOURCES
        .get_or_init(|| {
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
                        layering: Layering::default(),
                    },
                };
                Arc::new(source)
            }
            #[cfg(not(test))]
            {
                // Grammars come from extensions: the ones that ship with Loom and
                // the ones installed. One added later is seen after a restart,
                // unless it is downloaded on demand from the registry below.
                let mut source = Packages::from_directories(installed_extension_packs());
                let user = user_resource_directory("extensions");
                if let Some(registry) =
                    bundled_extensions_directory().map(|directory| directory.join("registry.toml"))
                    && let Err(error) = source.enable_catalog(
                        &registry,
                        user.as_ref().map(|p| p.join(".cache").join("languages")),
                        user,
                    )
                {
                    Packages::report(source.loaded.get_mut().unwrap(), &error);
                }
                source.set_query_sources(
                    user_resource_directory("queries"),
                    installed_extension_queries(),
                    false,
                );
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
    #[cfg(not(test))]
    watch_queries();
}

/// Read the stacked queries again whenever a query file or an extension's
/// manifest changes, so editing a highlight rule shows in open documents.
#[cfg(not(test))]
fn watch_queries() {
    static WATCHERS: OnceLock<Vec<super::query_watcher::DirWatcher>> = OnceLock::new();
    WATCHERS.get_or_init(|| {
        let reload = || {
            let packages = resources();
            packages.set_query_sources(
                user_resource_directory("queries"),
                installed_extension_queries(),
                true,
            );
        };
        ["queries", "extensions"]
            .into_iter()
            .filter_map(user_resource_directory)
            .filter_map(|directory| {
                super::query_watcher::watch(&directory, super::query_watcher::relevant, reload)
            })
            .collect()
    });
}
pub(crate) fn index() -> TreeIndex {
    TreeIndex::new(resources())
}

#[cfg(test)]
#[path = "language_queries_tests.rs"]
mod query_tests;
#[cfg(test)]
#[path = "language_packs_unit_tests.rs"]
mod unit_tests;
