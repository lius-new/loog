# Extension development loop

Status: M0/M1 prototype. The contract (`wit/loom-extension/world.wit`) is not frozen.

## Sample extensions: a theme and a language

`tests/extensions/packages` holds two extensions that need no component: a theme
extension (`loom-midnight`, two themes) and a language extension
(`loom-properties`, a real WebAssembly grammar plus snippets). They are what a
new extension author copies, and `src/app/extensions/package_tests.rs` loads
them the way the application does and highlights real text with the grammar, so
a break in loading, themes or syntax highlighting for extensions shows up as a
failing test.

## Pack and import an extension

`node scripts/extensions/package/pack.mjs <folder>` builds a `.loomext` package;
`workspace::ImportExtension` installs one. The manifest must carry the name,
publisher, description, categories, license, repository, icon, readme, changelog
and the oldest Loom version, because the extensions page and the import show
them. See `scripts/extensions/package/README.md`.

## Write an extension in TypeScript

`sdk/typescript` is the TypeScript SDK: types generated from the contract, a
runtime that makes `async`/`await` work across the host's rounds, UTF-8 offset
helpers and a build that type-checks and componentizes. See
`sdk/typescript/README.md`.

## Run the tests

```sh
node scripts/extensions/test.mjs          # everything
node scripts/extensions/test.mjs --quick  # without the JavaScript guest
```

Targeted runs:

```sh
cargo test -p loom-extension-api
cargo test --release -p loom-extension-host --test rust_guest
cargo test --bin Loom -- app::extensions
```

The Rust guest needs `rustup target add wasm32-unknown-unknown`. The JavaScript
guest installs its pinned toolchain (`tests/extensions/js-guest/package.json`)
on first use and writes `target/extension-guest/js_guest.wasm`.

## Try a package in the app

A package is a folder with `manifest.json` and (for behaviour) the component it
names. Put it under the per-user directory Loom reports for `extensions`:

