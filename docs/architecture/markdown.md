# Shared Markdown architecture

This is the maintained design for file reading and in-memory documentation.
User behavior and limits are in [Markdown reading](../markdown.md).

## Boundaries and ownership

```text
domain Workspace: documents(FileId) <- views(ViewId) <- panes(PaneId)
                | DocumentStamp / source anchor
app file/card hosts -> infrastructure parse worker -> features MarkdownDocument
                | publication lease                       |
                +-> app layout/resource worker <- pure structural layout
                              | thread-owned Skia Paragraph
                              v
                 immutable Rendered retention envelope
                              |
            pure ReaderFrame + value events -> ui Markdown reader
                              |
            shared features reducer -> effects -> host clipboard/navigation
```

| Layer | Owns | Forbidden dependencies |
| --- | --- | --- |
| domain/view + workspace | Opaque IDs, one text/history per document, independent source/reader positions, explicit associations | GUI, parsing, native resources and I/O |
| features/markdown/model + parser | Flat node arena, canonical text, UTF-8 source/display mapping, syntax flags and bounded parse outcome | Workspace identities, GUI; external parser outside parser.rs |
| features/markdown/layout + reader | Whole-paragraph contract, structural flow, source anchors, logical selection and shared event reducer | Root state, platform I/O, threads and native handles |
| ui/markdown/reader + card | Controlled projection, visible semantic/hit geometry, value callbacks | AppContext, service lookup, filesystem/network and hidden clipboard access |
| ui/markdown/native | Private native paragraph adapter and bounded raster tiles | Root state and platform navigation |
| infrastructure/markdown_worker | Parse scheduling, cancellation, result retention and memory ledger | Root state and GUI projections |
| infrastructure/markdown_resources | Bounded PNG/JPEG/SVG file/HTTP loading, decoder validation and SVG rasterization (resvg) | Application model |
| app/markdown_service/render/assets/cards/view/drag | Runtime lifetime, host validation, layouts/resources, clipboard/navigation, drag timer and narrow UI bindings | Sharing native paragraphs across threads or serializing runtime state |

The architecture gate enforces the pure/module boundaries and registered
state ownership. crates/markdown-tests compiles the exact production rule
files without GUI dependencies. It contains no parallel parser/layout model.
The old P0 implementation was retired; only historical measurements remain.

## Identity, versions and lifetime

A path has one OpenDocument with one text buffer/history/saved baseline and
DocumentStamp. A pane stores ViewIds. A DocumentView targets Text(FileId),
Diff(DiffId) or Builtin; the latter two do not own text buffers. Presentation
changes keep ViewId and invalidate delayed source input via input_generation.
Copies receive fresh IDs and independent positions; moves retain IDs.

Associated side preview is an explicit weak ViewId reference, not same-path
guessing. Closing/converting the source detaches it. Session schema 2 saves
typed views and pane/item references, never runtime IDs; restore validates
same-document/source mode before rebuilding associations. The sole legacy
reader converts unversioned sessions; malformed/future formats remain intact.

File ParseKey contains the complete DocumentStamp, format, syntax and limits
(with a reserved parse-policy generation). View input generations are checked
by hosts, so different views still share the same parse publication. Card keys use Consumer::Card(owner) plus immutable Markup
identity/revision; no fake file/view identity is manufactured. Card content
identity is assigned at ingestion, with composed identities retaining parts.
Plaintext is preserved even when mixed with formatted documentation.

Publication owns its parse budget lease. LayoutEntry holds Publication and its
layout lease. Rendered holds LayoutEntry, image references and its frame lease.
UI bindings retain the envelope, so eviction cannot hide memory still held by
a visible/retiring frame. Ready results are validated against wanted keys before
publication; callbacks also validate document stamp, view/input generation,
card identity and the parse of the frame actually displayed. A newer raster
publication of that same parse cannot invalidate an in-flight logical gesture. Old bounded geometry is discarded after a
horizontal change or a viewport jump outside its overscan window.

One parser worker coalesces edits (80 ms, at most about 250 ms of continuous
waiting), prioritizes active demand and de-duplicates matching keys. One layout
worker owns all native paragraph objects and resource work. Shutdown invalidates
delivery, cancels timers and retires results asynchronously; the UI never waits
for an uninterruptible parser constructor or blocking I/O. Native parse/layout
calls observe cancellation at the supported boundaries, not inside every native
instruction.

## Text, layout and interaction

DisplayByte and SourceByte are UTF-8 offsets. Mapping segments declare Exact,
Transformed or Synthetic relationships; entities, removed delimiters and list/
table separators cannot be treated as identical source offsets. Copy uses the
complete canonical display text, including list/task markers, table separators,
code whitespace and image alt text. Reading anchors are source offsets with a
block fraction, transformed together with editor-core peer positions on edits.
Selections retain their parse revision; a new parse clears an unmappable old
selection rather than applying stale display offsets to changed content.

ParagraphIndex converts byte/scalar/UTF-16 boundaries. Skia line metrics,
rectangles and hit positions use UTF-16; portable output uses grapheme-safe
UTF-8 ranges. Summary, geometry and paint share the same styled paragraph and
font/layout key. No independently shaped words are placed to approximate a line.
Tabs use measured monospace stops while retaining one logical tab unit. Normal
paragraphs never resolve a monospace face; tab advances have a bounded cache
keyed by font generation and scale.

