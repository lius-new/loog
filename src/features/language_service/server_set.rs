//! The language servers Loom can run and which languages each one serves.
//!
//! The binding runs from the server to the languages, as in other editors, so a
//! language can have several servers and adding a server never touches a language
//! description. A language is named by its canonical name; a binding that names a
//! language this process does not know is ignored, because the language may come
//! from a package that is not installed.
//!
//! Servers are ordered by `priority` (higher first), then by name. That order is
//! the order in which a language's servers are listed and tried.
use crate::domain::language::{Registry, ResolvedLanguage, is_canonical_name};
use serde_json::{Map, Value};
use std::sync::Arc;

/// How a server finds the project root of a file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RootStrategy {
    /// The nearest folder above the file that holds a marker.
    #[default]
    Nearest,
    /// The outermost such folder, but never above the workspace folder.
    Outermost,
}

/// What to use when no marker folder is found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootFallback {
    /// The workspace folder that holds the file.
    Workspace,
    /// The folder the file is in.
    FileDirectory,
    /// No server for such a file.
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootPolicy {
    /// File names that mark a project folder, such as `Cargo.toml`.
    pub markers: Vec<Box<str>>,
    pub strategy: RootStrategy,
    pub fallback: RootFallback,
}

/// A language, or one variant of it, that a server serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageBinding {
    /// The canonical language name.
    pub language: Box<str>,
    /// Limits the binding to one variant, such as `tsx`; every variant otherwise.
    pub variant: Option<Box<str>>,
    /// The `languageId` the server expects. It is the server's vocabulary, not
    /// Loom's, so it may differ from the names here.
    pub language_id: Box<str>,
}

#[derive(Clone, Debug)]
pub struct ServerDefinition {
    pub name: Arc<str>,
    pub display_name: Box<str>,
    pub command: Box<str>,
    pub args: Vec<Box<str>>,
    pub root: RootPolicy,
    /// Compact JSON object text, if the server wants initialization options.
    pub initialization_options: Option<Box<str>>,
    pub languages: Vec<LanguageBinding>,
    /// Higher is listed and preferred first.
    pub priority: i32,
}

struct RootDescriptor {
    markers: Vec<String>,
    strategy: RootStrategy,
    fallback: RootFallback,
}

struct BindingDescriptor {
    language: String,
    variant: Option<String>,
    language_id: String,
}

struct Descriptor {
    name: String,
    display_name: String,
    command: String,
    args: Vec<String>,
    root: RootDescriptor,
    initialization_options: Option<Value>,
    languages: Vec<BindingDescriptor>,
    priority: i32,
}

/// The fields of a JSON object, rejecting any field not in `allowed` so that a
/// misspelled field is an error rather than silently ignored.
fn fields<'a>(
    value: &'a Value,
    what: &str,
    allowed: &[&str],
) -> Result<&'a Map<String, Value>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{what} must be an object"))?;
    match object.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(key) => Err(format!("unknown field `{key}` in {what}")),
        None => Ok(object),
    }
}

fn text(object: &Map<String, Value>, key: &str, what: &str) -> Result<String, String> {
    match object.get(key) {
        Some(Value::String(text)) => Ok(text.clone()),
        Some(_) => Err(format!("`{key}` in {what} must be text")),
        None => Err(format!("missing field `{key}` in {what}")),
    }
}

fn optional_text(
    object: &Map<String, Value>,
    key: &str,
    what: &str,
) -> Result<Option<String>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(format!("`{key}` in {what} must be text")),
    }
}

fn text_list(object: &Map<String, Value>, key: &str, what: &str) -> Result<Vec<String>, String> {
    match object.get(key) {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => Ok(text.clone()),
                _ => Err(format!("`{key}` in {what} must be a list of text")),
            })
            .collect(),
        Some(_) => Err(format!("`{key}` in {what} must be a list of text")),
    }
}

