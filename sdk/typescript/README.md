# Loom extension SDK for TypeScript

This is the general SDK for **writing extensions in TypeScript**. It supports
extensions for any target language or tool. JavaScript/TypeScript language server
clients belong to their plugins in `loom-extensions`; they are not part of this SDK.

Status: candidate. It follows `wit/loom-extension/world.wit`, which is not frozen.

An extension is a TypeScript module that registers commands and providers and
exports the SDK's `guest`:

```ts
import { commands, guest as sdkGuest, providers, workspace } from '@loom/extension';

commands.register('acme.tools.uppercase', async (call) => {
  const snapshot = call.snapshot;
  if (!snapshot) return;
  const edits = call.selections.map((s) => ({ range: s.range, text: snapshot.read(s.range).toUpperCase() }));
  await workspace.applyEdit({
    title: 'Uppercase Selection',
    selectionPrecondition: call.selectionPrecondition,
    documents: [{ snapshot, edits }],
  }, call.call);
});

providers.register('acme.tools.hover', 'hover', ({ cursor }) =>
  cursor === undefined ? undefined : { contents: 'Hello', range: undefined });

export const guest = sdkGuest;
```

`async`/`await` works across the host's rounds: a host operation returns a Promise
that a later round settles, and the SDK reports to the host whether work is still
pending. Handlers may throw; the failure is logged and does not stall the host.
The snapshot of a command or provider is released when the handler ends.

## What the SDK takes care of

- **Offsets.** The contract counts UTF-8 bytes; JavaScript counts UTF-16 units.
  `byteOffset`, `stringIndex`, `slice` and `byteRange` convert without ever
  producing an offset inside a character. `Snapshot.textIn(range)` reads a byte
  range out of the whole text.
- **Provider kinds.** `providers.register(id, kind, handler)` types the result by
  kind (`diagnostics`, `formatting`, `save`, `completion`, `completion-resolve`,
  `hover`, `definition`, `document-symbols`, `signature-help`, `inlay-hints`,
  `code-actions`, `code-action-resolve`, `references`, `workspace-symbols`,
  `document-highlights`, `rename`, `prepare-rename`, `folding-ranges`,
  `selection-ranges`, `call-hierarchy`, `type-hierarchy`, `code-lens`,
  `code-lens-resolve`, `format-on-type`, `execute-command`, `highlights`) and sends
  it to the matching host call. A request whose id or kind has no handler is
  logged. Registering a resolve handler preserves the original provider handler.
- **Cancellation.** `call.cancelled` turns true after the host cancels a command
  or provider request. Cancelled provider handlers cannot publish late answers;
  their request snapshots are released. Requests have host-enforced deadlines.

## Documents and provider results

`workspace.captureDocument(document)` captures an immutable snapshot of an open
document together with its path, selected language/variant, version and lifecycle.
Release the returned `snapshot` after reading it. Captures require document read
permission and follow the host's synchronization policy; closed documents and
documents excluded by that policy cannot be captured. Each instance has at most
32 captures and 64 MiB of captured text. Document event queues coalesce pending
edits, use resynchronization when needed and do not retain every full-text version.

`workspace.onDidSave(listener)` receives the exact bytes and version saved, even
if the editor has since changed. The SDK keeps that snapshot alive until all
listeners finish, then releases it. `onMessage` receives document lifecycle events,
settings, roots and `workspace-files-changed` notifications. Workspace file notification
overflow produces `rescan`; it must trigger a fresh inventory. Disk watches and
editor file operations are separate event sources.

Provider requests expose `context` for operation modes, queries, rename names,
formatting options, signature context and opaque resolve data. Typed result
variants represent locations, ordered workspace edits, actions, hierarchy edges,
lenses and theme-class highlights. File ranges explicitly carry their column
unit (UTF-8 bytes or UTF-16 units); snapshot offsets always use UTF-8 bytes.
Signature parameter label ranges use UTF-8 byte offsets into the signature label.

`providers.configure(configuration)` updates a declared provider's availability,
resolve/prepare support, trigger characters, commands and aliases. Aliases let
existing formatter settings select the same provider. Disabling or changing a
provider invalidates its pending requests and cached results. Diagnostics may also
be pushed with `providers.publishDiagnostics(provider, path, version, items)`;
the host validates ownership, document identity and version before consumption.

`workspace.applyWorkspaceEdit(request)` supports ordered text and resource
changes through the host's existing validation, atomic application and undo path.
Text edits require the workspace edit grant; resource changes additionally require
file write permission. `workspace.showDocument(location)` uses native navigation.
Both return a Promise settled by the host's operation receipt. They contain no
protocol-specific request handling.

## Persistent processes and file watches

`processes.spawn(request, onOutput)` starts a binary, bidirectional process without
a shell. `framing: 'raw'` delivers chunks; `'content-length'` delivers complete
message bodies on stdout and adds the length header to each stdin write. Stderr
always contains raw bytes. `write(Uint8Array)` resolves after the bytes have been
written, preserving call order and applying backpressure. `closeStdin()` waits
for queued writes; `kill()` ends the process tree; `exited` resolves once with the
exit code or startup error. A message is at most 32 MiB; pending stdin and guest
output each have a 64 MiB native budget. Tiny output messages also consume queue
credit, so empty frames cannot create an unlimited event queue.

Use `workspace.roots()` for opaque root handles and native paths. Processes share
the task admission rule: the execution capability must be declared/granted and
the target root and effective working directory must be trusted. Omit `root`
only when one open workspace folder can be inferred. `cwd` is relative to that
folder and may not escape it through links. Package resources can be referenced
with `${extension}/...`; runtime resources use `${runtime.node}/...` and require
a matching installed dependency in `requires`. These are general resource paths,
not server-specific launch logic.

