//! Extension manifest: parsing and validation (design §10).
//!
//! A manifest is data. Validation never reads the package, runs a component or
//! evaluates a condition from the package or workspace.
use serde::Deserialize;
use std::collections::BTreeSet;

/// The only behaviour contract version understood by v0.1 hosts (exact match).
pub const API_VERSION: &str = "loom:extension@0.1.0";
pub const SCHEMA: u32 = 1;

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub id: String,
    pub version: String,
    /// Exact WIT contract; present exactly when `component` is.
    #[serde(default)]
    pub api: Option<String>,
    /// Package-relative path of the component. Absent for resource-only packages.
    #[serde(default)]
    pub component: Option<String>,
    #[serde(default)]
    pub activation: Vec<Activation>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub contributes: Contributes,
    /// Hosts the extension asks to reach; needs the `network` capability.
    #[serde(default)]
    pub network: Option<NetworkDecl>,
    // What people see before and after installing: the extensions page and the
    // import both read these. A folder used for development may omit them; a
    // package that is built for distribution may not ([`Manifest::publishing_problems`]).
    /// The name shown to people; the id is only the identity.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub publisher: Option<String>,
    /// One sentence: what it is for.
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    /// An SPDX license expression such as `MIT` or `Apache-2.0 OR MIT`.
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    /// Package-relative paths of the icon (PNG or SVG), the long description
    /// and the version history (Markdown).
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub readme: Option<String>,
    #[serde(default)]
    pub changelog: Option<String>,
    #[serde(default)]
    pub engines: Option<Engines>,
    /// Extensions that must be installed with this one, as `id@range`
    /// (`loom.runtime.node@^22`).
    #[serde(default)]
    pub requires: Vec<String>,
    /// Extension ids worth suggesting when this one is installed.
    #[serde(default)]
    pub recommends: Vec<String>,
    /// Package-relative files that are programs; only these are made executable.
    #[serde(default)]
    pub executables: Vec<String>,
}

/// What the extension needs from the application.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Engines {
    /// The Loom versions that can run it, as a range such as `>=0.1.8 <0.3`.
    pub loom: String,
}

/// An entry of `requires`: the extension and the versions of it that will do.
pub fn parse_requirement(text: &str) -> Option<(&str, super::range::Range)> {
    let (id, range) = text.split_once('@')?;
    valid_id(id).then_some(())?;
    Some((id, super::range::Range::parse(range)?))
}

/// The categories an extension may list (at most three).
pub const CATEGORIES: &[&str] = &[
    "Themes",
    "Syntax Highlighting",
    "Language Services",
    "Snippets",
    "Linters",
    "Formatters",
    "Editing",
    "Source control",
    "Productivity",
    "Other",
];

