# Packing an extension

```sh
node scripts/extensions/package/pack.mjs <extension folder> [--out <directory>]
```

The result is `<id>-<version>.loogext` (default directory:
`target/extension-packages`) and a `.sha256` file beside it. In Loog click
**Import Extension** on the Extensions page (or run **workspace::ImportExtension**)
and choose the file. The extension is installed, loaded, and selected in the
detail pane; failures appear as error notifications. An extension with the same
id is replaced; a failed import leaves the
installed one as it was. An extension that carries a language grammar needs a
restart of Loog before the language works.

## What a package is

A gzip-compressed tar archive of the extension folder. The files of the folder
are at the archive root, `manifest.json` among them. The same folder always
packs to the same bytes, so the SHA-256 identifies a build. Links, `node_modules`,
`.git` and earlier packages are left out. Limits: 64 MiB packed, 128 MiB
unpacked, 2048 files.

## What the manifest has to carry

The extensions page and the import show these, so a package cannot be built
without them:

| Field | Meaning |
| --- | --- |
| `id`, `version` | Identity (`vendor.name`) and `major.minor.patch`. |
| `name` | The name people see (up to 60 characters). |
| `publisher` | Who publishes it. |
| `description` | One sentence (up to 200 characters). |
| `categories` | One to three of: Themes, Syntax Highlighting, Language Services, Snippets, Linters, Formatters, Editing, Source control, Productivity, Other. |
| `license` | An SPDX expression such as `MIT`. |
| `repository` | An `https://` address. |
| `icon` | A PNG or SVG file in the package (up to 256 KiB). |
| `readme`, `changelog` | Markdown files in the package (up to 512 KiB each). |
| `engines.loog` | The oldest Loog version it runs on, `major.minor.patch`. |

Categories describe a specific function. Use `Syntax Highlighting` for grammar
packs and highlight queries, and `Language Services` for completion, hover,
navigation and language server integrations. Use `Linters`, `Formatters` and
`Snippets` for those functions. An extension providing both highlighting and
language services can list both categories. `Languages` is no longer accepted;
existing extensions must replace it with the categories for their functions and
publish updated packages and registry metadata.

Optional: `keywords` (up to ten), `homepage` (`https://`). Every path in the
manifest must exist in the package and stay inside it; the packer lists all
problems at once. A folder you only use while developing may leave these out;
the packer and the import insist on them.

The importer (`src/infrastructure/extensions/package.rs`) checks again and is the
authority: the archive paths, regular files only, no duplicates (ignoring case),
the manifest, the files it names and the Loog version.

## Tests

```sh
node --test scripts/extensions/package
cargo test --bin Loog -- extensions::package app::extensions::package_tests
```

The second runs the packer on the sample extensions in
`tests/extensions/packages` and imports the results.
