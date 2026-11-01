# Language services and syntax architecture

This is the maintained architecture for shared document contracts, language
identity and local syntax highlighting. It describes the implementation reviewed
on 2026-10-25. Installation, manifest compatibility and resource paths are in
[Language resources](../languages.md); server behavior is in [LSP](lsp.md).
Outstanding implementation and acceptance work is tracked in
[Remaining work](../language-services-todo.md).

## Boundaries and ownership

Local syntax and LSP are independent services. They share document identity,
language resolution and byte ranges, but neither requires the other to run.
Unavailable resources or servers must leave plain-text editing usable.

| Layer | Responsibility |
| --- | --- |
| `crates/editor-core` | Text, selections, edit transactions and bounded revision geometry; no GUI, filesystem, Tree-sitter or LSP dependency |
| `src/domain/document.rs`, `document_snapshot.rs` | Document identity, immutable snapshots and metadata |
| `src/domain/language/` | Open language, variant and grammar identities; recognition and registry rules |
| `src/features/highlighting/` | Language-independent classes and sparse style resolution |
| `src/features/syntax/` | Trees, queries, injections, display projection and the `Resources` port |
| `src/infrastructure/language_packs.rs`, `language_standard.rs`, `language_catalog.rs` | Grammar packs of extensions, WebAssembly loading, stacked query layers and registry installation |
| `src/infrastructure/query_watcher.rs` | Debounced watching of query files and extension manifests for hot reload |
| `src/features/syntax/query_stack.rs`, `inspect.rs`, `generic.rs` | Query layering with file/line tracing, the highlight inspector, and the built-in highlighter for text no grammar covers |
| `src/infrastructure/syntax_worker.rs`, `syntax_worker_retained.rs` | Background scheduling, exact-result cache and display retention |
| `src/app/syntax.rs`, `runtime.rs` | Service lifetime and coalesced, version-checked UI invalidation |
| `src/ui/editor/syntax.rs` | Current-theme colors, grapheme-safe ranges and default-text gaps |

`AppRuntime` owns the mounted services. Application snapshots do not own parser
or server processes. Background work must not hold the application state lock
while parsing, performing I/O or waiting for another thread.

## Identity, coordinates and revisions

`DocumentStamp` contains `document`, `incarnation`, `version`,
`snapshot_identity` and `language_generation`. Compare the complete stamp when
publishing a document-derived result. An allocation address can be reused and
is not a version on its own. Changing the effective language invalidates results
even when the text is unchanged. Closing and reopening a document creates a new
lifetime. Multiple views share the document and its text history; see
[Markdown architecture](markdown.md) for `FileId`/`ViewId` ownership.

Language, variant and grammar IDs belong to their process registry; persist
stable names rather than numeric IDs. Resource generation is separate from the
document stamp. `Resources::generation()` changes when resources become
available; the tree index clears dependent parser/query/tree state when it changes.

| Boundary | Coordinate |
| --- | --- |
| Text, edits and highlight ranges | UTF-8 byte offsets, half-open ranges |
| Tree-sitter points | Zero-based row and byte column |
| LSP positions | Zero-based row and negotiated UTF-8 or UTF-16 column; UTF-16 is the default |
| Editor layout and hit testing | Grapheme boundaries and measured screen geometry |

CRLF bytes remain in the text. Tree-sitter rows count LF; byte columns are not
display columns. Protocol adapters convert against the exact snapshot, and UI
projection avoids splitting graphemes. UTF-32 is not advertised.

Revision records contain edit ranges, old byte points and inserted text extents,
without retaining deleted/inserted text in the geometry journal. All edits within
one revision refer to the pre-edit text. Apply revisions chronologically and
each revision's edits in reverse order to a private copy of the tree. Derive
`new_end_byte` from `old.start + inserted_extent.bytes`, not the edit's final
post-transaction position. A missing revision chain or incompatible document/
grammar state requires a full parse. Cancelled work never replaces the stored tree.

## Registry and resource loading

The host has a generic Tree-sitter runtime and no linked concrete grammar.
Registry identity and resource availability are distinct: a catalogued language
can be recognized before its module has been installed. Server bindings live in
`ServerSet`, outside the language registry.

