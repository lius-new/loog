# Windows language migration measurements

The current native harness additionally includes named Go/Rust/JS/TS types for Rust-client hierarchy probes, and opt-in JSX/TSX fixtures. The archived reports retain the original fixtures. Recollect a matching reference before using LOOM_MIGRATION_COMPARE with the changed fixtures; do not combine these historical reports with new runs. The current TS-component/Rust-component comparison uses freshly paired runs of identical fixtures and servers.

The paired JSON files retain every measured cohort, including p50, p95, count,
Loom process memory, owner stop and extension restart observations. Each operation
has 100 measured requests after ten warm-ups. Server indexing has a separate
readiness deadline: ten consecutive usable completion/hover/definition probes
and at least two seconds. Failed measured requests fail the run; none are removed.

Environment: Windows 11 Pro x64, kernel 10.0.26200; AMD Ryzen 5 5600GT;
33,605,750,784 bytes physical RAM; Rust 1.98.0 (`88d9e12ae`), Node 24.19.0.
Both routes use release builds on this machine and the same staged servers:
rust-analyzer 2026-10-19, gopls v0.24.0 built with Go 1.27.0, and
typescript-language-server 6.0.2 with TypeScript 5.9.3. JS and TS have separate
projects and measurements. Exact 1 MiB and 8 MiB documents contain Chinese,
surrogate pairs and CRLF in a trailing comment. Documents above 8 MiB remain
excluded from synchronization.

The reference is Loom `5ff103ed69dbded3df479cb0f17133ace8de0c1b`, including its
locked LGUI revision. `original-host-harness.rs.txt` is an audit artifact for that
source snapshot, not current host code. Place it at
`src/infrastructure/lsp/language_migration_baseline.rs` in a copy of that commit
under `.tmp/loom-baseline-5ff103ed`, add its test module declaration to that
snapshot's `lsp/mod.rs`, and build with its own target directory. The shared
measurement helper is `tests/fixtures/language_migration_metrics.rs` in this
checkout. The harness reads the same projects and staged servers as the extension
route, and changes no production behavior in the reference.

Commands, with `RUST_MIN_STACK=16777216` and no concurrent compilation:

```text
node .tmp/loom-extensions/language-clients/stage.mjs
cargo test --release --bin Loom --offline app::extensions::tests::official_language_components_reach_native_consumers_without_host_servers -- --ignored --exact --test-threads=1
```

Use the fully qualified test name
`app::extensions::tests::official_language_components_reach_native_consumers_without_host_servers`
with `--exact`. Set `LOOM_MIGRATION_BYTES` to `0`, `1048576` and `8388608` in
successive runs. Set `LOOM_MIGRATION_COMPARE=1` to require and check matching
`target/language-migration/projects/*/baseline-observed.json`. The reference test
name is `infrastructure::lsp::language_migration_baseline::original_host_language_migration_baseline`.

All 36 operation/size/language comparisons passed the unchanged gate:
extension p95 <= reference p95 + max(10 ms, 20% of reference p95). Completion p95
in milliseconds is shown below; the JSON files also contain hover and definition.

| Language | Small reference / extension | 1 MiB reference / extension | 8 MiB reference / extension |
| --- | --- | --- | --- |
| Rust | 14.16 / 11.40 | 11.53 / 11.22 | 15.75 / 10.68 |
| Go | 16.84 / 19.01 | 18.85 / 22.79 | 24.07 / 18.48 |
| JavaScript | 18.10 / 20.94 | 18.43 / 21.25 | 24.01 / 22.97 |
| TypeScript | 16.55 / 20.03 | 19.28 / 20.86 | 26.95 / 21.26 |

Memory counters describe the Loom process, including its WASM runtime, and exclude
the separately running server processes. `observed_peak_memory` samples outside
request timing; the OS process peak is also retained. Extension restart ranges
from 1.41 to 1.97 seconds, including configured backoff and recovery to usable
hover. Owner stop reclaims processes in 12–26 ms. These are observations, not new
resource limits. The guest memory limit remains 256 MiB. Earlier attempts that
exceeded latency or exhausted guest memory were diagnosed before this passing
cohort: repeated coordinate scans, copied large writes, and per-message byte
counting were removed. No threshold was relaxed.

The client retains one bounded validated completion response for identical-body
decode reuse; requests still reach the server. Changes, errors, cancellation and
other methods use normal decoding. Native result conversion reuses coordinates
only for the same immutable text allocation. The client tests cover changed
responses and synchronization lifetimes. Timings describe these warmed repeated
requests, not a claim about every possible editing workload or other platforms.

These twelve native reports were refreshed after deleting the host LSP. The
original reference reports are unchanged. The final functional suite passed
1700 tests with 28 ignores. The staged WASM SHA-256 values are:

| Component | Bytes | SHA-256 |
| --- | --- | --- |
| Rust | 13639208 | `cd27553a4afc8a09455363a197bb4fc8e06c639043d0ffe7cf4e4c3f6ed07a24` |
| Go | 13638671 | `e9702a7739a9f4a938e5ce4886ea446884755a6500585b0f28c4db1b14301f35` |
| JS/TS | 13643001 | `db55ee5c5ed27d3bfb48ffadb03bd39e961e83dad93693f81772641df7e95241` |

All three remain below the original 16 MiB component budget. Their separately
packaged server/runtime payloads are excluded from that component budget.

Final package builds, including source/runtime notices, passed import through
the updated production validator:

| Package | Bytes | SHA-256 |
| --- | --- | --- |
| `loom.lsp.rust-analyzer-0.2.0-windows-x86_64.loomext` | 19368095 | `3cc266ec26472772a34c470e8948a75f23752495cc43ed8203d1799a6918b53c` |
| `loom.lsp.gopls-0.2.0-windows-x86_64.loomext` | 27473281 | `ccc8caaeea49f74fa84fff37eeebbc88ebe2492151bcf68669c5a1c5600f56a9` |
| `loom.lsp.typescript-language-server-0.2.0-any.loomext` | 9428895 | `dbdf93a36413d369c4bf8f46fe11da773700b5a982d7c2e4a6e5314a0defd94d` |

Build from each extension directory with `LOOM_SOURCE` set to this Loom source:
`node build.mjs windows-x86_64` for Rust and Go, `node build.mjs` for JS/TS.
The exact component toolchain is locked by Loom's
`tests/extensions/js-guest/package-lock.json`. Package hashes identify these local
artifacts; published CI rebuilds retain their own checksums.
