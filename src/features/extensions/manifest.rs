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
}

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
    #[serde(default)]
    pub commands: Vec<CommandDecl>,
    #[serde(default)]
    pub providers: Vec<ProviderDecl>,
    #[serde(default)]
    pub themes: Vec<ResourceDecl>,
    #[serde(default)]
    pub languages: Vec<ResourceDecl>,
    #[serde(default)]
    pub snippets: Vec<ResourceDecl>,
    #[serde(default)]
    pub language_servers: Vec<LanguageServerDecl>,
    #[serde(default)]
    pub settings: Vec<SettingDecl>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
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

/// Declarative language server integration. Parsing never starts a program.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageServerDecl {
    pub id: String,
    pub languages: Vec<String>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// File names that mark a project root (for example `Cargo.toml`).
    pub root_markers: Vec<String>,
    #[serde(default)]
    pub initialization_options: Option<serde_json::Value>,
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
    /// A network domain is not a plain host name pattern, or is declared without the capability.
    BadDomain(String),
    /// The server declaration is not one the language service accepts.
    BadLanguageServer {
        id: String,
        reason: String,
    },
    /// A server is a program the machine runs, so the package must ask for that.
    ServerNeedsExecution(String),
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
            Self::BadLanguageServer { id, reason } => {
                write!(f, "language server `{id}`: {reason}")
            }
            Self::ServerNeedsExecution(id) => write!(
                f,
                "language server `{id}` runs a program, so the package must declare `process.execute-local`"
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
        self.validate_contributions()?;
        self.validate_activation()
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
            if !c.providers.is_empty() {
                return Err(ManifestError::BehaviourWithoutComponent("providers"));
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
        for provider in &c.providers {
            self.namespaced(&provider.id)?;
            unique(&provider.id)?;
            if provider.selector.languages.is_empty() {
                return Err(ManifestError::EmptySelector(provider.id.clone()));
            }
        }
        for resource in c.themes.iter().chain(&c.languages).chain(&c.snippets) {
            self.namespaced(&resource.id)?;
            unique(&resource.id)?;
            check_package_path(&resource.path)?;
        }
        for setting in &c.settings {
            self.namespaced(&setting.key)?;
            unique(&setting.key)?;
            if !super::settings::matches_kind(&setting.default, setting.kind) {
                return Err(ManifestError::BadSettingDefault(setting.key.clone()));
            }
        }
        for server in &c.language_servers {
            self.namespaced(&server.id)?;
            unique(&server.id)?;
            if server.languages.is_empty() {
                return Err(ManifestError::EmptySelector(server.id.clone()));
            }
            // Declaring a server is data, but starting it is local execution.
            if !self
                .capabilities
                .iter()
                .any(|c| c == "process.execute-local")
            {
                return Err(ManifestError::ServerNeedsExecution(server.id.clone()));
            }
            // Checked with the rules the language service itself applies, so a
            // declaration that could never start is rejected when the package
            // loads. Nothing is started here (design §6.6).
            super::servers::validate(server).map_err(|reason| {
                ManifestError::BadLanguageServer {
                    id: server.id.clone(),
                    reason,
                }
            })?;
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
fn valid_id(id: &str) -> bool {
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
    fn language_server_declarations_need_the_execution_capability() {
        let source = r#"{
            "schema": 1, "id": "acme.rust", "version": "1.0.0",
            "capabilities": ["process.execute-local"],
            "contributes": { "language_servers": [
                { "id": "acme.rust.analyzer", "languages": ["rust"], "command": "rust-analyzer", "args": ["--stdio"], "root_markers": ["Cargo.toml"] }
            ] }
        }"#;
        let manifest = Manifest::parse(source).unwrap();
        assert_eq!(
            manifest.contributes.language_servers[0].command,
            "rust-analyzer"
        );
        // Without the capability the declaration could never be allowed to run.
        let without = source.replace("\"capabilities\": [\"process.execute-local\"],", "");
        assert!(matches!(
            Manifest::parse(&without),
            Err(ManifestError::ServerNeedsExecution(_))
        ));
    }

    #[test]
    fn provider_selectors_must_name_a_language() {
        let source = SELECTION_TOOLS.replace(
            "\"commands\": [",
            "\"providers\": [{ \"id\": \"example.selection-tools.diag\", \"kind\": \"diagnostics\", \"selector\": { \"languages\": [] } }], \"commands\": [",
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
}
