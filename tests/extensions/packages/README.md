# Sample extensions

Two small extensions that need no component, kept here to prove that the
extension loader supports themes and syntax highlighting end to end. They load
like any extension: put the folder under the extensions directory (see
`tests/extensions/README.md`).

| Folder | Id | What it shows |
| --- | --- | --- |
| `loom-midnight` | `loom.midnight` | Two color themes (dark and light) written as small files on top of a built-in theme. |
| `loom-properties` | `loom.properties` | A new language: a Tree-sitter grammar for Java `.properties` files plus snippets. |

`src/app/extensions/package_tests.rs` loads both from these folders, finds the
grammar the way startup does, highlights real text with the WebAssembly module
and checks the theme colors for the captured classes.

`loom-properties/grammar` is the upstream tree-sitter-properties 0.3.0 release
(MIT; see `grammar/LICENSE` and `loom-properties/PROVENANCE.md`).
