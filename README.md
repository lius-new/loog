<p align="center">
  <img src="icons/source/app-icon-transparent-128.png" alt="Loog" width="96" height="96">
</p>

<h1 align="center">Loog</h1>

<p align="center">
  <strong>English</strong> | <a href="README.zh-CN.md">简体中文</a>
</p>

<p align="center">
  A fast, minimal code editor with an integrated terminal and Git, written in Rust.
</p>

<p align="center">
  <a href="https://github.com/lius-new/loog/actions/workflows/ci.yml"><img src="https://github.com/lius-new/loog/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg" alt="License: MIT OR Apache-2.0"></a>
</p>

Loog is a desktop editor for Windows, macOS and Linux built on
[lgui](https://github.com/lius-new/lgui), a Rust GUI library that renders with
Skia. It aims to stay small and responsive while covering the daily loop of
editing, searching, running commands and working with Git, without leaving the
window.

> [!NOTE]
> Loog is in early development (version 0.1). Expect rough edges, and expect
> settings and behavior to change between releases.

![Loog with Source Control, a diff of the working tree, the file tree and a terminal](docs/images/screenshot.png)

## Features

- **Editing** — grapheme-aware text editing (CJK, combining marks, emoji),
  multiple cursors, rectangular selections, soft wrap, code folding, split panes,
  IME input and an optional Vim mode. Views of the same file share text and
  undo history while keeping their own selection and scroll position. Files up
  to 64 MiB open on a background worker; large files use plain text rendering.
- **Languages** — Tree-sitter syntax highlighting with embedded languages and
  syntax resources downloaded on demand. Language servers provide completion,
  hover documentation, signature help, definitions, references, rename,
  formatting, diagnostics and code actions. The Outline shows Markdown headings
  or symbols from language servers. Servers are installed manually from the
  language panel by default; automatic installation is optional.
- **Encodings** — UTF-8, UTF-16, GBK, GB18030, Big5, Shift_JIS and more, with
  automatic detection and a status-bar encoding selector. Saves preserve the
  selected encoding, BOM and LF/CRLF line endings. See
  [Text file encodings](docs/encoding.md).
- **Markdown** — live reading and side previews of unsaved buffers, shared with
  hover/completion/signature documentation; logical copy, outline, code, tables
  and bounded images. See [Markdown reading](docs/markdown.md).
- **Sessions** — restore file tabs, pane layout, source positions and terminal
  working directories after a restart. Extra cursors and folded regions are
  temporary; terminal sessions start new shells.
- **Workspace search** — search every folder with case, whole-word, regex and
  include/exclude globs; respects `.gitignore` and searches unsaved buffers.
- **Integrated terminal** — PowerShell, Command Prompt, Git Bash, bash, zsh,
  fish or any custom shell, in tabs and splits that follow your working
  directory across `cd` and restarts.
- **Git** — staging, commits, fetch/pull/push, branches, tags, stashes,
  history, single-file diffs and worktrees, refreshed in the background
  across every repository in the workspace.
- **Customizable** — light, dark and high-contrast themes, a Zed-style
  `keymap.json` with contexts and key sequences, per-project
  `.loog/settings.json`, interface scaling from 80% to 200% and Material file
  icons. Editor and terminal fonts have independent family, size, weight and
  line-height settings.

## Installation

Download a package for your platform from the
[Releases](https://github.com/lius-new/loog/releases) page.

| Platform | Packages |
| --- | --- |
| Windows x64 | `…-windows-x86_64-setup.exe` (per-user installer, no admin rights) or `…-portable.zip` |
| macOS 11+ (Apple Silicon and Intel) | `…-macos-universal.dmg` or `.zip` |
| Linux x64 | `.deb`, `.AppImage`, or `.tar.gz` (run `./install.sh` to install for the current user) |

Each release includes a `SHA256SUMS` file per platform.

- **Windows**: packages are not code-signed yet, so SmartScreen warns on first
  run. A Git runtime is included.
- **macOS**: until the app is notarized, Gatekeeper blocks the first launch.
  Control-click Loog.app and choose **Open**, or run
  `xattr -dr com.apple.quarantine /Applications/Loog.app`. Loog uses the
  system `git`.
- **Linux**: requires glibc 2.39 or newer (for example Ubuntu 24.04, Fedora 40)
  and an X11 or Wayland session. Loog uses the system `git`.

Use `loog .` or `loog path/to/file` to open folders and files from a terminal.
The Windows installer offers to add `loog` to your PATH; Linux `.deb` and
`install.sh` installs provide the command. On macOS, link the bundled launcher
into a directory on your PATH, for example:

```sh
ln -s /Applications/Loog.app/Contents/Resources/bin/loog /usr/local/bin/loog
```

See [Packaging](scripts/package/README.md) for portable launchers and what each
package installs, and [Updates](docs/updates.md) for automatic update support by
installation type.

## Building from source

Loog builds with the Rust toolchain pinned in
[`rust-toolchain.toml`](rust-toolchain.toml) (currently 1.97.1); `rustup`
installs it automatically. Install Node.js 20+ and the platform C/C++ SDK as
well; the shared scripts prepare CMake and Skia privately.

```sh
git clone https://github.com/lius-new/loog.git
cd loog
node scripts/build/cargo.mjs build --release --locked
node scripts/build/stage-resources.mjs target/release
node scripts/build/cargo.mjs run --release --locked
```

For a development run, prepare resources with `node scripts/build/stage-resources.mjs`
before `node scripts/build/cargo.mjs run --locked`. It stages both syntax resources
and the language-server catalog beside the executable; Cargo alone does not copy them.

On Debian or Ubuntu, install the native libraries first:

```sh
sudo apt install build-essential \
    libfreetype-dev libfontconfig-dev \
    libegl1-mesa-dev libgl1-mesa-dev libgles2-mesa-dev \
    libwayland-dev libxkbcommon-dev
```

Pinned CMake and prebuilt Skia archives are downloaded and SHA-256 verified during
the first build; no system CMake or manual Skia download is required. See
[Building Loog](docs/building.md) for offline builds, tests, CI and packaging.

## Documentation

- [Editing](docs/editing.md) — files, sessions, multiple cursors, folding, soft wrap and Vim
- [Text file encodings](docs/encoding.md) — detection, switching and saving
- [Language resources](docs/languages.md) — syntax packages and downloads on demand
- [Settings and key bindings](docs/settings.md) — language servers, formatting, themes and shortcuts
- [Markdown reading](docs/markdown.md) — reading mode and live previews
- [Workspace search](docs/search.md) and [Outline](docs/outline.md) — find text and navigate file structure
- [Terminal](docs/terminal.md) — shells, WSL and working directories
- [Git](docs/git.md) — source control, diffs, history and worktrees
- [Updates](docs/updates.md) — checking, downloading and applying releases
- [Building Loog](docs/building.md) — toolchains, tests, CI and packaging

For implementation details, see the architecture docs for
[language services](docs/architecture/language-services.md),
[LSP](docs/architecture/lsp.md) and [Markdown](docs/architecture/markdown.md),
plus [remaining language-service work](docs/language-services-todo.md).

## Contributing

Bug reports and pull requests are welcome on
[GitHub](https://github.com/lius-new/loog/issues). Start with
[CONTRIBUTING.md](CONTRIBUTING.md). Use Node.js 24 for contribution tooling and
enable the local commit hook and message template after cloning:

```sh
node scripts/commit/install.mjs
```

Every commit must contain exactly one `Release Notes:` section in its body.
Use English release-note bullets for user-facing changes and `- N/A` for
internal changes only; see [Release notes](scripts/release/README.md) for
accepted note types. Preserve the section when squashing or merging.

Before opening a pull request, run the same checks as CI:

```sh
cargo fmt --all -- --check
node scripts/build/cargo.mjs test --locked --workspace
node scripts/architecture/check.mjs
```

and Clippy with the flags in [`ci.yml`](.github/workflows/ci.yml). The editing
core in [`crates/editor-core`](crates/editor-core/README.md) has no GUI
dependencies and is the easiest place to start.

## License

Loog is licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Unless you explicitly state otherwise, any contribution
intentionally submitted for inclusion in Loog shall be dual licensed as above,
without any additional terms or conditions.

Loog includes third-party code (the vt100 parser, parts of Zed's keymap
design and, on Windows, Git); see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
The Material Icon Theme icons carry their MIT license and attribution in
[`assets/icon-themes/material`](assets/icon-themes/material).
