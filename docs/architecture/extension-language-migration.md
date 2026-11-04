# Language extensions migration record

This records implementation and evidence for
[the language support plan](../../EXTENSIONS_LANGUAGE_SUPPORT_PLAN.md) and
[the LGUI requirements](../../LGUI_REQUIREMENTS.md). A checked library feature is
not, by itself, completion of its Loom integration or of the language migration.

The requested development checkouts are `.tmp/lgui` and `.tmp/loom-extensions`.
LGUI is now committed and pushed as `5ba703140407b6f6741c212d4c9d62aa976cdae7`;
Loom's manifest and lock use that exact GitHub revision. Building Loom no longer
requires a temporary LGUI checkout. The extension repository remains independent.

## Progress

| Stage | State | Evidence / remaining gate |
| --- | --- | --- |
| P0 | Source, negotiated-support and field audit recorded | Baseline, server omissions, retained fields and evaluation protocol below; performance acceptance remains P5 |
| PG | Automated native acceptance passed | Native integration below; latest LGUI core/widgets run passed 226 + 45 tests on Windows, including public-ID replacement and duplicate rejection |
| PT | Automated acceptance passed | Tasks/processes share admission; nested working-directory trust and revocation/removal passed |
| P1 | Automated acceptance passed | Real WASM/fake_lsp split-frame IO, binary streams, credits, tree ownership, roots and disk watches passed; 235 extension regressions passed, 4 pre-existing ignored tests |
| P2 | Platform implementation and automated checks passed | Unified request/resolve path, baseline kinds, audited fields, typed results, capture/save and workspace operations; actual client acceptance remains P4 |
| PU/P3 | Automated acceptance passed | Owned descriptions, quotas, latest-tree mailbox, native controls/events and Rust guest checks passed; real TypeScript component suite passed all 5 tests |
| P4 | Automated acceptance passed | Four real-server functional and two-root cases passed; native editor interactions use the actual client component |
| P5 | Passed before host removal | All 36 language/size/operation p95 comparisons passed the unchanged budget; raw reports, memory and restart observations are retained in the evidence directory |
| P6 | Automated acceptance passed | Host protocol, transport, manager, declarations, old settings, language panel and details card removed; 1700 native tests passed, 28 ignored; all 36 performance comparisons passed again after removal |

## PG native use sites

| Requirements | Loom use |
| --- | --- |
| LY0, LY1, LY2 | Status bar uses shared content sizing, Dock and Flex; extension split supplies constrained sizes |
| LY3 | Extension contribution tables use Grid tracks and cells, with measured ellipsis |
| LY4 | Extension category badges use Flex wrapping and the same container measurement rules |
| PS1 | Status bar's full-width divider is positioned outside Dock allocation |
| PS2 | Notifications align through Layer and explicit stacking order |
| PS3, CP5 | Editing menu anchors to the named indentation/line-ending node; outside pointer, focus scope and close intent |
| PS4 | Settings section headers stick within the page's scroll clip |
| CP1 | Extension search and Git commit input use portable editing; progress rings no longer leak static SVG strings; lists/splits use library scrollbars and handles |
| CP2 | Extension list/detail Split writes its constrained ratio to application state |
| CP3 | Installed extension list uses uniform virtualization; contribution/changelog blocks use variable virtualization; contribution tree and detail tabs use portable controls |
| CP4 | Extension category badges and Outline toolbar icon buttons |
| IN1, IN2 | Named status bar/mount/action nodes expose a public semantic projection |
| IN3 | Owned icon keys pass through the application icon helper and library controls |
| IN4 | Current application palette supplies ThemeContext; explicit styles remain available |
| IN5, IN6 | Runtime descriptions render status text/actions through the retained window tree; public-ID events invoke existing application commands |
| IN7 | Registry activity uses presence transitions and existing frame scheduling |

WidgetValue binds portable controls directly to the application's projected
state. It does not introduce an extension-specific state or window runtime.
Existing specialized editor controls keep their application command adapters.

## Language capability baseline

All three official language plugins now compile private Rust clients into
independent WASM components from `loom-extensions/language-clients/rust`.
The previous TypeScript clients and TypeScript SDK have been removed. Extensions
generate Rust bindings directly from Loom's WIT contract; protocol initialization,
capability negotiation and language behavior remain in the plugins. Historical
TypeScript test counts below describe the earlier migration baseline, not the
current test suite.

