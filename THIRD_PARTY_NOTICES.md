# Third-party notices

## vt100 terminal parser

`vendor/vt100` contains vt100 0.16.2 by Jesse Luehrs, licensed under MIT
(see `vendor/vt100/LICENSE`). Loog modifies height resizing to preserve terminal
history and exposes the row displacement for selection tracking. Upstream:
https://github.com/doy/vt100-rust.

## tree-sitter

`vendor/tree-sitter` contains tree-sitter 0.27.2, licensed under MIT (see
`vendor/tree-sitter/LICENSE`). Loog adds a function that loads a precompiled
WebAssembly grammar module; see `vendor/tree-sitter/LOOG-PATCH.md`. Upstream:
https://github.com/tree-sitter/tree-sitter.

Official Loog packages may include a managed Git runtime. Git is licensed under
the GNU General Public License version 2. Runtime packages must retain Git's
license, source-offer information, upstream version, download URL, and the
generated `runtime/git/MANIFEST.json` file.

The packaging pipeline treats the runtime as a separate executable component;
Loog invokes it through its command-line interface and does not link Git into
the Loog binary.

## Zed GPUI keymap

The key-context predicate language and binding resolution in
`src/input/context.rs`, `src/input/keymap.rs` and `src/input/dispatcher.rs` are
adapted from the `gpui` crate of Zed (https://github.com/zed-industries/zed,
`crates/gpui/src/keymap*` and `crates/gpui/src/key_dispatch.rs`), Copyright
Zed Industries, Inc., licensed under the Apache License, Version 2.0. The
adapted code was rewritten for Loog's types; its behavior and test cases follow
the original.

## Tree-sitter and language grammars

Syntax highlighting parses source with Tree-sitter and these grammars. All grammars, including JSON and Markdown,
ship as separate standard WASM or native modules loaded on demand, together with the highlight queries referenced by `assets/language-packs/<id>/tree-sitter.json`.
Optional `PROVENANCE.md` files name the upstream repository, commit,
license and any change (the queries of one JavaScript and TypeScript family are
only concatenated).

| Component | Version | Upstream | Copyright |
| --- | --- | --- | --- |
| tree-sitter | 0.27.2 | https://github.com/tree-sitter/tree-sitter | Max Brunsfeld |
| tree-sitter-rust | 0.24.2 | https://github.com/tree-sitter/tree-sitter-rust | Maxim Sokolov |
| tree-sitter-javascript | 0.25.0 (query for JavaScript), 0.23.1 (query for TypeScript and TSX) | https://github.com/tree-sitter/tree-sitter-javascript | Max Brunsfeld |
| tree-sitter-typescript | 0.23.2 | https://github.com/tree-sitter/tree-sitter-typescript | its contributors |
| tree-sitter-c-sharp | 0.23.5 | https://github.com/tree-sitter/tree-sitter-c-sharp | Max Brunsfeld, Damien Guard, Amaan Qureshi, and contributors |
| tree-sitter-json | 0.24.8 | https://github.com/tree-sitter/tree-sitter-json | Max Brunsfeld, Amaan Qureshi |
| tree-sitter-css | 0.25.0 | https://github.com/tree-sitter/tree-sitter-css | Max Brunsfeld, Amaan Qureshi |
| tree-sitter-html | 0.23.2 | https://github.com/tree-sitter/tree-sitter-html | Max Brunsfeld, Amaan Qureshi |
| tree-sitter-md | 0.5.3 (block and inline grammars) | https://github.com/tree-sitter-grammars/tree-sitter-markdown | MDeiml and contributors |

All of them are licensed under the MIT License:

> Permission is hereby granted, free of charge, to any person obtaining a copy
> of this software and associated documentation files (the "Software"), to deal
> in the Software without restriction, including without limitation the rights
> to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
> copies of the Software, and to permit persons to whom the Software is
> furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in
> all copies or substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
> IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
> FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
> AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
> LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
> OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
> SOFTWARE.


## Wasmtime and Cranelift

