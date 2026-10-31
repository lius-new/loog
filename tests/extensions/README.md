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

Declare providers under `contributes.providers` (`diagnostics` and `formatting`
are served; `completion` and `code-actions` are accepted by the manifest but not
routed yet). A document in a selected language wakes the extension and the
host sends a `provider-request` for that exact document state; answer it with
`provide-diagnostics` or `provide-edits`. An answer that arrives late or for an
older state is discarded and the current state is asked again. When a formatting
provider matches the language, `editor::Format` uses it instead of the language
server.