Language service packages contribute metadata-only `contributes.languages`
resources for file associations. They work without syntax packages; when both
are installed, existing parser bindings are retained. They use the existing
language-registry restart lifecycle. A service cannot smuggle parser bindings
through a language identity resource.

Source baseline: Loom `5ff103ed`, before changing the host language subsystem.
All four language IDs share the same host routing and interaction code. Provider
availability must continue to follow the selected server's negotiated capabilities;
absence is not permission to drop a previously working host interaction.

| Existing behavior | Current source / migration obligation |
| --- | --- |
| Completion and resolve | `app/language_ui/completion.rs`; preserve replacement/additional edits and snippets. P2 also fixes the observed commit-character follow/caret bugs and adds accepted-item command execution, which the old host did not actually perform |
| Diagnostics | Document push/pull and open-document problems consumption; whole-workspace diagnostics remain the documented LS-07 gap |
| Hover/signature | Existing Markdown presentation and stale-result checks |
| Definition family, references, workspace symbols | Existing navigation/location consumption and source routing |
| Symbols, folds, selection ranges | Existing outline, folding and selection consumers |
| Rename, formatting, code actions | Existing prepare, range/on-type forms, resolve, edits, commands and rollback |
| Inlay hints and code lens | Preserve current hints and lens list/resolve; rich label-part interactions and inline lenses remain LS-04/LS-05 gaps |
| Hierarchies | Preserve existing relation queries/source identity; repeated navigation remains LS-06 |
| Highlights | Extension converts server results to versioned byte ranges and theme classes; existing syntax remains independent |
| Save | Preserve ordered format/action participation, cancellation, failure and document edit application |
| Session/lifecycle | Preserve document versions/resync, roots, watches, server edits/show-document, shutdown and crash recovery through extension-owned clients |

JavaScript and TypeScript require separate fixtures, although one extension serves
both. HTML/Markdown/YAML/JSON do not gate removal of the host LSP. No language
panel or server-specific popup is required in the extension UI acceptance.

Negotiated support was recorded with rust-analyzer 2026-10-19, gopls v0.24.0,
and typescript-language-server 6.0.2 with TypeScript 5.9.3. JavaScript and
TypeScript were exercised separately. These are server omissions, not missing
generic provider APIs:

| Negotiated capability | Rust | Go | JS / TS |
| --- | --- | --- | --- |
| Position columns | UTF-8 | UTF-16 | UTF-16 |
| Definition / declaration / type / implementation | All four | All except declaration | All except declaration |
| Full / range / on-type formatting | Full, on-type | Full | Full, range |
| Completion resolve | Yes | No | Yes |
| Code action resolve | Yes | Yes | Yes |
| Lens resolve | Yes | No | Yes |
| Call / type hierarchy | Call | Both | Call |
| Document pull diagnostics | Yes | No; push diagnostics | No; push diagnostics |
| Semantic full / delta / range | All three | Full, range | Full, range |

The field audit retains completion label details, deprecated/preselect flags,
insert-text mode and the five required edit/snippet/commit/command fields, plus
symbol deprecation. Raw diagnostic codes/data remain private to the client and
are reused for code actions. Hover contents and admitted results are no longer
silently clipped by the old 8,000-character/2,000-entry conversion limits. Whole
message/result budgets remain enforced. Location links use their target
selection range; their origin/full ranges had no native consumer in the source
baseline. Existing LS-01 through LS-10 gaps are not relabeled as implemented.

## P0 evaluation protocol

The comparison uses the same Windows x64 machine, release profile, documents,
server builds, package settings and warm-up for both routes. Record OS, CPU,
memory, Rust/Node versions, server versions and artifact hashes alongside results;
results from another environment cannot substitute for that comparison. Keep the
old host at `5ff103ed` as the reference until P5 has passed.

For each of Rust, Go, JavaScript and TypeScript, use a small single-root project
and a multi-root fixture. Exercise completion/resolve (including auto-import and
snippets), diagnostics after edits, hover, definition family, references, symbols,
signature, formatting, rename, code actions, hints/lenses, folds, selections,
hierarchies, semantic updates and save participants where the server supports
them. Record unsupported server capabilities separately, with the negotiated
capability and the same baseline observation. Replay fake-server cases for
cancel, stale answers, split frames, server edits, watches, root removal and exit.

