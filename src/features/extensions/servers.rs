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
    let mut root = json!({
        "markers": decl.root_markers,
        "fallback": decl.root_fallback.as_deref().unwrap_or("workspace"),
    });
    if let Some(strategy) = &decl.root_strategy {
        root["strategy"] = strategy.clone().into();
    }
    let languages: Vec<Value> = if decl.bindings.is_empty() {
        decl.languages
            .iter()
            .map(|language| json!({ "language": language, "language_id": language }))
            .collect()
    } else {
        decl.bindings
            .iter()
            .map(|binding| {
                let mut value =
                    json!({ "language": binding.language, "language_id": binding.language_id });
                if let Some(variant) = &binding.variant {
                    value["variant"] = variant.clone().into();
                }
                value
            })
            .collect()
    };
    let mut definition = json!({
        "name": name,
        "display_name": decl.name.as_deref().unwrap_or(&decl.id),
        "command": decl.command,
        "args": decl.args,
        "root": root,
        "languages": languages,
    });
    if let Some(priority) = decl.priority {
        definition["priority"] = priority.into();
    }
    if let Some(options) = &decl.initialization_options {
        definition["initialization_options"] = options.clone();
    }
    (name, definition)
}

/// The program a declaration starts. `${extension}/…` is a file of the package in
/// `package`; `${runtime.<name>}/…` is a file of the `loom.runtime.<name>`
/// extension, found with `runtime`. A command with neither is left as written
/// (a bare program name, or a path the package declared in its own words).
/// On Windows `.exe` is added to a package or runtime file that has no extension.
pub fn resolve_command(
    command: &str,
    package: &std::path::Path,
    runtime: impl Fn(&str) -> Option<std::path::PathBuf>,
    windows: bool,
) -> Result<String, String> {
    let (base, rest) = if let Some(rest) = command.strip_prefix("${extension}/") {
        (package.to_path_buf(), rest)
    } else if let Some(after) = command.strip_prefix("${runtime.")
        && let Some((name, rest)) = after.split_once("}/")
    {
        let id = format!("loom.runtime.{name}");
        let dir = runtime(&id).ok_or_else(|| format!("{id} is not installed"))?;
        (dir, rest)
    } else {
        return Ok(command.to_owned());
    };
    if rest.is_empty()
        || rest
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || rest.contains(['\\', ':'])
    {
        return Err(format!("`{command}` does not name a file inside the extension"));
    }
    let mut path = base.join(rest);
    if windows && path.extension().is_none() {
        path.set_extension("exe");
    }
    Ok(path.to_string_lossy().into_owned())
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
            root_strategy: None,
            root_fallback: None,
            priority: None,
            bindings: vec![],
            initialization_options: None,
            name: None,
        }
    }

    #[test]
    fn the_extended_fields_reach_the_definition() {
        let mut server = decl();
        server.name = Some("Analyzer".into());
        server.root_strategy = Some("outermost".into());
        server.root_fallback = Some("file_directory".into());
        server.priority = Some(10);
        server.bindings = vec![crate::features::extensions::manifest::LanguageBinding {
            language: "rust".into(),
            variant: Some("script".into()),
            language_id: "rs".into(),
        }];
        assert!(validate(&server).is_ok());
        let (_, definition) = definition(&server);
        assert_eq!(definition["display_name"], "Analyzer");
        assert_eq!(definition["root"]["strategy"], "outermost");
        assert_eq!(definition["root"]["fallback"], "file_directory");
        assert_eq!(definition["priority"], 10);
        assert_eq!(definition["languages"][0]["variant"], "script");
        assert_eq!(definition["languages"][0]["language_id"], "rs");
        server.root_strategy = Some("middle".into());
        assert!(validate(&server).unwrap_err().contains("strategy"));
    }

    #[test]
    fn command_placeholders_point_into_the_package_or_its_runtime() {
        use std::path::{Path, PathBuf};
        let runtime = |id: &str| (id == "loom.runtime.node").then(|| PathBuf::from("/rt/node"));
        let package = Path::new("/ext/pkg");
        let resolve = |command, windows| resolve_command(command, package, runtime, windows);
        assert_eq!(
            resolve("${extension}/bin/server", false).unwrap(),
            Path::new("/ext/pkg").join("bin/server").to_string_lossy()
        );
        assert_eq!(
            resolve("${runtime.node}/bin/node", false).unwrap(),
            Path::new("/rt/node").join("bin/node").to_string_lossy()
        );
        assert!(resolve("${extension}/server", true).unwrap().ends_with("server.exe"));
        assert!(resolve("${extension}/server.cmd", true).unwrap().ends_with("server.cmd"));
        assert_eq!(resolve("rust-analyzer", true).unwrap(), "rust-analyzer");
        assert!(resolve("${runtime.python}/bin/python", false).unwrap_err().contains("not installed"));
        assert!(resolve("${extension}/../x", false).is_err());
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
