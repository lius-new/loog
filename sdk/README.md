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