| Platform | Directory |
| --- | --- |
| Windows | `%LOCALAPPDATA%\Loom\extensions\<name>\` |
| macOS | `~/Library/Application Support/Loom/extensions/<name>/` |
| Linux | `$XDG_DATA_HOME/loom/extensions/<name>/` (default `~/.local/share/loom/...`) |

Packages are read at start. After changing one on disk, run
`workspace::ReloadExtensions`; the old instance is stopped, its generation moves
on, and nothing unconfirmed is replayed. A package that changed on disk must be
granted again.

Bind a command in `keymap.json`:

```json
[{ "context": "Editor", "bindings": { "ctrl-alt-u": ["extension::Run", "example.tools.uppercase"] } }]
```

`workspace::ShowExtensions` prints each package's state, failure stage,
permissions, ignored settings, rejected resources and its latest log lines (never
document text). A failed extension stays stopped until retried:
`workspace::RetryExtensions` gives every failed one a new generation, and
`workspace::StopExtensions` stops them all (and the language servers they
declare) until `workspace::ReloadExtensions` brings them back.

## Trust

New folders are untrusted: language servers are not started for them. Run
`workspace::TrustWorkspace` while a file of the folder is active (and
`workspace::RevokeWorkspaceTrust` to undo it). The decision is stored in
`trust/roots.json` next to the extensions directory. The innermost open folder
that holds a file decides: a folder opened inside a trusted folder has to be
trusted on its own.

## Settings

Declare settings under `contributes.settings` (namespaced keys). Users set
values in `<extensions dir>/settings.json`; a trusted folder may override
project-scoped settings in `.loom/extension-settings.json`. Both files are read
at start and on reload.

## Compatibility

The contract is `loom:extension@0.1.0` and it is not frozen: a component is
linked only against the exact interface it was built for, and an enum or record
that changed makes an older component fail at load with a type mismatch. Rebuild
the component with the current `wit/loom-extension/world.wit` after every change
of the contract (the JavaScript guest is rebuilt by deleting
`target/extension-guest/js_guest.wasm`).

| Check | What it covers | Where |
| --- | --- | --- |
| Compatibility fixtures | unknown imports, a newer Loom interface, a missing guest export, a wrong export version, an unused WASI import | `crates/extension-host/tests/compatibility.rs` |
| Rust guest | commands, edits, providers of every kind, tasks, storage, files, network, save participants, file operations | `crates/extension-host/tests/rust_guest.rs`, `src/app/extensions/tests.rs` |
| JavaScript guest | rounds, suspended callbacks resuming, interrupted loops and microtask floods, providers | `crates/extension-host/tests/js_guest.rs` |
| TypeScript SDK | generated types match the contract, offsets and rounds with a fake host, the built component in the real host | `sdk/typescript/test`, `crates/extension-host/tests/ts_guest.rs` |
| Measurements | compile, instantiate and warm round times, component size | `crates/extension-host/tests/measure.rs` (ignored by default) |

| Platform | Rust guest | JavaScript guest | Process tasks and language servers |
| --- | --- | --- | --- |
| Windows x64 | run | run | run |
| Linux x64 | not run | not run | process group path not run |
| macOS | not run | not run | process group path not run |

A platform that was not run counts as not passing; the contract stays unfrozen
until the Linux and macOS rows are filled in.

## Known gaps

No source maps or debugger integration yet; the guests keep line tables only.
Settings files are not watched. Only the first workspace folder supplies project
settings. Linux and macOS are not yet verified.

## Providers

Declare providers under `contributes.providers` (`diagnostics`, `formatting`,
`completion`, `save-participant`, `hover`, `definition`, `document-symbols`,
`signature-help`, `inlay-hints` and `code-actions` are served). A document in a
selected language wakes the extension and the host sends a `provider-request`
for that exact document state; answer it with `provide-diagnostics` or
`provide-edits`. An answer that arrives late or for an older state is discarded
and the current state is asked again. When a formatting provider matches the
language, `editor::Format` uses it instead of the language server.

### Hover, definition, outline, signature, inlay hints and code actions

These providers are asked whenever the language UI asks the language servers,
for the same document state, and need the `documents.read` and `languages`
capabilities. The request carries the caret in `cursor` (hover, definition,
signature help) or a `range` (inlay hints, code actions); the answer goes to
`provide-hover`, `provide-locations`, `provide-symbols`, `provide-signature`,
`provide-inlay-hints` or `provide-code-actions`. Offsets are UTF-8 bytes of the
snapshot; a location is a zero-based line and a UTF-8 column. The host turns the
answer into the reply a language server would give, so the existing hover popup,
go to definition, outline, signature help, inlay hints and lightbulb show it
after the servers' own results. An entry that cannot be placed (outside the text,
inside a character, a parent that is missing) is dropped and logged; a code
action with a bad edit is dropped as a whole. An extension may have 32 requests
open at once; the newest beyond that gets no answer.

## Themes and snippets

A package needs no component to contribute themes or snippets. Both are data:
they need no grant and no trusted folder, each file is checked on its own when
the package loads, and one that is wrong is rejected whole with the reason shown
by `workspace::ShowExtensions` while the package's other files stay in use.

`contributes.themes` lists `{ "id", "path" }`. A theme file starts from a
built-in theme and changes what it names:

```json
{
  "name": "Acme Night",
  "base": "aura-dark",
  "colors": { "bg": "#101418", "accent": "#ff8800", "git.added": "#00cc66" },
  "ansi": ["#000000", "... 16 colors ..."],
  "shadow_alpha": 120,
  "syntax": { "keyword": "#ff8800", "comment": { "fg": "#888888", "italic": true } }
}
```

Color names are the palette fields (`bg`, `sidebar`, `text`, `diff.add_bg`,
`git.added`, `badge.rust` and so on), syntax names the highlight classes
(`keyword`, `function.method`, `string`...). An unknown name, a color that is not
`#rrggbb` or an unknown field rejects the theme. The theme appears in Settings
under the id from the manifest. A theme chosen in settings is applied once its
package has loaded; if the package goes away the choice draws as the default
theme until it returns.