Structural layout computes flow for body/headings, quotes/lists, tables, code,
images and explicit large-block fallbacks. Fine geometry is requested only for
visible lines/columns, in at most 512-scalar chunks and 8192 glyphs per frame.
Native paint produces transparent, aligned 256-pixel tiles with bounded overscan;
tile identity lets overlapping frames reuse encoded pixels. GUI image caches
retain them only while visible. The pinned lgui lacks a styled paragraph scene
primitive, so this project-owned adapter uses encoded tiles without changing a
Cargo cache or requiring an uncommitted upstream change.

File and card hosts invoke one pure reducer for pointer selection, link click
versus drag, scrolling, keyboard/grapheme movement, select/copy and code copy.
The reducer returns values; hosts execute clipboard and navigation effects.
One cancellable timer per held gesture continues outside-viewport drag scroll.
Text/IME events are intercepted and Source EditorMut is unavailable to a reader.
The visible semantics expose document/text/link/image and copy-button roles;
Tab/Enter also provide link navigation. This is not a claim of an OS screen
reader audit, which requires a desktop accessibility bridge test.

## Resources and extension points

Rendering policy is shared with the rest of Loog: `ui/fonts` owns the proportional
font stack, `ui/text_rendering` and the theme supply raster options, and
`ui/image_rendering` selects image sampling. Worker requests snapshot text options
in `Ink`; those options participate in paragraph/tile identity, so a changed
profile cannot reuse pixels from an older profile. Native paragraphs remain on
their owning worker thread. Glyph tiles use pixel-exact presentation, while
ordinary document images and image previews share filtered sampling. lgui only
provides generic controls for these choices and has no document/theme policy.

Address resolution is pure: base document, percent-decoded path/query/fragment,
allowed HTTP(S), local files or heading anchor. Application navigation uses the
normal asynchronous FileLoader and rejects delayed results for changed input.
Heading slugs preserve alphanumeric graphemes and globally disambiguate collisions.

Image loading checks encoded size and dimensions before bounded decoding. The
asset cache retains encoded+decoded cost, watches successful/missing local files
and invalidates layouts on changes. Documents load remote HTTP(S) and UNC images
automatically. Cards have no base path and perform no resource I/O.
HTML blocks inside the closed subset in features/markdown/html.rs become ordinary
nodes plus an alignment side table; any other HTML block stays literal. SVG/math/
Mermaid are not executable extension hooks.

To add syntax, extend stable NodeKind/diagnostics, then the parser adapter,
canonical copy/mapping tests and structural layout. To add a rendering backend,
implement the paragraph contract and prove shared measure/paint/hit coordinates.
To add a resource type, add explicit policy, decode/cost limits and lifecycle
tests behind the resource port. To add a host, provide immutable frames and
value events, consumer identity and a retention envelope; do not copy parser or
selection logic or add RootState/native handles to a feature/UI model.

Visible fenced code uses the existing grammar/query resources with a generation
key, bounded snippet cache and plain fallback. No source language model or
background LSP is created for a snippet. Reading metrics are typed settings,
separate from syntax flags and resource permissions.

## Budgets and validation

| Retained/work limit | Initial setting |
| --- | --- |
| Source / events | 2 MiB / 200k |
| Depth / parsed result | 64 / 16 MiB |
| Native paragraph | 16 KiB; observable fallback preserves canonical copy |
| Parent cache / parse-layout-image children | 128 MiB / 32–32–64 MiB |
| Native cache reserve | 16 MiB conservative reservation, including paragraph/tile-key estimates |
| Visible raster result | 24 MiB; density adapts to viewport area |
| Images | 8 MiB encoded, 4096 per side, 16 MiB decoded |

These are retained ledgers and work admission limits, not process RSS or hard
native temporary heap quotas. A future hard process limit needs isolation.

```sh
cargo test --locked -p loog-markdown-tests
cargo test --locked -p loog-workspace-tests
node scripts/build/cargo.mjs test --locked --workspace
node --test scripts/architecture/markdown.test.mjs
node scripts/architecture/check.mjs
# Set LOOG_MARKDOWN_EVIDENCE_DIR to an output folder first:
node scripts/build/cargo.mjs test --locked --release -p loog app::markdown_tests::production_markdown_evidence -- --ignored --exact --nocapture
```

The opt-in evidence test uses the production runtime, UiSession events and Skia
software scene rendering, writes real README/rich-document PNGs and JSON latency
samples. It measures warm-font 64 KiB first-readable/edit-to-preview latency and
UI projection plus software draw; it does not measure the desktop compositor,
native event delivery, screen reader, process peak RSS or GPU presentation.
CI runs the same production rules/native/UI tests on Windows, Linux and macOS,
uploads the evidence, then builds/packages the host. Platform conclusions and
build/run URLs are recorded in the implementation tracker and
[CI validation](../../tests/markdown/ci-validation.json). Windows process-level
memory observations are archived separately from retained cache accounting;
they include native heaps and the harness, and do not establish a hard limit.
