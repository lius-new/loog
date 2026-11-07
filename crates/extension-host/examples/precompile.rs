//! Precompile a component into the same private cache used by the executor.
//! Usage: cargo run -p loog-extension-host --release --example precompile -- component.wasm cache-directory
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.len() != 2 {
        return Err("expected component.wasm and a user-owned cache directory".into());
    }
    let bytes = std::fs::read(&arguments[0])?;
    let runtime = loog_extension_host::Runtime::new()?;
    let (_, hit) = runtime.compile_cached(&bytes, std::path::Path::new(&arguments[1]))?;
    println!("component cache {}", if hit { "hit" } else { "prepared" });
    Ok(())
}
