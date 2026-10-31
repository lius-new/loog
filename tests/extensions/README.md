# Extension development loop

Status: M0/M1 prototype. The contract (`wit/loom-extension/world.wit`) is not frozen.

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
document text). A failed extension stays stopped until retried.

## Trust

New folders are untrusted: language servers are not started for them. Run
`workspace::TrustWorkspace` while a file of the folder is active (and
`workspace::RevokeWorkspaceTrust` to undo it). The decision is stored in
`trust/roots.json` next to the extensions directory.

## Settings

Declare settings under `contributes.settings` (namespaced keys). Users set
values in `<extensions dir>/settings.json`; a trusted folder may override
project-scoped settings in `.loom/extension-settings.json`. Both files are read
at start and on reload.

## Known gaps

No source maps or debugger integration yet; the guests keep line tables only.
Settings files are not watched. Only the first workspace folder supplies project
settings. Linux and macOS are not yet verified.

## Providers

Declare providers under `contributes.providers` (`diagnostics`, `formatting` and
`completion` are served; `code-actions` is accepted by the manifest but not
routed yet). A document in a selected language wakes the extension and the
host sends a `provider-request` for that exact document state; answer it with
`provide-diagnostics` or `provide-edits`. An answer that arrives late or for an
older state is discarded and the current state is asked again. When a formatting
provider matches the language, `editor::Format` uses it instead of the language
server.

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
