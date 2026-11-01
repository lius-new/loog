//! The `language_servers`, `languages` and `lsp` settings.
//!
//! `language_servers` is an object keyed by server name. A name an extension
//! declares is an *extension's* server: only how it behaves can be changed (turning
//! it off, its arguments, options, settings and environment); the program it runs
//! cannot. Any other name registers a server of the person's own,
//! with a full definition whose `command` is an absolute path. Loom never looks a
//! program up on `PATH`.
//!
//! Only the user's `settings.json` may hold these. A project file that could choose
//! which program Loom runs would turn opening a folder into running its code.
use crate::features::language_service::server_set::{ServerDefinition, ServerSet, is_server_name};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// A language a registered server serves.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageBindingSetting {
    pub language: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    pub language_id: String,
}

/// How a registered server finds its project folder.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RootSetting {
    pub markers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
}

/// One server's entry. The name is the key it is stored under.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageServerSetting {
    /// The server's name; set from the key, never written inside the entry.
    #[serde(skip)]
    pub server: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
    /// The program of a registered server: an absolute path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Replaces the definition's arguments, including with an empty list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    /// Replaces the definition's `initializationOptions`; must be an object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initialization_options: Option<Value>,
    /// What the server gets when it asks for its configuration
    /// (`workspace/configuration`); changing it does not restart the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<Value>,
    /// Environment variables added to the server's process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub languages: Option<Vec<LanguageBindingSetting>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<RootSetting>,
    /// Higher is listed and preferred first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
}

fn enabled() -> bool {
    true
}

impl LanguageServerSetting {
    pub fn named(server: &str) -> Self {
        Self {
            server: server.to_owned(),
            enabled: true,
            command: None,
            args: None,
            initialization_options: None,
            settings: None,
            env: None,
            display_name: None,
            languages: None,
            root: None,
            priority: None,
        }
    }

    /// Why this entry cannot be used, if it cannot. `catalog` is the set of
    /// catalogued servers; any other name is a registration.
    pub fn validate(&self, catalog: &ServerSet) -> Result<(), String> {
        if self
            .args
            .as_ref()
            .is_some_and(|a| a.iter().any(|x| x.contains('\0')))
        {
            return Err("args contain a NUL character".to_owned());
        }
        if self
            .initialization_options
            .as_ref()
            .is_some_and(|o| !o.is_object())
        {
            return Err("initialization_options must be an object".to_owned());
        }
        if self.settings.as_ref().is_some_and(|s| !s.is_object()) {
            return Err("settings must be an object".to_owned());
        }
        if let Some(env) = &self.env {
            for (key, value) in env {
                if key.is_empty() || key.contains(['=', '\0']) || value.contains('\0') {
                    return Err(format!("env variable {key:?} is not usable"));
                }
            }
        }
        if catalog.get(&self.server).is_some() {
            for (field, set) in [
                ("command", self.command.is_some()),
                ("display_name", self.display_name.is_some()),
                ("languages", self.languages.is_some()),
                ("root", self.root.is_some()),
            ] {
                if set {
                    return Err(format!(
                        "`{field}` of a catalogued server cannot be changed; \
                         register another server under a new name"
                    ));
                }
            }
            return Ok(());
        }
        // A registration: build its definition and let the strict reader judge it.
        self.definition().map(|_| ())
    }

    /// The definition of a registered server, as the strict reader takes it.
    pub fn definition(&self) -> Result<Value, String> {
        if !is_server_name(&self.server) {
            return Err(format!(
                "\"{}\" is not a server of the catalog, and is not a name a server can be registered under (lowercase words joined by - or _)",
                self.server
            ));
        }
        let command = self
            .command
            .as_deref()
            .ok_or("a server of your own needs a `command`")?;
        if !std::path::Path::new(command).is_absolute() {
            return Err("`command` must be an absolute path: Loom does not search PATH".to_owned());
        }
        let languages = self
            .languages
            .as_ref()
            .ok_or("a server of your own needs `languages`")?;
        let root = self
            .root
            .as_ref()
            .ok_or("a server of your own needs a `root`")?;
        let mut definition = json!({
            "name": self.server,
            "display_name": self.display_name.clone().unwrap_or_else(|| self.server.clone()),
            "command": command,
            "args": self.args.clone().unwrap_or_default(),
            "root": {
                "markers": root.markers,
                "strategy": root.strategy.clone().unwrap_or_else(|| "nearest".to_owned()),
                "fallback": root.fallback.clone().unwrap_or_else(|| "workspace".to_owned()),
            },
            "languages": languages.iter().map(|l| {
                let mut binding = json!({ "language": l.language, "language_id": l.language_id });
                if let Some(variant) = &l.variant {
                    binding["variant"] = json!(variant);
                }
                binding
            }).collect::<Vec<_>>(),
            "priority": self.priority.unwrap_or(0),
        });
        if let Some(options) = &self.initialization_options {
            definition["initialization_options"] = options.clone();
        }
        // The strict reader is the one place that decides what a definition may be.
        ServerSet::from_sources(&[(&self.server, &definition.to_string())])?;
        Ok(definition)
    }
}

/// The definitions of the registered servers among `settings` that are valid,
/// and a message for each that is not.
pub fn registered(
    settings: &[LanguageServerSetting],
    catalog: &ServerSet,
) -> (Vec<(String, Value)>, Vec<String>) {
    let mut found = Vec::new();
    let mut problems = Vec::new();
    for entry in settings.iter().filter(|e| catalog.get(&e.server).is_none()) {
        match entry.definition() {
            Ok(definition) => found.push((entry.server.clone(), definition)),
            Err(error) => problems.push(format!("{}: {error}", entry.server)),
        }
    }
    (found, problems)
}

