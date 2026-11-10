# Loog patch to tree-sitter

`vendor/tree-sitter` is the `tree-sitter` 0.27.2 crate from crates.io plus one
small change, selected through `[patch.crates-io]` in the root `Cargo.toml`.
The change is stored as `LOOG.patch` (four files,
about 110 lines) and every changed line carries a `LOOG-PATCH(precompiled-wasm)`
marker.

## Upstream baseline

- Repository: https://github.com/tree-sitter/tree-sitter (crate path `lib/`)
- Commit: `3fb7be49145acfb07e33b61ed5689e2b6771b2e9`, published as 0.27.2

Update these when upgrading.

## Why

Wasmtime keeps compiler state in the `Engine` that compiled a module and never
releases it. Compiling the two Markdown grammars in Loog's long-lived engine
left about 38 MB resident; a module loaded from serialized form costs about
0.5 MB. Upstream `WasmStore::load_language` always compiles from bytes, so a
precompiled module cannot be supplied without this patch.

## What it adds

- C: `ts_wasm_store_load_language_precompiled`, a sibling of
  `ts_wasm_store_load_language`. Upstream's body is kept as a private function
  that takes an optional serialized module and calls
  `wasmtime_module_deserialize` instead of `wasmtime_module_new` when given one.
- Rust: `unsafe WasmStore::load_language_precompiled(name, wasm, compiled)` and
  its FFI declaration.

Loog's use is `load_language` in `src/infrastructure/language_packs.rs`: compile
with a short-lived engine, serialize, drop the engine, load the result. Any
failure falls back to the upstream path.

## Grammar compatibility

Grammars are built outside this repository (loog-extensions) with the
`tree-sitter` CLI, so the host and the grammars move independently. The host
accepts grammar ABI `MIN_COMPATIBLE_LANGUAGE_VERSION..=LANGUAGE_VERSION`, which
is 13 to 15 in 0.27.2; `validate_abi` in `language_packs.rs` rejects others. A
newer host keeps loading older grammars, but a grammar built by a newer CLI can
need a newer host.

On 2026-11-10 all 50-odd locally built grammars loaded: ABI 13 (5), 14 (about 20)
and 15 (the rest). Before upgrading the CLI used for grammars, or the vendored
crate, run the opt-in gate against the built modules:

```text
LOOG_GRAMMAR_DIRS=<dir with built .wasm> cargo test --release --bin Loog bundled_grammars -- --ignored
```

## Maintaining it

```bash
node scripts/dependencies/tree-sitter-patch.mjs check
```

fails when the vendored crate is not exactly upstream plus the stored patch.

To upgrade `tree-sitter`:

1. Download the new version from
   `https://static.crates.io/crates/tree-sitter/tree-sitter-<version>.crate`
   (a gzipped tar), note its sha256, and replace the contents of
   `vendor/tree-sitter` with the extracted crate. Keep `LOOG-PATCH.md` and
   `LOOG.patch`, and delete the crate's `.cargo-ok` and `.cargo_vcs_info.json`
   after reading the new commit from the latter.
2. Reapply the change. Try
   `git apply --directory=vendor/tree-sitter vendor/tree-sitter/LOOG.patch`
   from the repository root. If hunks are rejected, redo the edits by hand,
   following the `LOOG-PATCH(precompiled-wasm)` markers in the old patch. On
   Windows `git apply` can write CRLF into `api.h` and `wasm_store.c`; convert
   them back to LF so the regenerated patch stays small.
3. In `scripts/dependencies/tree-sitter-patch.mjs`, add the new version and its
   sha256 to `checksums`. In this file, update the upstream baseline.
4. `node scripts/dependencies/tree-sitter-patch.mjs regenerate`, then `check`.
5. Run the grammar compatibility gate described above and the focused
   `language_packs` tests, and measure that opening a Markdown file still costs
   about the same as opening a plain text file.

Upgrading also moves `wasmtime-c-api`, whose newer builds need CMake; use
`node scripts/build/cargo.mjs`, which provides it. tree-sitter 0.27.2 is the first
release that copies all supertype map entries of a WASM grammar, which queries such
as Haskell's `(pattern/wildcard)` depend on.

Drop the patch when upstream accepts a way to load precompiled modules, or when
`tree-sitter` and Loog share a Wasmtime whose engine releases compiler state.
Check after each upgrade whether either is true. No removal version has been
scheduled.
