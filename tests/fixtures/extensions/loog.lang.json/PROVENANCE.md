# json

- Grammar reference: tree-sitter-json 0.24.8; the complete package includes the corresponding WASM module.
- Upstream: https://github.com/tree-sitter/tree-sitter-json
- Upstream reference commit: `ee35a6ebefcef0c5c416c0d1ccec7370cfca5a24`
- License: MIT
- Query: queries/highlights.scm of the same upstream version.
- Modified: Yes: object keys are `@property` instead of `@string.special.key`.

The query uses the capture names, predicates and priority rule described in
`src/features/syntax/queries.rs`; injection queries follow
`src/features/syntax/injections.rs`. Update this file together with the upstream module
version/commit, and run the language fixtures and the query tests.

The WASM module was built and validated with scripts/language-packs,
using the pinned source above, Tree-sitter CLI 0.27.1 and WASI SDK 34.0,
then manually placed here. Application packaging copies this complete package.
