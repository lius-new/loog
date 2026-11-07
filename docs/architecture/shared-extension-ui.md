# Shared extension UI and application boundaries

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
