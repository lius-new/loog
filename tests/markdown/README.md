# Production Markdown evidence

The Windows x64 JSON was recorded on 2026-10-23 using the production release
runtime, 65636 bytes of source, warm fonts, nine first-readable/edit samples and
30 UI projection + software Skia draw samples. Nearest-rank p95 is reported.
It is an offscreen UiSession measurement, not OS input/compositor latency,
screen-reader coverage, process RSS or native transient heap measurement.

The inspected [rich screenshot](../../docs/images/markdown-preview.png) comes
from that same production UI scene. Exact reproduction and cache/lifecycle
boundaries are in [the architecture](../../docs/architecture/markdown.md).
Three-platform CI uploads separate PNG/JSON artifacts; do not substitute this
Windows observation for Linux or macOS measurements.


## Final cross-platform acceptance

Source commit: ea73b4093a0ae8bcf18318d726a4b1e6ca3270ac. [CI run](https://github.com/lius-new/loog/actions/runs/39954372874) completed successfully under
Rust 1.97.1. Windows x64, Linux x64 and macOS universal build/package checks
passed. The quality job was retried after a slow Ubuntu package mirror; the
three successful platform jobs were preserved. ci-validation.json records the
exact jobs and six artifacts. Hosted CI artifacts have limited retention.

The additional *.dev.json files are platform-specific development-build
observations, not release latency claims. The Windows release baseline remains
the local acceptance measurement. process-memory.windows-x64.json samples the
whole release evidence process (requested 10 ms interval, 285 samples): observed
lifetime working-set peak 114864128 bytes and private-byte peak 100429824 bytes.
This includes native fonts, raster caches, code and the harness; it is not a hard
limit, isolated Skia allocation count or all-input memory maximum. Reproduce
with the release test executable printed by Cargo, running only
app::markdown_tests::production_markdown_evidence, and sample Windows
Process.PeakWorkingSet64 / PrivateMemorySize64 from its parent while it is live.
Keep memory sampling separate from latency comparisons.