impl Descriptor {
    /// Reads one definition. Unknown fields, missing required fields and values of
    /// the wrong type are errors; whether the values make sense is `validate`'s job.
    fn parse(value: &Value) -> Result<Self, String> {
        let object = fields(
            value,
            "a server",
            &[
                "name",
                "display_name",
                "command",
                "args",
                "root",
                "initialization_options",
                "languages",
                "priority",
            ],
        )?;
        let root_value = object.get("root").ok_or("missing field `root`")?;
        let root = fields(root_value, "root", &["markers", "strategy", "fallback"])?;
        let strategy = match optional_text(root, "strategy", "root")?.as_deref() {
            None | Some("nearest") => RootStrategy::Nearest,
            Some("outermost") => RootStrategy::Outermost,
            Some(other) => return Err(format!("unknown root strategy `{other}`")),
        };
        let fallback = match text(root, "fallback", "root")?.as_str() {
            "workspace" => RootFallback::Workspace,
            "file_directory" => RootFallback::FileDirectory,
            "none" => RootFallback::None,
            other => return Err(format!("unknown root fallback `{other}`")),
        };
        let bindings = match object.get("languages") {
            Some(Value::Array(items)) => items,
            Some(_) => return Err("`languages` must be a list".into()),
            None => return Err("missing field `languages`".into()),
        };
        let mut languages = Vec::with_capacity(bindings.len());
        for binding in bindings {
            let binding = fields(
                binding,
                "a language binding",
                &["language", "variant", "language_id"],
            )?;
            languages.push(BindingDescriptor {
                language: text(binding, "language", "a language binding")?,
                variant: optional_text(binding, "variant", "a language binding")?,
                language_id: text(binding, "language_id", "a language binding")?,
            });
        }
        let priority = match object.get("priority") {
            None => 0,
            Some(value) => value
                .as_i64()
                .and_then(|priority| i32::try_from(priority).ok())
                .ok_or("`priority` must be a whole number")?,
        };
        Ok(Self {
            name: text(object, "name", "a server")?,
            display_name: text(object, "display_name", "a server")?,
            command: text(object, "command", "a server")?,
            args: text_list(object, "args", "a server")?,
            root: RootDescriptor {
                markers: text_list(root, "markers", "root")?,
                strategy,
                fallback,
            },
            initialization_options: object.get("initialization_options").cloned(),
            languages,
            priority,
        })
    }
}

/// A server name: lowercase words that may be joined by hyphens.
pub fn is_server_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

