# Native Rust language-client fixture

`extension.wasm` runs the production private Rust client against
`examples/fake_lsp.rs`. Its entry point is in `lius-new/loog-extensions` at
`language-clients/fixtures/native-ui/rust/src/lib.rs`; it imports the production
`language-clients/rust/src` modules directly. A test-only process wrapper supplies
fake-server arguments from `example.language-fixture.options`. No language-server
binary or JavaScript engine is embedded. The committed binary lets ordinary Loog
tests run without a sibling extension checkout or a local Rust WASM build.

Build in the extension repository with `LOOG_SOURCE` pointing to this checkout:

```text
node language-clients/fixtures/native-ui/build.mjs
```

Copy `.work/client/extension.wasm`, `build.json` and `licenses/` from the fixture
directory here. The builder uses locked Cargo dependencies, componentizes the
Rust module and records hashes of the fixture, shared client, componentizer and
WIT inputs in `build.json`. Keep this README's hashes in sync after rebuilding.
The hashes describe the original build inputs. This pre-motion component is
intentionally retained to verify that adding the independent motion interface
does not break existing base/tree components.

- Source fingerprint: `29f898c08584eb9422e2215b1adc67ea0d3bda1b8cd8e447c573b7c03829c63c`
- WASM SHA-256: `bf40e8849eb0a8a0516ca2c9e990486a1c88d7bb5af24ae1f336f158489ed3de`
- Rust toolchain: `rustc 1.98.0 (88d9e12ae 2026-08-18)`
- Contract: `loog:extension@0.1.0` plus `loog:ui/tree@0.1.0`.
- Dependency versions: the fixture and componentizer `Cargo.lock` files in the
  extension repository; both are included in the input fingerprint.

The fixture/client sources use the extension repository's MIT OR Apache-2.0
license, retained under `licenses/loog-client-*`. Rust dependency notices and
licenses are retained in `licenses/CLIENT-NOTICES.txt` and `licenses/rust/`.
