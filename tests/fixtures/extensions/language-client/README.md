# Native language-client fixture

`extension.wasm` runs the official private client against `examples/fake_lsp.rs`.
Its source is in `lius-new/loom-extensions`, at
`language-clients/fixtures/native-ui/src/main.ts`, sharing the actual production
`client.ts`, `protocol.ts` and `results.ts`. The general SDK contains no language
client. This binary lets the ordinary Loom tests run without a sibling checkout.

Build from the extension repository with `LOOM_SOURCE` pointing to this checkout:

```text
node language-clients/fixtures/native-ui/build.mjs
```

Copy the resulting `.work/client/extension.wasm` here. The builder type-checks,
verifies generated WIT bindings and fingerprints all entry/client/SDK/WIT inputs,
the SDK builder/configuration and the locked component toolchain.

- Source fingerprint: `bbefd9a49ee22c78c7fd51d48b41791eb29467a1e7ed8f57a711e16e7f7e72f8`
- WASM SHA-256: `05df56bb64d6966391781fd2e8f9307f65cca05dc36cb384eb23b9f120041b6e`
- Contract: `loom:extension@0.1.0`, including diagnostic documentation URLs.
- Toolchain: Node 24.19.0, componentize-js 0.23.0, jco 1.40.0; exact dependency
  versions are in `tests/extensions/js-guest/package-lock.json`.

The fixture/client/SDK sources use the repositories' MIT OR Apache-2.0 license.
The embedded runtime is supplied by componentize-js/StarlingMonkey; its Apache-2.0
WITH LLVM-exception license is retained in `LICENSE-runtime`. Upstream runtime
sources and additional notices are available from
[componentize-js](https://github.com/bytecodealliance/ComponentizeJS) and
[StarlingMonkey](https://github.com/bytecodealliance/StarlingMonkey), including its
SpiderMonkey sources. No language-server binary is embedded in this fixture.
