# Languages and syntax highlighting

Every language Loom highlights comes from an extension. Loom itself contains the
Tree-sitter engine (a generic WebAssembly runtime, incremental parsing, the
background worker and its caches) and a small built-in highlighter for text no
grammar covers; grammars, highlight queries and language descriptions are data
that extensions bring. Plain text needs nothing.

Implementation boundaries and highlighting contracts are in
[Language services architecture](architecture/language-services.md).

## Where languages come from

| Source | How it gets there |
| --- | --- |
| Bundled | `loom.lang.json` and `loom.lang.markdown` ship with Loom in `extensions/` beside the executable (`Loom.app/Contents/Resources/extensions/` on macOS). They can be disabled, not uninstalled. |
| Registry | `registry.toml`, next to the bundled extensions, lists the other languages Loom can install. When visible content needs one, it is downloaded and installed as the extension `loom.lang.<name>`. |
| Installed | A `.loomext` package that contributes a language (see below), installed from the Extensions page. |

The user's extensions are in `%APPDATA%/Loom/extensions/` on Windows,
`~/Library/Application Support/Loom/extensions/` on macOS and
`$XDG_DATA_HOME/loom/extensions/` (otherwise `~/.local/share/loom/extensions/`) on
Linux. `XDG_DATA_HOME` must be absolute; locations never depend on the working
directory. Grammars are read when Loom starts: install, remove, enable or disable
an extension and restart. Languages installed from the registry work at once.

## A language extension

List the language pack's `tree-sitter.json` under `contributes.languages`:

~~~json
{
  "schema": 1,
  "id": "acme.weave",
  "version": "1.0.0",
  "contributes": {
    "languages": [{ "id": "acme.weave.grammar", "path": "grammars/weave/tree-sitter.json" }]
  }
}
~~~

The folder that file is in is the pack: upstream `tree-sitter.json`, the grammar's
WebAssembly module and the query files it names. The path must stay inside the
package after resolving links. The pack runs no code of its own. **Only
WebAssembly grammars are accepted**: they run in a sandbox, so an extension cannot
execute native code inside Loom.

Loom reads the `grammars` array of `tree-sitter.json`. A grammar's `name` selects
its `tree_sitter_<name>` entry point and uses lowercase ASCII letters, digits and
underscores, starting with a letter. One file can declare several grammars
(Markdown block and inline, TypeScript and TSX). `file-types` are filename suffixes
as in Tree-sitter, including compound suffixes, whole filenames and dotfiles;
matching is ASCII-insensitive, whole filenames match first, then the longest
suffix. Conflicting identical claims are rejected.

`highlights` and `injections` take a relative path or an ordered array; they are
relative to the directory holding `tree-sitter.json`. The defaults are
`queries/highlights.scm` and `queries/injections.scm`; several files are joined in
order. Every declared highlight file is required, injection files are optional, and
a missing one fails only when the grammar is used. The module is the first of
`tree-sitter-<name>.wasm` and `<name>.wasm`, looked for in the pack directory and
then in the grammar's `path`. Absolute paths, parent traversal and backslashes are
rejected, and reads check containment.

The application's own presentation data for a language (display name, badge,
aliases, comment markers) is in `src/infrastructure/language_defaults.json`; a
grammar without an entry there still works.

## Changing how code is coloured

Highlighting is decided by the grammar's `highlights.scm`. You can add to it or
replace it without touching the extension that owns the grammar.

**Your own queries.** Put `<grammar>/highlights.scm` (or `injections.scm`) in your
queries folder: `%APPDATA%/Loom/queries/` on Windows, `.../Loom/queries/` under
Application Support on macOS, `$XDG_DATA_HOME/loom/queries/` on Linux. Saving the
file is enough; open documents are coloured again at once.

**Queries from an extension.** Declare them in the manifest, for any grammar:

~~~json
{ "contributes": { "queries": [
  { "id": "acme.rust-extra", "grammar": "rust", "file": "highlights.scm", "path": "queries/rust.scm" }
] } }
~~~

**Stacking.** The grammar's own query is at the bottom, then extensions in id
order, then yours. A layer whose first non-blank line is a comment containing
`extends` (`; extends`) is appended to what is below it; any other layer replaces
everything below it. When two patterns colour the same range, the later one wins,
so an appended pattern overrides the one it competes with. For different ranges the
shorter one wins.

**Mistakes cost only the layer.** Each layer is compiled against the grammar. One
that does not compile (an unknown node, an unsupported capture) is left out, the
layers below it stay, and the language keeps its colours. The status bar shows how
many layers were left out; **Show Query Problems** lists them with the file and the
error.

**Finding the rule.** **Inspect Highlight at Cursor** parses the file and lists
every capture that covers the caret, the class each gives and the theme colour it
resolves to, which one wins (`>`), and the query file and line it was written on.
Embedded languages are not looked into.