impl Descriptor {
    fn validate(&self, file_stem: &str) -> Result<(), String> {
        if !is_server_name(&self.name) {
            return Err(format!("`{}` is not a lowercase server name", self.name));
        }
        if self.name != file_stem {
            return Err(format!(
                "name `{}` must match the file name `{file_stem}`",
                self.name
            ));
        }
        if self.display_name.trim().is_empty() || self.display_name != self.display_name.trim() {
            return Err("display_name must be non-empty text without outer spaces".into());
        }
        if self.command.trim().is_empty() || self.command.chars().any(char::is_control) {
            return Err("command must be a program name or path".into());
        }
        if self
            .args
            .iter()
            .any(|arg| arg.chars().any(char::is_control))
        {
            return Err("arguments must not contain control characters".into());
        }
        if self.root.markers.is_empty() {
            return Err("root.markers needs at least one file name".into());
        }
        for marker in &self.root.markers {
            let bad = marker.trim().is_empty()
                || marker
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '/' | '\\'));
            if bad {
                return Err(format!("invalid root marker {marker:?}"));
            }
        }
        if let Some(options) = &self.initialization_options
            && !options.is_object()
        {
            return Err("initialization_options must be a JSON object".into());
        }
        if self.languages.is_empty() {
            return Err("languages needs at least one entry".into());
        }
        for (index, binding) in self.languages.iter().enumerate() {
            if !is_canonical_name(&binding.language) {
                return Err(format!(
                    "language `{}` is not a canonical name",
                    binding.language
                ));
            }
            if let Some(variant) = &binding.variant
                && !is_canonical_name(variant)
            {
                return Err(format!("variant `{variant}` is not a canonical name"));
            }
            let id = &binding.language_id;
            if id.is_empty() || id.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(format!("language_id {id:?} must be a plain word"));
            }
            if self.languages[..index]
                .iter()
                .any(|other| other.language == binding.language && other.variant == binding.variant)
            {
                return Err(format!(
                    "language `{}` is bound twice",
                    match &binding.variant {
                        Some(variant) => format!("{}/{variant}", binding.language),
                        None => binding.language.clone(),
                    }
                ));
            }
        }
        Ok(())
    }

    fn into_definition(self) -> ServerDefinition {
        ServerDefinition {
            name: self.name.as_str().into(),
            display_name: self.display_name.into(),
            command: self.command.into(),
            args: self.args.into_iter().map(Into::into).collect(),
            root: RootPolicy {
                markers: self.root.markers.into_iter().map(Into::into).collect(),
                strategy: self.root.strategy,
                fallback: self.root.fallback,
            },
            initialization_options: self
                .initialization_options
                .map(|options| options.to_string().into()),
            languages: self
                .languages
                .into_iter()
                .map(|binding| LanguageBinding {
                    language: binding.language.into(),
                    variant: binding.variant.map(Into::into),
                    language_id: binding.language_id.into(),
                })
                .collect(),
            priority: self.priority,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ServerSet {
    /// In listing order: priority descending, then name.
    servers: Vec<ServerDefinition>,
}

impl ServerSet {
    /// The servers Loom itself knows: none. Loom ships no server definitions; they come
    /// from extensions and from the person's own settings.
    pub fn current() -> Arc<ServerSet> {
        Arc::default()
    }

    /// `sources` are `(name, definition JSON)` pairs. Parsing is strict: an
    /// unknown or misspelled field, a repeated name or a malformed value is an error.
    pub fn from_sources(sources: &[(&str, &str)]) -> Result<Self, String> {
        let mut parsed = Vec::with_capacity(sources.len());
        for (stem, json) in sources {
            let value: Value =
                serde_json::from_str(json).map_err(|error| format!("server `{stem}`: {error}"))?;
            let descriptor =
                Descriptor::parse(&value).map_err(|error| format!("server `{stem}`: {error}"))?;
            parsed.push(((*stem).to_owned(), descriptor));
        }
        Self::assemble(parsed)
    }

    fn assemble(parsed: Vec<(String, Descriptor)>) -> Result<Self, String> {
        let mut servers = Vec::with_capacity(parsed.len());
        for (stem, descriptor) in parsed {
            descriptor
                .validate(&stem)
                .map_err(|error| format!("server `{stem}`: {error}"))?;
            servers.push(descriptor.into_definition());
        }
        servers.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.name.cmp(&b.name))
        });
        if let Some(pair) = servers.windows(2).find(|pair| pair[0].name == pair[1].name) {
            return Err(format!("server `{}` is defined twice", pair[0].name));
        }
        Ok(Self { servers })
    }

    /// This set with the servers of `extra` (name and definition JSON) added.
    /// A definition that is not valid, or has the name of one already here, is
    /// left out and reported as `(name, reason)`.
    pub fn extended(&self, extra: &[(String, Value)]) -> (ServerSet, Vec<(String, String)>) {
        let mut combined = self.clone();
        let mut problems = Vec::new();
        for (name, json) in extra {
            if combined.get(name).is_some() {
                problems.push((name.clone(), "a server of that name exists".to_owned()));
                continue;
            }
            match Descriptor::parse(json).and_then(|descriptor| {
                descriptor.validate(name)?;
                Ok(descriptor.into_definition())
            }) {
                Ok(definition) => combined.servers.push(definition),
                Err(error) => problems.push((name.clone(), error)),
            }
        }
        combined.servers.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.name.cmp(&b.name))
        });
        (combined, problems)
    }

    /// Every server, in listing order.
    pub fn all(&self) -> &[ServerDefinition] {
        &self.servers
    }

    pub fn get(&self, name: &str) -> Option<&ServerDefinition> {
        self.servers.iter().find(|server| &*server.name == name)
    }

    /// Where `name` stands in listing order; a lower number is preferred.
    pub fn rank(&self, name: &str) -> Option<usize> {
        self.servers.iter().position(|server| &*server.name == name)
    }

    /// The servers that serve `resolved`, in listing order, each with the
    /// `languageId` to send. A binding for the exact variant wins over one for the
    /// whole language.
    pub fn for_language<'a>(
        &'a self,
        registry: &Registry,
        resolved: ResolvedLanguage,
    ) -> Vec<(&'a ServerDefinition, &'a str)> {
        let language = &registry.definition_of(resolved).name;
        let variant = registry.variant_name(resolved);
        self.servers
            .iter()
            .filter_map(|server| {
                let mut general = None;
                for binding in &server.languages {
                    if binding.language != *language {
                        continue;
                    }
                    match binding.variant.as_deref() {
                        Some(name) if name == variant => {
                            return Some((server, &*binding.language_id));
                        }
                        None => general = Some(&*binding.language_id),
                        Some(_) => {}
                    }
                }
                general.map(|id| (server, id))
            })
            .collect()
    }
}

/// A `rust-analyzer` server that runs `command`, for tests that start a stand-in.
#[cfg(test)]
pub(crate) fn rust_analyzer_running(command: &str) -> Arc<ServerSet> {
    let definition = serde_json::json!({
        "name": "rust-analyzer", "display_name": "rust-analyzer",
        "command": command,
        "root": { "markers": ["Cargo.toml", "rust-project.json"],
                  "strategy": "outermost", "fallback": "file_directory" },
        "languages": [{ "language": "rust", "language_id": "rust" }] })
    .to_string();
    Arc::new(ServerSet::from_sources(&[("rust-analyzer", &definition)]).unwrap())
}

