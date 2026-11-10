//! The set of known languages: ids, display data and how files are recognised.
//! Built from descriptions, never from per-language code.
use crate::domain::language::descriptor::{
    Comments, Descriptor, Highlight, normalize_extension, normalize_file_name, normalize_suffix,
};
use crate::domain::language::{LanguageId, ResolvedLanguage};
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, LazyLock, RwLock};

static CURRENT: LazyLock<RwLock<Arc<Registry>>> = LazyLock::new(|| {
    let registry = crate::domain::language::embedded::registry()
        .unwrap_or_else(|error| panic!("invalid built-in language resources: {error}"));
    RwLock::new(Arc::new(registry))
});

/// The language every unrecognised file gets. A registry must define it.
pub const PLAIN_TEXT: &str = "plaintext";

#[derive(Debug, PartialEq, Eq)]
pub struct RegistryError(String);

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RegistryError {}

fn fail<T>(message: impl Into<String>) -> Result<T, RegistryError> {
    Err(RegistryError(message.into()))
}

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct LanguageVariant {
    pub name: Box<str>,
    pub grammar: Option<Arc<str>>,
}

#[derive(Debug)]
pub struct LanguageDefinition {
    /// Stable name for settings and sessions; ids are not.
    pub name: Arc<str>,
    pub display_name: Box<str>,
    pub badge: Box<str>,
    pub comments: Comments,
    /// Rules for colouring the language when no grammar does.
    pub highlight: Highlight,
    /// Never empty; the first one is the default.
    pub variants: Vec<LanguageVariant>,
}

#[derive(Debug)]
pub struct Registry {
    languages: Vec<Arc<LanguageDefinition>>,
    by_name: HashMap<Box<str>, LanguageId>,
    grammars: Vec<Arc<str>>,
    by_extension: HashMap<Box<str>, (LanguageId, usize)>,
    by_file_name: HashMap<Box<str>, (LanguageId, usize)>,
    by_suffix: HashMap<Box<str>, (LanguageId, usize)>,
    plain_text: LanguageId,
}

