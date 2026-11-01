# Language services: remaining work

Reviewed on 2026-10-25 against the source at `d8d2569`. This list replaces the
open migration checklists in the root language/syntax and LSP drafts. Maintained
architecture is in [Language services](architecture/language-services.md) and
[LSP](architecture/lsp.md). Historical plans, measurements and repair logs remain
in Git at that revision; they are not current acceptance results.

The implementation gaps below have identifiable source limitations. Acceptance
items have existing implementation or tests, but need a reproducible result for
the stated scenario. Keep those states separate and record source commit,
platform/toolchain, command, inputs and results when closing an item.

## Implementation gaps

| ID | Remaining behavior | Current evidence and completion condition |
| --- | --- | --- |
| LS-02 | Semantic token viewport coverage | `infrastructure/lsp/manager.rs` requests a stamped whole-document range when the server lacks full tokens. Add bounded viewport-range coverage, invalidation and cancellation without coloring uncovered/stale regions as fresh. |
| LS-03 | Complete snippet compatibility | `features/language_service/snippet/transform.rs` uses bounded `fancy_regex` substitutions. Common transforms, choices and mirrors exist; audit full ECMAScript/UTF-16 behavior, remaining variable sources and complex nested deletion/undo against explicit fixtures before claiming compatibility. |
| LS-04 | Rich inlay hints | `app/language_ui/inlay.rs` converts hints into text/offsets. Preserve label-part locations, commands and resolve data through rendering; validate actions against the original hint, instance and document. |
| LS-05 | Inline code lens | `app/language_ui/lists.rs` offers lens commands and resolve in a list. Add above-line lens layout, hit testing and invalidation; list availability does not complete inline rendering. |
| LS-06 | Navigable hierarchy trees | `app/language_ui/lists.rs` presents one relation level. Add repeated expand/return navigation while keeping the source instance, item identity and stale-result protection. |
| LS-07 | Whole-workspace diagnostics | Document push/pull and the open-document problems view exist. Add workspace diagnostic requests, bounded partial/incremental results and filtering without equating open documents with the whole project. |
| LS-08 | Pre-rename server edits | Client capabilities advertise did-create/rename/delete, not will-rename. Integrate `willRenameFiles` edits with Explorer rename and the existing workspace-edit transaction/rollback path. |
| LS-09 | Breadcrumbs and shared location rows | The Outline drawer is implemented. Breadcrumbs and complete reuse of search/location row rendering remain outside that delivery; specify their interactions and share presentation without merging unrelated query state. |
| LS-10 | Syntax font attributes | `features/highlighting/style.rs` models font attributes, while `ui/editor/syntax.rs` renders foreground colors. Enable further attributes only with matching shaping, width, caret and hit-test behavior. |

## Acceptance and measurement

| ID | Work to record | Evidence required |
| --- | --- | --- |
| LV-01 | Syntax performance baseline | Existing fixtures and `app/editor_baseline_tests.rs` provide starting points. Record reproducible multi-sample p50/p95 with machine, profile, fixture hash, cold/warm state, cache/eviction behavior, visible typing and 8 MiB/64 MiB/very-long-line fallback cases. Do not revive removed lexer output as a correctness oracle. |
| LV-02 | Platform/resource lifecycle matrix | CI already runs host tests, source-independent host validation and installed WASM tests. Map actual successful runs to native/WASM loading, user overrides, lazy loading, remove/restart, incremental edits, macOS signing/JIT and cold loading. Preserve untested cases rather than repeating the old blanket claim that no Linux/macOS build ran. |
| LV-03 | Real server and managed-pack matrix | Real Rust/TypeScript interactions and shared Node have opt-in tests in `app/real_language_server_tests.rs`; installation probes live in `infrastructure/lsp_install_tests.rs`. Record exact pack versions, OS and results for remaining server combinations, offline discovery, cancellation and restart. |
| LV-04 | Watchers, roots and process ownership | Windows Job Object and Unix process-group cleanup are implemented; registration leases and watched-file support exist. Collect real platform results for child-tree shutdown, unopened-file changes, project-manifest reload, branch switching, root changes and stale callbacks. |
| LV-05 | Completion and editing combinations | Verify real multi-server/late completion, incomplete lists, resolve/additional edits, signature and snippet interaction, rename/format/actions and resource-operation combinations, including late failure, state rollback and cross-document text undo. Existing fake-server coverage does not replace those real-server scenarios. |
| LV-06 | End-to-end interaction and latency | Old logs measured warm command-to-layout samples. Record cold/load combinations and automatic-trigger input-to-display latency with explicit budgets; inspect popup clipping, pointer hit targets, focus/IME, scrolling, installation notifications and narrow windows on real desktops. Offscreen UI tests do not establish compositor or accessibility acceptance. |

## Items corrected during consolidation

- The Outline sidebar is delivered, including tree expansion, sorting and caret
  tracking; see [Outline](outline.md). It is no longer an unimplemented feature.
- LSP documentation cards use the shared Markdown runtime, not the earlier
  small-reader experiment; see [Markdown architecture](architecture/markdown.md).
- Unix process-tree ownership and semantic range requests are implemented.
  Their remaining work is platform acceptance and viewport coverage, respectively.
- The checked-in [CI receipt](../tests/markdown/ci-validation.json) records a
  successful three-platform run for source `ea73b40` under Rust 1.97.1. It is
  historical evidence for those jobs, not proof that opt-in real-server or manual
  desktop tests passed at the current source revision.
- Extension-supplied WASM loading has replaced the original static grammar-factory
  migration. Do not carry old C-grammar build steps or first-commit plans into
  current requirements. Loader compatibility remains as documented in
  [Language resources](languages.md).

Full third-party theme import, additional standard Tree-sitter CLI semantics
(`locals`, `tags`, content/first-line recognition), front-matter injection and
remote/notebook/subdocument language services need separately scoped decisions.
They are not completion conditions for the already delivered base migration.

## Completed editor work (2026-10-28)

LS-01 is implemented in the editor folding change following 7ddb416. Current
Tree-sitter trees publish structural ranges; LSP supplies a versioned fallback.
View-local folding shares display rows with wrapping and covers hidden caret/
selection, navigation, hit testing, scrolling and target expansion. See
[Editing](editing.md#folding) for limits and shortcuts.

Historical evidence on Windows: workspace tests (1,728 passed, 31 ignored), fmt,
architecture gate and workspace all-targets Clippy; the retired language-runtime
suite (12 catalog and 9 WASM checks plus installed probe); folding tests
repeated 10 times at 32 threads (6 passed each). Includes a real Rust WASM tree
and a fake-server save barrier for a late folding response after switching tabs.
Linux/macOS interaction results remain for subsequent CI; this record does not
claim those platforms were run locally.
