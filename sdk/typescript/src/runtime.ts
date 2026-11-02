// The round protocol behind `async`/`await` (design §7.1).
//
// The host drives the extension in bounded rounds: `dispatch` receives a batch,
// lets everything runnable run, and returns. Nothing may wait inside a round for
// something the host delivers later. A host operation therefore returns a Promise
// that a later `dispatch` settles when the matching `operation-complete` arrives,
// and the round reports `pending` while any callback is still waiting.
import type * as Host from '../generated/interfaces/loom-extension-host.js';
import type {
  CodeAction,
  Completion,
  CompletionItem,
  CommandContext,
  Diagnostic,
  DocumentSymbol,
  HoverAnswer,
  InlayHint,
  Inbound,
  Location,
  ProviderKind,
  ProviderCommandCall,
  ProcessRequest,
  ProcessOutput,
  ProcessExit,
  WatchRequest,
  FileChanged,
  HostError,
  RootRef,
  DocumentRef,
  ProviderContext,
  ProviderConfiguration,
  LanguageAnswer,
  FileLocation,
  WorkspaceEditRequest,
  ProviderRequest,
  RoundStatus,
  Selection,
  SignatureAnswer,
  TextEdit,
  ApplyEditRequest,
  FileOpsRequest,
  TaskExit,
  TaskOutput,
  TaskRequest,
} from '../generated/interfaces/loom-extension-types.js';
import { slice } from './text.ts';
import { createUi, type Ui } from './ui.ts';

export type HostApi = typeof Host;
export interface CompletionAnswer { items: CompletionItem[]; incomplete: boolean }

/** What a provider of each kind answers with. */
export interface ProviderResults {
  diagnostics: Diagnostic[];
  formatting: TextEdit[];
  save: TextEdit[];
  completion: CompletionItem[] | CompletionAnswer;
  'completion-resolve': CompletionItem;
  hover: HoverAnswer | undefined;
  definition: Location[] | Extract<LanguageAnswer, { tag: 'places' }>;
  'document-symbols': DocumentSymbol[];
  'signature-help': SignatureAnswer | undefined | Extract<LanguageAnswer, { tag: 'signatures' }>;
  'inlay-hints': InlayHint[];
  'code-actions': CodeAction[] | Extract<LanguageAnswer, { tag: 'actions' }>;
  'code-action-resolve': Extract<LanguageAnswer, { tag: 'actions' }>;
  references: Extract<LanguageAnswer, { tag: 'places' }>;
  'workspace-symbols': Extract<LanguageAnswer, { tag: 'workspace-symbols' }>;
  'document-highlights': Extract<LanguageAnswer, { tag: 'document-highlights' }>;
  rename: Extract<LanguageAnswer, { tag: 'workspace-edit' }>;
  'prepare-rename': Extract<LanguageAnswer, { tag: 'rename-target' }>;
  'folding-ranges': Extract<LanguageAnswer, { tag: 'folds' }>;
  'selection-ranges': Extract<LanguageAnswer, { tag: 'selections' }>;
  'call-hierarchy': Extract<LanguageAnswer, { tag: 'hierarchy' | 'calls' }>;
  'type-hierarchy': Extract<LanguageAnswer, { tag: 'hierarchy' }>;
  'code-lens': Extract<LanguageAnswer, { tag: 'lenses' }>;
  'code-lens-resolve': Extract<LanguageAnswer, { tag: 'lenses' }>;
  'format-on-type': TextEdit[];
  'execute-command': Extract<LanguageAnswer, { tag: 'completed' }>;
  highlights: Extract<LanguageAnswer, { tag: 'highlights' }>;
}

/** The snapshot a command or provider was started for. */
export class Snapshot {
  readonly #host: HostApi;
  readonly token: bigint;
  #released = false;
  #whole: string | undefined;
  readonly #readWhole: (() => string) | undefined;
  readonly length: bigint | undefined;

  constructor(host: HostApi, token: bigint, length?: bigint, readWhole?: () => string) {
    this.#host = host;
    this.token = token;
    this.length = length;
    this.#readWhole = readWhole;
  }