impl Registry {
    /// `sources` are `(file stem, description JSON)` pairs and `grammars` the
    /// names of the available grammar resources. Ids come from sorted names, so
    /// the same inputs always give the same ids whatever order they arrive in.
    pub fn from_sources(
        sources: &[(&str, &str)],
        grammars: &[&str],
    ) -> Result<Self, RegistryError> {
        let mut parsed = Vec::with_capacity(sources.len());
        for (stem, json) in sources {
            let descriptor = Descriptor::parse(json)
                .and_then(|descriptor| descriptor.validate(stem).map(|()| descriptor))
                .or_else(|error| fail(format!("language `{stem}`: {error}")))?;
            parsed.push(descriptor);
        }
        parsed.sort_by(|a, b| a.name.cmp(&b.name));
        if let Some(pair) = parsed.windows(2).find(|pair| pair[0].name == pair[1].name) {
            return fail(format!("language `{}` is defined twice", pair[0].name));
        }

        let mut grammar_names: Vec<Arc<str>> = grammars.iter().map(|name| (*name).into()).collect();
        grammar_names.sort();
        grammar_names.dedup();
        let mut languages = Vec::with_capacity(parsed.len());
        let mut by_name: HashMap<Box<str>, LanguageId> = HashMap::new();
        for (index, descriptor) in parsed.iter().enumerate() {
            let id = LanguageId(index as u32);
            for name in std::iter::once(&descriptor.name).chain(&descriptor.aliases) {
                if let Some(other) = by_name.insert(name.as_str().into(), id) {
                    return fail(format!(
                        "name `{name}` is claimed by `{}` and `{}`",
                        parsed[other.index()].name,
                        descriptor.name
                    ));
                }
            }
            let mut language_variants = Vec::new();
            for variant in &descriptor.variants {
                let grammar = match &variant.grammar {
                    None => None,
                    Some(name) => match grammar_names.binary_search_by(|known| (**known).cmp(name))
                    {
                        Ok(index) => Some(grammar_names[index].clone()),
                        Err(_) => {
                            return fail(format!(
                                "language `{}` variant `{}` names unknown grammar `{name}`",
                                descriptor.name, variant.id
                            ));
                        }
                    },
                };
                language_variants.push(LanguageVariant {
                    name: variant.id.as_str().into(),
                    grammar,
                });
            }
            languages.push(Arc::new(LanguageDefinition {
                name: descriptor.name.as_str().into(),
                display_name: descriptor.display_name.as_str().into(),
                badge: descriptor.badge.as_str().into(),
                comments: descriptor.comments.clone(),
                highlight: descriptor.highlight.clone(),
                variants: language_variants,
            }));
        }

        let mut by_extension: HashMap<Box<str>, (LanguageId, usize)> = HashMap::new();
        let mut by_file_name: HashMap<Box<str>, (LanguageId, usize)> = HashMap::new();
        let mut by_suffix = HashMap::new();
        for (index, descriptor) in parsed.iter().enumerate() {
            let language = LanguageId(index as u32);
            for rule in &descriptor.files {
                let position = descriptor
                    .variants
                    .iter()
                    .position(|v| v.id == rule.variant)
                    .expect("validated: the rule's variant exists");
                for suffix in &rule.suffixes {
                    let key = normalize_suffix(suffix).map_err(RegistryError)?;
                    let claim = (language, position);
                    if by_suffix
                        .insert(key.clone(), claim)
                        .is_some_and(|old| old != claim)
                    {
                        return fail(format!(
                            "file suffix `{key}` is claimed by multiple languages"
                        ));
                    }
                }
                let claims = rule
                    .extensions
                    .iter()
                    .map(|e| normalize_extension(e).map(|key| (key, true)))
                    .chain(
                        rule.names
                            .iter()
                            .map(|n| normalize_file_name(n).map(|key| (key, false))),
                    );
                for claim in claims {
                    let (key, is_extension) = claim.or_else(|error| {
                        fail(format!("language `{}`: {error}", descriptor.name))
                    })?;
                    let table = if is_extension {
                        &mut by_extension
                    } else {
                        &mut by_file_name
                    };
                    if let Some((other, _)) =
                        table.insert(key.as_str().into(), (language, position))
                    {
                        return fail(format!(
                            "{} `{key}` is claimed by `{}` and `{}`",
                            if is_extension {
                                "extension"
                            } else {
                                "file name"
                            },
                            parsed[other.index()].name,
                            descriptor.name
                        ));
                    }
                }
            }
        }

        let Some(&plain_text) = by_name.get(PLAIN_TEXT) else {
            return fail(format!("the `{PLAIN_TEXT}` language is required"));
        };
        for (suffix, claim) in &by_suffix {
            if by_extension
                .get(suffix.as_str())
                .or_else(|| by_file_name.get(suffix.as_str()))
                .is_some_and(|old| old != claim)
            {
                return fail(format!(
                    "file suffix `{suffix}` conflicts with an existing file rule"
                ));
            }
        }
        let by_suffix = by_suffix
            .into_iter()
            .map(|(key, value)| (key.into_boxed_str(), value))
            .collect();
        Ok(Self {
            languages,
            by_name,
            grammars: grammar_names,
            by_extension,
            by_file_name,
            by_suffix,
            plain_text,
        })
    }

    /// Snapshot of the live catalog. Previously resolved languages own their metadata.
    pub fn builtin() -> Arc<Registry> {
        CURRENT.read().unwrap().clone()
    }

    pub(crate) fn install(registry: Registry) {
        *CURRENT.write().unwrap() = Arc::new(registry);
    }

    pub fn grammar_by_name(&self, name: &str) -> Option<Arc<str>> {
        let index = self
            .grammars
            .binary_search_by(|known| (**known).cmp(name))
            .ok()?;
        Some(self.grammars[index].clone())
    }

    pub fn languages(&self) -> &[Arc<LanguageDefinition>] {
        &self.languages
    }

    pub fn definition(&self, id: LanguageId) -> &LanguageDefinition {
        &self.languages[id.index()]
    }

    /// A language by canonical name or alias.
    pub fn language_by_name(&self, name: &str) -> Option<LanguageId> {
        self.by_name.get(name).copied()
    }

    /// The language's default variant, the first one its description lists.
    pub fn default_resolved(&self, language: LanguageId) -> ResolvedLanguage {
        self.resolved(language, 0)
    }

    /// A named variant of a language, for example `tsx` of `typescript`.
    pub fn resolve_variant(&self, language: LanguageId, variant: &str) -> Option<ResolvedLanguage> {
        let position = self
            .definition(language)
            .variants
            .iter()
            .position(|v| &*v.name == variant)?;
        Some(self.resolved(language, position))
    }

    pub fn plain_text(&self) -> ResolvedLanguage {
        self.default_resolved(self.plain_text)
    }

