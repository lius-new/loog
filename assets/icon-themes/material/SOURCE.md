# Material Icon Theme assets

This directory contains a focused subset of SVG file icons from
[Material Icon Theme](https://github.com/material-extensions/vscode-material-icon-theme).

- Upstream revision: `cb1dfb6d9cb73b15681a93939983d75dbba7bf5b`
- License: MIT (see `LICENSE`)
- Copyright: Material Extensions contributors

The SVGs are embedded into the Loog binary and used only by its file tree.
`folder.svg` and `folder-open.svg` are generated from the upstream paths and
default `#90a4ae` color defined by the same revision. The selection and
filename matching table live in
`src/file_icons.rs`.
