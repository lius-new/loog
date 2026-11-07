# Shared build entry point

`rust-toolchain.toml` and `Cargo.lock` pin Rust and crate inputs. Use Node.js 20+
and the platform C/C++ SDK; CI selects Node 24. The shared preparer downloads and
verifies private CMake and target-specific Skia before the first host build.

```sh
node scripts/build/cargo.mjs build --release --locked
node scripts/build/cargo.mjs test --locked --workspace
node scripts/build/stage-resources.mjs target/release
node scripts/build/cargo.mjs run --release --locked
node scripts/dependencies/ensure.mjs
node scripts/build/cargo.mjs build --release --locked --offline
```

Before running a source build, stage its runtime resources once and repeat after
updating the bundled catalogs:

```sh
node scripts/build/stage-resources.mjs
node scripts/build/cargo.mjs run --locked
```

The default destination is `target/debug`; pass `target/release` for `--release`,
or the executable's actual directory for custom targets/profiles. This copies
the bundled syntax collections, syntax catalog and LSP server catalog together.
It downloads nothing and leaves other installed collections in place. Cargo does
not copy these resources itself. Restart Loog after staging either catalog.

`--offline` / `--frozen` require populated dependency and Cargo caches and never
silently switch to network downloads. Formatting, Cargo informational commands,
and tests selecting only loog-editor-core / loog-workspace-tests skip preparation.
`--target` (including `--target=...`) and CARGO_BUILD_TARGET select Skia inputs.

Use the same Node.js build entry point in PowerShell:

```powershell
node scripts/build/cargo.mjs test --locked --offline --workspace
node scripts/build/cargo.mjs --skia-archive 'D:/cache/skia.tar.gz' build --release --locked
```

The baseline runner also uses Node.js:

```powershell
node scripts/baseline/run.mjs --offline --samples 5
```

An explicit Skia archive must match the pinned target, features, SHA-256 and embedded
version/key. CMake is still prepared automatically. The old Windows-only cache
manifest/resolver has been replaced by [scripts/dependencies](../dependencies/README.md).
The Node.js build and baseline entry points scope environment changes to children.
The build entry point forwards Cargo's exit code. Bare Cargo does not bootstrap
external dependencies.
