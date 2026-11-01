//! External artifact integration suite against the production domain, parser
//! and worker sources. No grammar source or grammar crate dependency.
#![allow(dead_code)]
#![allow(unused_imports)]
#![cfg_attr(test, allow(linker_messages))]
#[path = "../../../src/domain/mod.rs"]
pub(crate) mod domain;
mod features;
mod infrastructure;

/// Validate producer artifacts using the real loader and query implementation.
pub fn validate_pack() -> Result<(), String> {
    use features::syntax::Resources;
    use std::path::PathBuf;
    let mut args = std::env::args_os().skip(1);
    let root = PathBuf::from(args.next().ok_or("missing package root")?);
    let samples = PathBuf::from(args.next().ok_or("missing validation samples")?);
    let resources = infrastructure::language_packs::Packages::discover(&root, None);
    let errors = resources.activity().errors;
    if !errors.is_empty() {
        return Err(errors.into_iter().collect::<Vec<_>>().join("\n"));
    }
    resources.registry()?;
    let samples: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(samples).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    for sample in samples {
        let grammar = sample["grammar"].as_str().ok_or("missing grammar")?;
        let file = sample["sample"].as_str().ok_or("missing sample path")?;
        let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
        let parser = resources.parser(grammar)?;
        let colored =
            features::syntax::validate_producer_grammar(&resources, grammar, parser, &text)?;
        println!(
            "{}",
            serde_json::json!({"grammar":grammar,"colored":colored})
        );
    }
    Ok(())
}

const VALIDATE_USAGE: &str = "usage: loom-ext-validate <extension-folder> [--samples samples.json]";

/// `loom-ext-validate`: load an extension folder the way Loom does and color its
/// samples. The manifest follows the application's rules, every language resource
/// is read by the application's loader, the queries the extension contributes are
/// compiled against the grammars it carries, and each grammar's WebAssembly module
/// is loaded. `samples.json` is `[{"grammar": "rust", "sample": "<file>"}]`; the
/// color count of every grammar is printed as one JSON object per line, and a sample
/// that parses with errors or gets no color fails the run.
pub fn validate_extension() -> Result<(), String> {
    use features::syntax::Resources;
    use std::path::PathBuf;
    let mut folder = None;
    let mut samples_file = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--samples" {
            samples_file = Some(PathBuf::from(args.next().ok_or(VALIDATE_USAGE)?));
        } else if folder.is_none() {
            folder = Some(PathBuf::from(arg));
        } else {
            return Err(VALIDATE_USAGE.into());
        }
    }
    let folder = folder.ok_or(VALIDATE_USAGE)?;
    let text = std::fs::read_to_string(folder.join("manifest.json"))
        .map_err(|e| format!("{}: manifest.json: {e}", folder.display()))?;
    let manifest = features::extensions::manifest::Manifest::parse(&text)
        .map_err(|e| format!("{}: manifest.json: {e}", manifest_name(&folder)))?;

    let mut problems = Vec::new();
    for resource in &manifest.contributes.languages {
        if let Err(error) =
            infrastructure::language_packs::check_extension_pack(&folder, &resource.path)
        {
            problems.push(format!("{}: {error}", resource.id));
        }
    }
    let none = Default::default();
    let packs = infrastructure::language_packs::packs_in(vec![folder.clone()], &none);
    let resources = infrastructure::language_packs::Packages::from_directories(packs);
    resources.set_query_sources(
        None,
        infrastructure::language_packs::queries_in(vec![folder.clone()], &none),
        false,
    );

    let samples: Vec<serde_json::Value> = match samples_file {
        Some(file) => serde_json::from_str(
            &std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?,
        )
        .map_err(|e| format!("samples: {e}"))?,
        None => Vec::new(),
    };
    let grammars = resources.grammar_names();
    for sample in &samples {
        let grammar = sample["grammar"].as_str().unwrap_or_default();
        if !grammars.iter().any(|known| known == grammar) {
            problems.push(format!(
                "a sample names {grammar:?}, which this extension has no grammar for"
            ));
        }
    }
    let mut results = Vec::new();
    for grammar in &grammars {
        let parser = match resources.parser(grammar) {
            Ok(parser) => parser,
            Err(error) => {
                problems.push(format!("{grammar}: {error}"));
                continue;
            }
        };
        // Reading the stacked queries compiles every layer against the grammar.
        for file in ["highlights.scm", "injections.scm"] {
            if let Err(error) = resources.text(grammar, file) {
                problems.push(format!("{grammar}/{file}: {error}"));
            }
        }
        let sample = samples
            .iter()
            .find(|sample| sample["grammar"].as_str() == Some(grammar))
            .and_then(|sample| sample["sample"].as_str());
        let colored = match sample {
            None => 0,
            Some(file) => {
                let text = std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
                match features::syntax::validate_producer_grammar(
                    &resources, grammar, parser, &text,
                ) {
                    Ok(count) => count,
                    Err(error) => {
                        problems.push(error);
                        continue;
                    }
                }
            }
        };
        results.push(serde_json::json!({"grammar": grammar, "colored": colored}));
    }
    problems.extend(resources.query_problems());
    problems.extend(resources.activity().errors);
    for result in results {
        println!("{result}");
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

fn manifest_name(folder: &std::path::Path) -> String {
    folder.file_name().map_or_else(
        || folder.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// A headless production-mode probe: each invocation starts with a fresh
/// registry, using exactly the same installed package loader as Loom.
pub fn probe() {
    use features::syntax::{Source, Window};
    use std::{path::PathBuf, sync::atomic::AtomicBool};
    let file = PathBuf::from(std::env::args_os().nth(1).expect("input path"));
    infrastructure::language_packs::initialize();
    let language = domain::language::from_path(&file);
    let content = std::fs::read_to_string(&file).expect("input text");
    let text = loom_editor_core::text::TextBuffer::new(&content);
    let frame = infrastructure::language_packs::index()
        .prepare(
            domain::identity::FileId::new(1),
            Source::new(&text),
            language,
            &[Window {
                line: 0..text.len(),
                range: 0..text.len(),
            }],
            &AtomicBool::new(false),
        )
        .unwrap();
    let activity = infrastructure::language_packs::resources().activity();
    println!(
        "{}",
        serde_json::json!({"language":language.name(),"colored":frame[0].spans.iter().filter(|s|s.class.is_some()).count(),"modules":activity.modules,"wasm_modules":activity.wasm_modules,"errors":activity.errors})
    );
}
