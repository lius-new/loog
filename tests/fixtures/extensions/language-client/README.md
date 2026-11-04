# Native Rust language-client fixture

`extension.wasm` runs the production private Rust client against
`examples/fake_lsp.rs`. Its entry point is in `lius-new/loom-extensions` at
`language-clients/fixtures/native-ui/rust/src/lib.rs`; it imports the production
`language-clients/rust/src` modules directly. A test-only process wrapper supplies
fake-server arguments from `example.language-fixture.options`. No language-server
binary or JavaScript engine is embedded. The committed binary lets ordinary Loom
tests run without a sibling extension checkout or a local Rust WASM build.

Build in the extension repository with `LOOM_SOURCE` pointing to this checkout:

```text
node language-clients/fixtures/native-ui/build.mjs
```

Copy `.work/client/extension.wasm`, `build.json` and `licenses/` from the fixture
directory here. The builder uses locked Cargo dependencies, componentizes the
Rust module and records hashes of the fixture, shared client, componentizer and
WIT inputs in `build.json`. Keep this README's hashes in sync after rebuilding.

- Source fingerprint: `29f898c08584eb9422e2215b1adc67ea0d3bda1b8cd8e447c573b7c03829c63c`
- WASM SHA-256: `75352739add46bdd3e2efdb305c24db2c68c518008590716c719030b94df3d4f`
- Rust toolchain: `rustc 1.98.0 (88d9e12ae 2026-08-18)`
- Contract: `loom:extension@0.1.0` plus `loom:ui/tree@0.1.0`.
- Dependency versions: the fixture and componentizer `Cargo.lock` files in the
  extension repository; both are included in the input fingerprint.

The fixture/client sources use the extension repository's MIT OR Apache-2.0
license, retained under `licenses/loom-client-*`. Rust dependency notices and
licenses are retained in `licenses/CLIENT-NOTICES.txt` and `licenses/rust/`.