    /// Exact names, longest standard dotted suffix, extension, then plain text. Matching ignores
    /// ASCII case, as the previous extension lookup did.
    pub fn resolve_path(&self, path: &Path) -> ResolvedLanguage {
        let by_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| self.by_file_name.get(name.to_ascii_lowercase().as_str()));
        let by_extension = || {
            path.extension()
                .and_then(|extension| extension.to_str())
                .and_then(|extension| {
                    self.by_extension
                        .get(extension.to_ascii_lowercase().as_str())
                })
        };
        let by_suffix = || {
            let filename = path.file_name()?.to_str()?.to_ascii_lowercase();
            if let Some(claim) = self.by_suffix.get(filename.as_str()) {
                return Some(claim);
            }
            // Match Tree-sitter's whole filename / longest dotted extension
            // suffix rules. Arbitrary trailing letters ("notes" vs "ts") do
            // not make a file TypeScript. Dotfiles match by their full name.
            let mut stem = Path::new(&filename);
            let mut extensions = Vec::new();
            while let Some(extension) = stem.extension() {
                extensions.push(extension.to_str()?);
                stem = Path::new(stem.file_stem()?);
            }
            extensions.reverse();
            (0..extensions.len())
                .find_map(|start| self.by_suffix.get(extensions[start..].join(".").as_str()))
        };
        match by_name.or_else(by_suffix).or_else(by_extension) {
            Some(&(language, position)) => self.resolved(language, position),
            None => self.plain_text(),
        }
    }

    fn resolved(&self, language: LanguageId, position: usize) -> ResolvedLanguage {
        ResolvedLanguage {
            definition: self.languages[language.index()].clone(),
            position,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::language::registry::*;
    use std::path::Path;

    fn demo(name: &str, body: &str) -> String {
        format!(
            r#"{{ "name": "{name}", "display_name": "{name}", "badge": "X",
                 "variants": [{{ "id": "{name}" }}] {body} }}"#
        )
    }
    fn plain() -> String {
        demo("plaintext", "")
    }
    fn build(sources: &[(&str, String)], grammars: &[&str]) -> Result<Registry, RegistryError> {
        let borrowed: Vec<(&str, &str)> = sources.iter().map(|(n, j)| (*n, j.as_str())).collect();
        Registry::from_sources(&borrowed, grammars)
    }

    #[test]
    fn ids_follow_sorted_names_whatever_the_input_order() {
        let a = demo("alpha", "");
        let z = demo("zeta", "");
        let forward = build(
            &[
                ("alpha", a.clone()),
                ("plaintext", plain()),
                ("zeta", z.clone()),
            ],
            &[],
        )
        .unwrap();
        let backward = build(&[("zeta", z), ("plaintext", plain()), ("alpha", a)], &[]).unwrap();
        for registry in [&forward, &backward] {
            let names: Vec<_> = registry.languages().iter().map(|l| &*l.name).collect();
            assert_eq!(names, ["alpha", "plaintext", "zeta"]);
            assert_eq!(registry.language_by_name("zeta"), Some(LanguageId(2)));
        }
        assert_eq!(
            forward.language_by_name("alpha"),
            backward.language_by_name("alpha")
        );
    }

    #[test]
    fn the_first_listed_variant_is_the_default() {
        let json = r#"{ "name": "web", "display_name": "Web", "badge": "W",
            "variants": [{ "id": "zed" }, { "id": "abc" }] }"#;
        let registry = build(&[("plaintext", plain()), ("web", json.into())], &["g"]).unwrap();
        let web = registry.language_by_name("web").unwrap();
        let default = registry.default_resolved(web);
        assert_eq!(
            default.variant_name(),
            "zed",
            "listing order picks the default"
        );
        let abc = registry.resolve_variant(web, "abc").unwrap();
        assert_eq!(abc.variant_name(), "abc");
        assert_eq!(registry.resolve_variant(web, "nope"), None);
    }

    #[test]
    fn files_resolve_by_exact_name_then_extension_then_plain_text() {
        let rules = r#", "files": [
            { "extensions": ["Dm", "demo2"], "variant": "demo" },
            { "names": ["Demofile", "Special.txt"], "variant": "demo" }] "#;
        let other = r#", "files": [{ "extensions": ["txt"], "variant": "other" }] "#;
        let registry = build(
            &[
                ("demo", demo("demo", rules)),
                ("other", demo("other", other)),
                ("plaintext", plain()),
            ],
            &[],
        )
        .unwrap();
        let name = |path: &str| {
            let resolved = registry.resolve_path(Path::new(path));
            resolved.definition().name.to_string()
        };
        assert_eq!(name("a/b/file.DM"), "demo", "extension, any case");
        assert_eq!(name("x.demo2"), "demo");
        assert_eq!(name("dir/DEMOFILE"), "demo", "exact name, any case");
        assert_eq!(
            name("Special.TXT"),
            "demo",
            "an exact name beats an extension"
        );
        assert_eq!(name("notes.txt"), "other");
        assert_eq!(name("noextension"), "plaintext");
        assert_eq!(name(""), "plaintext");
        assert_eq!(
            registry.resolve_path(Path::new("x.unknown")),
            registry.plain_text()
        );
    }

    #[test]
    fn a_language_without_files_or_grammar_is_valid_and_grammars_resolve_by_name() {
        let with_grammar = r#"{ "name": "coded", "display_name": "Coded", "badge": "C",
            "variants": [{ "id": "coded", "grammar": "rust" }],
            "files": [{ "extensions": ["cd"], "variant": "coded" }] }"#;
        let registry = build(
            &[("coded", with_grammar.into()), ("plaintext", plain())],
            &["rust", "tsx"],
        )
        .unwrap();
        let resolved = registry.resolve_path(Path::new("a.cd"));
        assert_eq!(resolved.grammar().unwrap().as_ref(), "rust");
        assert_eq!(registry.plain_text().grammar(), None);
    }

    #[test]
    fn conflicts_and_missing_pieces_are_reported() {
        let ext = |name: &str, extension: &str| {
            demo(
                name,
                &format!(
                    r#", "files": [{{ "extensions": ["{extension}"], "variant": "{name}" }}] "#
                ),
            )
        };
        let cases: Vec<(&str, Result<Registry, RegistryError>)> = vec![
            (
                "claimed by `one` and `two`",
                build(
                    &[
                        ("one", ext("one", "x")),
                        ("two", ext("two", "X")),
                        ("plaintext", plain()),
                    ],
                    &[],
                ),
            ),
            (
                "defined twice",
                build(
                    &[
                        ("one", demo("one", "")),
                        ("one", demo("one", "")),
                        ("plaintext", plain()),
                    ],
                    &[],
                ),
            ),
            ("required", build(&[("one", demo("one", ""))], &[])),
            (
                "unknown grammar",
                build(
                    &[
                        (
                            "one",
                            r#"{ "name": "one", "display_name": "One", "badge": "O",
                                "variants": [{ "id": "one", "grammar": "missing" }] }"#
                                .into(),
                        ),
                        ("plaintext", plain()),
                    ],
                    &["rust"],
                ),
            ),
            (
                "claimed by `one` and `two`",
                build(
                    &[
                        ("one", demo("one", r#", "aliases": ["same"]"#)),
                        ("two", demo("two", r#", "aliases": ["same"]"#)),
                        ("plaintext", plain()),
                    ],
                    &[],
                ),
            ),
            (
                "claimed by `one` and `two`",
                build(
                    &[
                        ("one", demo("one", "")),
                        ("two", demo("two", r#", "aliases": ["one"]"#)),
                        ("plaintext", plain()),
                    ],
                    &[],
                ),
            ),
            (
                "must match the file name",
                build(
                    &[("elsewhere", demo("one", "")), ("plaintext", plain())],
                    &[],
                ),
            ),
        ];
        for (expected, result) in cases {
            let error = result.unwrap_err().to_string();
            assert!(
                error.contains(expected),
                "expected {expected:?} in {error:?}"
            );
        }
        // A variant of one language may not claim the same extension twice either.
        let twice = r#", "files": [{ "extensions": ["a"], "variant": "one" }, { "extensions": ["A"], "variant": "one" }] "#;
        let error = build(&[("one", demo("one", twice)), ("plaintext", plain())], &[]).unwrap_err();
        assert!(error.to_string().contains("claimed by `one` and `one`"));
    }

    #[test]
    fn aliases_resolve_to_the_same_language_as_the_name() {
        let registry = build(
            &[
                ("one", demo("one", r#", "aliases": ["uno"]"#)),
                ("plaintext", plain()),
            ],
            &[],
        )
        .unwrap();
        assert_eq!(
            registry.language_by_name("uno"),
            registry.language_by_name("one")
        );
        assert_eq!(registry.language_by_name("two"), None);
    }
}