/// What the server gets for `workspace/configuration`.
pub fn server_settings(settings: &[LanguageServerSetting], server: &str) -> Value {
    settings
        .iter()
        .rev()
        .find(|entry| entry.server == server)
        .and_then(|entry| entry.settings.clone())
        .unwrap_or_else(|| json!({}))
}

/// What actually runs for one server once the definition and the person's entry
/// are combined. Two equal values start the same process the same way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerLaunch {
    pub command: String,
    pub args: Vec<String>,
    /// Compact JSON object text.
    pub initialization_options: Option<String>,
    /// Added to the process's environment.
    pub env: BTreeMap<String, String>,
}

/// The launch for `base`, or `None` when the person turned it off.
pub fn launch_for(
    base: &ServerDefinition,
    settings: &[LanguageServerSetting],
) -> Option<ServerLaunch> {
    // The last entry for a server wins, like a later key in a settings file.
    let over = settings.iter().rev().find(|s| s.server == *base.name);
    if over.is_some_and(|s| !s.enabled) {
        return None;
    }
    Some(ServerLaunch {
        command: base.command.to_string(),
        args: over
            .and_then(|s| s.args.clone())
            .unwrap_or_else(|| base.args.iter().map(|a| a.to_string()).collect()),
        initialization_options: match over.and_then(|s| s.initialization_options.as_ref()) {
            Some(options) => Some(options.to_string()),
            None => base.initialization_options.as_deref().map(str::to_owned),
        },
        env: over.and_then(|s| s.env.clone()).unwrap_or_default(),
    })
}

/// A per-language entry of the `languages` setting.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LanguageSetting {
    /// Which servers serve the language and in what order; `"..."` stands for
    /// the servers not named, in their usual order. Empty means all, usual order.
    pub language_servers: Vec<String>,
    /// The server that formats; by default the first that can.
    pub formatter: Option<String>,
    pub format_on_save: bool,
    /// Code action kinds applied when saving, such as `source.organizeImports`.
    pub code_actions_on_save: Vec<String>,
}

impl LanguageSetting {
    /// Puts `servers` (names in the usual order) in the order this entry asks
    /// for, leaving out the ones it does not name.
    pub fn arrange<'a>(&self, servers: Vec<&'a str>) -> Vec<&'a str> {
        if self.language_servers.is_empty() {
            return servers;
        }
        let mut arranged: Vec<&str> = Vec::new();
        for wanted in &self.language_servers {
            if wanted == "..." {
                for name in &servers {
                    if !self.language_servers.iter().any(|w| w == name) && !arranged.contains(name)
                    {
                        arranged.push(name);
                    }
                }
            } else if let Some(name) = servers.iter().find(|name| **name == wanted) {
                if !arranged.contains(name) {
                    arranged.push(name);
                }
            }
        }
        arranged
    }
}

/// How completion behaves.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CompletionSettings {
    /// Open the list while typing; off means only on the completion key.
    pub auto: bool,
    /// `enter_and_tab`, `enter` or `tab`.
    pub accept_on: String,
    /// Offer the words of the file when no server has anything to say.
    pub words_fallback: bool,
}

impl Default for CompletionSettings {
    fn default() -> Self {
        Self {
            auto: true,
            accept_on: "enter_and_tab".to_owned(),
            words_fallback: true,
        }
    }
}

/// The `lsp` setting: behaviour common to all servers.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LspSettings {
    /// How long the pointer rests on a word before it is asked about.
    pub hover_delay_ms: u64,
    pub completion: CompletionSettings,
    pub inlay_hints: bool,
    /// `combined` draws the servers' semantic tokens over the syntax colours;
    /// `off` leaves them out.
    pub semantic_highlighting: String,
    /// Larger documents are not sent to servers.
    pub max_document_bytes: usize,
}

impl Default for LspSettings {
    fn default() -> Self {
        Self {
            hover_delay_ms: 500,
            completion: CompletionSettings::default(),
            inlay_hints: false,
            semantic_highlighting: "combined".to_owned(),
            max_document_bytes: 8 * 1024 * 1024,
        }
    }
}

impl LspSettings {
    /// Why the values cannot be used, if they cannot.
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(
            self.completion.accept_on.as_str(),
            "enter_and_tab" | "enter" | "tab"
        ) {
            return Err("lsp.completion.accept_on must be enter_and_tab, enter or tab".to_owned());
        }
        if !matches!(self.semantic_highlighting.as_str(), "combined" | "off") {
            return Err("lsp.semantic_highlighting must be combined or off".to_owned());
        }
        if self.hover_delay_ms > 10_000 {
            return Err("lsp.hover_delay_ms must be at most 10000".to_owned());
        }
        if self.max_document_bytes < 1024 {
            return Err("lsp.max_document_bytes must be at least 1024".to_owned());
        }
        Ok(())
    }
}

/// `language_servers` as an object keyed by server name, held as a list.
pub mod as_object {
    use super::LanguageServerSetting;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<S: Serializer>(
        entries: &[LanguageServerSetting],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let map: BTreeMap<&str, &LanguageServerSetting> =
            entries.iter().map(|e| (e.server.as_str(), e)).collect();
        map.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<LanguageServerSetting>, D::Error> {
        let map = BTreeMap::<String, LanguageServerSetting>::deserialize(deserializer)?;
        Ok(map
            .into_iter()
            .map(|(name, mut entry)| {
                entry.server = name;
                entry
            })
            .collect())
    }
}
