//! Production contains only the plain-text fallback descriptor. Tests use the
//! bundled metadata fixtures. Grammar/query payloads always stay on disk.
use crate::domain::language::registry::{Registry, RegistryError};

#[cfg(test)]
include!("../../../tests/fixtures/language_registry.rs");

#[cfg(not(test))]
static LANGUAGE_FILES: &[(&str, &str)] = &[(
    "plaintext",
    r#"{"name":"plaintext","display_name":"Plain Text","badge":"TXT","variants":[{"id":"plaintext"}]}"#,
)];
#[cfg(not(test))]
static GRAMMAR_NAMES: &[&str] = &[];

/// The registry for the embedded descriptions and grammar directories.
pub(crate) fn registry() -> Result<Registry, RegistryError> {
    let sources: Vec<(&str, &str)> = LANGUAGE_FILES
        .iter()
        .map(|(name, contents)| (*name, *contents))
        .collect();
    Registry::from_sources(&sources, GRAMMAR_NAMES)
}

#[cfg(test)]
mod tests {
    use crate::domain::language::embedded::*;
    use std::path::Path;

    #[test]
    fn the_embedded_registry_builds_and_keeps_the_previous_recognition() {
        let registry = Registry::builtin();
        let expect = |path: &str, name: &str, variant: &str| {
            let resolved = registry.resolve_path(Path::new(path));
            assert_eq!(&*resolved.definition().name, name, "{path}");
            assert_eq!(resolved.variant_name(), variant, "{path}");
        };
        // The extension table of the former Language::from_path.
        expect("main.RS", "rust", "rust");
        expect("a.cs", "csharp", "csharp");
        for ts in ["a.ts", "a.mts", "a.cts"] {
            expect(ts, "typescript", "typescript");
        }
        expect("component.tsx", "typescript", "tsx");
        for js in ["a.js", "a.jsx", "script.mjs", "a.cjs"] {
            expect(js, "javascript", "javascript");
        }
        expect("README.md", "markdown", "markdown");
        expect("notes.txt", "plaintext", "plaintext");
        expect("Makefile", "plaintext", "plaintext");
    }

    #[test]
    fn display_data_moved_out_of_code_matches_what_the_status_bar_showed() {
        let registry = Registry::builtin();
        let shown = |name: &str| {
            let definition = registry.definition(registry.language_by_name(name).unwrap());
            (
                definition.badge.to_string(),
                definition.display_name.to_string(),
            )
        };
        assert_eq!(shown("csharp"), ("C#".into(), "C#".into()));
        assert_eq!(shown("rust"), ("RS".into(), "Rust".into()));
        assert_eq!(shown("typescript"), ("TS".into(), "TypeScript".into()));
        assert_eq!(shown("javascript"), ("JS".into(), "JavaScript".into()));
        assert_eq!(shown("plaintext"), ("TXT".into(), "Plain Text".into()));
        assert_eq!(
            registry.language_by_name("ts"),
            registry.language_by_name("typescript")
        );
    }
}
