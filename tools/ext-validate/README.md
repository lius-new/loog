# Extension validation with Loom's loaders

`loom-ext-validate` checks an unpacked extension using the production manifest
rules, WebAssembly language loader and query compilers. It has no GUI or concrete
grammar dependency. Those modules are compiled directly from Loom's `src/`.

```sh
cargo build --locked --manifest-path tools/ext-validate/Cargo.toml --bin loom-ext-validate
tools/ext-validate/target/debug/loom-ext-validate <extension-folder> --samples samples.json
cargo test --locked --manifest-path tools/ext-validate/Cargo.toml --test cli
cargo fmt --manifest-path tools/ext-validate/Cargo.toml -- --check
```

On Windows the executable ends in `.exe`. Native build prerequisites follow
[Building Loom](../../docs/building.md); no Skia or application build is needed.
Pass `--offline` to Cargo when its dependencies are already cached.

The optional samples file is a JSON array. Each entry needs a grammar name and
a UTF-8 sample file path (relative paths use the current working directory):

```json
[{"grammar": "json", "sample": "samples/example.json"}]
```

Every grammar's module, highlight query and injection query are checked, even
without samples. Every supplied sample must parse without errors and produce
at least one recognized highlight capture. Several samples may name the same
grammar; their capture counts are added. Successful validation prints one JSON
object per grammar with `grammar` and `colored`. Without samples, `colored` is
zero. Invalid resources or samples produce a nonzero exit status and an error
on stderr. Themes and other resource-only extensions can be checked without
language samples; validation does not launch language servers.

The CLI regression tests cover bundled JSON and both Markdown grammars, themes,
invalid modules and queries, malformed samples and parse failures. They use
`tests/fixtures/extensions` and do not download grammar packages. The CI
`validator` group runs these tests on Linux, Windows and macOS when related
sources change, or during full validation.

Application releases include the validator for Windows x64, Linux x64 and macOS
arm64. The independent [extensions repository](https://github.com/lius-new/loom-extensions)
pins published binaries in `tools/validator.lock`, or uses a local build named by
`LOOM_EXT_VALIDATE`. Packaging and publication live in that repository; the
retired language-pack producer and runtime suites are not part of this tool.
