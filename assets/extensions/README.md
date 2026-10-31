# Extensions that ship with Loom

Every folder here is an extension package (see `manifest.json` in each). Packaging
copies them next to the executable, in `extensions/`, and Loom loads them like any
other extension, except that they cannot be uninstalled.

| Folder | Provides |
| --- | --- |
| `loom.aura` | The Aura Light, Ember and High Contrast color themes |
| `loom.lang.json` | JSON syntax highlighting |
| `loom.lang.markdown` | Markdown syntax highlighting |

`registry.toml` lists the language extensions that are not shipped but can be
installed on demand: when a file needs one, Loom downloads it from the pinned
release, checks its size and SHA-256, and installs it into the user's extensions
folder as `loom.lang.<name>`. It is then an ordinary extension that can be
disabled or removed.

A language extension contributes its grammar with `contributes.languages`, a path to
a `tree-sitter.json` inside the package (WebAssembly grammars only), and may add or
replace highlight rules for any grammar with `contributes.queries`. See
`docs/languages.md`.
