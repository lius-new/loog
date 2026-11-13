# Resource workspaces

Loog identifies documents by `ResourceRef`: either a native OS path or a
provider-owned path inside a persistent `EnvironmentKey`. The workspace API is
independent of connection protocols. Connection forms, authentication, deployment,
reconnection policy, target tool selection and language-server protocols belong
to extensions. No production host adapter selects a particular remote plugin.

## Identity and ownership

`EnvironmentKey` contains the provider package and its stable instance key. It
contains neither credentials nor runtime handles. `ProviderPath` is a bounded
UTF-8 token interpreted and canonicalized by its provider. The host never joins,
normalizes, case-folds or converts it to `PathBuf`. A provider can use object IDs
instead of filesystem paths. Providers must return the same canonical token for
aliases they consider one editable object; case and distinct symlink identities
are not guessed by the client. Native paths keep their OS representation.

Environment, scope, request and stream IDs are runtime capabilities. Ownership
includes the provider activation generation and the consuming activation. A
matching integer from another scope or generation grants no access. Documents
add their own identity, lifecycle and buffer version. Async consumers also guard
their originating pane/view and the input or operation sequence where applicable.
Shared UI uses a complete, unambiguous resource context key; display labels are
never identity keys.

## Dispatch and authorization

`loog:workspace/session@0.1.0` accepts typed JSON envelopes. Submission returns a
ticket; a later dispatch delivers its receipt, routed request, completion or
notification. The application does not synchronously invoke another component
while a component is running. Components and native consumers use the same
capability router for provided resources.

A provider declares `workspace.provide`, registers an environment, mounts an
opaque root and explicitly presents it. Consumers need declared and granted
capabilities for reads, writes, search, process or terminal operations. Process
and terminal access additionally require trust for the exact environment/root.
Untrusted nested roots are passed as opaque execution exclusions; containment is
checked by the provider. Revoking execution stops executable handles while
keeping file access and subscriptions. Retirement removes roots, requests,
subscriptions, process ownership, diagnostics and service configurations.

`loog:workspace/tools@0.1.0` launches a user-level tool independently of a local
workspace. It requires `process.execute-tool`; it does not inherit permission to
execute project programs. Tool arguments and binary transport contents are not
automatically copied into Output. Old local-process output behavior remains a
marked compatibility path. Providers may explicitly publish bounded diagnostics.

## Paths and bytes

Parent, child, relative-path, expression and URI operations belong to the
provider. Directory pages carry explicit continuation or failure; a failed list
is not an empty directory. Reads return raw bytes, revision and EOF. Native text
decoding, BOM/newline preservation and encoding selection use shared pure rules.
File loading and saving run away from UI rendering. Existing native filesystem
adapters remain asynchronous; their platform semantics are retained.

Provided saves use BeginWrite, ordered AppendWrite and CommitWrite, with an
expected revision and an explicit completion receipt. Until commit, a provider
retains a staged write instead of partially replacing the file. The provider
defines its atomic replacement and revision guarantee. Revision checking plus
replacement is not a universal compare-and-swap against arbitrary external
writers. A conflict or unknown mutation outcome retains dirty text and blocks
another save until reconciliation. Cancellation after dispatch is not evidence
that a write did not happen. The host never automatically retries a mutation.

A successful save acknowledges its captured version; edits made while it was
running stay dirty. Multi-file editor proposals first validate all identities,
versions, ranges and reads, then form one buffer undo group. They do not promise
cross-file disk atomicity, and cross-environment edit transactions are rejected.

Watch notifications are hints. Sequence gaps and overflow trigger reconciliation;
reads and saves still verify revisions. Session recovery persists resource roots
and provider-owned bounded recovery descriptions. Providers activate without an
open local folder. Missing, disabled and disconnected providers leave explainable
offline state. Native recent folders remain native; provider recovery appears in
the resource tree and never becomes a local recent-path entry.

## Native consumers and compatibility

