//! `loom-ext-validate`: checks an extension folder with the loaders and query
//! compilers Loom itself uses. They are compiled from the application's own sources
//! (the modules below are paths into `src/`), not copied, so what passes here loads
//! in the application. No grammar source or grammar crate is a dependency.
#![allow(dead_code)]
#![allow(unused_imports)]
#[path = "../../../src/domain/mod.rs"]
pub(crate) mod domain;
mod features;
mod infrastructure;

const VALIDATE_USAGE: &str = "usage: loom-ext-validate <extension-folder> [--samples samples.json]";

#[derive(serde::Deserialize)]
struct Sample {
    grammar: String,
    sample: std::path::PathBuf,
}

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

    let samples: Vec<Sample> = match samples_file {
        Some(file) => serde_json::from_str(
            &std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?,
        )
        .map_err(|e| format!("samples: {e}"))?,
        None => Vec::new(),
    };
    let grammars = resources.grammar_names();
    for sample in &samples {
        let grammar = &sample.grammar;
        if !grammars.iter().any(|known| known == grammar) {
            problems.push(format!(
                "a sample names {grammar:?}, which this extension has no grammar for"
            ));
        }
    }
    let mut results = Vec::new();
    for grammar in &grammars {
        let mut parser = match resources.parser(grammar) {
            Ok(parser) => parser,
            Err(error) => {
                problems.push(format!("{grammar}: {error}"));
                continue;
            }
        };
        // Compile even when no sample was supplied; reading query text alone
        // does not check the base query against the grammar.
        if let Err(error) = features::syntax::validate_resources(&resources, grammar) {
            problems.push(error);
            continue;
        }
        let mut colored = 0;
        for sample in samples.iter().filter(|sample| sample.grammar == *grammar) {
            let text = std::fs::read_to_string(&sample.sample)
                .map_err(|e| format!("{}: {e}", sample.sample.display()))?;
            match features::syntax::validate_producer_grammar(
                &resources,
                grammar,
                &mut parser,
                &text,
            ) {
                Ok(count) => colored += count,
                Err(error) => problems.push(format!("{}: {error}", sample.sample.display())),
            }
        }
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
