# Language-package production

These tools produce Loom's own Tree-sitter packages in this repository,
independently of Cargo and the application installer build.

## Two manifests

- `scripts/language-packs/build.toml`: maintained producer recipes, pinned CLI,
  WASI SDK, Binaryen, source commits/archive hashes, queries, licenses and samples.
  It has no release tags or output hashes.
- `assets/extensions/registry.toml`: the extension registry. It lists successfully
  published package names, grammar/file associations, fixed resource tags, archive
  hashes and sizes. It has no build recipes and is copied to the application
  installation. A listed package is installed on demand as the extension
  `loom.lang.<name>`.

The initial publication catalog is empty: recipes do not imply that a package
already exists remotely. Only Markdown and JSON remain under `assets/extensions`
(`loom.lang.json`, `loom.lang.markdown`), as complete extensions including WASM.
Other inputs come from the producer recipes.

## Build

Node 20+, Git and the repository Rust toolchain are required. The independent
runtime validator uses the existing private CMake preparer; no GUI build is needed.

```sh
npm ci --ignore-scripts --prefix scripts/language-packs
node scripts/language-packs/check.mjs --all
node scripts/language-packs/build.mjs --all
node scripts/language-packs/build.mjs --packages python,rust
node scripts/language-packs/build.mjs --all --offline
```

All downloaded tools, cached sources and outputs stay under
`.packaging/language-packs` (or `--work`). Offline runs require a populated cache
and Rust dependencies. Tool archives are pinned for Windows, Linux and macOS on
x64/arm64. The resource workflow builds on Linux and validates identical resulting
WASM files on Windows and macOS.

Source archives are checked against SHA-256. Internal archive links are copied
within the extraction root so Windows needs no symlink privileges. A source uses
either `repository`/`revision`/`sha256` or `directory` relative to this checkout.
Dependencies explicitly reference another pinned source and a contained target
path. No upstream install scripts or implicit npm installs run.

Each grammar compiles its existing `src/parser.c`; `generate = true` explicitly
regenerates it with the pinned CLI/ABI. An explicit `prebuilt` source-relative
path can instead adopt an upstream module pinned by the source archive; it still
passes exactly the same runtime validation and is recorded in provenance. Git
Commit instead sets `optimize = false`: it uses the same pinned WASI compiler and
shared-module flags, omitting its unusually slow optional Binaryen pass. Its
repository's old prebuilt module is incompatible and is not distributed.
Its lexer also needs `iswcntrl`, which Tree-sitter's miniature libc does not
export. The explicit `support` reference links the small WASI libc implementation
under `support/`, with its source attribution and COPYRIGHT included in the pack.
Query references are ordered inline tables
with `path` and optional `source`. Paths are relative to the referenced source
root, not the grammar subdirectory. `capture-map` explicitly maps upstream capture
tokens to Loom classes, preserving strings and comments; helper captures starting
with `_` carry no color. Unsupported predicates still fail validation. Optional
package `patches` can modify staged query files only. Small query overrides are
under `queries/` with their license retained.

The validator uses Loom's real loader, WASM store and highlight/injection query
compilers. Every declared sample must parse without errors and produce highlight
captures. Failed packages are reported and make the batch fail. Compiled modules
are cached by source/toolchain inputs and validated again when reused.

```text
.packaging/language-packs/
  stage/<id>/              complete standard resource directories
  dist/<id>.tar.gz         one top-level <id>/ directory
  dist/SHA256SUMS
  dist/build-report.json
```

Markdown and JSON use this same producer and produce individual archives like
other packages. Their validated WASM files have been manually placed in the
complete extensions under assets/extensions. Application packaging consumes
that checked-in directory directly. The producer does not copy results back
there and does not create an installer collection archive.

Archives have stable ordering, ownership metadata and timestamps. They contain
standard `tree-sitter.json`, WASM, assembled queries, licenses and provenance.
TypeScript/TSX and Markdown block/inline are multi-grammar packages. The archive
can be manually installed with the existing loader. To validate downloaded build
artifacts on another platform:

```sh
node scripts/language-packs/validate.mjs
```

The 50-package scope covers C, C++, Rust, Go, Zig; C#, Java, Kotlin, Swift,
Objective-C, Dart, Scala; Python, PHP, Ruby, Lua, Perl, R, Julia; JavaScript/JSX,
TypeScript/TSX, HTML, CSS, SCSS, Vue, Svelte; JSON, JSON5, YAML, TOML, XML, INI;
Bash, PowerShell, Fish; Dockerfile, Make, CMake, HCL, Nix; Markdown,
reStructuredText, LaTeX; SQL, GraphQL, Protocol Buffers; Diff, Git Ignore,
Git Attributes and Git Commit. This covers syntax resources, not LSP installation
or every dialect.

## Prepare and publish

```sh
node scripts/language-packs/publish.mjs prepare --release language-packs-001
node scripts/language-packs/publish.mjs verify
```

Preparation uses the latest complete local build report. It checks current input
fingerprints and every archive, then generates a candidate `dist/language-packs.toml`
and `dist/publication.json`. These are generated outputs. Existing published
entries are retained for partial batches. Preparation performs no remote writes
and does not replace the catalog in `assets`.

Download addresses are derived as:

```text
https://github.com/<repository>/releases/download/<release>/<id>.tar.gz
```

Review the candidate/report and commit the producer inputs before upload. The
publisher requires a clean checkout so the resource tag identifies an exact
producer commit. Authenticate GitHub CLI for this repository, then run:

```sh
node scripts/language-packs/publish.mjs publish --draft-only
node scripts/language-packs/publish.mjs publish
```

The publisher creates or resumes a draft, verifies existing assets instead of
overwriting, uploads missing assets and verifies server hashes. An interrupted
upload is resumable. Different existing assets require a new tag. `--draft-only`
never promotes the catalog. Normal publication confirms the release is public
before atomically promoting the candidate to the manifest under `assets`.
Commit that manifest before shipping Loom. Failures leave the old catalog intact.

Resource tags start with `language-packs-` and are explicitly **not Latest**:
the app updater uses `releases/latest/download`. Existing assets are never
uploaded with `--clobber`. Older releases remain available for installed Loom
versions that reference them. No scheduled upstream updates or fallback provider
is involved.

The manually dispatched `.github/workflows/language-packs.yml` defaults to build
and validation only. Selecting `publish` from a branch also publishes, exports the
promoted catalog as an artifact, and commits and pushes it back to that branch.
The resource tag still identifies the producer commit; the catalog follows in a
separate bot commit. An unchanged catalog needs no new commit.

Before pushing, the workflow rebases the catalog commit onto the latest branch
tip, preserving unrelated changes. A catalog conflict or a rejected push fails
the job without force-pushing. The resource release remains published and the
`released-language-package-catalog` artifact remains available for recovery;
download and commit it manually if necessary. Branch rules must allow the
workflow token to push the catalog commit. Publication from a tag is rejected
before uploading resources because there is no target branch to synchronize.

```sh
node --test scripts/language-packs/pipeline.test.mjs scripts/language-packs/catalog-sync.test.mjs
```

The application consumes the shipped catalog for on-demand, verified WASM
installation; see [runtime behavior](../../docs/languages.md#automatic-installation-from-the-bundled-catalog).
Catalog refresh and remote update polling are not part of that consumer.