Before measurements run ten warm-ups; then collect at least 100 request samples
per interactive operation, reporting p50/p95 and sample count. Measure from the
host request to the result being available to the native consumer. Also report
JSON decode/result conversion time separately. The extension route must preserve
behavior, have p95 no greater than baseline p95 + max(10 ms, 20% of baseline p95),
and never perform server IO or unbounded parsing on the UI thread. Diagnose any
timeout or dropped result individually; excluding failed samples is not allowed.

Use identical 1 MiB and 8 MiB document fixtures, including non-ASCII text, to check
bounded snapshots, framing, incremental synchronization and cancellation. The
existing >8 MiB synchronization exclusion remains explicit. Record idle memory,
peak memory during a large answer, and exit/restart time. All streams and queues
must have enforced bounds. Root removal releases its process/watch leases;
trust revocation stops local execution. Stopping, failing or unloading an owner
reclaims all of its resources, pending provider work and UI nodes. UI authorization
continues to use installation confirmation; revoking execution trust does not add
a new UI permission layer.

Each of the three TypeScript component artifacts has a 16 MiB uncompressed WASM
budget, excluding the separately versioned language-server/runtime resources.
Record WASM size and the complete extension package size, plus the exact build
command. A larger artifact or an exceeded latency budget leaves P5 open and must
be resolved or explicitly reviewed before P6; it cannot be relabeled as passing.

## Current implementation and evidence

The latest P2 implementation adds the following platform mechanisms:

- Completion, resolve, formatting and the other native consumers issue requests
  through the shared provider broker. Feature selection, request ownership,
  aggregation, cancellation and positions live in `features/providers`. The old
  host transport, manager and protocol modules have been removed. Native editor
  regressions run a checked-in WASM built from the official private client source,
  with provenance in `tests/fixtures/extensions/language-client/README.md`.
- Declared provider kinds cover the recorded baseline, including rename/prepare,
  references, definition modes, workspace symbols, folds, selections, hierarchies,
  lens/resolve, on-type formatting, action resolve/commands and theme highlights.
  Source identity survives resolve and command execution. Configuration changes
  invalidate affected requests and results. Language selection follows document
  file associations, rather than inferring the language again from its extension.
- Typed locations retain UTF-8/UTF-16 column units for unopened files. Ordered
  workspace changes use existing validation, rollback and shared undo. Native
  navigation and asynchronous operation receipts are exposed through general APIs.
- Immutable document capture includes selected language/variant and lifecycle.
  Saved events preserve the version and bytes written. Pending document and file
  events coalesce with explicit resynchronization/rescan, including cold starts.
  Capture handles, queued answers, logs, push diagnostics and background requests
  have enforced budgets; owner stop/failure and caller cancellation release work.
- Provider diagnostics are stored independently of transport and mapped
  through bounded revision metadata. Versioned highlights overlay existing syntax.
  Generic native UI/edit/save adapters remain as the provider consumers; they own
  no LSP sessions or server processes. Diagnostic documentation links are a typed
  field, rather than retaining server JSON in the host.

`language_servers` and `lsp` settings are retired. `language_features` keeps native
interaction preferences; `languages.<language>.providers.<kind>` orders or filters
extension/provider IDs. Resolve and commands retain their original owner. Process
configuration uses extension-contributed settings. The same persisted trust store
still checks every process launch; granting trust retries relevant language owners.

Registry language-service versions require `loom:extension@0.1.0` ABI metadata.
The extension index now derives served languages from provider selectors. Eleven
historical recipes for other languages remain in the repository and are explicitly
deferred from automatic/manual builds until their component migration; published
assets are preserved. New clients require Loom >=0.1.9 to avoid loading the changed
candidate ABI into released 0.1.8 hosts.

The final measurements are in [the retained evidence](language-migration-evidence/README.md).
The pre-removal native editor run passed 129 tests with four existing ignores.
Real Rust/Go/JS/TS functional and two-root checks passed; private client tests passed
29/29; general SDK tests passed 21/21, along with TypeScript and generated-binding
checks.

Final post-removal verification on Windows:

- Loom release test binary: 1700 passed, 28 ignored, zero failures (1728 total).
  Removed protocol/manager tests account for the smaller count than earlier runs;
  native editor tests now exercise the private client component. The final run
  includes trust revocation/regrant, provider preferences and component index gates.
- Independent editor/core contracts: 117; extension API: 35; workspace: 87;
  Markdown rules: 28. All 267 passed.