/// A server like rust-analyzer, for tests that need one. Loom ships no definitions,
/// so tests supply their own.
#[cfg(test)]
pub(crate) fn rust_analyzer_for_tests() -> Arc<ServerSet> {
    use std::sync::OnceLock;
    static SET: OnceLock<Arc<ServerSet>> = OnceLock::new();
    SET.get_or_init(|| {
        let definition = r#"{
            "name": "rust-analyzer", "display_name": "rust-analyzer",
            "command": "rust-analyzer",
            "root": { "markers": ["Cargo.toml", "rust-project.json"],
                      "strategy": "outermost", "fallback": "file_directory" },
            "languages": [{ "language": "rust", "language_id": "rust" }] }"#;
        Arc::new(ServerSet::from_sources(&[("rust-analyzer", definition)]).unwrap())
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn server(name: &str, languages: &str, extra: &str) -> String {
        format!(
            r#"{{ "name": "{name}", "display_name": "{name}", "command": "{name}-bin",
                 "root": {{ "markers": ["marker.toml"], "fallback": "workspace" }},
                 "languages": {languages} {extra} }}"#
        )
    }

    fn build(servers: &[(&str, String)]) -> Result<ServerSet, String> {
        let sources: Vec<(&str, &str)> = servers.iter().map(|(n, j)| (*n, j.as_str())).collect();
        ServerSet::from_sources(&sources)
    }

    fn names(set: &ServerSet, path: &str) -> Vec<(String, String)> {
        let registry = Registry::builtin();
        set.for_language(registry, registry.resolve_path(Path::new(path)))
            .into_iter()
            .map(|(server, id)| (server.name.to_string(), id.to_owned()))
            .collect()
    }

    #[test]
    fn servers_are_listed_by_priority_then_name() {
        let rust = r#"[{ "language": "rust", "language_id": "rust" }]"#;
        let set = build(&[
            ("zeta", server("zeta", rust, "")),
            ("alpha", server("alpha", rust, "")),
            ("urgent", server("urgent", rust, r#", "priority": 10"#)),
            ("late", server("late", rust, r#", "priority": -1"#)),
        ])
        .unwrap();
        let order: Vec<_> = set.all().iter().map(|s| &*s.name).collect();
        assert_eq!(order, ["urgent", "alpha", "zeta", "late"]);
        assert_eq!(set.rank("urgent"), Some(0));
        assert_eq!(set.rank("late"), Some(3));
        assert_eq!(set.rank("missing"), None);
        // A language lists its servers in that order.
        let listed: Vec<_> = names(&set, "a.rs").into_iter().map(|(n, _)| n).collect();
        assert_eq!(listed, ["urgent", "alpha", "zeta", "late"]);
    }

    #[test]
    fn the_server_names_its_own_language_ids_and_a_variant_wins_over_the_language() {
        let set = build(&[(
            "web",
            server(
                "web",
                r#"[{ "language": "typescript", "language_id": "typescript" },
                    { "language": "typescript", "variant": "tsx", "language_id": "typescriptreact" },
                    { "language": "rust", "language_id": "rs-dialect" }]"#,
                "",
            ),
        )])
        .unwrap();
        assert_eq!(names(&set, "a.ts"), [("web".into(), "typescript".into())]);
        assert_eq!(
            names(&set, "a.tsx"),
            [("web".into(), "typescriptreact".into())]
        );
        assert_eq!(names(&set, "a.rs"), [("web".into(), "rs-dialect".into())]);
        assert!(names(&set, "a.txt").is_empty());
    }

    #[test]
    fn a_variant_only_binding_leaves_the_other_variants_unserved() {
        let set = build(&[(
            "jsx",
            server(
                "jsx",
                r#"[{ "language": "typescript", "variant": "tsx", "language_id": "tsx" }]"#,
                "",
            ),
        )])
        .unwrap();
        assert_eq!(names(&set, "a.tsx").len(), 1);
        assert!(names(&set, "a.ts").is_empty());
    }

    #[test]
    fn a_language_may_have_several_servers_and_one_server_several_languages() {
        let set = build(&[
            (
                "first",
                server(
                    "first",
                    r#"[{ "language": "rust", "language_id": "rust" },
                        { "language": "typescript", "language_id": "ts" }]"#,
                    "",
                ),
            ),
            (
                "second",
                server(
                    "second",
                    r#"[{ "language": "rust", "language_id": "rust-2" }]"#,
                    "",
                ),
            ),
        ])
        .unwrap();
        assert_eq!(
            names(&set, "a.rs"),
            [
                ("first".into(), "rust".into()),
                ("second".into(), "rust-2".into())
            ]
        );
        assert_eq!(names(&set, "a.ts"), [("first".into(), "ts".into())]);
    }

    #[test]
    fn a_binding_to_an_unknown_language_is_ignored() {
        let set = build(&[(
            "future",
            server(
                "future",
                r#"[{ "language": "nolang", "language_id": "x" },
                    { "language": "rust", "language_id": "rust" }]"#,
                "",
            ),
        )])
        .unwrap();
        assert_eq!(names(&set, "a.rs").len(), 1);
    }

    #[test]
    fn definitions_keep_their_launch_data() {
        let set = build(&[(
            "alpha-server",
            server(
                "alpha-server",
                r#"[{ "language": "rust", "language_id": "rust" }]"#,
                r#", "args": ["--stdio"], "initialization_options": { "k": 1 }"#,
            ),
        )])
        .unwrap();
        let alpha = set.get("alpha-server").unwrap();
        assert_eq!(&*alpha.command, "alpha-server-bin");
        let args: Vec<_> = alpha.args.iter().map(|a| &**a).collect();
        assert_eq!(args, ["--stdio"]);
        assert_eq!(alpha.initialization_options.as_deref(), Some(r#"{"k":1}"#));
        assert_eq!(alpha.root.fallback, RootFallback::Workspace);
        assert_eq!(alpha.root.strategy, RootStrategy::Nearest);
        assert!(set.get("beta").is_none());
    }

    #[test]
    fn loom_ships_no_server_definitions() {
        assert!(ServerSet::current().all().is_empty());
        let registry = Registry::builtin();
        let rust = registry.resolve_path(Path::new("a.rs"));
        assert!(ServerSet::current().for_language(registry, rust).is_empty());
    }

    #[test]
    fn a_rust_server_and_its_root_policy_are_data() {
        let set = rust_analyzer_for_tests();
        assert_eq!(
            names(&set, "a.rs"),
            [("rust-analyzer".into(), "rust".into())]
        );
        let server = set.get("rust-analyzer").unwrap();
        assert_eq!(&*server.command, "rust-analyzer");
        let markers: Vec<_> = server.root.markers.iter().map(|m| &**m).collect();
        assert_eq!(markers, ["Cargo.toml", "rust-project.json"]);
        assert_eq!(server.root.strategy, RootStrategy::Outermost);
        assert_eq!(server.root.fallback, RootFallback::FileDirectory);
        for other in ["a.txt", "a.cs", "a.js", "a.ts", "a.tsx"] {
            assert!(names(&set, other).is_empty(), "{other}");
        }
    }

    #[test]
    fn problems_are_reported_with_a_reason() {
        let one = r#"[{ "language": "rust", "language_id": "rust" }]"#;
        let cases: Vec<(&str, Result<ServerSet, String>)> = vec![
            (
                "defined twice",
                build(&[
                    ("dup", server("dup", one, "")),
                    ("dup", server("dup", one, "")),
                ]),
            ),
            (
                "must match the file name",
                build(&[("other", server("dup", one, ""))]),
            ),
            (
                "root.markers needs",
                build(&[(
                    "bad",
                    format!(
                        r#"{{ "name": "bad", "display_name": "bad", "command": "x",
                             "root": {{ "markers": [], "fallback": "none" }},
                             "languages": {one} }}"#
                    ),
                )]),
            ),
            (
                "initialization_options must be a JSON object",
                build(&[(
                    "bad",
                    server("bad", one, r#", "initialization_options": [1]"#),
                )]),
            ),
            (
                "unknown field",
                build(&[("bad", server("bad", one, r#", "port": 1"#))]),
            ),
            (
                "languages needs",
                build(&[("bad", server("bad", "[]", ""))]),
            ),
            (
                "not a canonical name",
                build(&[(
                    "bad",
                    server("bad", r#"[{ "language": "Rust", "language_id": "x" }]"#, ""),
                )]),
            ),
            (
                "plain word",
                build(&[(
                    "bad",
                    server(
                        "bad",
                        r#"[{ "language": "rust", "language_id": "two words" }]"#,
                        "",
                    ),
                )]),
            ),
            (
                "is bound twice",
                build(&[(
                    "bad",
                    server(
                        "bad",
                        r#"[{ "language": "rust", "language_id": "a" },
                            { "language": "rust", "language_id": "b" }]"#,
                        "",
                    ),
                )]),
            ),
            (
                "is not a lowercase server name",
                build(&[("Bad_Name", server("Bad_Name", one, ""))]),
            ),
        ];
        for (expected, result) in cases {
            let error = result.unwrap_err();
            assert!(
                error.contains(expected),
                "expected {expected:?} in {error:?}"
            );
        }
    }
}
