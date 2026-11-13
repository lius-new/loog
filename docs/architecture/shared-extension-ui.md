# Shared extension UI and application boundaries

For resource identity, asynchronous workspace capabilities, native consumers and
recovery, see [Resource workspaces](resource-workspaces.md).

## Required ownership boundary

Loog provides reusable application capabilities. It MUST NOT contain an adapter,
container declaration, shared key, presentation state, priority mapping, language
selector or display text for a particular plugin or plugin family. Adding a plugin
that uses an existing capability MUST NOT require changing or releasing Loog.

Plugins own their UI descriptions, shared keys, context membership, ordering,
state interpretation, icons, colors, wording and animation requests. A private
library compiled into several plugins may implement their common policy. That
library is plugin code, not an application-owned service adapter.

The application may implement a general mechanism once, such as creating text,
placing a root, playing keyframes or reconciling a shared region. Its inputs are
extension data; the implementation MUST NOT branch on plugin IDs, shared keys,
server programs, protocol messages, languages or plugin state names.

Auto-sized text occupies its measured advance. Extra paragraph paint width used
to prevent wrapping MUST NOT become an implicit external layout margin. Shared
regions inherit spacing from their native layout; plugin padding is explicit.

## General shared-region contract

`loog:ui/shared@0.1.0` is independent of the original host ABI. An extension:

1. Creates its private candidate tree with the existing UI operations.
2. Places the candidate root against a public native target with `tree.place`.
3. Calls `shared.publish(root, contribution)` with an opaque versioned `key`, an
   optional list of eligible `contexts`, and an integer `priority`.

This is an opt-in contribution to a shared region, not permission to mutate a
different extension's nodes. Handles and event origins stay instance-owned.
Publishing another candidate with the same key joins the existing region. It
does not mount another native container or create a hidden native status block.
Only the selected candidate's tree is mounted, inside one stable native region
keyed by the extension-supplied shared key. Other candidates remain descriptions
in their owners' stores and incur no per-frame guest callbacks.

Publication replaces one owned root's contribution atomically. The application
serially reconciles the complete candidate set, so concurrent activation cannot
produce duplicate mounted regions. For each key it selects the highest-priority
visible eligible candidate; ties use owner ID then local description ID, never
arrival time. Priority numbers have no application-defined business meaning.

`contexts = none` is global. `some([])` is dormant. A nonempty list matches the
current application context by exact string equality. The current standard UI
context is the active view's document path, with the same spelling supplied in
`DocumentRef.path`; plugins MUST copy that value rather than inventing path
normalization rules. No active document or no eligible candidate hides a scoped
region. Context selection does not start, stop or change any plugin process.

Removing a root, unloading/reloading its owner or ending the extension session
releases that owner's candidate only. Another eligible participant continues to
use the same region. There is no privileged first creator. The region disappears
when there is no eligible candidate, and its remaining registrations disappear
when their last owner leaves. Stale generations cannot replace newer candidates.

## Service status is data, not UI

Language service status is not a shared region. An extension that runs a service
reports it with `loog:service/status@0.1.0`: `publish(report)` and `withdraw(key)`,
where a report is a `key` naming one running session, a `state` (`stopped`, `starting`,
`ready`, `working`, `error`), an optional one-line `detail` and optional `contexts`
(document paths, spelled as in `DocumentRef.path`; none means every document it
serves). The application owns the status bar icon, its words, colors and motion, and the
language services panel: they are drawn from these reports and the extension lifecycle,
for the services that answer for the file being edited. An extension creates no node for
them and requests no animation.

The state names are the application's contract, shared by every extension. They are not
plugin state names, and the application does not branch on anything else a plugin says.
Reports are bounded (16 sessions, 200 bytes of detail, the same context limits as shared
regions), replaced by key, withdrawn when the extension stops, and unavailable during save
participation. A refused report leaves the previous one. Extensions older than this
interface keep publishing their own status region, which the shared-region rules above
still mount; no removal of that path is scheduled by this change, and it disappears as
those extensions are replaced by versions that report instead.

Resource-aware clients use the independent workspace session's `ReportServices`
command. It atomically replaces the current activation's reports; an empty list
withdraws them. Each report has the same state/detail meaning, with optional
`ResourceRef` contexts matched by full identity, including provider and environment.
The host admits at most 16 reports, 128 contexts per report, 200 detail bytes and
256 KiB of serialized reports per activation. It rejects duplicate keys and
stale generations, and clears reports on unload. `documents.read` and `languages`
grants are required. State interpretation and transitions remain plugin-owned.

The base status ABI retains literal `DocumentRef.path` matching for local resources;
it never treats a provider path as a local path. Global reports remain global.
There is no scheduled removal of that ABI. The language services panel and status
bar now select their context using the active document's complete resource identity.