/// Largest icon, and largest README or changelog, a package may carry.
pub const MAX_ICON_BYTES: u64 = 256 * 1024;
pub const MAX_DOCUMENT_BYTES: u64 = 512 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkDecl {
    /// Exact host names, or `*.example.com` for every subdomain.
    pub domains: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Activation {
    Command { id: String },
    Document { selector: Selector },
    Event { name: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selector {
    #[serde(default)]
    pub languages: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contributes {
    /// Declarative language identities and file associations, independent of
    /// syntax parsers and executable providers.
    #[serde(default)]
    pub languages: Vec<ResourceDecl>,
    #[serde(default)]
    pub commands: Vec<CommandDecl>,
    /// Color themes.
    #[serde(default)]
    pub themes: Vec<ResourceDecl>,
    #[serde(default)]
    pub snippets: Vec<ResourceDecl>,
    /// Syntax highlighting: grammar packs (`tree-sitter.json`) and descriptions of
    /// languages the built-in highlighter colours (`language.json`).
    #[serde(default)]
    pub highlight: Vec<ResourceDecl>,
    /// Highlight and injection queries for a grammar, layered over its own.
    #[serde(default)]
    pub highlight_queries: Vec<QueryDecl>,
    /// What answers hints and diagnostics for a language.
    #[serde(default)]
    pub language_service: LanguageService,
    #[serde(default)]
    pub settings: Vec<SettingDecl>,
}

/// Language providers answered by the package's own component.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageService {
    #[serde(default)]
    pub providers: Vec<ProviderDecl>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SettingKind {
    String,
    Number,
    Boolean,
}

/// Where a setting may be set. A user-scoped setting ignores project files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SettingScope {
    #[default]
    Project,
    User,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingDecl {
    pub key: String,
    pub kind: SettingKind,
    pub default: serde_json::Value,
    #[serde(default)]
    pub scope: SettingScope,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandDecl {
    pub id: String,
    pub title: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    Completion,
    Diagnostics,
    Formatting,
    CodeActions,
    SaveParticipant,
    Hover,
    Definition,
    DocumentSymbols,
    SignatureHelp,
    InlayHints,
    References,
    WorkspaceSymbols,
    DocumentHighlights,
    Rename,
    FoldingRanges,
    SelectionRanges,
    CallHierarchy,
    TypeHierarchy,
    CodeLens,
    FormatOnType,
    ExecuteCommand,
    Highlights,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderDecl {
    pub id: String,
    pub kind: ProviderKind,
    pub selector: Selector,
}

/// A data resource inside the package (theme, grammar, snippet file, ...).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceDecl {
    pub id: String,
    pub path: String,
}

/// A query file for a grammar the package may not own. It is stacked on the
/// grammar's own query: appended when it starts with `; extends`, otherwise it
/// replaces what is below it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryDecl {
    pub id: String,
    /// The grammar's canonical name, such as `rust`.
    pub grammar: String,
    /// `highlights.scm` or `injections.scm`.
    pub file: String,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestError {
    Syntax(String),
    UnsupportedSchema(u32),
    InvalidId(String),
    InvalidVersion(String),
    /// `api` and `component` must be given together.
    ApiComponentMismatch,
    UnsupportedApi(String),
    UnsafePath(String),
    UnknownCapability(String),
    UnknownEvent(String),
    /// A resource-only package cannot declare behaviour.
    BehaviourWithoutComponent(&'static str),
    UndeclaredCommand(String),
    NotNamespaced {
        item: String,
        owner: String,
    },
    Duplicate(String),
    EmptySelector(String),
    /// A setting's default does not have the declared type.
    BadSettingDefault(String),
    /// A query names a grammar or a query file that does not exist.
    BadQuery(String),
    /// A network domain is not a plain host name pattern, or is declared without the capability.
    BadDomain(String),
    /// A display field (name, categories, license, ...) is not usable.
    BadMetadata(String),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Syntax(message) => write!(f, "manifest is not valid: {message}"),
            Self::UnsupportedSchema(schema) => write!(f, "unsupported manifest schema {schema}"),
            Self::InvalidId(id) => write!(f, "invalid extension id `{id}`"),
            Self::InvalidVersion(version) => write!(f, "invalid version `{version}`"),
            Self::ApiComponentMismatch => {
                write!(f, "`api` and `component` must be declared together")
            }
            Self::UnsupportedApi(api) => write!(
                f,
                "unsupported api `{api}`; this host needs `{API_VERSION}`"
            ),
            Self::UnsafePath(path) => write!(f, "path `{path}` must stay inside the package"),
            Self::UnknownCapability(name) => write!(f, "unknown capability `{name}`"),
            Self::UnknownEvent(name) => write!(f, "unknown event `{name}`"),
            Self::BehaviourWithoutComponent(what) => write!(f, "{what} needs a component"),
            Self::UndeclaredCommand(id) => {
                write!(f, "activation refers to undeclared command `{id}`")
            }
            Self::NotNamespaced { item, owner } => write!(f, "`{item}` must start with `{owner}.`"),
            Self::Duplicate(item) => write!(f, "`{item}` is declared twice"),
            Self::EmptySelector(id) => write!(f, "provider `{id}` selects no language"),
            Self::BadDomain(domain) => {
                write!(f, "`{domain}` is not a valid network domain declaration")
            }
            Self::BadMetadata(problem) => write!(f, "{problem}"),
            Self::BadQuery(id) => write!(
                f,
                "query `{id}` needs a lowercase grammar name and highlights.scm or injections.scm"
            ),
            Self::BadSettingDefault(key) => {
                write!(f, "the default of setting `{key}` has the wrong type")
            }
        }
    }
}

impl std::error::Error for ManifestError {}

/// Capabilities a package may request. Declaring one grants nothing by itself.
pub const CAPABILITIES: &[&str] = &[
    "documents.read",
    "workspace.edit",
    "workspace.files.read",
    "workspace.files.write",
    "events",
    "settings",
    "languages",
    "storage",
    "network",
    "tasks",
    "process.execute-local",
];

pub const EVENTS: &[&str] = &[
    "document.open",
    "document.change",
    "document.close",
    "document.save",
    "workspace.files",
    "selection.change",
    "focus.change",
    "root.change",
];

impl Manifest {
    pub fn parse(source: &str) -> Result<Self, ManifestError> {
        let manifest: Manifest = serde_json::from_str(source)
            .map_err(|error| ManifestError::Syntax(error.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn has_behaviour(&self) -> bool {
        self.component.is_some()
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema != SCHEMA {
            return Err(ManifestError::UnsupportedSchema(self.schema));
        }
        if !valid_id(&self.id) {
            return Err(ManifestError::InvalidId(self.id.clone()));
        }
        if !valid_version(&self.version) {
            return Err(ManifestError::InvalidVersion(self.version.clone()));
        }
        match (&self.api, &self.component) {
            (Some(api), Some(component)) => {
                if api != API_VERSION {
                    return Err(ManifestError::UnsupportedApi(api.clone()));
                }
                check_package_path(component)?;
            }
            (None, None) => {}
            _ => return Err(ManifestError::ApiComponentMismatch),
        }
        for capability in &self.capabilities {
            if !CAPABILITIES.contains(&capability.as_str()) {
                return Err(ManifestError::UnknownCapability(capability.clone()));
            }
        }
        if let Some(network) = &self.network {
            let declared = self.capabilities.iter().any(|c| c == "network");
            if !declared || network.domains.is_empty() {
                return Err(ManifestError::BadDomain("network".to_owned()));
            }
            for domain in &network.domains {
                if !valid_domain(domain) {
                    return Err(ManifestError::BadDomain(domain.clone()));
                }
            }
        }
        self.validate_display()?;
        self.validate_contributions()?;
        self.validate_activation()
    }

    /// The display fields that are present must be usable as they are.
    fn validate_display(&self) -> Result<(), ManifestError> {
        let bad = |problem: String| Err(ManifestError::BadMetadata(problem));
        let text = |field: &str, value: &Option<String>, max: usize| match value {
            Some(value) if value.trim().is_empty() || value.chars().count() > max => Err(
                ManifestError::BadMetadata(format!("`{field}` must be 1 to {max} characters")),
            ),
            Some(value) if value.contains(['\n', '\r', '\0']) => Err(ManifestError::BadMetadata(
                format!("`{field}` must be a single line"),
            )),
            _ => Ok(()),
        };
        text("name", &self.name, 60)?;
        text("publisher", &self.publisher, 60)?;
        text("description", &self.description, 200)?;
        text("license", &self.license, 100)?;
        if self.categories.len() > 3 {
            return bad("`categories` lists more than three".to_owned());
        }
        if let Some(category) = self
            .categories
            .iter()
            .find(|category| !CATEGORIES.contains(&category.as_str()))
        {
            return bad(format!(
                "`{category}` is not a category; use one of {}",
                CATEGORIES.join(", ")
            ));
        }
        if self.keywords.len() > 10
            || self
                .keywords
                .iter()
                .any(|keyword| keyword.trim().is_empty() || keyword.chars().count() > 30)
        {
            return bad("`keywords` allows at most ten words of up to 30 characters".to_owned());
        }
        for (field, value) in [
            ("repository", &self.repository),
            ("homepage", &self.homepage),
        ] {
            if let Some(url) = value
                && (!url.starts_with("https://")
                    || url.len() > 300
                    || url.contains(char::is_whitespace))
            {
                return bad(format!("`{field}` must be an https:// address"));
            }
        }
        for (field, path) in [
            ("icon", &self.icon),
            ("readme", &self.readme),
            ("changelog", &self.changelog),
        ] {
            if let Some(path) = path {
                check_package_path(path)?;
                let lower = path.to_ascii_lowercase();
                let fits = match field {
                    "icon" => lower.ends_with(".png") || lower.ends_with(".svg"),
                    _ => lower.ends_with(".md"),
                };
                if !fits {
                    return bad(format!(
                        "`{field}` must be a {} file",
                        if field == "icon" {
                            "PNG or SVG"
                        } else {
                            "Markdown"
                        }
                    ));
                }
            }
        }
        if let Some(engines) = &self.engines
            && super::range::Range::parse(&engines.loom).is_none()
        {
            return bad("`engines.loom` must be a version range such as >=0.1.8 <0.3".to_owned());
        }
        for entry in &self.requires {
            match parse_requirement(entry) {
                Some((id, _)) if id != self.id => {}
                _ => return bad(format!("`requires` entry `{entry}` must look like id@^1")),
            }
        }
        if let Some(id) = self
            .recommends
            .iter()
            .find(|id| !valid_id(id) || **id == self.id)
        {
            return bad(format!(
                "`recommends` entry `{id}` is not another extension id"
            ));
        }
        for path in &self.executables {
            check_package_path(path)?;
        }
        Ok(())
    }

    /// What is missing for this manifest to be built into a distributable
    /// package, so that the extensions page and the import can show everything
    /// about it. `size_of` gives the size of a package file, or `None` when the
    /// file is not there. Every problem is listed, not only the first.
    pub fn publishing_problems(&self, size_of: impl Fn(&str) -> Option<u64>) -> Vec<String> {
        let mut problems = Vec::new();
        let mut need = |present: bool, field: &str, hint: &str| {
            if !present {
                problems.push(format!("`{field}` is missing: {hint}"));
            }
        };
        need(self.name.is_some(), "name", "the name people see");
        need(self.publisher.is_some(), "publisher", "who publishes it");
        need(
            self.description.is_some(),
            "description",
            "one sentence about what it does",
        );
        need(
            !self.categories.is_empty(),
            "categories",
            "one to three of the allowed categories",
        );
        need(
            self.license.is_some(),
            "license",
            "an SPDX license such as MIT",
        );
        need(
            self.repository.is_some(),
            "repository",
            "an https:// address",
        );
        need(
            self.icon.is_some(),
            "icon",
            "a PNG or SVG file in the package",
        );
        need(
            self.readme.is_some(),
            "readme",
            "a Markdown file in the package",
        );
        need(
            self.changelog.is_some(),
            "changelog",
            "a Markdown file in the package",
        );
        need(
            self.engines.is_some(),
            "engines.loom",
            "the oldest Loom version it runs on",
        );
        for (field, path, limit) in [
            ("icon", &self.icon, MAX_ICON_BYTES),
            ("readme", &self.readme, MAX_DOCUMENT_BYTES),
            ("changelog", &self.changelog, MAX_DOCUMENT_BYTES),
        ] {
            let Some(path) = path else { continue };
            match size_of(path) {
                None => problems.push(format!("`{field}`: {path} is not in the package")),
                Some(0) => problems.push(format!("`{field}`: {path} is empty")),
                Some(size) if size > limit => problems.push(format!(
                    "`{field}`: {path} is larger than {} KiB",
                    limit / 1024
                )),
                Some(_) => {}
            }
        }
        problems
    }

    /// Whether a Loom of version `current` meets `engines.loom`.
    pub fn runs_on(&self, current: &str) -> bool {
        let Some(engines) = &self.engines else {
            return true;
        };
        match (
            super::range::parse_version(current),
            super::range::Range::parse(&engines.loom),
        ) {
            (Some(current), Some(range)) => range.matches(current),
            _ => false,
        }
    }

    fn namespaced(&self, item: &str) -> Result<(), ManifestError> {
        match item.strip_prefix(&self.id) {
            Some(rest) if rest.starts_with('.') && rest.len() > 1 => Ok(()),
            _ => Err(ManifestError::NotNamespaced {
                item: item.to_owned(),
                owner: self.id.clone(),
            }),
        }
    }

    fn validate_contributions(&self) -> Result<(), ManifestError> {
        let c = &self.contributes;
        if !self.has_behaviour() {
            if !c.commands.is_empty() {
                return Err(ManifestError::BehaviourWithoutComponent("commands"));
            }
            if !c.language_service.providers.is_empty() {
                return Err(ManifestError::BehaviourWithoutComponent(
                    "language_service.providers",
                ));
            }
            if !self.activation.is_empty() {
                return Err(ManifestError::BehaviourWithoutComponent("activation"));
            }
        }
        let mut seen = BTreeSet::new();
        let mut unique = |id: &str| {
            seen.insert(id.to_owned())
                .then_some(())
                .ok_or_else(|| ManifestError::Duplicate(id.to_owned()))
        };
        for command in &c.commands {
            self.namespaced(&command.id)?;
            unique(&command.id)?;
        }
        for provider in &c.language_service.providers {
            self.namespaced(&provider.id)?;
            unique(&provider.id)?;
            if provider.selector.languages.is_empty() {
                return Err(ManifestError::EmptySelector(provider.id.clone()));
            }
        }
        for resource in c
            .themes
            .iter()
            .chain(&c.highlight)
            .chain(&c.snippets)
            .chain(&c.languages)
        {
            self.namespaced(&resource.id)?;
            unique(&resource.id)?;
            check_package_path(&resource.path)?;
        }
        for query in &c.highlight_queries {
            self.namespaced(&query.id)?;
            unique(&query.id)?;
            check_package_path(&query.path)?;
            let canonical = query
                .grammar
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase())
                && query
                    .grammar
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            if !canonical || !["highlights.scm", "injections.scm"].contains(&query.file.as_str()) {
                return Err(ManifestError::BadQuery(query.id.clone()));
            }
        }
        for setting in &c.settings {
            self.namespaced(&setting.key)?;
            unique(&setting.key)?;
            if !super::settings::matches_kind(&setting.default, setting.kind) {
                return Err(ManifestError::BadSettingDefault(setting.key.clone()));
            }
        }
        Ok(())
    }

    fn validate_activation(&self) -> Result<(), ManifestError> {
        for activation in &self.activation {
            match activation {
                Activation::Command { id } => {
                    if !self
                        .contributes
                        .commands
                        .iter()
                        .any(|command| &command.id == id)
                    {
                        return Err(ManifestError::UndeclaredCommand(id.clone()));
                    }
                }
                Activation::Document { selector } => {
                    if selector.languages.is_empty() {
                        return Err(ManifestError::EmptySelector(
                            "document activation".to_owned(),
                        ));
                    }
                }
                Activation::Event { name } => {
                    if !EVENTS.contains(&name.as_str()) {
                        return Err(ManifestError::UnknownEvent(name.clone()));
                    }
                }
            }
        }
        Ok(())
    }
}

/// A host name or `*.host` pattern: lowercase letters, digits, dots, hyphens;
/// no scheme, port, path or user information, and no bare `*`.
pub fn valid_domain(domain: &str) -> bool {
    let host = domain.strip_prefix("*.").unwrap_or(domain);
    !host.is_empty()
        && host.len() <= 253
        && !host.starts_with(['.', '-'])
        && !host.ends_with(['.', '-'])
        && !host.contains("..")
        && host
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
}

/// `vendor.name` style: two or more dot-separated lowercase segments.
pub fn valid_id(id: &str) -> bool {
    let segments: Vec<&str> = id.split('.').collect();
    segments.len() >= 2
        && segments.iter().all(|segment| {
            !segment.is_empty()
                && segment.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
                && segment
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

fn valid_version(version: &str) -> bool {
    let core = version.split(['-', '+']).next().unwrap_or("");
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

/// A path must be relative, use `/`, and never climb out of the package.
pub fn check_package_path(path: &str) -> Result<(), ManifestError> {
    let unsafe_path = || ManifestError::UnsafePath(path.to_owned());
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains(':')
        || path.contains('\0')
    {
        return Err(unsafe_path());
    }
    if path
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(unsafe_path());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SELECTION_TOOLS: &str = r#"{
        "schema": 1,
        "id": "example.selection-tools",
        "version": "0.1.0",
        "api": "loom:extension@0.1.0",
        "component": "extension.wasm",
        "activation": [{ "kind": "command", "id": "example.selection-tools.uppercase" }],
        "capabilities": ["documents.read", "workspace.edit"],
        "contributes": {
            "commands": [{ "id": "example.selection-tools.uppercase", "title": "Uppercase Selection" }]
        }
    }"#;

    #[test]
    fn parses_the_design_example() {
        let manifest = Manifest::parse(SELECTION_TOOLS).unwrap();
        assert!(manifest.has_behaviour());
        assert_eq!(manifest.contributes.commands.len(), 1);
    }

    fn with(replace: &str, with: &str) -> Result<Manifest, ManifestError> {
        assert!(SELECTION_TOOLS.contains(replace), "fixture lacks {replace}");
        Manifest::parse(&SELECTION_TOOLS.replace(replace, with))
    }

    #[test]
    fn rejects_unknown_fields_and_schemas() {
        assert!(matches!(
            with("\"schema\": 1", "\"schema\": 2"),
            Err(ManifestError::UnsupportedSchema(2))
        ));
        assert!(matches!(
            with("\"version\"", "\"surprise\": 1, \"version\""),
            Err(ManifestError::Syntax(_))
        ));
    }

    #[test]
    fn api_must_match_exactly_and_travel_with_component() {
        assert!(matches!(
            with("loom:extension@0.1.0", "loom:extension@0.2.0"),
            Err(ManifestError::UnsupportedApi(_))
        ));
        assert!(matches!(
            with("\"component\": \"extension.wasm\",", ""),
            Err(ManifestError::ApiComponentMismatch)
        ));
    }

    #[test]
    fn component_paths_cannot_escape_the_package() {
        for path in [
            "../x.wasm",
            "/abs.wasm",
            "C:/x.wasm",
            "a\\\\b.wasm",
            "a//b.wasm",
            "./x.wasm",
        ] {
            assert!(
                matches!(
                    with("extension.wasm", path),
                    Err(ManifestError::UnsafePath(_))
                ),
                "{path}"
            );
        }
        assert!(check_package_path("dist/ext.wasm").is_ok());
    }

    #[test]
    fn commands_are_namespaced_unique_and_activation_must_be_declared() {
        assert!(matches!(
            with(
                "\"id\": \"example.selection-tools.uppercase\", \"title\"",
                "\"id\": \"other.uppercase\", \"title\""
            ),
            Err(ManifestError::NotNamespaced { .. })
        ));
        let duplicated = SELECTION_TOOLS.replace(
            "[{ \"id\": \"example.selection-tools.uppercase\", \"title\": \"Uppercase Selection\" }]",
            "[{ \"id\": \"example.selection-tools.uppercase\", \"title\": \"A\" }, { \"id\": \"example.selection-tools.uppercase\", \"title\": \"B\" }]",
        );
        assert!(matches!(
            Manifest::parse(&duplicated),
            Err(ManifestError::Duplicate(_))
        ));
        assert!(matches!(
            with(
                "\"activation\": [{ \"kind\": \"command\", \"id\": \"example.selection-tools.uppercase\" }]",
                "\"activation\": [{ \"kind\": \"command\", \"id\": \"example.selection-tools.lower\" }]"
            ),
            Err(ManifestError::UndeclaredCommand(_))
        ));
    }

    #[test]
    fn capabilities_and_events_are_a_closed_set() {
        assert!(matches!(
            with("workspace.edit", "root.everything"),
            Err(ManifestError::UnknownCapability(_))
        ));
        assert!(matches!(
            with(
                "{ \"kind\": \"command\", \"id\": \"example.selection-tools.uppercase\" }",
                "{ \"kind\": \"event\", \"name\": \"startup\" }"
            ),
            Err(ManifestError::UnknownEvent(_))
        ));
    }

    #[test]
    fn resource_only_packages_need_no_component_but_cannot_declare_behaviour() {
        let theme = r#"{
            "schema": 1, "id": "acme.night", "version": "1.2.3",
            "contributes": { "themes": [{ "id": "acme.night.dark", "path": "themes/dark.json" }] }
        }"#;
        let manifest = Manifest::parse(theme).unwrap();
        assert!(!manifest.has_behaviour());
        let with_command = theme.replace(
            "\"themes\"",
            "\"commands\": [{ \"id\": \"acme.night.go\", \"title\": \"Go\" }], \"themes\"",
        );
        assert!(matches!(
            Manifest::parse(&with_command),
            Err(ManifestError::BehaviourWithoutComponent("commands"))
        ));
    }

    #[test]
    fn retired_host_server_declarations_are_rejected() {
        let source = r#"{
            "schema": 1, "id": "acme.rust", "version": "1.0.0",
            "capabilities": ["process.execute-local"],
            "contributes": { "language_service": { "servers": [
                { "id": "acme.rust.analyzer", "languages": ["rust"], "command": "rust-analyzer", "args": ["--stdio"], "root_markers": ["Cargo.toml"] }
            ] } }
        }"#;
        assert!(
            matches!(Manifest::parse(source), Err(ManifestError::Syntax(message)) if message.contains("servers"))
        );
    }

    const COMPLETE: &str = r##"{
        "schema": 1, "id": "acme.night", "version": "1.2.0",
        "name": "Acme Night", "publisher": "Acme", "description": "A quieter palette.",
        "categories": ["Themes"], "keywords": ["dark"], "license": "MIT",
        "repository": "https://example.com/acme/night", "icon": "icon.svg",
        "readme": "README.md", "changelog": "CHANGELOG.md", "engines": { "loom": ">=0.1.8 <1" },
        "contributes": { "themes": [{ "id": "acme.night.dark", "path": "themes/dark.json" }] }
    }"##;

    fn sizes(path: &str) -> Option<u64> {
        ["icon.svg", "README.md", "CHANGELOG.md"]
            .contains(&path)
            .then_some(100)
    }

    #[test]
    fn a_complete_manifest_has_nothing_missing_and_knows_which_loom_runs_it() {
        let manifest = Manifest::parse(COMPLETE).unwrap();
        assert!(manifest.publishing_problems(sizes).is_empty());
        assert!(
            manifest.runs_on("0.1.8") && manifest.runs_on("0.2.0") && manifest.runs_on("0.9.9")
        );
        assert!(!manifest.runs_on("0.1.7") && !manifest.runs_on("1.0.0"));
        // A development manifest without the fields still loads, and says what it lacks.
        let bare = Manifest::parse(r#"{ "schema": 1, "id": "a.b", "version": "0.1.0" }"#).unwrap();
        assert!(bare.runs_on("0.0.1"));
        let missing = bare.publishing_problems(|_| None);
        for field in [
            "name",
            "publisher",
            "description",
            "categories",
            "license",
            "repository",
            "icon",
            "readme",
            "changelog",
            "engines.loom",
        ] {
            assert!(
                missing.iter().any(|p| p.starts_with(&format!("`{field}`"))),
                "{field}: {missing:?}"
            );
        }
    }

    #[test]
    fn language_categories_name_a_specific_function() {
        for category in ["Syntax Highlighting", "Language Services"] {
            let manifest = Manifest::parse(&COMPLETE.replace("Themes", category)).unwrap();
            assert!(manifest.publishing_problems(sizes).is_empty());
        }
        let error = Manifest::parse(&COMPLETE.replace("Themes", "Languages")).unwrap_err();
        assert!(error.to_string().contains("is not a category"), "{error}");
    }

    #[test]
    fn display_fields_that_are_present_must_be_usable() {
        for (from, to, expected) in [
            ("\"Themes\"", "\"Nonsense\"", "is not a category"),
            (
                "[\"Themes\"]",
                "[\"Themes\",\"Editing\",\"Linters\",\"Other\"]",
                "more than three",
            ),
            ("\"MIT\"", "\"\"", "`license`"),
            ("\"Acme Night\"", "\"Line\\nbreak\"", "single line"),
            (
                "https://example.com/acme/night",
                "http://example.com",
                "https://",
            ),
            ("icon.svg", "icon.gif", "PNG or SVG"),
            ("README.md", "README.txt", "Markdown"),
            ("README.md", "../README.md", "stay inside"),
            ("0.1.8", "latest", "engines.loom"),
        ] {
            let error = Manifest::parse(&COMPLETE.replace(from, to))
                .err()
                .unwrap_or_else(|| panic!("{to} was accepted"));
            assert!(error.to_string().contains(expected), "{to}: {error}");
        }
    }

    #[test]
    fn requires_recommends_and_executables_are_checked() {
        let with =
            |extra: &str| COMPLETE.replace("\"schema\": 1,", &format!("\"schema\": 1, {extra},"));
        let manifest = Manifest::parse(&with(
            r#""requires": ["loom.runtime.node@^22"], "recommends": ["loom.lsp.pyright"], "executables": ["bin/server"]"#,
        ))
        .unwrap();
        assert_eq!(manifest.requires, ["loom.runtime.node@^22"]);
        assert_eq!(manifest.executables, ["bin/server"]);
        for (extra, expected) in [
            (r#""requires": ["loom.runtime.node"]"#, "requires"),
            (r#""requires": ["loom.runtime.node@latest"]"#, "requires"),
            (r#""requires": ["acme.night@^1"]"#, "requires"),
            (r#""recommends": ["Not An Id"]"#, "recommends"),
            (r#""executables": ["../bin/server"]"#, "inside the package"),
        ] {
            let error = Manifest::parse(&with(extra)).unwrap_err();
            assert!(error.to_string().contains(expected), "{extra}: {error}");
        }
    }

    #[test]
    fn a_missing_empty_or_oversized_document_is_reported() {
        let manifest = Manifest::parse(COMPLETE).unwrap();
        let problems = manifest.publishing_problems(|path| match path {
            "icon.svg" => None,
            "README.md" => Some(0),
            _ => Some(MAX_DOCUMENT_BYTES + 1),
        });
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems[0].contains("icon.svg is not in the package"));
        assert!(problems[1].contains("README.md is empty"));
        assert!(problems[2].contains("CHANGELOG.md is larger than 512 KiB"));
    }

    #[test]
    fn provider_selectors_must_name_a_language() {
        let source = SELECTION_TOOLS.replace(
            "\"commands\": [",
            "\"language_service\": { \"providers\": [{ \"id\": \"example.selection-tools.diag\", \"kind\": \"diagnostics\", \"selector\": { \"languages\": [] } }] }, \"commands\": [",
        );
        assert!(matches!(
            Manifest::parse(&source),
            Err(ManifestError::EmptySelector(_))
        ));
    }

    #[test]
    fn ids_and_versions_are_strict() {
        for id in ["single", "Upper.case", "a..b", "a.-b"] {
            assert!(
                matches!(
                    with("example.selection-tools\",", &format!("{id}\",")),
                    Err(ManifestError::InvalidId(_)) | Err(ManifestError::NotNamespaced { .. })
                ),
                "{id}"
            );
        }
        assert!(matches!(
            with("0.1.0\",\n        \"api", "1.0\",\n        \"api"),
            Err(ManifestError::InvalidVersion(_))
        ));
    }

    #[test]
    fn highlight_queries_name_a_grammar_a_query_file_and_a_path_inside_the_package() {
        let parse = |query: &str| {
            Manifest::parse(&format!(
                r#"{{ "schema": 1, "id": "acme.q", "version": "0.1.0",
                     "contributes": {{ "highlight_queries": [{query}] }} }}"#
            ))
        };
        let good = r#"{ "id": "acme.q.rust", "grammar": "rust", "file": "highlights.scm",
                         "path": "queries/rust.scm" }"#;
        assert_eq!(parse(good).unwrap().contributes.highlight_queries.len(), 1);
        for bad in [
            good.replace("rust\",", "Rust\","),
            good.replace("highlights.scm", "folds.scm"),
            good.replace("acme.q.rust", "other.rust"),
        ] {
            assert!(parse(&bad).is_err(), "{bad}");
        }
        assert!(matches!(
            parse(&good.replace("queries/rust.scm", "../rust.scm")),
            Err(ManifestError::UnsafePath(_))
        ));
    }
}
