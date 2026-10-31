# Tree-sitter language resources

Loom loads standard Tree-sitter language modules through its generic engine and
WASM runtime. A language installation needs upstream tree-sitter.json, a compatible
prebuilt module, and the query files it references. No Loom-specific package.json,
SOURCE.md, grammar crate, source edit or editor rebuild is required. Plain text
works without installed resources. All languages, including JSON and Markdown,
load modules and queries only when displayed content needs them.

Implementation boundaries and highlighting contracts are in
[Language services architecture](architecture/language-services.md). See
[remaining work](language-services-todo.md) for implementation and acceptance gaps.

## Installation and discovery

At startup Loom scans immediate child directories in these locations:

| Source | Location |
| --- | --- |
| Application, Windows/Linux | language-packs/ beside the executable |
| Application, macOS | Loom.app/Contents/Resources/language-packs/ |
| User, Windows | %APPDATA%/Loom/language-packs/ |
| User, macOS | ~/Library/Application Support/Loom/language-packs/ |
| User, Linux | $XDG_DATA_HOME/loom/language-packs/, otherwise ~/.local/share/loom/language-packs/ |

XDG_DATA_HOME must be absolute. Locations do not depend on the working directory.
On Windows, user language resources share `%APPDATA%/Loom` (Roaming) with settings
and sessions. `%LOCALAPPDATA%/Loom` is not used for user data.
Missing directories are valid and discovery does not create them. Directory names
may retain upstream names, such as tree-sitter-rust; they need not equal a grammar
name. Directory symlinks and unrelated folders are ignored. Restart after manually
installing, removing or updating resources. Catalog downloads activate without restart.

## Language packs inside extension packages

An extension package can carry a language pack. List the pack's `tree-sitter.json`
(or `package.json`) under `contributes.languages` in the package manifest:

~~~json
{ "contributes": { "languages": [
  { "id": "acme.weave.grammar", "path": "grammars/weave/tree-sitter.json" }
] } }
~~~

The folder that file is in is read like any other collection, with the same
files and rules. At startup it is discovered after the application's collections
and before the user's: it cannot replace a bundled collection (the duplicate is
reported and refused), and a collection in the user location replaces it. The
file must stay inside the package after resolving links. Extension packs are
read at startup, so install or change one and restart Loom; `workspace::ShowExtensions`
lists a pack that does not pass the same checks. The pack runs no code of its
own: grammars are WASM modules loaded by the language runtime as usual.

## Automatic installation from the bundled catalog

At startup Loom reads `language-packs.toml` beside the bundled collections once.
It registers the catalog's language identities and file associations without
downloading modules. Installed application/user collections remain authoritative;
the catalog supplies languages that do not overlap installed collections.
Explicitly disabled legacy user collections stay disabled.

When visible content first needs a missing grammar, an independent background
queue installs its package. Requests for the same package are deduplicated and at
most two downloads run at once. Embedded languages use the same queue when their
regions become visible. Editing remains available with plain text highlighting
while resources are pending. The language status panel shows progress/failures and
offers **Retry downloads**. A failed request has a 30-second retry cooldown;
explicit retry bypasses it. Missing resources are never cached as permanent module
failures. Installation completion invalidates syntax caches and reparses the
current visible documents without an edit, reopening a tab or restarting Loom.

Archives use the fixed GitHub repository, release tag and asset in the shipped
catalog. Size and SHA-256 must match. Extraction rejects traversal, links, special
files, duplicate paths and oversized payloads. The package's grammars and file
associations must match the catalog; only WASM modules are accepted for automatic
installation, and Loom validates them with its real loader and query compilers
before activating the complete package. Manual native collections remain supported.

Verified archives and complete collections are kept under `.managed/` in the user
language-packs directory, keyed by archive hash. They do not modify application
files or user-installed collections. Cached archives work offline and are verified
again when requested in a new process. An absent catalog leaves installed-resource
loading available; an invalid catalog is reported and ignored. The application
does not poll or download a newer catalog or upgrade existing user/application
collections. A new application release supplies the next catalog snapshot.

## Install Rust using upstream downloads

Use a build containing standard-manifest support. Download these v0.24.2 files,
keeping their names and query subdirectory:

| File | Download |
| --- | --- |
| tree-sitter.json | [Upstream configuration](https://raw.githubusercontent.com/tree-sitter/tree-sitter-rust/v0.24.2/tree-sitter.json) |
| tree-sitter-rust.wasm | [Official release module](https://github.com/tree-sitter/tree-sitter-rust/releases/download/v0.24.2/tree-sitter-rust.wasm) |
| queries/highlights.scm | [Highlight query](https://raw.githubusercontent.com/tree-sitter/tree-sitter-rust/v0.24.2/queries/highlights.scm) |
| queries/injections.scm | [Injection query](https://raw.githubusercontent.com/tree-sitter/tree-sitter-rust/v0.24.2/queries/injections.scm) |

~~~text
Loom.exe
language-packs/
  tree-sitter-rust/
    tree-sitter.json
    tree-sitter-rust.wasm
    queries/
      highlights.scm
      injections.scm
~~~

Alternatively put the entire tree-sitter-rust directory under the user location.
Restart Loom, then open a .rs file. No manifest rewrite, DLL, CLI, SDK or compiler
is needed. The WASM bytes work across supported operating systems and CPUs.
The verified module SHA-256 is
24c89bd9252255e4aebbcbd7d2d308bd92c86dd95a130fdc80efa49577b8d738.

For redistributed resources, retain upstream licenses, such as
[the Rust grammar LICENSE](https://raw.githubusercontent.com/tree-sitter/tree-sitter-rust/v0.24.2/LICENSE).
Loom does not require a specially named provenance file to recognize a language.

This enables syntax recognition and highlighting. LSP is independent: the
application retains its Rust rust-analyzer definition and user server overrides.
Installing syntax resources does not itself run an installer or start a server.

## Standard metadata and resource resolution

Loom reads the grammars array from tree-sitter.json. Each grammar's name selects
its standard tree_sitter_<name> entry point. Names use lowercase ASCII letters,
digits and underscores, starting with a letter. Metadata, bindings, schema URLs
and other upstream fields do not need to be removed. One manifest can declare
several grammars, such as Markdown block/inline or TypeScript/TSX.

file-types are filename suffixes, as in Tree-sitter, including compound suffixes,
whole filenames and dotfiles. Matching keeps Loom's ASCII-insensitive behavior;
whole filenames match first, then the longest dot-separated suffix; trailing
letters alone never identify a language (notes does not match ts). Conflicting identical suffix claims are rejected.
The standard file-types remain authoritative; application presentation, aliases
and comment editing markers live separately in
src/infrastructure/language_defaults.json. A new grammar works without an entry
in that optional defaults table. These defaults contain no parser or query code.
Language servers are not part of a language or its package: each server entry in
the language server catalog (assets/lsp-servers/lsp-servers.toml, generated by the
publication workflow) names the languages it serves, so a language can have several
servers. Loom ships no server definitions in its source.

highlights and injections accept a string or ordered array of relative paths.
Paths are relative to the directory containing tree-sitter.json, including when
a grammar's path points into a subdirectory. Defaults are queries/highlights.scm
and queries/injections.scm. Multiple queries are joined in declared order with
newlines. Every declared highlight file is required; injection files are optional.
Missing required queries fail only when the grammar is used and the failure is cached.

path defaults to a dot and identifies the grammar subdirectory. For each grammar,
Loom looks first for tree-sitter-<name>.wasm, then <name>.wasm. For each filename it
checks the manifest directory, then the grammar's path directory. Native fallbacks
are <name>.<platform-extension>, libtree-sitter-<name>.<platform-extension>, then
parser.<platform-extension>, in the same two directories. The extensions are dll
on Windows, so on Linux and dylib on macOS. All WASM candidates precede native
ones; a selected corrupt WASM never silently falls back to native execution.
Native libraries must match the host OS/CPU and export the standard C entry point.
WASM must be a compatible standard Tree-sitter language module, including its
dynamic-link metadata, rather than an arbitrary WASI program or component.

Paths may not escape the collection: absolute paths, parent traversal, Windows
prefixes and backslash paths are rejected; actual module/query reads also check
canonical containment. References into a node_modules subdirectory are supported
when the required files are supplied inside the collection. Loom never downloads
query dependencies or builds parser source. Queries must match the module version
and the captures/predicates supported by Loom; editor-specific query extensions
are not implicitly supported.

This change supports standard syntax resource installation, not every Tree-sitter
CLI feature. first-line-regex/content-regex detection, injection-regex matching,
locals-based semantic coloring and tags/navigation are not evaluated in this
version. Canonical language names and existing application aliases still resolve
injections. Ambiguous conflicting file claims are rejected rather than guessed.

## Overrides, lifecycle and compatibility

Collections are processed in sorted directory order, application first, user last.
The first grammar name identifies a collection internally. A user collection with
the same identity or complete set of grammar names replaces the application copy
atomically, even with another folder name. A replacement supplies its own queries
and modules; it never borrows missing files from the application or Cargo output.
Invalid metadata leaves the application copy available. Different collections
cannot claim the same grammar, language/alias or identical file suffix.

Discovery reads only manifests and constructs the process registry. Browsing,
search and restoring hidden tabs do not load modules or queries. The syntax worker
loads requested resources in the background. The first WASM request initializes
one shared Wasmtime engine; each document/injection parser owns its own execution
store. Languages, queries and failures are cached. Off-screen embedded languages
remain unloaded. A bad module, export, ABI or query degrades that language to plain
text while other collections continue working. Native handles remain alive until
process exit; WASM languages are reference-counted. Removing the installed
collection and restarting removes its installed copy; a catalog entry may still
offer that language for automatic installation.

Existing installed Loom package.json collections remain readable as a migration
fallback. When tree-sitter.json exists it is authoritative, even if invalid; Loom
never falls back to the legacy manifest in that directory. Legacy collections
keep their old modules/ and grammars/ paths, replacement and enabled behavior.
SOURCE.md is not read by the loader and is never required by the packager. New installations
and all repository templates use tree-sitter.json and standard query paths.

Limits remain bounded: 128 directories per discovery root, 128 active collections,
1 MiB per manifest, 128 grammars, 128 languages and 64 server definitions,
64 native modules and 64 cached WASM languages, 32 MiB per WASM, 4 MiB per combined
query and 8 MiB total cached query text. A grammar may list at most 128 query paths
per query kind and 512 file-types.

## Builds, packaging and verification

The host contains no concrete grammar dependency or linked language producer.
Its generic Tree-sitter WASM runtime uses the private CMake prepared by
[scripts/dependencies](../scripts/dependencies/README.md) when building Loom.
Users of a compiled application need no compiler. Host commands work without any
language resource directory:

~~~sh
node scripts/build/cargo.mjs build --locked
node scripts/build/cargo.mjs test --locked --workspace
node scripts/architecture/standalone-host.mjs --offline
~~~

To run a source build with the same catalogs as a packaged application, stage
both syntax and LSP resources before launching:

~~~sh
node scripts/build/stage-resources.mjs
node scripts/build/cargo.mjs run --locked
~~~

The default executable directory is `target/debug`; pass `target/release` or a
custom target/profile directory when appropriate. Cargo itself does not copy
runtime resources. Staging only `language-packs` makes syntax available but does
not register any catalogued language servers. Syntax download status is temporary;
its disappearance does not mean a server has started. Restart after staging catalogs.

assets/language-packs contains complete Markdown and JSON packages, including
WASM modules, standard metadata, queries, licenses and provenance, plus the
publication catalog. Both packages remain in scripts/language-packs/build.toml
and use the same [batch producer](../scripts/language-packs/README.md) as other
languages. Their validated WASM files were manually placed in the bundled
packages; producer runs do not rewrite these checked-in resources.
The catalog contains only successfully published artifacts.
Rust's unchanged upstream regression resources live under
tests/language-runtime/fixtures/rust. Cargo does not build or install grammars.

Application packaging consumes assets/language-packs directly on all platforms:

~~~sh
node scripts/package/stage-languages.mjs /path/to/app/language-packs
~~~

The consumer preserves tree-sitter.json, module bytes and declared query paths,
and copies the publication catalog. No language resource feed, archive download
or grammar compilation is part of application packaging. macOS signs native
modules and the application, preserves WASM bytes and enables Wasmtime JIT with
allow-jit. Runtime on-demand installation uses the shipped catalog as described
above; installed resources and verified cached archives continue to work offline.

The independent runtime workspace compiles production loader, syntax and worker
sources against supplied modules. Its default WASM-only regression installs the
unchanged upstream Rust files without package.json or SOURCE.md:

~~~sh
node tests/language-runtime/run-wasm.mjs
node tests/language-runtime/run-wasm.mjs --wasm-rust /path/to/tree-sitter-rust.wasm --offline
node tests/language-runtime/run.mjs --packages /path/to/prebuilt-packages --wasm-rust /path/to/tree-sitter-rust.wasm --offline
~~~

Online test tooling caches a pinned upstream WASM input under .packaging/downloads;
offline runs need that cache or an explicit file. Every CI platform runs the
WASM-only suite, including the complete checked-in JSON and Markdown packages.
Installation probes keep the executable hash unchanged while testing application/user discovery,
resource removal and restart, highlighting, lazy loading and failure isolation.
The native baseline accepts both standard and legacy collection layouts and also
covers legacy collection compatibility with an isolated test-only metadata view.