`contributes.snippets` lists `{ "id", "path" }`. A snippet file names one
language and its snippets in the editor's snippet syntax:

```json
{ "language": "rust", "snippets": [
  { "prefix": "tfn", "body": ["#[test]", "fn ${1:name}() {", "\t$0", "}"], "description": "A test" }
] }
```

The snippets appear in the completion list of that language after the language
servers' items (up to 500 per file). A `contributes.languages` resource is the
`tree-sitter.json` of a language pack inside the package; it is found at startup
and needs a restart (see `docs/languages.md`). The roadmap of what is not done
yet is `docs/extensions-roadmap.md`.

## Language servers

`contributes.language_servers` declares a server (`id`, `languages`, `command`,
`args`, `root_markers`, `initialization_options`). A package that declares one
must also declare `process.execute-local`, because starting it runs a program.
A command with a path separator that is not absolute starts inside the package;
a bare name is looked up on `PATH` by the system. Loading a package only checks
the declaration; the server starts for a project folder only when the package is
granted local execution, the folder is trusted (`workspace::TrustWorkspace`) and
the folder's tools are allowed (`workspace::AllowExtensionTools`). Otherwise the
language panel lists the server as not started and says which of the three is
missing. Revoking any of them, reloading a changed package or removing it stops
the server at once; a name used by a catalogued or your own server is never
replaced.

A completion answer carries items with an opaque `data` string. Highlighting an
item asks the same provider to complete it (`completion-resolve`), and the
request carries the item with its `data` unchanged; an item is never sent to
any other extension. Items from extensions are listed after the language
servers' and merged with them as the list is filtered while typing.

## Tasks (running tools)

An extension that declares `process.execute-local` can run a program in a
folder: `task-start` takes a program and an argument list (never a shell
line), a folder handle from `list-roots`, an optional working directory inside
the folder and a short allow list of environment variables. Output arrives as
`task-output`, and the end as exactly one `task-exited`. A task needs three
things at once: the extension's own grant, a trusted folder
(`workspace::TrustWorkspace`) and the folder's tool permission
(`workspace::AllowExtensionTools`). The permission is a local-execution
permission, not a sandbox: the program can do whatever you can. Revoking any of
them, or removing the folder, stops what runs there. A checker can show its
findings with `publish-diagnostics` for the document version it read.

## Storage, files and network

`storage` keeps small values per extension in three scopes (user, workspace,
folder); limits are 256-byte keys, 64 KiB values, 256 keys and 512 KiB per scope.
`workspace.files.read` lets an extension `read-file` and `find-files` inside a
folder; paths may not leave it, not even through a link. `network` plus
`"network": { "domains": [...] }` in the manifest lets it `http-get` those hosts
only (`*.example.com` covers subdomains); redirects are checked again and a
downgrade to http is refused. All three are plain package grants: without the
grant the call fails with `permission-denied`.

## Save participants

A provider of kind `save-participant` changes a file just before it is saved.
Participants run one after another on a temporary copy, each seeing the previous
one's result, and answer with `provide-edits`. Only when all have answered is the
result committed to the real document, as one undo step, and the file written. If
a participant fails, answers too late (the whole chain has 3 seconds), returns
edits that do not fit, or the text keeps changing under it, nothing is changed
and the save is not done; the message says why. `editor::SaveWithoutExtensions`
saves without them. While a participant works, its calls with side effects
(edits, tasks, network, storing values, publishing) fail with
`permission-denied`; reading stays possible. Closing the window waits for the
participants as it waits for writes.

## Changing files

`apply-file-ops` creates, renames and deletes files inside a folder (capability
`workspace.files.write`). The steps run in order on a background thread. If one
fails, the earlier ones are undone and the error says so; if the undo cannot
finish, the error is marked `committed` and names where the rest is (the
`.loom-edit-*` folders next to the files, and a `.loom-fileops-*.txt` note in the
folder that stays until you remove it and is reported when the folder is opened
again). A file with unsaved changes is never touched, and `.git` and the
recovery entries cannot be changed. Open documents follow renames and close
with deletes.
