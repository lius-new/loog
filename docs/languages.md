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
| Registry | The signed extension index lists languages available in **Extensions → Browse**. For a file without syntax, the language panel offers the extension that adds it. |
| Installed | A `.loomext` package that contributes highlighting (see below), installed from the Extensions page. |

The user's extensions are in `%APPDATA%/Loom/extensions/` on Windows,
`~/Library/Application Support/Loom/extensions/` on macOS and
`$XDG_DATA_HOME/loom/extensions/` (otherwise `~/.local/share/loom/extensions/`) on
Linux. `XDG_DATA_HOME` must be absolute; locations never depend on the working
directory. Grammars are read when Loom starts: install, remove, enable or disable
an extension and restart, including languages installed from the registry.

## The highlight panel

Click the language badge at the right end of the status bar, or run **Show
Highlighting** (`workspace::ShowHighlighting`), to see what Loom can highlight.
The panel has four groups:

| Group | What it lists |
| --- | --- |
| Active | The languages of the open files. |
| All | Every language Loom has syntax for. |
| Recommended | Languages not installed whose file types the open files have, each with an Install button. |
| More | The other languages the registry offers, open each time and foldable; the field in its header filters them by name. |

Active and All show only each language's short mark; hold the pointer over one
and its name appears in the footer. Install downloads the extension in the
background; languages are read when Loom starts, so a banner at the top says
which ones wait for a restart until then. Languages come from the cached or
shipped registry index at once, and a newer index is fetched in the background.
Esc closes the panel; a press outside it does too.

## A highlight extension

List the pack's `tree-sitter.json` under `contributes.highlight`. What an
extension brings is named by what it is for: `highlight` and `highlight_queries`
are syntax colouring, `language_service` (`servers` and `providers`) is what
answers hints and diagnostics, `themes` are colour themes.

~~~json
{
  "schema": 1,
  "id": "acme.weave",
  "version": "1.0.0",
  "contributes": {
    "highlight": [{ "id": "acme.weave.grammar", "path": "grammars/weave/tree-sitter.json" }]
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
{ "contributes": { "highlight_queries": [
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
their description lists words.

The description above is a file named `language.json` in a folder of its own,
referenced from `contributes.highlight` like a grammar pack's `tree-sitter.json`:

~~~json
{ "id": "acme.sqlish.language", "path": "language/language.json" }
~~~

Its `name` must be a canonical name (lowercase letters, digits and underscores). A
folder may hold a `tree-sitter.json` as well; the description in `language.json`
then replaces the one Loom makes from the grammar, so a grammar pack can set its
own comment markers, badge and file rules. The older `package.json` pack format is
no longer read.

## Installing from the registry

Open **Extensions → Browse** to fetch the signed index, or use **Refresh** to fetch
it again. A cached index or the snapshot shipped with Loom supplies the list while
offline. Signatures, expiry and a monotonic sequence protect the index; an expired
index offers no new installations or updates. Files do not trigger automatic
grammar downloads. **Find an Extension for This File** opens matching extensions,
and the language panel offers installation when the current index has a match.

Choose **Install** or **Update**. When an extension needs capabilities such as
running a language server, Loom first explains them and asks for acceptance.
Required runtime extensions are installed with it. Downloads must match the
signed index's size and SHA-256; extraction rejects traversal, links, special
files, duplicate paths and oversized payloads. Each package must have a single
root directory named after its extension id and a matching manifest.

Installed versions live in `extensions/<id>/<version>/` and
`extensions/extensions.json` records their source, enabled state, pinned version
and previous version. **Pin Version** excludes an extension from updates;
**Go Back** selects its previous version and pins it. An installed runtime cannot
be removed while another installed extension requires it. Restart Loom after
installing or changing a language extension to use its syntax. Installing a
language may also name recommended server extensions; those are installed
separately from Browse.

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

To run a source build with the extensions of a packaged application,
stage them before launching (Cargo does not copy runtime resources):

~~~sh
node scripts/build/stage-resources.mjs
node scripts/build/cargo.mjs run --locked
~~~

The default directory is `target/debug`; pass another executable directory when
needed. The bundled extensions (`loom.theme.aura`, `loom.lang.json`,
`loom.lang.markdown`) are locked in `assets/registry/extensions.lock`, a generated,
git-ignored file that staging writes from the live signed index when it is missing.
Staging fetches their packages from the independent
[extensions repository](https://github.com/lius-new/loom-extensions) and checks
their hashes. Packaging copies them to `extensions/` on every platform
with `scripts/package/stage-extensions.mjs`, after
checks that stop on a missing module, a native library standing in for a grammar,
a manifest whose id differs from its folder, or a link in a package. Grammars are
WebAssembly, so macOS has nothing to sign for them; the application itself is
signed with `allow-jit` for Wasmtime.

The extension repository owns recipes, package publication and the signed index.
The lock and its signed index snapshot are copied to `registry/` beside the
application's extensions. Refresh the lock and snapshot together before an
application release. Test fixtures remain in `tests/fixtures/extensions`; they are
not the source of the bundled packages.

The standalone [extension validator](../tools/ext-validate/README.md) compiles the
production manifest rules, language loader and query compilers. It checks packages
and optionally parses and colours samples. Its CLI regression tests run against
the checked-in WASM fixtures on each selected CI platform:

~~~sh
cargo test --locked --manifest-path tools/ext-validate/Cargo.toml --test cli
~~~
