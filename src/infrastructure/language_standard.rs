//! Adapt upstream Tree-sitter metadata to the editor registry. No payload is
//! opened here: grammar modules and query files are resolved on first use.
use super::*;
use serde_json::json;

#[derive(Clone, Deserialize)]
#[serde(untagged)]
enum Queries {
    One(String),
    Many(Vec<String>),
}
impl Queries {
    fn paths(&self) -> Vec<String> {
        match self {
            Self::One(path) => vec![path.clone()],
            Self::Many(paths) => paths.clone(),
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) struct Grammar {
    name: String,
    #[serde(default)]
    camelcase: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    file_types: Option<Vec<String>>,
    #[serde(default)]
    highlights: Option<Queries>,
    #[serde(default)]
    injections: Option<Queries>,
}
impl Grammar {
    pub(super) fn directory(&self) -> &str {
        self.path.as_deref().unwrap_or(".")
    }
    pub(super) fn queries(&self, file: &str) -> Vec<String> {
        let query = if file == "highlights.scm" {
            &self.highlights
        } else {
            &self.injections
        };
        query
            .as_ref()
            .map(Queries::paths)
            .unwrap_or_else(|| vec![format!("queries/{file}")])
    }
}
#[derive(Deserialize)]
struct Document {
    grammars: Vec<Grammar>,
}

pub(super) fn relative(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains(['\\', ':', '\0'])
        || path.split('/').any(|part| part == "..")
    {
        return Err(format!(
            "resource path must stay inside the language directory: {path}"
        ));
    }
    Ok(())
}
pub(super) fn contained(root: &Path, path: &Path) -> Result<(), String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let resolved = path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if !resolved.starts_with(&root) {
        return Err(format!(
            "resource escapes language directory: {}",
            path.display()
        ));
    }
    Ok(())
}

pub(super) fn read(root: &Path) -> Result<Installed, String> {
    let path = root.join("tree-sitter.json");
    contained(root, &path)?;
    from_json(root, &read_text(&path, MAX_MANIFEST)?)
}

pub(super) fn from_json(root: &Path, json: &str) -> Result<Installed, String> {
    let document: Document = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if document.grammars.is_empty() || document.grammars.len() > 128 {
        return Err("tree-sitter.json needs 1..128 grammars".into());
    }
    // Application editing/LSP defaults are independent of syntax resources.
    // Unknown grammars work without an entry in this optional defaults table.
    let defaults: Value = serde_json::from_str(include_str!("language_defaults.json"))
        .expect("application language defaults");
    let mut grammars = BTreeMap::new();
    let mut languages = BTreeMap::new();
    let id = document.grammars[0].name.clone();
    let available: BTreeSet<_> = document.grammars.iter().map(|g| g.name.as_str()).collect();
    for grammar in &document.grammars {
        if !name(&grammar.name) || grammar.name == "plaintext" {
            return Err(format!("invalid grammar name {}", grammar.name));
        }
        relative(grammar.directory())?;
        for file in ["highlights.scm", "injections.scm"] {
            let paths = grammar.queries(file);
            if paths.len() > 128 {
                return Err("too many query files".into());
            }
            for path in paths {
                relative(&path)?;
            }
        }
        if grammars
            .insert(grammar.name.clone(), grammar.clone())
            .is_some()
        {
            return Err(format!("duplicate grammar {}", grammar.name));
        }
        if defaults["embedded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some(&grammar.name))
        {
            continue;
        }
        let known = defaults["languages"].as_array().unwrap().iter().find(|d| {
            d["variants"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["grammar"].as_str() == Some(&grammar.name))
        });
        let mut descriptor = known.cloned().unwrap_or_else(|| json!({
            "name":grammar.name, "display_name":grammar.title.as_deref().or(grammar.camelcase.as_deref()).unwrap_or(&grammar.name),
            "badge":grammar.name.chars().take(4).collect::<String>().to_ascii_uppercase(),
            "variants":[{"id":grammar.name,"grammar":grammar.name}], "files":[]
        }));
        descriptor["variants"]
            .as_array_mut()
            .unwrap()
            .retain(|v| available.contains(v["grammar"].as_str().unwrap()));
        descriptor["files"] = json!([]);
        let key = descriptor["name"].as_str().unwrap().to_owned();
        let descriptor = languages.entry(key).or_insert(descriptor);
        if let Some(types) = &grammar.file_types {
            if types.len() > 512 {
                return Err("too many file-types".into());
            }
            if !types.is_empty() {
                // Preserve the standard suffix semantics, including whole file
                // names and compound suffixes; do not guess extensions.
                let variant = descriptor["variants"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| v["grammar"].as_str() == Some(&grammar.name))
                    .unwrap()["id"]
                    .clone();
                descriptor["files"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"suffixes":types,"variant":variant}));
            }
        }
    }
    let languages: Vec<Value> = languages.into_values().collect();
    let manifest = Manifest {
        id: id.clone(),
        languages,
        grammars: grammars
            .keys()
            .map(|name| {
                (
                    name.clone(),
                    Module {
                        module: name.clone(),
                    },
                )
            })
            .collect(),
    };
    let installed = Installed {
        manifest,
        root: root.to_owned(),
        standard: grammars,
    };
    registry(&BTreeMap::from([(id, installed.clone())]))?;
    Ok(installed)
}

pub(super) fn module_path(root: &Path, name: &str, grammar_path: &str) -> PathBuf {
    let files = [format!("tree-sitter-{name}.wasm"), format!("{name}.wasm")];
    // Both standard locations: the pack's root and the grammar's own directory.
    let candidates: Vec<_> = files
        .iter()
        .flat_map(|file| [root.join(file), root.join(grammar_path).join(file)])
        .collect();
    candidates
        .iter()
        .find(|p| p.is_file())
        .cloned()
        .unwrap_or_else(|| candidates[0].clone())
}