## Resource text proposals

`ResolveSettings` resolves only the caller's declared defaults and user values.
It never inherits the application's native-project settings. Optional project
data must be an object of at most 256 KiB and accompany an available, readable
resource scope; only a trusted scope can override project-scoped declarations.
User-only declarations retain user values. Results contain JSON text for at most
256 declared keys and at most 256 KiB total. Reading and interpreting a provider's
project settings file belongs to the consumer plugin. This command is read-only
and is permitted during snapshot save participation.
An accepted query subscribes that activation to `SettingsChanged`. Only changes
to its declared user-layer values trigger this resource event; unrelated keys
and native-project overrides do not. Notifications coalesce until the next
accepted resolution, retry when mailbox credit is unavailable, and retire with
the activation. Resource root/trust changes use the existing `RootsChanged`
event. A consumer must invalidate any pending reads and its project cache before
resolving again. Project-file watching remains the consumer's responsibility.

`ConfigureLanguageProviders` supplies the actual options of bound, declared
providers for exact resource contexts. A batch atomically replaces the activation's
resource configurations; after its first accepted batch, omitted contexts are
unavailable. Native ABI configuration remains independent. Mode filters, aliases,
resolve/prepare flags and trigger characters participate in source selection and
capability queries. Limits are 128 groups, 128 contexts per group and 512 KiB total;
each string list has at most 64 items. Trigger strings may include newline and are
limited to 16 bytes; command/alias strings are limited to 256 bytes without line
breaks. Duplicate provider/resource pairs and stale activations are rejected.
Changing a resource's options invalidates its affected outstanding requests;
disabled contexts lose only their own findings. A native server configuration
change never cancels resource calls or clears their diagnostics.

`ProvideWorkspaceEdit` completes an outstanding resource rename request with
typed `ResourceDocumentEdit` values. URI interpretation and language-server
version translation belong to the plugin. The host requires `documents.read`,
`languages` and `workspace.edit`, a bound declared provider, the current activation,
the exact source revision and any supplied target revisions. Every target must
belong to the source environment. Duplicate resources, mixed identities and
reversed ranges are rejected before delivery. Limits are 16 resources, 1024 edits,
512 KiB replacement text and 1 MiB serialized data.

The editor reads unopened targets through the resource provider and checks all
captured buffer stamps again before committing one undo group. Input text is
limited to 8 MiB; at most four preparations can be retained across windows, each
with a 30-second deadline. Invalid positions or one stale target reject the entire
buffer transaction and restore any newly opened tabs. This operation never saves
files. Saves retain individual revision receipts and conflict handling; there is
no cross-file disk atomicity. The command's `Done` acknowledges result delivery,
not application or persistence.

`ProvideCodeActions` completes a resource action or resolve request through the
same owner, source-version, permission and target validation. Offers contain
typed resource edits, bounded opaque data and provider commands; no native path
conversion is involved. Limits are 64 actions (one for resolve), 768 KiB total,
64 KiB of data per action and 64 command arguments. `FinishLanguageRequest`
reports an explicit bounded provider failure, or actual execute-command completion.
Command completion may follow edits to the source, but still requires its exact
resource and document lifecycle. Failures enter the common request aggregation
and cannot become synthetic success parts.

Resource actions read unopened targets asynchronously. A following command runs
only after a successful buffer transaction and while the originating document
remains active in the same lifecycle. Save-time actions reuse the existing step
and overall save deadlines; expired or superseded preparations cannot commit.
The ordinary snapshot-only save-participant chain also accepts explicitly bound
resource providers. Participants read each preceding participant's temporary
snapshot, and the final buffer change remains one undo step. Save participation
does not gain process writes, filesystem mutations or other side effects.

`ApplyWorkspaceEdit` is a separate generic editor operation. It requires
`documents.read` and `workspace.edit`, an exact open origin revision and targets
in that origin's environment. It has the same proposal budgets and at most four
outstanding operations per host. Its ticket receives a deferred `EditsApplied`
reply with a transaction number and changed-resource count only after the buffer
commit. Failures return a typed failure. The origin, activation, authorization
and deadline remain guarded during reads. `CancelWorkspaceEdit` marks an owned
pending ticket before commit; the original ticket still receives an outcome.
Cancellation of a completed or unknown ticket fails and never implies rollback.
Lost mutation receipts must not cause automatic resubmission.

The base language-answer union continues to support old local-path results;
resource requests accept only its path-free variants or the typed workspace
commands. There is no scheduled removal of the local ABI.

## Output is data, not UI

An extension writes lines for the Output panel with `loog:log/output@0.1.0`:
`channel(key, name)` declares one of its channels, `append(key, level, text)` adds
a line (`trace`, `debug`, `info`, `warn`, `error`) and `clear(key)` empties it. The
application owns the panel, the channel list, the level filter and the words
around them; an extension creates no node for them. Nothing crosses back to the
extension when the person selects, filters or clears a channel.

