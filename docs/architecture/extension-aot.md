# Authenticated extension AOT

Official language-client packages can carry release-built native Wasmtime code.
An AOT-capable host loads a matching authenticated artifact without compiling
the component locally. The original WASM remains available for older hosts,
unsupported targets and incompatible runtime profiles.

## Artifact and authority

The package contains `aot/<target>/manifest.json`, `manifest.sig` and
`component.cwasm`. The detached Ed25519 signature covers the exact metadata bytes
prefixed by `loom.extension.aot.v1\0`. This separates AOT compiler attestations
from registry signatures even though the existing official publication key is
used. Metadata binds the plugin ID/version, original WASM SHA-256, native-code
SHA-256, target triple, versioned profile and engine compatibility fingerprint.

Native serialized code is not safe to load merely because a package supplied
it. The application pins the compiler authority; package data, project trust and
plugin settings cannot add an authority. Signature, identity, runtime and hashes
are checked before deserialization. Code is read into owned bytes, authenticated,
then deserialized from that same allocation to avoid verifying one file image
and later executing a changed mapping. Metadata, envelopes and code have bounded
read sizes. Production does not read the test-harness trust environment variable.

## Engines and compatibility

The first profile is `loom-wasmtime-50.0.1-portable-v1`. Both the host and the
publisher compiler pin Wasmtime 50.0.1. Explicit target triples use baseline CPU
features rather than the publisher machine's detected CPU. Publication supports
Windows, Linux and macOS on x86_64 and aarch64.

The ordinary engine and its existing CPU-tuned local cache remain unchanged.
A second portable engine loads AOT. Both receive epoch ticks; instantiation
accepts only engines owned by that Runtime, preserving round deadlines and the
existing store/resource limits. AOT does not add guest filesystem/process powers.

The loader reports `published-aot`, `local-cache` or `compiled`. The retained
`cache_hit` flag continues to mean a local compilation-cache hit; it is false for
published AOT. Missing AOT uses the existing WASM/cache path. Rejected AOT also
falls back and records why, rather than loading unauthenticated native bytes.
This compatibility path has no scheduled removal version.

An engine/profile change requires newly compiled, signed artifacts under new
plugin versions. Existing published packages remain immutable. A version label
alone does not make incompatible serialized code usable; the runtime fingerprint
must match. Old Loom binaries do not gain AOT support by installing a new plugin.

## Publisher and validation

`loom-extensions/scripts/prepare-aot.mjs` runs inside the trusted publisher.
It discards any native AOT handed over by plugin builders and compiles the actual
WASM with the locked compiler before signing. Plugin build jobs never receive
the signing key, and the compiler subprocess does not inherit it. The signed
package then enters the existing validator, immutable intake and signed registry
publication path. `any` packages include all six native targets; platform-specific
server packages include their corresponding target.

Before publication, Linux builds an ephemeral signed corpus from the actual
components. Six native CI runners use `aot_verify` to require direct AOT loading,
no compilation-cache creation, successful component ABI instantiation and an empty
dispatch. Downloading by the compiler job's immutable artifact ID binds the
corpus to that run. This corpus is never a production installable package. An
explicit no-client batch may skip the native matrix; a failing native check
blocks publication.

Windows end-to-end evidence separately runs real gopls, rust-analyzer and the
Node-backed JavaScript/TypeScript server, including JSX/TSX, negotiated providers,
restart and process reclamation. The benchmark asserts `published-aot` for every
AOT cohort and checks identical WASM/server/runtime hashes. Its temporary test
authority is injected only into the test executable, not a shipping application.

## Performance interpretation

AOT removes first-use local compilation, not language-server startup or indexing.
Cold means an empty Wasmtime cache; operating-system and server caches are kept.
Memory counters describe the windowless test host, excluding external language
servers. Cold compilation peaks, ready-state memory and warm cached runs are
reported separately. Existing local-cache mappings can be faster and smaller
than repeatedly authenticating and copying AOT; that tradeoff is retained in the
evidence rather than hidden. The packaged native images also increase download
size, especially an `any` package carrying six targets.

Wasmtime API references: [component deserialization](https://docs.rs/wasmtime/50.0.1/wasmtime/component/struct.Component.html#method.deserialize),
[explicit compilation targets](https://docs.rs/wasmtime/50.0.1/wasmtime/struct.Config.html#method.target).
