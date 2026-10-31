//! Language server integrations declared by a package (design §5.2, §6.6).
//!
//! A declaration is converted to the language service's own definition format
//! and checked with its validator. Starting, downloading or installing the
//! server is a separate, authorized step (M3); nothing here runs a program.
use super::manifest::LanguageServerDecl;
use crate::features::language_service::server_set::ServerSet;
use serde_json::{Value, json};

/// The language service's server name for a declaration: lowercase words
/// joined by hyphens (the package id's dots become hyphens).
pub fn server_name(decl: &LanguageServerDecl) -> String {
    decl.id.replace('.', "-")
}

/// `(name, definition JSON)` in the form `ServerSet` reads.
pub fn definition(decl: &LanguageServerDecl) -> (String, Value) {
    let name = server_name(decl);
    let mut definition = json!({
        "name": name,
        "display_name": decl.id,
        "command": decl.command,
        "args": decl.args,
        "root": { "markers": decl.root_markers, "fallback": "workspace" },
        "languages": decl.languages.iter().map(|language| json!({
            "language": language,
            "language_id": language,
        })).collect::<Vec<_>>(),
    });
    if let Some(options) = &decl.initialization_options {
        definition["initialization_options"] = options.clone();
    }
    (name, definition)
}

pub fn validate(decl: &LanguageServerDecl) -> Result<(), String> {
    let (name, definition) = definition(decl);
    ServerSet::from_sources(&[(name.as_str(), definition.to_string().as_str())]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl() -> LanguageServerDecl {
        LanguageServerDecl {
            id: "acme.rust-analyzer".into(),
            languages: vec!["rust".into()],
            command: "rust-analyzer".into(),
            args: vec![],
            root_markers: vec!["Cargo.toml".into()],
            initialization_options: None,
        }
    }

    #[test]
    fn a_complete_declaration_becomes_a_valid_definition_without_running_anything() {
        assert!(validate(&decl()).is_ok());
        let (name, definition) = definition(&decl());
        assert_eq!(name, "acme-rust-analyzer");
        assert_eq!(definition["languages"][0]["language_id"], "rust");
    }

    #[test]
    fn what_the_language_service_would_refuse_is_refused_at_load() {
        let mut no_markers = decl();
        no_markers.root_markers.clear();
        assert!(validate(&no_markers).unwrap_err().contains("root.markers"));

        let mut spaced = decl();
        spaced.command = "  ".into();
        assert!(validate(&spaced).is_err());

        let mut options = decl();
        options.initialization_options = Some(json!([1]));
        assert!(validate(&options).unwrap_err().contains("JSON object"));

        let mut language = decl();
        language.languages = vec!["Rust Lang".into()];
        assert!(validate(&language).unwrap_err().contains("canonical"));
    }
}
