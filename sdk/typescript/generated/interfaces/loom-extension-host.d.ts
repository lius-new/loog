/** @module Interface loom:extension/host@0.1.0 **/
/**
 * Read a bounded range of a captured snapshot. Fast, never blocks on the UI.
 */
export function readRange(snapshot: SnapshotToken, range: ByteRange): string;
export function releaseSnapshot(snapshot: SnapshotToken): void;
/**
 * Accept an edit for scheduling. Returning only means "accepted"; the
 * outcome arrives later as an `operation-complete` message.
 */
export function applyEdit(request: ApplyEditRequest): OperationId;
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
