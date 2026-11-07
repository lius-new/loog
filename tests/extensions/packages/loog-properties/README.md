# Properties

Highlighting and snippets for Java `.properties` files.

Files ending in `.properties` get colors for comments, keys, values, escapes,
`${substitutions}` and the separators between them. The colors come from your
theme, so they change with it.

## Snippets

| Prefix | Inserts |
| --- | --- |
| `key` | `name=value` |
| `section` | A commented group with one property |
| `env` | A property that reads an environment variable |

## Good to know

The grammar is loaded when a `.properties` file first needs it. Loog reads
language grammars at startup, so restart Loog after installing this extension.

The grammar is tree-sitter-properties 0.3.0 (MIT); see `PROVENANCE.md`.
