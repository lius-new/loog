/** @module Interface loom:extension/host@0.1.0 **/
/**
 * Read a bounded range of a captured snapshot. Fast, never blocks on the UI.
 */
export function readRange(snapshot: SnapshotToken, range: ByteRange): string;
export function releaseSnapshot(snapshot: SnapshotToken): void;
/**
 * Read an advertised open document without waiting for a provider request.
 * The immutable snapshot remains readable until released; closed handles
 * and documents beyond the host's synchronization policy are refused.
 */
export function captureDocument(document: bigint): DocumentSnapshot;
/**
 * Accept an edit for scheduling. Returning only means "accepted"; the
 * outcome arrives later as an `operation-complete` message.
 */
export function applyEdit(request: ApplyEditRequest): OperationId;
/**
 * Multi-file/versioned edits or navigation, with the result after application.
 */
export function performWorkspaceOperation(request: WorkspaceOperation): OperationId;
export function cancel(operation: OperationId): void;
/**
 * Persistent values of this extension, in its own namespace. Writes are
 * limited in key length, value size, number of keys and total size per scope.
 */
export function storageGet(scope: StorageScope, key: string): string | undefined;
export function storageSet(scope: StorageScope, key: string, value: string): void;
export function storageDelete(scope: StorageScope, key: string): void;
/**
 * Read a text file inside a folder, at most `max-bytes` of it. The path is
 * relative to the folder and may not leave it, directly or through a link.
 */
export function readFile(root: bigint, path: string, maxBytes: number): FileContent;
/**
 * Relative paths of files in a folder matching a glob (for example
 * `src/**.rs`), honoring ignore files, at most `limit` of them.
 */
export function findFiles(root: bigint, pattern: string, limit: number): Array<string>;
/**
 * A bounded GET request. Only hosts the manifest declares (and the user
 * granted) are reachable, every redirect is checked again, and the body is
 * cut at `max-bytes`.
 */
export function httpGet(url: string, maxBytes: number): HttpResponse;
/**
 * Create, rename and delete files inside a folder. Accepted at once; the
 * outcome arrives as `operation-complete`. A failure with `committed` set
 * means the disk may be partly changed and the message names where to look.
 */
export function applyFileOps(request: FileOpsRequest): OperationId;
/**
 * The workspace folders this extension may name.
 */
export function listRoots(): Array<RootRef>;
/**
 * Accept a task for scheduling. Refusals (permission, trust, limits) and
 * the end of the task arrive as `task-exited`; output arrives as `task-output`.
 */
export function taskStart(request: TaskRequest): TaskId;
/**
 * Stop a task and everything it started. The exit is still delivered once.
 */
export function taskCancel(task: TaskId): void;
/**
 * Persistent binary process; spawn failure arrives as process-exited.
 */
export function processSpawn(request: ProcessRequest): bigint;
/**
 * Whole-message admission. Zero means retry after process-writable.
 * In content-length mode bytes are one body and the host adds the header.
 */
export function processWrite(process: bigint, bytes: Uint8Array): number;
/**
 * UTF-8 convenience with identical admission, limits and write receipts.
 */
export function processWriteText(process: bigint, text: string): number;
export function processCloseStdin(process: bigint): void;
export function processKill(process: bigint): void;
/**
 * One-shot monotonic timers, at most 64 live per instance and 24 hours each.
 */
export function timerStart(milliseconds: number): bigint;
export function timerCancel(timer: bigint): void;
/**
 * Monotonic elapsed nanoseconds for this instance; unrelated to wall time.
 */
export function monotonicNanoseconds(): bigint;
/**
 * Root-relative globs, with recursive disk events. Requires file read access.
 */
export function watchFiles(request: WatchRequest): bigint;
export function unwatchFiles(watch: bigint): void;
/**
 * Only declared mounts and this instance's own nodes are accepted.
 */
export function uiCreate(parent: UiParent, description: UiDescription): bigint;
/**
 * Atomic property replacement; identity and parent stay fixed.
 */
export function uiUpdate(node: bigint, description: UiDescription): void;
export function uiRemove(node: bigint): void;
/**
 * Show findings for one state of an open file (for example from a checker).
 * Findings for a state that is no longer current are discarded.
 */
export function publishDiagnostics(provider: string, path: string, version: bigint, items: Array<Diagnostic>): void;
/**
 * The resolved value of one of this extension's declared settings, as JSON text.
 */