`workspace.watchFiles({ root, patterns }, listener, onError)` registers relative
globs, requires the existing file read grant, and returns `{ id, ready, dispose }`.
Await `ready` before taking an initial disk snapshot. A batch contains relative
paths and create/change/delete kinds; `rescan` with an empty path means refresh
the complete watched state after overflow or lost events. Events and queues are
bounded, and disabling an extension or removing a root releases its subscriptions.
`onMessage` also receives `roots-changed` with the complete current root list.

## Completion edits and commands

Completion items support `textEdit`, `additionalTextEdits`, `insertTextFormat`
(1 plain, 2 snippet), `commitCharacters` and `command`. Edit ranges refer to the
requested snapshot in UTF-8 bytes; malformed or overlapping edits reject the
item. `providers.onCommand(providerId, handler)` receives a command only after
accepted edits succeed, and only in the instance that offered the item. Command
arguments are an opaque JSON array string interpreted by the provider. This SDK
does not implement any language service protocol.

A completion handler may return an array or `{ items, incomplete }`. The latter
allows the native consumer to request an updated list as the user continues typing.

## Owned interface widgets

`ui.create('statusbar.extensions', description)` creates a node under the initial
mount; pass a returned node as the parent to build its subtree. `ui.block`,
`ui.row`, `ui.column`, `ui.text`, `ui.icon`, `ui.button` and `ui.input` construct
general descriptions. Colors are explicit RGB values and are not mapped to host
status categories. A handle offers `update(patch)`, `remove()` and `onEvent`;
removing a parent also removes its descendants. Inputs report complete committed
text and UTF-8 byte selections. The owner can update its controlled text value.

The host rejects foreign parents/handles, duplicate IDs, non-finite dimensions,
excessive depth and oversized descriptions. Each instance has at most 256 nodes,
16 levels, 64 KiB of description text and 64 mutations per dispatch round.
Publishing coalesces to at most once per 16 ms per instance, retaining only the
latest pending tree; state events coalesce in a bounded queue. Public IDs contain
an owner namespace, host session and activation generation. A stopped, failed or
unloaded owner loses its widgets; stale input cannot reach its replacement.
There is no interface query API and no operation for changing host-owned nodes.

## Building

`timers.after(milliseconds, callback)` creates an owned, cancellable one-shot
timer. `clock.now()` returns monotonic nanoseconds as a `bigint`, relative to the
current instance; use differences for elapsed durations. Neither API exposes the
wall clock. Timers are bounded and removed on delivery or instance disposal.

`onTiming(listener)` optionally observes SDK operation durations in monotonic
nanoseconds. Completion encoding and host submission have separate measurements.
No clock reads are added when it is unused, and observer errors cannot prevent
an answer from being delivered. Observations contain operation names and durations
only.

Immutable document text is cached within a shared 32 MiB budget, keyed by
document identity, lifecycle and version. Changed or reopened documents cannot
reuse an earlier text. Temporary save-participant snapshots are kept separate.

Ordered workspace changes create empty files; a following text change supplies
contents. Resource changes retain overwrite/ignore options and share native
validation, rollback and undo.

Large completion lists use UTF-8 bytes encoding the same typed completion records
to reduce canonical ABI overhead. This closed schema rejects unknown fields and
uses decimal byte offsets without Number rounding. It shares the existing
request, result and publication budgets; it does not pass server messages through
the host. Small lists and resolve retain the direct typed import.

Completion `context.query` is the native filter pattern when it is at most
256 bytes and the request is not a trigger-character request. Providers may
conservatively exclude candidates the native matcher would reject. Such a subset
must report `incomplete: true`; deleting or broadening the pattern requests a new
list, including a manual completion with an empty pattern.

Processes may select `content-length-text` framing for UTF-8 message streams.
The frame decoder validates UTF-8 and delivers `output.text`; `output.bytes` is
empty for these stdout frames. Raw and `content-length` modes retain arbitrary
binary bytes, and stderr remains binary in every mode. All modes share the same
process ownership, admission, framing and stream-credit budgets.
`process.writeText(text)` writes UTF-8 through that same ordered write queue;
it retains the same save-participant restriction as binary writes.
`process.writeOwned(bytes)` transfers a dedicated ArrayBuffer into the same
ordered queue, avoiding an extra large-message copy. Callers must stop using
the supplied bytes, whose buffer may be detached. Partial views and engines
without transfer support retain a defensive copy. Ordinary `write` always copies.

```sh
node sdk/typescript/build.mjs [entry.ts] [out.wasm]
```

It checks that `generated/` matches the contract, type-checks the sources with
`tsc`, bundles the entry with the SDK into one file and turns it into a component
with the pinned jco. With no arguments it builds `example/src/main.ts` into
`target/extension-guest/ts_guest.wasm`. The toolchain is the one pinned in
`tests/extensions/js-guest/package.json` (installed on first use).

`generated/` holds the TypeScript types of the contract. After changing the
contract run `node sdk/typescript/generate.mjs`; `--check` fails when they differ.

## Tests

```sh
node --test sdk/typescript/test/sdk.test.ts          # offsets and the round protocol, with a fake host
cargo test --release -p loom-extension-host --test ts_guest   # the built component in the real host
```

## Known gaps

No published package yet (`@loom/extension` is resolved by path), no source maps
from errors to TypeScript lines, and a component is about 12 MiB because it
carries the JavaScript engine: build and cache it ahead of activation.
