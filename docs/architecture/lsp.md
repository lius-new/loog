# Language provider architecture

Loom consumes typed language providers. Language clients and LSP protocol behavior
are owned by extensions in [loom-extensions](https://github.com/lius-new/loom-extensions/tree/main/language-clients).
The first migration covers Rust, Go, JavaScript and TypeScript. Its baseline,
server omissions and measurements are recorded in
[the migration record](extension-language-migration.md).

## Boundaries and ownership

| Layer | Responsibility |
| --- | --- |
| `wit/loom-extension/world.wit`, `wit/loom-extension/deps/ui/world.wit` | General document, provider, process, workspace, watch, timer and UI contracts; Rust bindings are independent of target language |
| `crates/extension-host` | WASM isolation, typed results, bounded queues and owned resources |
| `src/infrastructure/extensions` | Generic execution, framed streams, process-tree cleanup, watches and persisted trust |
| `src/features/providers` | Request ownership, aggregation, cancellation, coordinate indexes and retained results |
| `src/app/extensions` | Typed admission, document/root events, source routing, capability revisions and workspace operation receipts |
| `src/app/language_services.rs` | Transport-independent request broker and versioned publications |
| `src/features/language_service`, `src/app/language_ui`, `src/ui/editor` | Native presentation models, snippets, navigation, editing, save orchestration and drawing |
| Official extension clients | RPC, initialization, capability negotiation, synchronization, project selection, dynamic registration, diagnostics, semantic decoding and recovery |

There is no host LSP client, server manager, server declaration adapter or server
configuration catalog. Native presentation models use structured internal values;
raw server results are converted by the private client before crossing WIT.
Resource-only syntax extensions and language services are independent.

## Requests and freshness

Providers are matched by declared kind and selected document language, then by
negotiated availability. Per-language `providers` preferences order or exclude
owners for each kind. Formatting chooses the first candidate; aggregate features
retain source identities. Resolve, hierarchy and command requests return to the
original source regardless of later preference ordering.

Every request captures document incarnation, version, language generation, owner
generation and provider revision. Stale answers are discarded. Cancellation,
deadlines, owner retirement and changed configurations release pending work.
Command-triggered edits may advance the document version while retaining the
original incarnation, language and owner checks. Server-initiated edits use the
same generic validation, rollback, undo and completion-receipt path as other
extensions. Save actions wait for those receipts before formatting and writing.

Diagnostics follow bounded revision metadata while untouched ranges remain valid.
Semantic results apply only to their exact version and language generation. Native
coordinates are UTF-8 byte ranges; unopened-file locations retain their stated
column unit. Shared immutable coordinate indexes are bounded and never retain a
document after its last owner disappears.

## Execution and interface

Persistent process streams share task admission and the existing innermost
workspace trust decisions. A root removal or trust revocation terminates its
processes. Trust restoration retries relevant process-capable language owners;
it grants no execution permission and every subsequent launch is checked again.
Framing, backpressure and process-tree ownership are generic infrastructure.
Protocol methods and server lifecycle policy stay in the client component.

An extension creates, updates and deletes its own status color block through the
description UI API. Owner stop/failure reclaims its descriptions. Loom provides
the status-bar mount and general widgets. There is no language panel or server
details card.

The retired `language_servers` and `lsp` settings are reported as unknown. Use
extension-contributed configuration for client options, `languages` for provider
selection/save behavior and `language_features` for native interaction preferences.
Registry versions of language services must advertise the component ABI; old
declarative versions are excluded. Other languages' historical recipes remain in
the extension repository, explicitly deferred from current builds.

Manual desktop inspection and Linux/macOS execution at this revision remain
separate acceptance work. Existing product gaps in
[language services remaining work](../language-services-todo.md) are preserved;
the migration does not claim rich hint interactions, inline lenses, recursive
hierarchy views or workspace-wide diagnostics that the original host lacked.
