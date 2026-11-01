# LSP architecture

This is the maintained design of Loom's local stdio language-server client,
reviewed against the implementation on 2026-10-25. Shared identity and coordinate
contracts are in [Language services](language-services.md). User configuration
and shortcuts are in [Settings](../settings.md#language-servers); publication is
in the [extensions repository](https://github.com/lius-new/loom-extensions) (`loom.lsp.*`).
[Remaining work](../language-services-todo.md) distinguishes missing product
behavior from acceptance that still needs evidence.

## Boundaries and ownership

| Module | Responsibility |
| --- | --- |
| `src/features/language_service/` | Pure protocol/session, positions, sync plans, capability registration, decoding, routing policy and snippets |
| `src/features/language_service/server_set.rs` | Server definitions, language bindings and priority, outside the language registry |
| `src/infrastructure/lsp/client.rs`, `job.rs` | Process, stdio transport, deadlines and process-tree ownership |
| `src/infrastructure/lsp/manager.rs`, `manager_requests.rs` | Instances, document assignments, result freshness, request dispatch |
| `src/infrastructure/lsp/manager_registration.rs`, `lsp_watch.rs` | Atomic registration and filesystem-watch leases |
| `src/app/extensions/` | A server an extension declares: its command and arguments are resolved to files of the extension and of the runtime it requires, then it joins the manager |
| `src/app/language_services.rs`, `language_ui/` | Application snapshots, user interactions, save orchestration and workspace edits |
| `src/app/runtime.rs` | Mounted service lifetime; dropping a service retires its servers |

The coordinator owns instances on its own thread. Protocol reads, writes,
stderr consumption and client driving are independent so a blocked write does
not stop response consumption. Application/UI state stores interaction data and
immutable results, not server processes. Server startup may not block editing or
local syntax.

## Definitions and assignments

Server definitions come from installed extensions and explicit user registrations;
Loom itself ships none. Definitions are not embedded in language descriptions, and
the runtime does not search `PATH` or invoke a package manager. An extension
declares the server command, supported languages, root policy and priority, and
names its files as `${extension}/…` or, for a runtime it requires,
`${runtime.<name>}/…`; the application resolves these to real paths before the
manager sees them, and a command that still has a placeholder is unavailable.
User servers use explicit executable paths. Installing, updating and removing
servers is installing, updating and removing their extensions (and the runtime
they require), which the Extensions page does.
Configuration validation and precedence belong to `features/settings/servers.rs`.

An instance key is the server name, resolved project root and launch definition.
A document has one assignment for every selected server that serves its resolved
language. Different servers can select different roots for the same file.
Equivalent assignments share a process. A changed launch creates a different
instance; configuration updates that do not alter the launch can be sent to the
existing one. Each instance reports its own workspace-folder set.

## Lifecycle, failure and progress

Startup performs `initialize`, validates capabilities/position encoding, sends
`initialized`, and synchronizes the latest assigned documents. Shutdown attempts
`shutdown`/`exit` within bounded waits, then terminates owned processes. Windows
uses a Job Object; Unix launches a dedicated process group and kills that owned
group during cleanup. Both paths have source tests; platform acceptance remains
separate from the existence of that code.

The manager retries crashed servers after 1, 2 and 4 seconds, then requires an
explicit restart; a stable ready interval resets failure accounting. Idle
instances stop after 60 seconds. Unsupported/missing programs report an
unavailable state rather than retrying indefinitely. Work-done progress is keyed
by instance and token; cancellation, missing end reports and stale tasks have
bounded cleanup. Logs and progress lists are bounded.

Transport limits and deadlines are owned by `client.rs`, `manager.rs` and the
session types. In particular, the manager currently accepts messages up to
64 MiB; the old draft's 32 MiB proposal is not the implemented limit.

## Document synchronization and freshness

The application bridge fingerprints open documents, versions, language
generations, workspace roots and relevant settings. A changed fingerprint
produces immutable document snapshots for the manager. This state-based bridge
covers edits, reloads and other mutation paths without relying on individual
editor commands. Documents above `lsp.max_document_bytes` are omitted; the
default is 8 MiB. This limit does not bound the server's whole-project indexing.

Multiple views do not create duplicate server documents. Open sends the latest
text; save is notified only after successful disk save. Path/language changes
retire the old document session. Restart opens the latest text rather than
replaying stale requests. Server synchronization type selects no updates, full
text or an incremental replacement. Incremental planning compares the last sent
and latest snapshots, finds a character-safe common prefix/suffix and converts
the replaced range using the negotiated position encoding.

Protocol versions are allocated monotonically per connection and mapped to the
local document lifetime/version. Reopen and instance restart invalidate old
history. Sync messages and dependent requests use the same ordered transport.
Coalescing may replace pending local targets, but must not silently discard an
already committed protocol sequence. A failed sequence invalidates the instance.

The manager checks document and instance generations; application callbacks
add full `DocumentStamp`, view/input generation, caret, item or query identity as
appropriate. Completion resolve must match the originating instance and actual
candidate payload, not just its display label. Late results cannot update a
different view or a newer interaction. Dropping a request handle cancels it.
Retryable content-modified/server-cancelled failures get at most one retry for
the same still-valid target within the original deadline; commands are not
automatically replayed.

## Capability routing and results

The authoritative advertised client capability object is generated in
`features/language_service/capabilities.rs`. Static providers and per-instance
dynamic registrations determine whether a request is sent. Registration batches
are validated atomically; document selectors and registration IDs affect routing.
Watch registration is acknowledged only after its candidate disk lease is ready.
Run/lease generations reject obsolete callbacks; unregister/restart releases
watches and invalidates affected work. Dynamic support is explicitly limited to
implemented provider methods and watched files; it is not enabled for every
protocol capability.

`Feature::merge()` in `features/language_service/request.rs` defines routing:

| Policy | Features |
| --- | --- |
| Merge all eligible instances | Hover, definition family, references, workspace symbols, completion, code actions, inlay hints and code lens |
| First non-empty result in priority order | Document highlights/symbols, signature help, prepare-rename/rename, folding/selection ranges and hierarchy preparation |
| One selected capable instance | Formatting, range formatting and on-type formatting; language formatter preference is respected |
| Return to originating instance | Completion/code-action/code-lens resolve, hierarchy continuation and execute-command |

Diagnostics remain separated by instance and are combined for display. Push
diagnostics and document pull diagnostics are implemented; a document problem
panel is not a workspace-wide diagnostic engine. Semantic tokens support full,
delta and a stamped whole-document range fallback for range-only servers.
There is no viewport-range coverage cache yet. Decoding validates positions and
delta edits before composing current tokens over local syntax.

## Application interactions and editing

Hover, completion and signature documentation use the shared
[Markdown reader](markdown.md). The Outline drawer consumes document symbols
and local Markdown headings; its current behavior is in [Outline](../outline.md).
Definitions/Peek, references, hierarchy/problem lists, lightbulbs, completion,
signature switching, snippets and save actions are implemented in
`app/language_ui/`. A protocol request type is not a claim of complete UI support:
rich inlay interaction and inline code lens still have gaps. Versioned folding
uses syntax ranges with an LSP fallback; see [Editing](../editing.md#folding).

Workspace edits from rename, formatting, code actions and server `applyEdit`
use `app/language_ui/edit.rs`. Planning validates source stamps, protocol
versions, byte ranges and non-overlap before applying changes. Unopened text
files are read during planning and opened as edited documents; text changes
remain unsaved until normal save. Resource create/rename/delete steps affect
disk through `infrastructure/edit_journal.rs`, using same-volume staging for
reversible filesystem changes.

Application document state, history and transaction bookkeeping are restored
when application fails; filesystem rollback failures are reported and retained
backups are identified. Successful text edits share a cross-document transaction
ID. Undo/redo requires the participating documents' history tops to match; it
must not silently undo only one part. This shared text undo contract does not
promise that committed disk resource operations are themselves user-undoable.
Save actions run through the same edit validation before the actual save.

## Verification and extensions

Pure protocol, fake-server, manager and application tests cover framing, Unicode,
sync, cancellation, dynamic registration, stale responses and workspace edits.
`examples/fake_lsp.rs` supplies reproducible server behavior. Real-server tests
in `src/app/real_language_server_tests.rs` are opt-in and require installed servers;
ordinary workspace test success does not imply they ran.

```sh
node scripts/architecture/check.mjs
node scripts/build/cargo.mjs test --locked --workspace
```

Real installation prerequisites are documented in the test ignore reasons. Add capabilities only with matching
negotiation, routing, decoding, freshness, cancellation and host tests. Keep
language-specific server recipes outside protocol code. Remote/TCP servers,
DAP, notebooks, embedded subdocument LSP and unnamed buffers are separate future
scopes, not implied by the current local client.