- Component host library: 12; real JavaScript guest: 8; Rust guest: 14;
  TypeScript SDK guest: 6. All 40 passed. JS/TS tests reuse the actual engine's
  compatibility-keyed component cache instead of recompiling each assertion.
- General SDK: 21; private clients: 29; real-server functional/two-root cases: 4;
  extension tools: 81; production validator CLI: 6; architecture rules: 66. Passed.
- Workspace/all-target strict Clippy passed with the repository CI allowances;
  formatting, architecture ownership and patch-whitespace checks passed.
- All twelve post-removal performance cohorts (36 operation comparisons) passed
  the unchanged p95 gate with the final staged components, tracing disabled and
  no overlapping builds. Every operation retained 100 measured samples. Raw JSON
  and artifact identifiers are in the evidence directory.

The full-suite failures encountered during finalization were fixed and the whole
suite rerun: a forged-source test's bitwise OR could produce the original ID, and
the problems test accepted an initial empty diagnostic publication. The corrected
tests require a different source and nonempty findings, respectively. No failing
test was removed from the final run. Manual desktop QA and Linux/macOS execution
at this source remain unclaimed.

## Earlier stage verification (historical counts)

Latest full Loom run: `RUST_MIN_STACK=16777216 cargo test --bin Loom --offline --
--test-threads=2 --quiet` passed 1919 tests, with 35 ignored and no failures. The
previous combined run overlapped heavy TypeScript component compilation and had
timing failures as well as actual integration regressions. The native completion
fixture now uses a persistent file association, the old direct-completion test
uses the common broker, and public-ID/border defects were fixed. The complete
subsequent run was sequential with respect to heavy WASM compilation; no failed
test was excluded from that rerun.

Latest focused checks: SDK protocol tests 15/15, TypeScript type checking and WIT
generated binding verification passed. LGUI core/widgets passed 218 + 45 tests.
`cargo check --bin Loom --tests --offline` passed after removing the unused host
entry points. `cargo test -p loom-extension-host --lib --test rust_guest --offline
-- --test-threads=2 --quiet` passed all 6 host library and 14 real Rust guest tests.
A real TypeScript WASM test passed for typed references and 40 consecutive saved
snapshots, exceeding the 32-handle capture limit cumulatively to verify release.
The native completion tests passed through the running guest, including resolve,
snippets, imports and accepted-item commands. Fractional-scale border rendering,
terminal layout interactions, shell directory restoration and asynchronous large
file restoration passed individually after the integration fixes.

Retained LGUI rendering now validates public-ID uniqueness after stale nodes are
pruned, allowing an unkeyed node to move while still rejecting two live duplicates.
Status bar content clips independently of its pixel-aligned outer divider.

Earlier stage evidence follows; these counts describe their respective runs.

P1/PT: the `extensions` test filter passed 235 tests with 4 pre-existing ignored
tests. It includes a real guest driving `fake_lsp` with split length headers,
raw/non-UTF-8 IO, a helper retaining inherited pipes, stream failure/credit tests,
root removal, trust denial/revocation, nested-root working directories and native
file watch delivery. The shared process-tree owner and length decoder no longer
belong to the LSP implementation. Background callbacks now hold weak host handles
so unloading actually releases the executor, processes and watches.

The subsequent P3 run passed 238 extension tests (same 4 ignores), including
description ownership, identity, ancestor visibility, shared unchanged subtrees,
guest color/text updates and failure reclamation. Two native widget tests passed
for actual pointer clicks and committed Chinese/emoji text with byte selections.
TypeScript SDK protocol tests passed 11 tests. The real TypeScript component's
5 runtime tests passed, including general widget creation, update, event handling
and removal. The extension host's 3 library tests passed for mutation/depth
budgets, ownership, invalid edits and process write credits.

Windows native build: `cargo check --bin Loom --tests --offline` passed after
selecting the local LGUI checkout. `RUST_MIN_STACK=16777216 cargo test --bin Loom
ui:: --offline -- --test-threads=2` passed all 233 tests, including the running
WASM guest completion integration, native Unicode input, 5,000-entry virtualization
and status bar description-tree clicks. The LGUI core/widgets library run passed
216 + 45 tests. Runtime descriptions preserve public identity; the description
diff's unchanged nodes retain their shared allocation. No manual visual QA or
Linux/macOS execution is claimed. Provider migration and host LSP removal remain
open gates.
