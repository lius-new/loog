# Loog extension API bindings

The shared extension contract is `wit/loog-extension/world.wit`, with independent
UI placement and motion in `wit/loog-extension/deps/ui/world.wit`. Rust extensions generate
bindings with `wit-bindgen`; see `tests/extensions/rust-guest` for a working guest.
The previous TypeScript SDK has been removed.

The contract exposes generic document, provider, process, workspace and UI
capabilities. Language server clients, protocol initialization and server
installation rules belong to the corresponding plugins in `loog-extensions`.
The three official language clients use Rust WASM and do not embed a JavaScript
engine. The TypeScript language server still uses the separate Node runtime.

For rotation, scaling, translation, opacity and smoothing through Rust bindings,
see [extension motion](../docs/architecture/extension-motion.md). The host plays
the descriptions without per-frame guest calls.

For one reusable UI region shared by independent plugins, use
`loog:ui/shared@0.1.0`; see [shared UI and application
boundaries](../docs/architecture/shared-extension-ui.md). The application never
defines a particular plugin's region, status wording or state-selection policy.

Workspace providers use the typed `loog-workspace-api` operations over the
asynchronous workspace session. Persistent `ResourceRef` values contain an
environment identity and an opaque provider path; neither a consumer nor the
host may reinterpret that path with the client's filesystem rules.

Content search starts with `Operation::Search { path, options }` and returns an
owned handle. `SearchNext` consumes a bounded `SearchPage`; an empty incomplete
page asks the consumer to back off before polling again. File publications may
continue across pages, identified by their path and decoded-content hash.
Progress counters are cumulative. Consumers validate display components, UTF-8
offsets, preview sizes and total budgets before publishing results. They close
the handle with `SearchClose` after completion, error or cancellation. A lost
page receipt has an unknown outcome and must not be retried automatically.
Unloading either owner releases its live handles through the normal router.

`SearchOptions.buffers` contains only unsaved resource identities. Providers
apply their traversal and ignore policy before returning an eligible buffer
entry; consumers then search their own immutable text without transmitting it.
The portable `loog-text-core` crate supplies identical decoding, regex, glob,
preview and navigation-fingerprint rules to local consumers and provider tools.
It contains no IO, transport, environment setup or plugin selection policy.
