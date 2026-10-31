//! The on-disk shape of a language description, and the rules a description must
//! satisfy before it can be registered. Parsing is strict: an unknown or
//! misspelled field anywhere is an error.
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Descriptor {
    pub name: String,
    pub display_name: String,
    pub badge: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub comments: Comments,
    #[serde(default)]
    pub highlight: Highlight,
    pub variants: Vec<VariantDescriptor>,
    #[serde(default)]
    pub files: Vec<FileRuleDescriptor>,
}

/// Editing markers only; they never replace a grammar's own rules.
#[derive(Debug, Default, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Comments {
    pub line: Option<String>,
    pub block: Option<[String; 2]>,
}

/// Rules for the generic highlighter, which colours a language that no grammar
/// covers. A list given here replaces the built-in one; a field left out keeps
/// it, so a few lines of data are enough to colour a language.
#[derive(Debug, Default, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Highlight {
    pub keywords: Option<Vec<String>>,
    /// Words like `true` and `null`, drawn as built-in constants.
    pub constants: Option<Vec<String>>,
    /// Characters that open a string ending on the same line.
    pub strings: Option<Vec<char>>,
    pub numbers: Option<bool>,
}

const MAX_WORDS: usize = 512;
const MAX_WORD_CHARS: usize = 40;

