//! Measure one component in a fresh process; cold and warm caches are explicit.
//! cargo run --release -p loog-extension-host --example component_cost -- INPUT CACHE
use loog_extension_host::{Limits, Runtime};
use std::{path::PathBuf, time::Instant};
#[allow(dead_code)] // The shared sampler also exposes an unused periodic sampling helper.
#[path = "../../../tests/fixtures/language_migration_metrics.rs"]
mod memory;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(2..=4).contains(&args.len()) {
        return Err(
            "expected component.wasm cache [package-directory [benchmark-public-key-file]]".into(),
        );
    }
    let bytes = std::fs::read(&args[0])?;
    let before_engine = memory::snapshot();
    let start = Instant::now();
    let runtime = if let Some(key) = args.get(3) {
        Runtime::with_aot_authorities(vec![(
            "benchmark-only".into(),
            std::fs::read_to_string(key)?.trim().into(),
        )])?
    } else {
        Runtime::new()?
    };
    let engine_ms = start.elapsed().as_secs_f64() * 1000.0;
    let before_compile = memory::snapshot();
    let start = Instant::now();
    let loaded = if let Some(package) = args.get(2) {
        let package = PathBuf::from(package);
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(package.join("manifest.json"))?)?;
        runtime.compile_package(
            &bytes,
            &PathBuf::from(&args[1]),
            &package,
            manifest["id"].as_str().ok_or("id")?,
            manifest["version"].as_str().ok_or("version")?,
        )?
    } else {
        // COMPAT(component-cost-arguments): Keep the original two-argument cache
        // benchmark reproducible; package-aware callers use the extra argument.
        // Remove after archived runs are no longer supported; no removal version is scheduled.
        let (component, hit) = runtime.compile_cached(&bytes, &PathBuf::from(&args[1]))?;
        loog_extension_host::LoadedCode {
            component,
            source: if hit {
                loog_extension_host::CodeSource::LocalCache
            } else {
                loog_extension_host::CodeSource::Compiled
            },
            aot_rejection: None,
        }
    };
    let cache_hit = loaded.source == loog_extension_host::CodeSource::LocalCache;
    let component = loaded.component;
    let compile_or_load_ms = start.elapsed().as_secs_f64() * 1000.0;
    let after_compile = memory::snapshot();
    let start = Instant::now();
    let mut instance = runtime.instantiate(&component, Limits::default())?;
    let instantiate_ms = start.elapsed().as_secs_f64() * 1000.0;
    let after_instantiate = memory::snapshot();
    let start = Instant::now();
    instance.dispatch(&[])?;
    let first_dispatch_ms = start.elapsed().as_secs_f64() * 1000.0;
    println!(
        "{}",
        serde_json::json!({
            "component_bytes":bytes.len(), "cache_hit":cache_hit, "code_source":loaded.source, "aot_rejection":loaded.aot_rejection,
            "engine_ms":engine_ms, "compile_or_load_ms":compile_or_load_ms,
            "instantiate_ms":instantiate_ms, "first_dispatch_ms":first_dispatch_ms,
            "before_engine":before_engine,"before_compile":before_compile,
            "after_compile":after_compile,"after_instantiate":after_instantiate,
            "after_dispatch":memory::snapshot(),
        })
    );
    Ok(())
}
