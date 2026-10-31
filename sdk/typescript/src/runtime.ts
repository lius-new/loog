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

export type HostApi = typeof Host;

/** What a provider of each kind answers with. */
export interface ProviderResults {
  diagnostics: Diagnostic[];
  formatting: TextEdit[];
  save: TextEdit[];
  completion: CompletionItem[];
  'completion-resolve': CompletionItem;
  hover: HoverAnswer | undefined;
  definition: Location[];
  'document-symbols': DocumentSymbol[];
  'signature-help': SignatureAnswer | undefined;
  'inlay-hints': InlayHint[];
  'code-actions': CodeAction[];
}

/** The snapshot a command or provider was started for. */
export class Snapshot {
  readonly #host: HostApi;
  readonly token: bigint;
  #released = false;
  #whole: string | undefined;
  readonly length: bigint | undefined;

  constructor(host: HostApi, token: bigint, length?: bigint) {
    this.#host = host;
    this.token = token;
    this.length = length;
  }

  /** The text of a range given in UTF-8 bytes. */
  read(range: { start: bigint; end: bigint }): string {
    return this.#host.readRange(this.token, range);
  }

  /** The whole text (providers know its length; commands read selections). */
  text(): string {
    if (this.#whole === undefined) {
      if (this.length === undefined) throw new Error('the length of this snapshot is not known');
      this.#whole = this.#host.readRange(this.token, { start: 0n, end: this.length });
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
}

export type CommandHandler = (call: CommandCall) => void | Promise<void>;

export interface EditRequest {
  title: string;
  selectionPrecondition?: bigint | undefined;
  documents: { snapshot: Snapshot | bigint; edits: TextEdit[] }[];
}

export interface Runtime {
  commands: { register(id: string, handler: CommandHandler): void };
  providers: {
    register<K extends ProviderKind>(
      id: string,
      kind: K,
      handler: (call: ProviderCall) => ProviderResults[K] | Promise<ProviderResults[K]>,
    ): void;
  };
  workspace: {
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
  onMessage(listener: (message: Inbound) => void): void;
}

const answers: {
  [K in ProviderKind]: (host: HostApi, request: bigint, value: ProviderResults[K]) => void;
} = {
  diagnostics: (host, request, value) => host.provideDiagnostics(request, value),
  formatting: (host, request, value) => host.provideEdits(request, value),
  save: (host, request, value) => host.provideEdits(request, value),
  completion: (host, request, value) => host.provideCompletions(request, value, false),
  'completion-resolve': (host, request, value) => host.provideCompletions(request, [value], false),
  hover: (host, request, value) => host.provideHover(request, value),
  definition: (host, request, value) => host.provideLocations(request, value),
  'document-symbols': (host, request, value) => host.provideSymbols(request, value),
  'signature-help': (host, request, value) => host.provideSignature(request, value),
  'inlay-hints': (host, request, value) => host.provideInlayHints(request, value),
  'code-actions': (host, request, value) => host.provideCodeActions(request, value),
};

export function createRuntime(host: HostApi): Runtime {
  const commandHandlers = new Map<string, CommandHandler>();
  const providerHandlers = new Map<
    string,
    { kind: ProviderKind; handler: (call: ProviderCall) => unknown }
  >();
  const operations = new Map<bigint, (completion: Completion) => void>();
  const tasks = new Map<
    bigint,
    { onOutput: ((output: TaskOutput) => void) | undefined; settle: (exit: TaskExit) => void }
  >();
  const calls = new Map<bigint, { cancelled: boolean }>();
  const listeners: ((message: Inbound) => void)[] = [];
  // Callbacks that started and have not finished: the round is `pending` while any exist.
  let running = 0;
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
    const entry = providerHandlers.get(request.provider);
    if (!entry || entry.kind !== request.kind) {
      host.log(1, `no ${request.kind} handler for ${request.provider}`);
      host.releaseSnapshot(request.snapshot);
      return;
    }
    const snapshot = new Snapshot(host, request.snapshot, request.length);
    const call: ProviderCall = {
      provider: request.provider,
      kind: request.kind,
      snapshot,
      document: request.document,
      version: request.version,
      cursor: request.cursor === undefined ? undefined : Number(request.cursor),
      range: request.range,
      trigger: request.trigger,
      item: request.item,
    };
    track(async () => {
      try {
        const value = await entry.handler(call);
        (answers[request.kind] as (h: HostApi, r: bigint, v: unknown) => void)(
          host,
          request.request,
          value,
        );
      } finally {
        snapshot.release();
      }
    }, `provider ${request.provider}`);
  }

  function deliver(message: Inbound): void {
    switch (message.tag) {
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
        listener(message);
      } catch (error) {
        host.log(2, `listener failed: ${describe(error)}`);
      }
    }
  }

  return {
    commands: {
      register(id, handler) {
        commandHandlers.set(id, handler);
      },
    },
    providers: {
      register(id, kind, handler) {
        providerHandlers.set(id, { kind, handler });
      },
    },
    workspace: {
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
        if (batch.length > 0 && running > 0) return 'runnable';
        return running > 0 || operations.size > 0 || tasks.size > 0 ? 'pending' : 'idle';
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