`Resources` supplies languages, per-parser execution stores, query text, resource
generation and availability notifications without exposing filesystem behavior
to syntax rules. The infrastructure adapter loads standard `tree-sitter.json`
collections, preferring compatible WASM modules before native fallbacks. It
enforces containment, ABI and query validation. A selected corrupt module fails
that resource rather than silently selecting another backend.

Manifest discovery and parsing are separate: discovering a collection does not
load its grammar. WASM uses a shared lazily initialized engine and a mutable
store per parser. Native handles outlive their languages/trees/queries. Failures
are isolated and cached, with resource generation allowing newly available
resources to be reconsidered. Legacy collection compatibility, overrides,
restart behavior and on-demand installation are specified in
[Language resources](../languages.md), not duplicated here.

## Trees, windows and embedded languages

`TreeIndex` parses whole documents, then queries only requested windows. A
multiline construct starting outside the viewport therefore retains its context.
The worker coalesces current view targets, cancels superseded work and caches
results by document lifetime, text, language and byte coverage. Cached pages are
anchored to source lines, independently of viewport width and horizontal scroll.
An empty span list can be a completed plain-text result; it is not proof that
work is still pending.

Injection queries discover embedded regions. A region is parsed only when a
requested window reaches it. Nested injection layers have separate parsers and
bounded depth, region count, retained trees and bytes. Unchanged regions can
reuse their trees. Over-budget regions retain host coloring. Large combined
injections are split into bounded groups rather than parsed as an unbounded
included-range list.

Highlight query captures map into Loom's stable class names; capture indexes,
LSP legend indexes and Loom class IDs are different namespaces. The query
adapter accepts an explicit predicate/capture subset and rejects unsupported
directives. Within a highlight query, shorter ranges win; equal ranges use the
later pattern. Follow `src/features/syntax/queries.rs` when changing that contract.

## Fresh results and display continuity

Fresh results describe the current stamp. While a newer parse is pending, the
worker can project previously correct visible spans through compatible revisions.
These provisional spans are a drawing aid: they do not become exact parse-cache
hits, semantic results, folding data or a new correct parse anchor. Retention
tracks the original parsed version separately from its current projected pages.
Broken chains, changed document/language lifetime or projection limits discard
the approximation and use default text until fresh results arrive.

Semantic token decoding and composition live in
`src/features/language_service/semantic.rs`. Only current results overlay base
syntax; unknown semantic classes do not erase useful local coloring. The output
is ordered and clipped to the requested window. Selection, search backgrounds,
diagnostic decorations, caret and IME remain separate rendering layers.

Class names have dotted fallback and sparse styles distinguish unset attributes
from explicit `false`. The editor syntax path currently draws foreground colors.
The style model's font attributes do not imply that all those attributes are
rendered; adding them requires matching shaping, measurement and hit testing.
Changing theme colors does not require reparsing text.

## Bounds and verification

These are current code limits, not the old draft's proposed budgets:

| Work or retained state | Limit and source |
| --- | --- |
| Whole-document syntax | 8 MiB; `features/syntax/treesitter.rs` |
| Retained document trees / captures per query window | 16 / 200,000; `treesitter.rs` |
| Exact window cache / display retention | 4 MiB each; `infrastructure/syntax_worker.rs`, `syntax_worker_retained.rs` |
| Display projection input | 4,000 spans, 16 revisions, 64 edits; `features/syntax/presentation.rs` |
| Injection depth / discovered regions | 4 / 10,000 per document; `features/syntax/layers.rs` |
| Live injection layers / injected text | 64 / 4 MiB per document; `layers.rs` |

These limits bound admission and retained work, not process RSS or all native
temporary allocations. Oversized input and failed resources fall back to text;
tree eviction does not by itself invalidate retained display spans.

Run the workspace rules and architecture checks as described in
[Building Loom](../building.md). The independent
[extension validator](../../tools/ext-validate/README.md) exercises the production
manifest rules, loader and query compilers with WASM fixtures and optional samples.

```sh
node scripts/architecture/check.mjs
node --test scripts/architecture/language-resources.test.mjs
node scripts/build/cargo.mjs test --locked --workspace
cargo test --locked --manifest-path tools/ext-validate/Cargo.toml --test cli
```

The validator tests use the checked-in extension fixtures. Report
fixture, build profile, toolchain, platform, cache conditions and sample counts
with performance measurements. CI configuration alone is not evidence that a
specific real-server or desktop scenario passed.