A channel belongs to the extension that declared it, so two extensions may use
the same key. At most 8 channels per extension, names of one line and at most 64
bytes, lines of at most 8 KiB. Output is handed to the application after each
round; a burst beyond the budget is refused with an error the extension can see,
never dropped silently. Each channel keeps its newest lines only (10,000 lines or
4 MiB). Unavailable during save participation, like service status.

The application also records, without any plugin-specific rule, the stderr and
stdout of an extension's processes and the extension's own lifecycle (activated,
could not start, stopped) in channels named after the extension.

## Bounds and compatibility

Only an instance-owned detached root placed against a public target can be
published. Reparenting a published root into a private subtree is rejected.
Existing UI ownership, depth, node-count and 64-operations-per-round limits apply.
Publication is a side effect and is unavailable during save participation.

Keys contain 1–128 UTF-8 bytes, without NUL. A contribution has at most 256 context
strings, each containing 1–4096 bytes without NUL, with at most 32 KiB total.
Contexts are sorted and deduplicated. Invalid publication leaves the previous
contribution unchanged and returns an error; it never silently truncates data.

The base `ui-description` and base extension world are unchanged. Existing
base/tree/motion components remain supported without participating in sharing.
New components require the independent shared interface. Unknown versions are
rejected rather than stubbed. No removal of the old import surfaces is scheduled.

## Plugin-owned vector icons

The existing icon string accepts a registered icon name or
`svg-path:<width>x<height>:<SVG path data>`. The latter draws a filled path in
the node's foreground color, scaled from the supplied origin-zero viewport to
the icon's layout box. It uses nonzero winding. Plugins own the geometry; adding
an icon must not require host registry entries or plugin-specific host aliases.
The string remains bounded to 256 UTF-8 bytes and viewport dimensions to 1–4096.
Invalid paths render empty. Only SVG path command letters, numeric characters
and whitespace are accepted; XML, external resources, fonts and scripts are not
supported. Paths reuse the existing SVG icon rendering and bounded cache,
including foreground color, display scaling and composited animation.
This additive string syntax requires a path-capable host; it does not add a new
WIT import or alter the base record ABI. Named icons keep their existing meaning.

## Command palette sessions

`loog:palette/picker@0.1.0` lets an extension use the application's command
palette without the application knowing what the words mean. The palette owns the
field, the rows, the tags before the text, the busy and failed marks, the masking
of a secret and the keys. An extension supplies words and opaque ids.

- `publish-entries` replaces the extension's rows in the command list (at most 64).
  Choosing one answers `event.entry` with the extension's own id.
- `open`, `update` and `close` run one session at a time: tags, a placeholder, a
  mode (`choose`, `text`, `secret`), choices grouped by name, and an activity
  (`idle`, `busy`, `failed`) with a one-line note. A new step or mode starts the
  field empty. `secret` text is drawn as bullets, is not exposed to accessibility
  and is cleared after it is sent.
- Answers come back through `poll`: `accepted` (the chosen id or the text) and
  `cancelled`. Closing the palette by any means cancels the session; an extension
  that closes it is not told. A replaced session gets no answer.
- `pick-file` shows the system file dialog and answers with a path or none.
- Mode `browse` is a field with a tree under it, for an extension that lets the
  user pick something from a hierarchy. The rows are given in order and drawn as
  given; the palette does not search or filter them. A choice may carry `depth`
  (at most 32), `expanded` (an arrow, open or closed), `icon` (`folder`, `file`),
  `dim` and an `action`: a hint shown as a mark at the row's end only while the
  pointer is on the row. The view may also set the field `text` and the `selected`
  row, which the list scrolls to. The extension hears the typed text as `input`
  (coalesced), a Tab on a row as `complete`, and Enter, a row click or the action
  mark as `accepted` with a `trigger` (`enter`, `click`, `action`). A row click
  does not end the session; the extension decides what a click means.

The application MUST NOT branch on an extension, a tag, an id or a group name.
The activation event `application.start` wakes an extension once when the
application starts, so rows it publishes exist before any command runs.

## Verification and development

Host tests cover ownership, bounds, validation atomicity and import versions.
The application model covers context matching, deterministic selection, removal,
reload and stale generations. Native retained rendering covers one mounted
region and stable animation during ordinary frame reuse. Real component
acceptance checks extension-provided shared metadata across the WASM boundary.

Local staging is a development operation. It MUST NOT silently update published
immutable packages, bump versions or publish a release. Before a future release,
SDK source, host validator and AOT compiler pins must contain the interfaces used
by the new components. That release alignment is separate from local acceptance.
