# Loom extension API bindings

The shared extension contract is `wit/loom-extension/world.wit`, with independent
UI placement in `wit/loom-extension/deps/ui/world.wit`. Rust extensions generate
bindings with `wit-bindgen`; see `tests/extensions/rust-guest` for a working guest.
The previous TypeScript SDK has been removed.

The contract exposes generic document, provider, process, workspace and UI
capabilities. Language server clients, protocol initialization and server
installation rules belong to the corresponding plugins in `loom-extensions`.
The three official language clients use Rust WASM and do not embed a JavaScript
engine. The TypeScript language server still uses the separate Node runtime.