impl Highlight {
    fn validate(&self) -> Result<(), String> {
        for (what, words) in [("keywords", &self.keywords), ("constants", &self.constants)] {
            let Some(words) = words else { continue };
            if words.len() > MAX_WORDS {
                return Err(format!(
                    "highlight {what} may list at most {MAX_WORDS} words"
                ));
            }
            for word in words {
                let usable = !word.is_empty()
                    && word.chars().count() <= MAX_WORD_CHARS
                    && word.chars().all(|c| c.is_alphanumeric() || c == '_');
                if !usable {
                    return Err(format!("highlight {what}: `{word}` is not a plain word"));
                }
            }
        }
        if let Some(strings) = &self.strings
            && (strings.len() > 8
                || strings
                    .iter()
                    .any(|c| c.is_alphanumeric() || c.is_whitespace() || c.is_control()))
        {
            return Err("highlight strings must be at most 8 punctuation characters".into());
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VariantDescriptor {
    pub id: String,
    pub grammar: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileRuleDescriptor {
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub names: Vec<String>,
    /// Tree-sitter file-types are filename suffixes, including compound ones.
    #[serde(default)]
    pub suffixes: Vec<String>,
    pub variant: String,
}

pub(crate) fn is_canonical_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// A file extension as rules and lookups compare it: lowercase, no dot.
pub(crate) fn normalize_extension(extension: &str) -> Result<String, String> {
    let bad = extension.is_empty()
        || extension.starts_with('.')
        || extension
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '\\' | '*' | '?'));
    if bad {
        return Err(format!("invalid file extension {extension:?}"));
    }
    Ok(extension.to_ascii_lowercase())
}

/// An exact file name as rules and lookups compare it: lowercase, no separators.
pub(crate) fn normalize_file_name(name: &str) -> Result<String, String> {
    let bad = name.trim().is_empty()
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\'));
    if bad {
        return Err(format!("invalid file name {name:?}"));
    }
    Ok(name.to_ascii_lowercase())
}

pub(crate) fn normalize_suffix(suffix: &str) -> Result<String, String> {
    if suffix.contains(['*', '?', ':']) {
        return Err(format!("invalid file suffix {suffix:?}"));
    }
    normalize_file_name(suffix)
}

impl Descriptor {
    pub fn parse(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|error| error.to_string())
    }

    /// Checks one description on its own; clashes between descriptions are the
    /// registry's job. `file_stem` is the description's file name without
    /// `.json`, which must equal its `name`.
    pub fn validate(&self, file_stem: &str) -> Result<(), String> {
        if !is_canonical_name(&self.name) {
            return Err(format!("`{}` is not a canonical lowercase name", self.name));
        }
        if self.name != file_stem {
            return Err(format!(
                "name `{}` must match the file name `{file_stem}.json`",
                self.name
            ));
        }
        if self.display_name.trim().is_empty() || self.display_name != self.display_name.trim() {
            return Err("display_name must be non-empty text without outer spaces".into());
        }
        let badge_len = self.badge.chars().count();
        if !(1..=4).contains(&badge_len)
            || self
                .badge
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(format!(
                "badge {:?} must be 1 to 4 visible characters",
                self.badge
            ));
        }
        for (index, alias) in self.aliases.iter().enumerate() {
            if !is_canonical_name(alias) {
                return Err(format!("alias `{alias}` is not a canonical lowercase name"));
            }
            if alias == &self.name || self.aliases[..index].contains(alias) {
                return Err(format!("alias `{alias}` is repeated"));
            }
        }
        for marker in [&self.comments.line].into_iter().flatten() {
            if marker.is_empty() || marker.contains(['\n', '\r']) {
                return Err("the line comment marker must be one line of text".into());
            }
        }
        if let Some([open, close]) = &self.comments.block
            && (open.is_empty() || close.is_empty())
        {
            return Err("block comment markers must both be non-empty".into());
        }
        self.highlight.validate()?;
        if self.variants.is_empty() {
            return Err("at least one variant is required".into());
        }
        for (index, variant) in self.variants.iter().enumerate() {
            if !is_canonical_name(&variant.id) {
                return Err(format!("variant `{}` is not a canonical name", variant.id));
            }
            if self.variants[..index].iter().any(|v| v.id == variant.id) {
                return Err(format!("variant `{}` is repeated", variant.id));
            }
            if let Some(grammar) = &variant.grammar
                && !is_canonical_name(grammar)
            {
                return Err(format!("grammar `{grammar}` is not a canonical name"));
            }
        }
        for rule in &self.files {
            if rule.extensions.is_empty() && rule.names.is_empty() && rule.suffixes.is_empty() {
                return Err("a file rule needs at least one extension or name".into());
            }
            if !self.variants.iter().any(|v| v.id == rule.variant) {
                return Err(format!(
                    "file rule names unknown variant `{}`",
                    rule.variant
                ));
            }
            for extension in &rule.extensions {
                normalize_extension(extension)?;
            }
            for name in &rule.names {
                normalize_file_name(name)?;
            }
            for suffix in &rule.suffixes {
                normalize_suffix(suffix)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::language::descriptor::*;

    const GOOD: &str = r##"{
        "name": "demo", "display_name": "Demo", "badge": "DM",
        "aliases": ["dm"],
        "comments": { "line": "#", "block": ["<!--", "-->"] },
        "variants": [{ "id": "demo", "grammar": "rust" }, { "id": "demo_x" }],
        "files": [{ "extensions": ["demo"], "names": ["Demofile"], "variant": "demo" }]
    }"##;

    fn check(json: &str) -> Result<(), String> {
        Descriptor::parse(json)?.validate("demo")
    }

    #[test]
    fn a_complete_description_is_accepted() {
        check(GOOD).unwrap();
    }

    #[test]
    fn highlight_rules_are_plain_words_and_punctuation() {
        let with = |rules: &str| {
            GOOD.replace(
                "\"aliases\"",
                &format!("\"highlight\": {rules}, \"aliases\""),
            )
        };
        let good = with(
            r#"{ "keywords": ["let", "fn_2"], "constants": [], "strings": ["\"", "'"], "numbers": false }"#,
        );
        let parsed = Descriptor::parse(&good).unwrap();
        parsed.validate("demo").unwrap();
        assert_eq!(
            parsed.highlight.keywords.as_deref().map(<[_]>::len),
            Some(2)
        );
        assert_eq!(parsed.highlight.strings, Some(vec!['"', '\'']));
        assert_eq!(
            Descriptor::parse(GOOD).unwrap().highlight,
            Highlight::default()
        );
        for bad in [
            with(r#"{ "keywords": ["two words"] }"#),
            with(r#"{ "constants": [""] }"#),
            with(r#"{ "strings": ["a"] }"#),
            with(r#"{ "strings": ["\"","'","`","<",">","(",")","[","]"] }"#),
        ] {
            assert!(check(&bad).is_err(), "{bad}");
        }
        assert!(Descriptor::parse(&with(r#"{ "colours": [] }"#)).is_err());
    }

    #[test]
    fn unknown_fields_fail_at_every_level() {
        let cases = [
            GOOD.replace("\"badge\"", "\"badges\""),
            GOOD.replace("\"line\": \"#\"", "\"line\": \"#\", \"extra\": 1"),
            GOOD.replace(
                "{ \"id\": \"demo_x\" }",
                "{ \"id\": \"demo_x\", \"server\": \"x\" }",
            ),
            GOOD.replace(
                "\"variant\": \"demo\" }]",
                "\"variant\": \"demo\", \"glob\": \"*\" }]",
            ),
        ];
        for case in cases {
            assert!(Descriptor::parse(&case).is_err(), "{case}");
        }
        assert!(
            Descriptor::parse("{ \"name\": \"demo\" }").is_err(),
            "missing fields"
        );
        assert!(Descriptor::parse("not json").is_err());
    }

    #[test]
    fn invalid_values_are_rejected_with_a_reason() {
        let cases: [(&str, String); 14] = [
            (
                "canonical",
                GOOD.replace("\"name\": \"demo\"", "\"name\": \"Demo\""),
            ),
            (
                "match the file name",
                GOOD.replace("\"name\": \"demo\"", "\"name\": \"other\""),
            ),
            ("display_name", GOOD.replace("\"Demo\"", "\" Demo\"")),
            ("display_name", GOOD.replace("\"Demo\"", "\"\"")),
            ("badge", GOOD.replace("\"DM\"", "\"\"")),
            ("badge", GOOD.replace("\"DM\"", "\"TOOLONG\"")),
            ("badge", GOOD.replace("\"DM\"", "\"D M\"")),
            ("alias", GOOD.replace("[\"dm\"]", "[\"dm\", \"dm\"]")),
            ("alias", GOOD.replace("[\"dm\"]", "[\"Dm\"]")),
            ("line comment", GOOD.replace("\"#\"", "\"\"")),
            (
                "block comment",
                GOOD.replace("[\"<!--\", \"-->\"]", "[\"<!--\", \"\"]"),
            ),
            (
                "variant `demo` is repeated",
                GOOD.replace("\"demo_x\"", "\"demo\""),
            ),
            (
                "unknown variant",
                GOOD.replace("\"variant\": \"demo\" }]", "\"variant\": \"nope\" }]"),
            ),
            (
                "at least one",
                GOOD.replace(
                    "\"extensions\": [\"demo\"], \"names\": [\"Demofile\"], ",
                    "",
                ),
            ),
        ];
        for (expected, case) in cases {
            let error = check(&case).unwrap_err();
            assert!(
                error.contains(expected),
                "expected {expected:?}, got {error:?}\n{case}"
            );
        }
        assert!(
            check(&GOOD.replace("\"variants\": [{ \"id\": \"demo\", \"grammar\": \"rust\" }, { \"id\": \"demo_x\" }]", "\"variants\": []"))
                .unwrap_err()
                .contains("at least one variant")
        );
    }

    #[test]
    fn extensions_and_names_are_validated_and_normalized() {
        assert_eq!(normalize_extension("TSX").unwrap(), "tsx");
        assert_eq!(normalize_file_name("Makefile").unwrap(), "makefile");
        for bad in ["", ".rs", "a/b", "a\\b", "a b", "a*", "a?", "t\tab"] {
            assert!(normalize_extension(bad).is_err(), "{bad:?}");
        }
        for bad in ["", "  ", "a/b", "a\\b", "a\nb"] {
            assert!(normalize_file_name(bad).is_err(), "{bad:?}");
        }
        assert!(normalize_file_name("Dockerfile.dev").is_ok());
    }
}
