//! M0 cost measurements (design §12.2). Ignored by default; run with
//! `cargo test --release -p loom-extension-host --test measure -- --ignored --nocapture`.
//! Results depend on the machine and the cold/warm cache state; record them
//! together with the toolchain versions in EXTENSIONS_DESIGN.md.
use loom_extension_host::{ByteRange, CommandContext, Inbound, Limits, Runtime, Selection};
use std::path::Path;
use std::time::{Duration, Instant};

fn command(name: &str) -> Inbound {
    Inbound::RunCommand(CommandContext {
        call: 1,
        command: name.into(),
        snapshot: None,
        selections: vec![Selection {
            range: ByteRange { start: 0, end: 0 },
            reversed: false,
        }],
        selection_precondition: None,
    })
}

fn percentile(samples: &mut [Duration], p: f64) -> Duration {
    samples.sort();
    samples[((samples.len() as f64 - 1.0) * p).round() as usize]
}

fn measure(label: &str, wasm: &Path, limits: Limits) {
    let bytes =
        std::fs::read(wasm).unwrap_or_else(|_| panic!("build the guest first: {}", wasm.display()));
    let runtime = Runtime::new().unwrap();
    let start = Instant::now();
    let component = runtime.compile(&bytes).unwrap();
    let compile = start.elapsed();

    let start = Instant::now();
    let mut instance = runtime.instantiate(&component, limits).unwrap();
    let instantiate = start.elapsed();

    let start = Instant::now();
    instance
        .dispatch(&[command("example.tools.unknown")])
        .unwrap();
    let first_round = start.elapsed();

    let mut rounds = Vec::new();
    for _ in 0..200 {
        let start = Instant::now();
        instance
            .dispatch(&[command("example.tools.unknown")])
            .unwrap();
        rounds.push(start.elapsed());
    }
    println!(
        "{label}: size {} KiB | compile {:?} | instantiate {:?} | first round {:?} | warm round p50 {:?} p95 {:?}",
        bytes.len() / 1024,
        compile,
        instantiate,
        first_round,
        percentile(&mut rounds, 0.5),
        percentile(&mut rounds, 0.95)
    );
}

#[test]
#[ignore]
fn rust_and_js_component_costs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/extension-guest");
    let rust = root.join("wasm32-unknown-unknown/release/loom_rust_guest.wasm");
    // The Rust module must be wrapped into a component first; reuse the encoder.
    let module = std::fs::read(&rust).expect("build tests/extensions/rust-guest first");
    let component = wit_component::ComponentEncoder::default()
        .module(&module)
        .unwrap()
        .validate(true)
        .encode()
        .unwrap();
    let rust_component = root.join("rust_guest.component.wasm");
    std::fs::write(&rust_component, component).unwrap();
    measure("rust", &rust_component, Limits::default());
    measure(
        "js  ",
        &root.join("js_guest.wasm"),
        Limits {
            memory_bytes: 512 << 20,
            round_ticks: 500,
            ..Limits::default()
        },
    );
}
