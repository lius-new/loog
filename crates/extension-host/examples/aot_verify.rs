//! CI interoperability witness, not a production authority override.
use loog_extension_host::{CodeSource, Limits, Runtime, aot};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("expected trusted CI corpus directory and expected native target".into());
    }
    let folder = PathBuf::from(&args[0]);
    if args[1] != aot::target() {
        return Err(format!("runner target is {}, expected {:?}", aot::target(), args[1]).into());
    }
    // This authority comes from the exact artifact ID of the trusted CI compiler
    // job. Production never derives authority from an extension's own files.
    let key = std::fs::read_to_string(folder.join("public-key.txt"))?;
    let runtime =
        Runtime::with_aot_authorities(vec![("benchmark-only".into(), key.trim().into())])?;
    let corpus: serde_json::Value =
        serde_json::from_slice(&std::fs::read(folder.join("corpus.json"))?)?;
    for id in corpus["plugins"].as_array().ok_or("missing plugins")? {
        let id = id.as_str().ok_or("invalid plugin id")?;
        if ![
            "loog.lsp.gopls",
            "loog.lsp.rust-analyzer",
            "loog.lsp.typescript-language-server",
        ]
        .contains(&id)
        {
            return Err("unexpected corpus plugin".into());
        }
        let package = folder.join(id);
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(package.join("manifest.json"))?)?;
        let wasm = std::fs::read(package.join("extension.wasm"))?;
        let cache = folder.join(format!("must-not-compile-{id}"));
        if cache.exists() {
            return Err("witness cache must start empty".into());
        }
        let loaded = runtime.compile_package(
            &wasm,
            &cache,
            &package,
            id,
            manifest["version"].as_str().ok_or("missing version")?,
        )?;
        if loaded.source != CodeSource::PublishedAot || cache.exists() {
            return Err(format!("{id}: not direct AOT: {:?}", loaded.aot_rejection).into());
        }
        let mut instance = runtime.instantiate(&loaded.component, Limits::default())?;
        instance.dispatch(&[])?;
        println!(
            "{}",
            serde_json::json!({"plugin":id,"target":aot::target(),"code_source":loaded.source,"instantiated":true})
        );
    }
    Ok(())
}