Capture names are highlight classes (`@function.method`); a name that is not known
falls back to its parent (`@keyword.control` is a `@keyword`), and `@escape` and
`@label` are accepted as `string.escape` and `type`. The text predicates `#eq?`,
`#not-eq?`, `#match?`, `#not-match?`, `#any-of?` and `#not-any-of?` are supported,
and `#is-not? local` is accepted (there is no scope analysis, so such a pattern
applies to every identifier it matches). Other predicates and `#set!` are errors.
Themes decide the colours; see the theme files for the class names.

## Languages without a grammar

A language description may carry a `highlight` entry, and the built-in highlighter
uses it when no grammar covers the language (or the file is too large to parse):

~~~json
{
  "name": "sqlish", "display_name": "SQL-ish", "badge": "SQL",
  "comments": { "line": "--", "block": ["/*", "*/"] },
  "highlight": {
    "keywords": ["select", "from", "where"],
    "constants": ["null"],
    "strings": ["'"],
    "numbers": true
  },
  "variants": [{ "id": "sqlish" }],
  "files": [{ "extensions": ["sqlish"], "variant": "sqlish" }]
}
~~~

A list given here replaces the built-in one; a field left out keeps it
(`strings` defaults to `"`, `'` and a backtick). The highlighter reads one line at
a time and colours comments, strings that end on the same line, numbers and the
listed words; it never guesses at functions or types. Data and markup languages
(Markdown, JSON, TOML, YAML, INI, HTML, CSS, XML) get no keyword colouring unless
their description lists words. A language like this is a `package.json` language
description with `"grammars": {}`:

~~~json
{ "schema": 1, "id": "sqlish", "grammars": {}, "languages": [ { "name": "sqlish" } ]  }
~~~

(with the full description above in place of the abbreviated one), referenced from
`contributes.languages` like a grammar pack.

## Installing from the registry

`registry.toml` is read once at startup. It registers the listed languages' names
and file associations without downloading anything. When visible content first
needs a missing grammar, a background queue downloads the package: requests for the
same package are deduplicated and at most two downloads run at once. Editing stays
available with the built-in highlighting while the download is pending, the
language status shows progress and failures with **Retry downloads**, and a failed
request waits 30 seconds before trying again (explicit retry bypasses that).
Finishing a download invalidates the syntax caches and colours visible documents
without an edit or a restart.

Archives come from the repository, release tag and asset named in the registry, and
their size and SHA-256 must match. Extraction rejects traversal, links, special
files, duplicate paths and oversized payloads. The package's grammars and file
associations must match the registry; only WebAssembly modules are accepted, and
Loom compiles the module and the queries with its real loaders before it installs
anything. The language is then installed as the extension `loom.lang.<name>`: a
manifest is written beside the files, a `.registry.json` marker records that the
registry installed it, and verified archives are kept in `extensions/.cache/` so a
restart installs offline. An extension of the same name that you installed yourself
is never replaced. The registry is a snapshot shipped with each release; Loom does
not poll for a newer one.

## Lifecycle and limits

Discovery reads only manifests and builds the registry of languages. Browsing,
search and restoring hidden tabs load no module or query. The syntax worker loads
what visible content needs. The first WebAssembly request starts one shared
Wasmtime engine; each document and injection parser has its own store. Languages,
queries and failures are cached. A bad module, ABI or query degrades that language
to the built-in highlighting while the others keep working.

Limits: 128 packs, 1 MiB per manifest, 128 grammars and 128 languages, 64 cached
WebAssembly languages, 32 MiB per module, 4 MiB per combined query and 8 MiB of
cached query text; a grammar may list 128 query paths per kind and 512 file types.

## Builds, packaging and verification

The host has no concrete grammar dependency. Host commands work without any
language extension:

~~~sh
node scripts/build/cargo.mjs build --locked
node scripts/build/cargo.mjs test --locked --workspace
node scripts/architecture/standalone-host.mjs --offline
~~~

To run a source build with the extensions and catalogs of a packaged application,
stage them before launching (Cargo does not copy runtime resources):

~~~sh
node scripts/build/stage-resources.mjs
node scripts/build/cargo.mjs run --locked
~~~

The default directory is `target/debug`; pass another executable directory when
needed. `assets/extensions` holds the bundled extensions (`loom.aura`,
`loom.lang.json`, `loom.lang.markdown`) and `registry.toml`. Packaging copies it to
`extensions/` on every platform with `scripts/package/stage-extensions.mjs`, after
checks that stop on a missing module, a native library standing in for a grammar,
a manifest whose id differs from its folder, or a link in a package. Grammars are
WebAssembly, so macOS has nothing to sign for them; the application itself is
signed with `allow-jit` for Wasmtime.

The registry is produced by the [batch producer](../scripts/language-packs/README.md),
which builds language packages from pinned sources and publishes the archives; the
bundled JSON and Markdown extensions are maintained by hand and producer runs do not
rewrite them. The independent runtime workspace in `tests/language-runtime`
compiles the production loader, syntax and worker sources against supplied modules.