| Consumer | Provided resource behavior |
| --- | --- |
| Explorer and editor | Async pages/open/save/create/rename/delete, dirty buffers, undo and pane identity |
| Settings and EditorConfig | Scoped reads and watches, target-relative names; no local-project inheritance |
| Images and Markdown | Target bytes, scoped relative assets, guarded file/heading/directory navigation |
| Search | Provider disk pages plus eligible immutable dirty buffers; navigation checks the selected file again |
| Source Control | Scoped process execution, discovery/status, decoded text diff, stage/unstage and commit |
| Terminal | Bounded binary transport, input credits, resize, cwd and exit; stopped restoration and no input replay |
| Language plugins | Versioned resource documents/results and target processes; policy and JSON-RPC remain in plugins |

Git retains explicit local adapters for existing history, hunk patch and advanced
repository operations. Provided diff documents have a complete resource identity
and empty legacy local-path fields. Source Control supports one selected repository
per presented scope; its selected environment cannot silently fall back to local
Git after disconnection. Advanced history, branch/network operations and hunk
patching are not provided by the new resource panel. Decoded text diffs do not
pretend to be byte-exact Git patches. Symlink text diffs and metadata outside the
authorized scope fail explicitly.

Native OS drag/drop and Reveal in File Manager remain local operations. Tabs and
editor panes retain resource identity; provided paths are never sent to a local
shell or file manager. Provider resources are not implicitly uploaded/downloaded
by dragging across environments. Old document, language-result and session ABIs
continue to accept local paths only. Their `COMPAT(...)` markers state replacement
and removal conditions; no removal version is scheduled.

## Admission and cleanup

Default router limits are 32 environments, 128 scopes, 256 pending requests,
1,024 events, 1,024 owned handles, 1 MiB serialized payload and 16 MiB queued data.
These are router-wide ceilings, not per-plugin promises. Operation-specific
validation and consumer budgets are stricter. Control cancellation remains
admissible when data queues are full. Accepted output is charged until consumed.
Each dispatch drains bounded batches so one source cannot synchronously monopolize
the native UI. Deadlines, ownership changes and consumer/provider retirement
produce terminal outcomes and release their retained state.

Cold component compilation has its own worker. Its admission budget includes
eight pending/compiling/completed loads and 128 MiB of source bytes; a ninth load
fails explicitly and may be retried later. Running guests continue receiving IO,
timers and cancellation while another component compiles. Stop/reload invalidates
the pending activation, so late compilation cannot resurrect an instance. Shutdown
discards queued loads and waits for the one in-progress compilation to finish;
Wasmtime compilation itself is not preemptible. Instances still execute on the
single bounded-round executor after compiled code is returned.

Native saves admit 64 MiB encoded files and send 128 KiB chunks. Search limits
decoded files to 4 MiB and verifies provider page counters and payloads. Native
bounded command capture admits 8 MiB input, 16 MiB stdout, 256 KiB stderr and a
deadline of at most 600 seconds, with four Git workers. Configuration readers
have their own byte, ancestor and concurrency budgets. None of these ceilings
redefines the limits of the older local process ABI.

The host cannot promise to stop a disconnected target process. Each provider
must define connection-close, lease and cleanup behavior. Restored terminal tabs
remain stopped, and an unknown process or Git receipt is never replayed.

## Verification

`crates/workspace-api/tests/router.rs` covers ownership, old generations,
cancellation, queue admission, expiry and retirement. The independent memory
component in `tests/extensions/rust-guest/src/memory_workspace.rs` uses opaque
object tokens and deliberately delayed/lost receipts. Its native acceptance
shares the production Explorer, text loading, encoding, save and reconciliation
paths without importing any remote implementation.

The opt-in native provider suite accepts an externally built package and profile.
Remote-specific fixtures are test code only. They cover real target IO, native
Source Control controls, search navigation, terminal transport, language plugins,
configuration, previews and session recovery. Headless `UiSession` checks do not
constitute a desktop visual review. Platform CI, deployment-specific protocol
faults and repeated connection measurements remain separate evidence.

SDK users must build against the matching immutable Loog source revision. Unknown
interface versions are rejected, not stubbed. The extension validator and AOT
compiler/loader must use the same contract before publication. Development
artifacts do not change an already published package version.
