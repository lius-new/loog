# markdown

- Grammar reference: tree-sitter-md 0.5.3 (block grammar); the complete package includes the corresponding WASM module.
- Upstream: https://github.com/tree-sitter-grammars/tree-sitter-markdown
- Upstream reference commit: `f969cd3ae3f9fbd4e43205431d0ae286014c05b5`
- License: MIT
- Query: written for Loom from tree-sitter-markdown/queries/{highlights,injections}.scm of the same upstream version.
- Modified: Yes: captures renamed to Loom's classes; YAML/TOML front matter injections dropped (no such grammar yet); inline text is injected as one combined layer.

The query uses the capture names, predicates and priority rule described in
`src/features/syntax/queries.rs`; injection queries follow
`src/features/syntax/injections.rs`. Update this file together with the upstream module
version/commit, and run the language fixtures and the query tests.

# markdown_inline

- Grammar reference: tree-sitter-md 0.5.3 (inline grammar); the complete package includes the corresponding WASM module.
- Upstream: https://github.com/tree-sitter-grammars/tree-sitter-markdown
- Upstream reference commit: `f969cd3ae3f9fbd4e43205431d0ae286014c05b5`
- License: MIT
- Query: written for Loom from tree-sitter-markdown-inline/queries/highlights.scm of the same upstream version.
- Modified: Yes: captures renamed to Loom's classes.

The query uses the capture names, predicates and priority rule described in
`src/features/syntax/queries.rs`; injection queries follow
`src/features/syntax/injections.rs`. Update this file together with the upstream module
version/commit, and run the language fixtures and the query tests.

The WASM modules were built and validated with scripts/language-packs,
using the pinned source above, Tree-sitter CLI 0.27.1 and WASI SDK 34.0,
then manually placed here. Application packaging copies this complete package.