export function getSetting(key: string): string | undefined;
/**
 * Answer a diagnostics request. Only the latest answer for the document's
 * current state is shown; an answer for an older state is discarded.
 */
export function provideDiagnostics(request: bigint, items: Array<Diagnostic>): void;
/**
 * Answer a completion request (several items) or a resolve request (the
 * one completed item). `incomplete` asks to be asked again as typing continues.
 */
export function provideCompletions(request: bigint, items: Array<CompletionItem>, incomplete: boolean): void;
/**
 * Bulk encoding of the same completion-item records, using their SDK
 * camelCase field names. Byte offsets are lossless decimal strings.
 * Closed, validated schema; 32 MiB maximum. This is not a server message.
 */
export function provideCompletionBatch(request: bigint, items: Uint8Array, incomplete: boolean): void;
/**
 * Answer a formatting request with edits against the requested snapshot.
 */
export function provideEdits(request: bigint, edits: Array<TextEdit>): void;
/**
 * Answer a hover request; `none` means there is nothing to show.
 */
export function provideHover(request: bigint, answer: HoverAnswer | undefined): void;
/**
 * Answer a definition request with the places the symbol is defined.
 */
export function provideLocations(request: bigint, items: Array<Location>): void;
/**
 * Answer an outline request.
 */
export function provideSymbols(request: bigint, items: Array<DocumentSymbol>): void;
/**
 * Answer a signature help request; `none` means there is nothing to show.
 */
export function provideSignature(request: bigint, answer: SignatureAnswer | undefined): void;
/**
 * Answer an inlay hint request with offsets of the requested snapshot.
 */
export function provideInlayHints(request: bigint, items: Array<InlayHint>): void;
/**
 * Answer a code action request; the edits are against the requested snapshot.
 */
export function provideCodeActions(request: bigint, items: Array<CodeAction>): void;
/**
 * Typed answers for navigation, rename, hierarchies, folding, lenses and highlights.
 */
export function provideLanguage(request: bigint, answer: LanguageAnswer): void;
/**
 * Update negotiated capabilities; only declared provider IDs are accepted by the app.
 */
export function configureProvider(configuration: ProviderConfiguration): void;
export function log(level: number, message: string): void;
export type ApplyEditRequest = import('./loom-extension-types.js').ApplyEditRequest;
export type CodeAction = import('./loom-extension-types.js').CodeAction;
export type CompletionItem = import('./loom-extension-types.js').CompletionItem;
export type Diagnostic = import('./loom-extension-types.js').Diagnostic;
export type DocumentSymbol = import('./loom-extension-types.js').DocumentSymbol;
export type FileContent = import('./loom-extension-types.js').FileContent;
export type FileOpsRequest = import('./loom-extension-types.js').FileOpsRequest;
export type HoverAnswer = import('./loom-extension-types.js').HoverAnswer;
export type HostError = import('./loom-extension-types.js').HostError;
export type HttpResponse = import('./loom-extension-types.js').HttpResponse;
export type InlayHint = import('./loom-extension-types.js').InlayHint;
export type Location = import('./loom-extension-types.js').Location;
export type RootRef = import('./loom-extension-types.js').RootRef;
export type SignatureAnswer = import('./loom-extension-types.js').SignatureAnswer;
export type StorageScope = import('./loom-extension-types.js').StorageScope;
export type TaskId = import('./loom-extension-types.js').TaskId;
export type TaskRequest = import('./loom-extension-types.js').TaskRequest;
export type OperationId = import('./loom-extension-types.js').OperationId;
export type SnapshotToken = import('./loom-extension-types.js').SnapshotToken;
export type ByteRange = import('./loom-extension-types.js').ByteRange;
export type TextEdit = import('./loom-extension-types.js').TextEdit;
export type ProcessRequest = import('./loom-extension-types.js').ProcessRequest;
export type WatchRequest = import('./loom-extension-types.js').WatchRequest;
export type UiDescription = import('./loom-extension-types.js').UiDescription;
export type UiParent = import('./loom-extension-types.js').UiParent;
export type DocumentSnapshot = import('./loom-extension-types.js').DocumentSnapshot;
export type LanguageAnswer = import('./loom-extension-types.js').LanguageAnswer;
export type ProviderConfiguration = import('./loom-extension-types.js').ProviderConfiguration;
export type WorkspaceOperation = import('./loom-extension-types.js').WorkspaceOperation;