Tree-sitter's WASM feature embeds Wasmtime and Cranelift 48.0.6/0.135.6 from
https://github.com/bytecodealliance/wasmtime. They are licensed under
Apache-2.0 WITH LLVM-exception. The Apache 2.0 text is included in LICENSE-APACHE;
the upstream LLVM exception follows:

--- LLVM Exceptions to the Apache 2.0 License ----

As an exception, if, as a result of your compiling your source code, portions
of this Software are embedded into an Object form of such source code, you
may redistribute such embedded portions in such Object form without complying
with the conditions of Sections 4(a), 4(b) and 4(d) of the License.

In addition, if you combine or link compiled forms of this Software with
software that is licensed under the GPLv2 ("Combined Software") and if a
court of competent jurisdiction determines that the patent provision (Section
3), the indemnity provision (Section 9) or other Section of the License
conflicts with the conditions of the GPLv2, you may retroactively and
prospectively choose to deem waived or otherwise exclude such Section(s) of
the License, but only in their entirety and only with respect to the Combined
Software.

## Language servers

Loog's source and installers contain no language server. Servers are separate
programs that Loog downloads on demand from its own releases, each pinned by
SHA-256 in `assets/lsp-servers/lsp-servers.toml`, and runs as separate processes
that it talks to over the Language Server Protocol; Loog does not link them.

Each pack Loog publishes repackages an upstream release unchanged (or, for Go and
npm servers, builds it from a pinned module or lockfile) and carries the upstream
license texts in its `licenses/` directory and its provenance in `PROVENANCE.md`.
The recipes are in `scripts/lsp-servers/build.toml`.

| Pack | License | Upstream |
| --- | --- | --- |
| rust-analyzer | MIT OR Apache-2.0 | https://github.com/rust-lang/rust-analyzer |
| clangd | Apache-2.0 WITH LLVM-exception | https://github.com/clangd/clangd |
| gopls | BSD-3-Clause | https://go.googlesource.com/tools |
| zls | MIT | https://github.com/zigtools/zls |
| taplo | MIT | https://github.com/tamasfe/taplo |
| marksman | MIT | https://github.com/artempyanykh/marksman |
| lua-language-server | MIT | https://github.com/LuaLS/lua-language-server |
| ruff | MIT | https://github.com/astral-sh/ruff |
| biome | MIT OR Apache-2.0 | https://github.com/biomejs/biome |
| Node.js | MIT (its bundled components keep their own licenses) | https://nodejs.org |
| pyright | MIT | https://github.com/microsoft/pyright |
| typescript-language-server, typescript | Apache-2.0 | https://github.com/typescript-language-server/typescript-language-server |
| vscode-langservers-extracted | MIT | https://github.com/hrsh7th/vscode-langservers-extracted |
| yaml-language-server | MIT | https://github.com/redhat-developer/yaml-language-server |
| bash-language-server | MIT | https://github.com/bash-lsp/bash-language-server |

The npm packs also contain the packages those servers depend on; each keeps its own
license inside `node_modules`, and the lockfile in `scripts/lsp-servers/npm` lists them.

## pulldown-cmark

Markdown parsing uses pulldown-cmark 0.13.5, https://github.com/pulldown-cmark/pulldown-cmark, under MIT.

The MIT License

Copyright 2015 Google Inc. All rights reserved.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.

## Material Design Icons

The language services icons of the status bar (a turning arc, a circle with a check, a
circle with an exclamation mark and a circle with a pause) are paths from Material Design
Icons by Pictogrammers, https://pictogrammers.com/library/mdi/, licensed under the
Apache License, Version 2.0; the text is included in LICENSE-APACHE.

## resvg

SVG images in Markdown previews are drawn with resvg, usvg and tiny-skia
(https://github.com/linebender/resvg; resvg and usvg are licensed under
Apache-2.0 OR MIT, tiny-skia under BSD-3-Clause), plus fontdb and rustybuzz
(MIT) for text. Loog enables only the `text` and `system-fonts` features.
