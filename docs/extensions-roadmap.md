# Extensions: state and roadmap

Reviewed on 2026-10-31 against the source at the commit that added this file.
The development guide is [tests/extensions/README.md](../tests/extensions/README.md)
and the TypeScript SDK is in [sdk/typescript](../sdk/typescript/README.md). The
contract is `wit/loom-extension/world.wit`; it is a candidate and **not frozen**.

Keep the states apart. An *implementation gap* has an identifiable source
limitation. *Blocked* work cannot be done until evidence that does not exist yet
is produced. When closing an item, record the source commit, platform and
toolchain, the command, the inputs and the result; "the code is written" is not
evidence.

## What works today

| Area | State |
| --- | --- |
| Packages | Folder with `manifest.json` under the per-user `extensions` directory; manifest checked on load (ids, paths, capabilities, events, settings, servers). Resource-only packages need no component. |
| Runtime | Wasmtime component model, one instance per package, bounded rounds (`dispatch`), epoch interruption, memory limits, precompiled component cache. Rust, JavaScript and TypeScript guests run on **Windows x64 only**. |
| Commands, edits | Commands and `extension::Run` key bindings, captured document and selection, atomic edits through the shared workspace transaction, one undo step. |
| Providers | Diagnostics, formatting, completion (+ resolve), save participants, hover, definition, document outline, signature help, inlay hints, code actions. Answers join the language servers' replies, so the existing UI shows them. |
| Host abilities | Settings (user and project), storage, bounded file read and find, bounded `GET`, create/rename/delete files with undo on failure, tasks (programs without a shell), published diagnostics. |
| Trust and grants | Folders are untrusted by default; the innermost open folder decides. Running programs needs the package grant, a trusted folder and the folder's tool grant (`workspace::AllowExtensionTools`). |
| Language servers | A package's declared servers start only where all three of those hold, and are stopped at once when any of them changes. |
| Data resources | Themes (palette, terminal colors, syntax styles), snippets (completion list) and language packs (`tree-sitter.json` inside the package, found at startup; needs a restart). Each file is accepted or rejected on its own, with the reason. |
| Management | `workspace::ShowExtensions`, `ReloadExtensions`, `RetryExtensions`, `StopExtensions`, trust and tool commands. |

## Blocked

| ID | Remaining work | Completion condition |
| --- | --- | --- |
| EXT-01 | Linux x64 and macOS evidence for the Rust, JavaScript and TypeScript guests, process tasks (process groups) and language servers. | Run `node scripts/extensions/test.mjs` on both platforms and record the results in [the guide's platform table](../tests/extensions/README.md). A platform that was not run counts as failing. |
| EXT-02 | Freeze the public contract: signatures, budget defaults (queue, snapshot lease, receipt retention), compatibility matrix, and the final decision record for the asynchronous model. | Needs EXT-01. Then add a second contract version and fixtures for old component on new host and new component on old host (today only one version exists, so those pairs cannot be tested), and write the decision. |

## Implementation gaps

| ID | Remaining behavior | Current evidence and completion condition |
| --- | --- | --- |
| EXT-03 | Runtime verification. | JavaScript memory limit and fuel are untested; re-entry protection inside a round and the "idle wait does not hold the Store" property are not verified; binary size and memory increase of the runtime are not measured. Add tests or measurements for each. |
| EXT-04 | Prototypes not wired in. | `crates/extension-api` holds scope/generation, the operation ledger, budgets and admission rules with tests, but the host does not use them. Connect them, then measure the background preparation and the UI-thread commit time of large edits (shared snapshots, many files, long lines, many cursors). |
| EXT-05 | Selection and focus events; selection lease. | Only document open/change/close events are delivered. Add selection and focus events with a generation on the selection a command captured, and explicit lease release. |
| EXT-06 | Activation on language demand. | Providers wake from their declared selector, but the subscription baseline for a language-driven wake-up is missing. The first event after activation must follow a baseline. |
| EXT-07 | Management UI. | Only commands exist. Add per-extension stop/retry, a persistent "disabled" state, a page that shows permissions and failures, and a confirmation flow for grants. Local packages are currently granted everything they declare (decision record in the design notes); a real install flow must ask first, and no third-party package may be loaded before it exists. |
| EXT-08 | Debugging. | No source maps: an error cannot be mapped to a TypeScript line. Add maps, error positions in the log, and reuse of grants in a development mode. |
| EXT-09 | Resource contributions: the rest. | Official default themes and language resources are not packages; version selection between bundled and independently updated resources, a deterministic rule for conflicts other than "the user chose", and removal/fallback are unspecified. Language packs in a package need a restart (they are read at startup); making them load without one needs the language registry to accept additions at run time. |
| EXT-10 | Language server install and update. | Declared servers run an existing program; download and install steps, pinned sources with checksums, and the official servers as package declarations are not done. They must go through the same grants as running a program. |
| EXT-11 | More language providers. | References, implementation, type definition, workspace symbols, call and type hierarchies, code lens, folding and selection ranges. Several providers for one feature are ordered by source rank only; add a user-configurable order, and one chosen formatter per language. |
| EXT-12 | Save participants. | Order is the provider id; add user-configurable order and multi-file batches. While a participant runs, only calls with side effects are refused: the instance's other callbacks are not paused. |
| EXT-13 | File operations. | Writing the content of an existing file still goes through text edits only; moves across volumes are not verified; there is no UI to recover from an interrupted run (only a note file and a recovery folder). |
| EXT-14 | Network and tasks. | `GET` only, no response headers or `POST`. Tasks run with a clean environment and a short allow list of variables; the non-Windows process-group path is untested (see EXT-01). |
| EXT-15 | Settings. | Settings files are read at start and on reload, not watched; only the first workspace folder supplies project settings. |
| EXT-16 | Workspace transaction service. | LSP and extensions share the text step; file resource steps and save transforms still live in `app/language_ui/edit.rs`. Move them so there is one implementation. |

## SDKs and performance

| ID | Remaining behavior | Current evidence and completion condition |
| --- | --- | --- |
| EXT-17 | TypeScript SDK completeness. | Commands, providers, edits, files and tasks are wrapped; storage, network, settings, file operations and save participants are available only through the generated types. Wrap them, publish the package with a version, and add source maps (EXT-08). |
| EXT-18 | Rust SDK. | The Rust test guest is hand written. Provide the equivalent SDK (context, futures resuming across rounds, receipts, RAII leases) so the two languages share one contract. |
| EXT-19 | Performance regression. | `crates/extension-host/tests/measure.rs` is a manual, ignored measurement (Rust compile 31 ms, JavaScript component 11.7 MiB and 9.2 s cold compile). Turn it into a check with thresholds that runs in CI, and keep the precompile cache a hard requirement for JavaScript. |

## Order of work

1. EXT-01, then EXT-02: nothing else may freeze the contract before they close.
2. EXT-07 (install flow and management): required before any third-party package is installable.
3. EXT-10 and EXT-09 together: official languages and servers as packages.
4. EXT-03 to EXT-06 and EXT-16: runtime depth and the single transaction path.
5. EXT-11 to EXT-15, EXT-17 to EXT-19 as demand appears.