  /** The text of a range given in UTF-8 bytes. */
  read(range: { start: bigint; end: bigint }): string {
    if (this.#released) throw new Error('snapshot has been released');
    return this.#host.readRange(this.token, range);
  }

  /** The whole text (providers know its length; commands read selections). */
  text(): string {
    if (this.#released) throw new Error('snapshot has been released');
    if (this.#whole === undefined) {
      if (this.length === undefined) throw new Error('the length of this snapshot is not known');
      this.#whole = this.#readWhole ? this.#readWhole() : this.#host.readRange(this.token, { start: 0n, end: this.length });
    }
    return this.#whole;
  }

  /** The part of the whole text a range covers; the range is in UTF-8 bytes. */
  textIn(range: { start: bigint; end: bigint }): string {
    return slice(this.text(), range);
  }

  /** Give the snapshot back. Safe to call twice; the host also frees it when the request ends. */
  release(): void {
    if (this.#released) return;
    this.#released = true;
    this.#host.releaseSnapshot(this.token);
  }
}

/** What a command handler receives. */
export interface CommandCall {
  readonly command: string;
  readonly call: bigint;
  /** The document the command was started for, when it has one. */
  readonly snapshot: Snapshot | undefined;
  readonly selections: Selection[];
  readonly selectionPrecondition: bigint | undefined;
  /** True once the host cancelled the call; long work should look at it between steps. */
  readonly cancelled: boolean;
}

/** What a provider handler receives. */
export interface ProviderCall {
  readonly request: bigint;
  readonly cancelled: boolean;
  readonly provider: string;
  readonly kind: ProviderKind;
  readonly snapshot: Snapshot;
  readonly document: bigint;
  readonly version: bigint;
  /** The caret (hover, definition, signature help, completion), in UTF-8 bytes. */
  readonly cursor: number | undefined;
  /** The range asked about (formatting, inlay hints, code actions). */
  readonly range: { start: bigint; end: bigint } | undefined;
  readonly trigger: string | undefined;
  /** The item to complete (completion-resolve). */
  readonly item: CompletionItem | undefined;
  readonly context: ProviderContext | undefined;
}

export type CommandHandler = (call: CommandCall) => void | Promise<void>;

export interface Process {
  readonly id: bigint;
  /** Settles after the bytes are written, never merely queued. Each call is a
   * whole body in content-length mode. Concurrent calls preserve call order. */
  write(bytes: Uint8Array): Promise<void>;
  /** Give a dedicated buffer to the queue. Its buffer may be detached; slices
   * and runtimes without ArrayBuffer.transfer fall back to an owned copy. */
  writeOwned(bytes: Uint8Array): Promise<void>;
  writeText(text: string): Promise<void>;
  closeStdin(): Promise<void>;
  kill(): void;
  readonly exited: Promise<ProcessExit>;
}

export interface EditRequest {
  title: string;
  selectionPrecondition?: bigint | undefined;
  documents: { snapshot: Snapshot | bigint; edits: TextEdit[] }[];
}

export interface Runtime {
  onTiming(listener: (name: string, elapsedNanoseconds: bigint) => void): void;
  timers: { after(milliseconds: number, callback: () => void): { cancel(): void } };
  clock: { now(): bigint };
  ui: Ui;
  processes: {
    spawn(request: ProcessRequest, onOutput?: (output: ProcessOutput) => void): Process;
  };
  commands: { register(id: string, handler: CommandHandler): void };
  providers: {
    publishDiagnostics(provider: string, path: string, version: bigint, items: Diagnostic[]): void;
    configure(configuration: ProviderConfiguration): void;
    /** Handle commands attached to this provider's accepted results. */
    onCommand(id: string, handler: (call: ProviderCommandCall) => void | Promise<void>): void;
    register<K extends ProviderKind>(
      id: string,
      kind: K,
      handler: (call: ProviderCall) => ProviderResults[K] | Promise<ProviderResults[K]>,
    ): void;
  };
  workspace: {
    /** Saved bytes and their exact version remain immutable while the callback runs. */
    onDidSave(listener: (saved: { document: DocumentRef; snapshot: Snapshot }) => void | Promise<void>): () => void;
    showDocument(location: FileLocation): Promise<Completion>;
    applyWorkspaceEdit(request: WorkspaceEditRequest): Promise<Completion>;
    roots(): RootRef[];
    /** Capture an immutable, bounded view; release its snapshot after reading. */
    captureDocument(document: bigint): { document: DocumentRef; snapshot: Snapshot };
    watchFiles(request: WatchRequest, listener: (event: FileChanged) => void, onError?: (error: HostError) => void): { id: bigint; ready: Promise<void>; dispose(): void };
    /** Ask for an edit; settles when the host has committed or refused it. */
    applyEdit(request: EditRequest, call?: bigint): Promise<Completion>;
    applyFileOps(request: FileOpsRequest): Promise<Completion>;
  };
  tasks: {
    /** Run a program; `onOutput` sees its output and the promise settles with its exit. */
    run(request: TaskRequest, onOutput?: (output: TaskOutput) => void): Promise<TaskExit>;
  };
  /** The `guest` export of the extension: the host calls `dispatch` for every round. */
  guest: { dispatch(batch: Inbound[]): RoundStatus };
  /** Called with every message that has no other home (documents, settings). */
  onMessage(listener: (message: Inbound) => void | Promise<void>): void;
}

type Timed = (name: string, operation: () => void) => void;
const answers: {
  [K in ProviderKind]: (host: HostApi, request: bigint, value: ProviderResults[K], timed: Timed) => void;
} = {
  diagnostics: (host, request, value) => host.provideDiagnostics(request, value),
  formatting: (host, request, value) => host.provideEdits(request, value),
  save: (host, request, value) => host.provideEdits(request, value),
  completion: (host, request, value, timed) => {
    const items = Array.isArray(value) ? value : value.items;
    const incomplete = Array.isArray(value) ? false : value.incomplete;
    if (items.length <= 64) timed('sdk-completion-submit', () => host.provideCompletions(request, items, incomplete));
    else {
      let encoded: Uint8Array = new Uint8Array();
      timed('sdk-completion-encode', () => {
      let json: string;
      try { json = JSON.stringify(items); }
      catch (error) {
        if (!(error instanceof TypeError)) throw error;
        json = JSON.stringify(items, (_key, value) => typeof value === 'bigint' ? value.toString() : value);
      }
      encoded = new TextEncoder().encode(json);
      });
      timed('sdk-completion-submit', () => host.provideCompletionBatch(request, encoded, incomplete));
    }
  },
  'completion-resolve': (host, request, value) => host.provideCompletions(request, [value], false),
  hover: (host, request, value) => host.provideHover(request, value),
  definition: (host, request, value) => Array.isArray(value) ? host.provideLocations(request, value) : host.provideLanguage(request, value),
  'document-symbols': (host, request, value) => host.provideSymbols(request, value),
  'signature-help': (host, request, value) => value && 'tag' in value ? host.provideLanguage(request,value) : host.provideSignature(request,value),
  'inlay-hints': (host, request, value) => host.provideInlayHints(request, value),
  'code-actions': (host, request, value) => Array.isArray(value) ? host.provideCodeActions(request, value) : host.provideLanguage(request, value),
  'code-action-resolve': (h,r,v) => h.provideLanguage(r,v),
  references: (h,r,v) => h.provideLanguage(r,v),
  'workspace-symbols': (h,r,v) => h.provideLanguage(r,v),
  'document-highlights': (h,r,v) => h.provideLanguage(r,v),
  rename: (h,r,v) => h.provideLanguage(r,v),
  'prepare-rename': (h,r,v) => h.provideLanguage(r,v),
  'folding-ranges': (h,r,v) => h.provideLanguage(r,v),
  'selection-ranges': (h,r,v) => h.provideLanguage(r,v),
  'call-hierarchy': (h,r,v) => h.provideLanguage(r,v),
  'type-hierarchy': (h,r,v) => h.provideLanguage(r,v),
  'code-lens': (h,r,v) => h.provideLanguage(r,v),
  'code-lens-resolve': (h,r,v) => h.provideLanguage(r,v),
  'format-on-type': (h,r,v) => h.provideEdits(r,v),
  'execute-command': (h,r,v) => h.provideLanguage(r,v),
  highlights: (h,r,v) => h.provideLanguage(r,v),
};

export function createRuntime(host: HostApi): Runtime {
  const timers = new Map<bigint, () => void>();
  const documents = new Map<bigint, DocumentRef>();
  const texts = new Map<string, { document: bigint; value: string; bytes: number }>();
  let textBytes = 0;
  function discardText(document: bigint): void {
    for (const [key, entry] of texts) if (entry.document === document) { texts.delete(key); textBytes -= entry.bytes; }
  }
  function documentSnapshot(token: bigint, length: bigint, document?: DocumentRef): Snapshot {
    if (!document) return new Snapshot(host, token, length);
    const key = `${document.document}:${document.lifecycle}:${document.version}`;
    return new Snapshot(host, token, length, () => {
      const cached = texts.get(key);
      if (cached) { texts.delete(key); texts.set(key, cached); return cached.value; }
      const value = host.readRange(token, { start: 0n, end: length });
      const bytes = Number(length);
      if (bytes <= 32 * 1024 * 1024) {
        while (textBytes + bytes > 32 * 1024 * 1024 && texts.size) {
          const oldest = texts.entries().next().value!;
          texts.delete(oldest[0]); textBytes -= oldest[1].bytes;
        }
        texts.set(key, { document: document.document, value, bytes }); textBytes += bytes;
      }
      return value;
    });
  }
  const widgets = createUi(host, track);
  const watches = new Map<bigint, { listener: (event: FileChanged) => void; onError?: (error: HostError) => void; resolve: () => void; reject: (error: Error) => void }>();
  type ProcessState = {
    output?: (output: ProcessOutput) => void;
    settle: (exit: ProcessExit) => void;
    writes: { resolve: () => void; reject: (error: Error) => void }[];
    ended: boolean;
    closing: boolean;
    tail: Promise<void>;
  };
  const processes = new Map<bigint, ProcessState>();
  const providerCommands = new Map<string, (call: ProviderCommandCall) => void | Promise<void>>();
  const providerCalls = new Map<bigint, { cancelled: boolean }>();
  const commandHandlers = new Map<string, CommandHandler>();
  const providerHandlers = new Map<
    string,
    Map<ProviderKind, (call: ProviderCall) => unknown>
  >();
  const operations = new Map<bigint, (completion: Completion) => void>();
  const tasks = new Map<
    bigint,
    { onOutput: ((output: TaskOutput) => void) | undefined; settle: (exit: TaskExit) => void }
  >();
  const calls = new Map<bigint, { cancelled: boolean }>();
  const listeners: ((message: Inbound) => void | Promise<void>)[] = [];
  const savedListeners = new Set<(saved: { document: DocumentRef; snapshot: Snapshot }) => void | Promise<void>>();
  // Callbacks that started and have not finished: the round is `pending` while any exist.
  let running = 0;
  let timingListener: ((name: string, elapsedNanoseconds: bigint) => void) | undefined;
  const timed: Timed = (name, operation) => {
    const start = timingListener ? host.monotonicNanoseconds() : undefined;
    try { operation(); }
    finally { if (start !== undefined) {
      try { timingListener?.(name, host.monotonicNanoseconds() - start); }
      catch (error) { host.log(1, `timing observer failed: ${describe(error)}`); }
    } }
  };
  let sequence = 0n;

  function track(work: () => void | Promise<unknown>, what: string): void {
    running += 1;
    let promise: Promise<unknown>;
    try {
      promise = Promise.resolve(work());
    } catch (error) {
      promise = Promise.reject(error);
    }
    promise
      .catch((error) => host.log(2, `${what} failed: ${describe(error)}`))
      .finally(() => {
        running -= 1;
      });
  }

  function waitFor(operation: bigint): Promise<Completion> {
    return new Promise((resolve) => operations.set(operation, resolve));
  }

  function snapshotToken(value: Snapshot | bigint): bigint {
    return typeof value === 'bigint' ? value : value.token;
  }

  function runCommand(context: CommandContext): void {
    const handler = commandHandlers.get(context.command);
    if (!handler) {
      host.log(1, `unknown command ${context.command}`);
      return;
    }
    const state = { cancelled: false };
    calls.set(context.call, state);
    const snapshot =
      context.snapshot === undefined ? undefined : new Snapshot(host, context.snapshot);
    const call: CommandCall = {
      command: context.command,
      call: context.call,
      snapshot,
      selections: context.selections,
      selectionPrecondition: context.selectionPrecondition,
      get cancelled() {
        return state.cancelled;
      },
    };
    track(async () => {
      try {
        await handler(call);
      } finally {
        snapshot?.release();
        calls.delete(context.call);
      }
    }, `command ${context.command}`);
  }

  function runProvider(request: ProviderRequest): void {
    const handler = providerHandlers.get(request.provider)?.get(request.kind);
    if (!handler) {
      host.log(1, `no ${request.kind} handler for ${request.provider}`);
      host.releaseSnapshot(request.snapshot);
      return;
    }
    const reference = documents.get(request.document);
    const snapshot = documentSnapshot(request.snapshot, request.length, request.kind !== 'save' && reference?.version === request.version ? reference : undefined);
    const state = { cancelled: false };
    providerCalls.set(request.request, state);
    const call: ProviderCall = {
      request: request.request,
      get cancelled() { return state.cancelled; },
      provider: request.provider,
      kind: request.kind,
      snapshot,
      document: request.document,
      version: request.version,
      cursor: request.cursor === undefined ? undefined : Number(request.cursor),
      range: request.range,
      trigger: request.trigger,
      item: request.item,
      context: request.context,
    };
    track(async () => {
      try {
        const value = await handler(call);
        if (!state.cancelled) (answers[request.kind] as (h: HostApi, r: bigint, v: unknown, timed: Timed) => void)(
          host,
          request.request,
          value,
          timed,
        );
      } catch (error) {
        if (!state.cancelled) throw error;
      } finally {
        snapshot.release();
        providerCalls.delete(request.request);
      }
    }, `provider ${request.provider}`);
  }

  function deliver(message: Inbound): void {
    switch (message.tag) {
      case 'document-opened':
      case 'document-resync':
        discardText(message.val.document);
        documents.set(message.val.document, message.val);
        break;
      case 'document-changed': {
        const reference = documents.get(message.val.document);
        if (reference && reference.lifecycle === message.val.lifecycle) {
          discardText(message.val.document);
          documents.set(message.val.document, { ...reference, version: message.val.toVersion });
        }
        break;
      }
      case 'document-closed': {
        if (documents.get(message.val.document)?.lifecycle === message.val.lifecycle) {
          documents.delete(message.val.document); discardText(message.val.document);
        }
        break;
      }
      case 'timer-elapsed': {
        const callback = timers.get(message.val);
        timers.delete(message.val);
        if (callback) { try { callback(); } catch (error) { host.log(2, describe(error)); } }
        break;
      }
      case 'document-saved': {
        const saved = message.val;
        const snapshot = new Snapshot(host,saved.snapshot,saved.length);
        track(async () => { try { for (const listener of [...savedListeners]) await listener({ document:saved.document,snapshot }); } finally { snapshot.release(); } }, 'document save');
        break;
      }
      case 'provider-cancelled': {
        const call = providerCalls.get(message.val); if (call) call.cancelled = true;
        break;
      }
      case 'ui-event': widgets.deliver(message.val); break;
      case 'watch-ready': watches.get(message.val)?.resolve(); break;
      case 'file-changed': {
        const watch = watches.get(message.val.watch);
        if (watch) track(() => watch.listener(message.val), 'file watch');
        break;
      }
      case 'watch-failed': {
        const watch = watches.get(message.val.watch); watches.delete(message.val.watch);
        if (watch) {
          watch.reject(new Error(message.val.error.message));
          if (watch.onError) track(() => watch.onError!(message.val.error), 'file watch failure');
        }
        break;
      }
      case 'process-output': {
        const state = processes.get(message.val.process);
        if (state?.output) track(() => state.output!(message.val), 'process output');
        break;
      }
      case 'process-writable':
        processes.get(message.val.process)?.writes.shift()?.resolve();
        break;
      case 'process-exited': {
        const state = processes.get(message.val.process);
        processes.delete(message.val.process);
        if (state) {
          state.ended = true;
          const error = new Error(message.val.error?.message ?? 'process exited before the write completed');
          for (const write of state.writes.splice(0)) write.reject(error);
          state.settle(message.val);
        }
        break;
      }
      case 'provider-command': {
        const call = message.val;
        const handler = providerCommands.get(call.provider);
        if (handler) track(() => handler(call), `provider command ${call.provider}`);
        else host.log(1, `no command handler for provider ${call.provider}`);
        break;
      }
      case 'run-command':
        runCommand(message.val);
        break;
      case 'provider-request':
        runProvider(message.val);
        break;
      case 'operation-complete': {
        const [operation, completion] = message.val;
        const settle = operations.get(operation);
        operations.delete(operation);
        settle?.(completion);
        break;
      }
      case 'task-output':
        tasks.get(message.val.task)?.onOutput?.(message.val);
        break;
      case 'task-exited': {
        const task = tasks.get(message.val.task);
        tasks.delete(message.val.task);
        task?.settle(message.val);
        break;
      }
      case 'cancel': {
        const call = calls.get(message.val);
        if (call) call.cancelled = true;
        break;
      }
      default:
        break;
    }
    for (const listener of listeners) {
      try {
        const pending = listener(message);
        if (pending) track(() => pending, 'message listener');
      } catch (error) {
        host.log(2, `listener failed: ${describe(error)}`);
      }
    }
  }

  return {
    onTiming(listener) { timingListener = listener; },
    clock: { now: () => host.monotonicNanoseconds() },
    timers: {
      after(milliseconds, callback) {
        if (!Number.isInteger(milliseconds) || milliseconds < 0 || milliseconds > 86_400_000) throw new RangeError('invalid timer delay');
        const id = host.timerStart(milliseconds);
        timers.set(id, callback);
        return { cancel() { if (timers.delete(id)) host.timerCancel(id); } };
      },
    },
    ui: widgets.api,
    processes: {
      spawn(request, onOutput) {
        const id = host.processSpawn(request);
        let settle: (exit: ProcessExit) => void = () => {};
        const exited = new Promise<ProcessExit>((resolve) => { settle = resolve; });
        const state: ProcessState = { output: onOutput, settle, writes: [], ended: false, closing: false, tail: Promise.resolve() };
        processes.set(id, state);
        function enqueue(owned: Uint8Array): Promise<void> {
          const next = state.tail.then(() => new Promise<void>((resolve, reject) => {
            if (state.ended) { reject(new Error('process has exited')); return; }
            try {
              const accepted = host.processWrite(id, owned);
              if (accepted !== owned.length) { reject(new Error('process input budget is exhausted')); return; }
              state.writes.push({ resolve, reject });
            } catch (error) { reject(error); }
          }));
          state.tail = next.catch(() => {});
          return next;
        }
        return {
          id, exited,
          write(bytes) {
            if (state.closing) return Promise.reject(new Error('process input is closed'));
            // Own a copy: callers may reuse their buffer while the write waits.
            return enqueue(bytes.slice());
          },
          writeOwned(bytes) {
            if (state.closing) return Promise.reject(new Error('process input is closed'));
            const buffer = bytes.buffer as ArrayBuffer & { transfer?: () => ArrayBuffer };
            const owned = bytes.byteOffset === 0 && bytes.byteLength === buffer.byteLength && typeof buffer.transfer === 'function'
              ? new Uint8Array(buffer.transfer()) : bytes.slice();
            return enqueue(owned);
          },
          closeStdin() {
            state.closing = true;
            const close = state.tail.then(() => { if (!state.ended) host.processCloseStdin(id); });
            state.tail = close.catch(() => {});
            return close;
          },
          writeText(text) {
            if (state.closing) return Promise.reject(new Error('process input is closed'));
            const next = state.tail.then(() => new Promise<void>((resolve, reject) => {
              if (state.ended) { reject(new Error('process has exited')); return; }
              try {
                const accepted = host.processWriteText(id, text);
                if (text.length && accepted === 0) { reject(new Error('process input budget is exhausted')); return; }
                state.writes.push({ resolve, reject });
              } catch (error) { reject(error); }
            }));
            state.tail = next.catch(() => {}); return next;
          },
          kill() { state.closing = true; host.processKill(id); },
        };
      },
    },
    commands: {
      register(id, handler) {
        commandHandlers.set(id, handler);
      },
    },
    providers: {
      publishDiagnostics(provider,path,version,items) {host.publishDiagnostics(provider,path,version,items);},
      configure(configuration) { host.configureProvider(configuration); },
      onCommand(id, handler) { providerCommands.set(id, handler); },
      register(id, kind, handler) {
        let kinds = providerHandlers.get(id);
        if (!kinds) { kinds = new Map(); providerHandlers.set(id, kinds); }
        kinds.set(kind, handler);
      },
    },
    workspace: {
      onDidSave(listener) { savedListeners.add(listener); return () => { savedListeners.delete(listener); }; },
      showDocument(location) {
        return new Promise((resolve, reject) => { try { operations.set(host.performWorkspaceOperation({tag:'show-document',val:location}),resolve); } catch (error) { reject(error); } });
      },
      applyWorkspaceEdit(request) {
        return new Promise((resolve,reject) => { try { operations.set(host.performWorkspaceOperation({tag:'edit',val:request}),resolve); } catch (error) { reject(error); } });
      },
      roots() { return host.listRoots(); },
      captureDocument(document) {
        const captured = host.captureDocument(document);
        return { document: captured.document, snapshot: documentSnapshot(captured.snapshot, captured.length, captured.document) };
      },
      watchFiles(request, listener, onError) {
        const id = host.watchFiles(request);
        let resolve: () => void = () => {}; let reject: (error: Error) => void = () => {};
        const ready = new Promise<void>((done, failed) => { resolve = done; reject = failed; });
        void ready.catch(() => {});
        watches.set(id, { listener, onError, resolve, reject });
        return { id, ready, dispose() { watches.delete(id); reject(new Error('watch disposed')); host.unwatchFiles(id); } };
      },
      applyEdit(request, call = 0n) {
        sequence += 1n;
        const operation = host.applyEdit({
          title: request.title,
          call,
          sequence,
          selectionPrecondition: request.selectionPrecondition,
          documents: request.documents.map((document) => ({
            snapshot: snapshotToken(document.snapshot),
            edits: document.edits,
          })),
        } satisfies ApplyEditRequest);
        return waitFor(operation);
      },
      applyFileOps(request) {
        return waitFor(host.applyFileOps(request));
      },
    },
    tasks: {
      run(request, onOutput) {
        return new Promise((resolve) => {
          const task = host.taskStart(request);
          tasks.set(task, { onOutput, settle: resolve });
        });
      },
    },
    guest: {
      dispatch(batch) {
        for (const message of batch) deliver(message);
        // The engine runs microtasks before this returns, but the state computed
        // here cannot see their outcome (design §15.5). After work was started,
        // ask for one more, empty round: it reports the settled state.
        if (batch.length > 0 && (running > 0 || processes.size > 0 || timers.size > 0)) return 'runnable';
        return running > 0 || operations.size > 0 || tasks.size > 0 || processes.size > 0 || timers.size > 0 ? 'pending' : 'idle';
      },
    },
    onMessage(listener) {
      listeners.push(listener);
    },
  };
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
