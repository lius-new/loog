// Run with: node --test sdk/typescript/test
import assert from 'node:test';
import { deepStrictEqual, equal, throws, rejects } from 'node:assert/strict';
import { byteOffset, byteRange, slice, stringIndex, utf8Length } from '../src/text.ts';
import { createRuntime } from '../src/runtime.ts';

const { test } = assert;

test('one-shot timers deliver once and cancelled callbacks stay cancelled', () => {
  const { host, calls } = fakeHost();
  const runtime = createRuntime(host);
  let fired = 0;
  runtime.timers.after(1000, () => fired++);
  const cancelled = runtime.timers.after(2000, () => fired += 10);
  cancelled.cancel();
  cancelled.cancel();
  runtime.guest.dispatch([{ tag: 'timer-elapsed', val: 101n }, { tag: 'timer-elapsed', val: 102n }]);
  runtime.guest.dispatch([{ tag: 'timer-elapsed', val: 101n }]);
  equal(fired, 1);
  equal(calls.filter(call => call[0] === 'timerCancel').length, 1);
  throws(() => runtime.timers.after(-1, () => {}), RangeError);
});

test('offsets count UTF-8 bytes and never land inside a character', () => {
  const text = 'a中😀b';
  equal(utf8Length(text), 1 + 3 + 4 + 1);
  equal(byteOffset(text, 0), 0);
  equal(byteOffset(text, 2), 4); // after 中
  equal(byteOffset(text, 4), 8); // after the emoji (two UTF-16 units)
  equal(stringIndex(text, 4), 2);
  equal(stringIndex(text, 5), 2, 'inside 中 rounds down');
  equal(stringIndex(text, 6), 2);
  equal(stringIndex(text, 9), 5);
  equal(stringIndex(text, 99), text.length);
  equal(stringIndex(text, -3), 0);
  equal(slice(text, byteRange(1, 4)), '中');
  equal(slice(text, byteRange(1, 5)), '中', 'a range ending inside a character stops before it');
});

test('a byte range must be ordered and whole', () => {
  throws(() => byteRange(3, 2), RangeError);
  throws(() => byteRange(-1, 2), RangeError);
  throws(() => byteRange(0.5, 2), RangeError);
});

test('immutable provider text is shared only inside the same document lifecycle and version', async () => {
  const fake = fakeHost();
  let reads = 0;
  const host = { ...fake.host, readRange() { reads++; return 'hello world'; } } as never;
  const runtime = createRuntime(host);
  runtime.providers.register('p', 'hover', ({ snapshot }) => ({ contents: snapshot.text(), range: undefined }));
  const reference = { document: 1n, lifecycle: 1n, path: '/work/a.txt', language: 'text', variant: undefined, version: 0n };
  runtime.guest.dispatch([{ tag: 'document-opened', val: reference }]);
  const query = (request: bigint, kind = 'hover', version = 0n) => ({ tag: 'provider-request', val: { request, provider: 'p', kind, snapshot: request, document: 1n, version, length: 11n, cursor: 0n } }) as never;
  for (const id of [1n, 2n]) { runtime.guest.dispatch([query(id)]); await Promise.resolve(); await Promise.resolve(); runtime.guest.dispatch([]); }
  equal(reads, 1);
  runtime.guest.dispatch([{ tag: 'document-changed', val: { document: 1n, lifecycle: 1n, fromVersion: 0n, toVersion: 1n, edits: 1 } }]);
  runtime.guest.dispatch([query(3n, 'hover', 1n)]); await Promise.resolve(); await Promise.resolve();
  equal(reads, 2);
  runtime.guest.dispatch([{ tag: 'document-closed', val: { ...reference, version: 1n } }, { tag: 'document-opened', val: { ...reference, lifecycle: 2n } }]);
  runtime.guest.dispatch([query(4n)]); await Promise.resolve(); await Promise.resolve();
  equal(reads, 3);
  // Save participants can see successive temporary texts with the same version.
  runtime.providers.register('p', 'save', ({ snapshot }) => { snapshot.text(); return []; });
  for (const id of [5n, 6n]) { runtime.guest.dispatch([query(id, 'save')]); await Promise.resolve(); await Promise.resolve(); }
  equal(reads, 5);
});

// A host that records what the SDK asks and lets the test answer later.
function fakeHost() {
  const calls: unknown[][] = [];
  const logs: string[] = [];
  let operation = 100n;
  let uiNode = 0n;
  const note = (name: string) => (...args: unknown[]) => {
    calls.push([name, ...args]);
  };
  const host = {
    timerStart: (milliseconds: number) => { calls.push(['timerStart', milliseconds]); return ++operation; },
    timerCancel: note('timerCancel'),
    monotonicNanoseconds: () => ++operation,
    readRange: (_snapshot: bigint, range: { start: bigint; end: bigint }) =>
      'hello world'.slice(Number(range.start), Number(range.end)),
    captureDocument: (document: bigint) => ({ snapshot: 80n, length: 11n, document: { document, lifecycle: 1n, path: '/project/file.txt', language: 'text', version: 4n } }),
    releaseSnapshot: note('release'),
    applyEdit: (request: unknown) => {
      calls.push(['applyEdit', request]);
      operation += 1n;
      return operation;
    },
    applyFileOps: () => 1n,
    taskStart: () => 5n,
    processSpawn: () => 9n,
    processWrite: (id: bigint, bytes: Uint8Array) => { calls.push(['processWrite', id, [...bytes]]); return bytes.length; },
    processWriteText: (id: bigint, text: string) => { calls.push(['processWriteText', id, text]); return utf8Length(text); },
    processCloseStdin: note('processCloseStdin'),
    processKill: note('processKill'),
    watchFiles: () => 12n,
    unwatchFiles: note('unwatchFiles'),
    uiCreate: (parent: unknown, description: unknown) => { calls.push(['uiCreate', parent, description]); return ++uiNode; },
    uiUpdate: note('uiUpdate'),
    uiRemove: note('uiRemove'),
    log: (_level: number, message: string) => logs.push(message),
    provideDiagnostics: note('provideDiagnostics'),
    provideEdits: note('provideEdits'),
    provideCompletions: note('provideCompletions'),
    provideCompletionBatch: note('provideCompletionBatch'),
    provideHover: note('provideHover'),
    provideLocations: note('provideLocations'),
    provideSymbols: note('provideSymbols'),
    provideSignature: note('provideSignature'),
    provideInlayHints: note('provideInlayHints'),
    provideCodeActions: note('provideCodeActions'),
    provideLanguage: note('provideLanguage'),
    configureProvider: note('configureProvider'),
    performWorkspaceOperation: (request: unknown) => { calls.push(['workspaceOperation',request]); return ++operation; },
  };
  return { host: host as never, calls, logs };
}

const run = (command: string, snapshot?: bigint) => ({
  tag: 'run-command' as const,
  val: { call: 7n, command, snapshot, selections: [], selectionPrecondition: undefined },
});

test('an awaited host operation settles in a later round and the round says so', async () => {
  const { host, calls, logs } = fakeHost();
  const runtime = createRuntime(host);
  let after = '';
  runtime.commands.register('c', async (call) => {
    const completion = await runtime.workspace.applyEdit({
      title: 't',
      documents: [{ snapshot: 5n, edits: [] }],
    }, call.call);
    after = completion.tag;
  });
  equal(runtime.guest.dispatch([run('c', 5n)]), 'runnable');
  equal(runtime.guest.dispatch([]), 'pending');
  const sent = calls.find((c) => c[0] === 'applyEdit')?.[1] as { sequence: bigint; call: bigint };
  equal(sent.sequence, 1n);
  equal(sent.call, 7n);

  runtime.guest.dispatch([
    { tag: 'operation-complete', val: [101n, { tag: 'committed', val: { transaction: 1n, version: 2n } }] },
  ]);
  // The engine drains its microtasks before a round returns; here a timer does.
  await new Promise((resolve) => setTimeout(resolve, 0));
  equal(runtime.guest.dispatch([]), 'idle');
  equal(after, 'committed');
  equal(calls.filter((c) => c[0] === 'release').length, 1, 'the snapshot is released once');
  deepStrictEqual(logs, []);
});

test('a handler that throws is logged and does not keep the round pending', async () => {
  const { host, logs } = fakeHost();
  const runtime = createRuntime(host);
  runtime.commands.register('boom', () => {
    throw new Error('deliberate');
  });
  runtime.guest.dispatch([run('boom')]);
  await Promise.resolve();
  await Promise.resolve();
  equal(runtime.guest.dispatch([]), 'idle');
  equal(logs.some((l) => l.includes('deliberate')), true, logs.join('|'));
});

test('an unknown command is logged', () => {
  const { host, logs } = fakeHost();
  const runtime = createRuntime(host);
  runtime.guest.dispatch([run('nobody')]);
  equal(logs[0], 'unknown command nobody');
});

test('a provider answer goes to the call for its kind and only for a matching kind', async () => {
  const { host, calls, logs } = fakeHost();
  const runtime = createRuntime(host);
  runtime.providers.register('p', 'hover', ({ cursor, snapshot }) => ({
    contents: `at ${cursor} of ${snapshot.length}`,
    range: undefined,
  }));
  const request = (kind: string, provider: string) => ({
    tag: 'provider-request' as const,
    val: { request: 9n, provider, kind, snapshot: 3n, document: 1n, version: 1n, length: 11n, range: undefined, cursor: 6n, trigger: undefined, item: undefined },
  });
  runtime.guest.dispatch([request('hover', 'p') as never]);
  await Promise.resolve();
  await Promise.resolve();
  const answer = calls.find((c) => c[0] === 'provideHover');
  deepStrictEqual(answer, ['provideHover', 9n, { contents: 'at 6 of 11', range: undefined }]);

  // Another kind for the same id, and an unknown id, get no answer.
  calls.length = 0;
  runtime.guest.dispatch([request('definition', 'p') as never, request('hover', 'nobody') as never]);
  equal(calls.some((c) => String(c[0]).startsWith('provide')), false);
  equal(logs.filter((l) => l.startsWith('no ')).length, 2);
});

test('registering a resolve handler preserves the completion handler on the same provider', async () => {
  const { host, calls, logs } = fakeHost();
  const runtime = createRuntime(host);
  runtime.providers.register('p', 'completion', () => []);
  runtime.providers.register('p', 'completion-resolve', ({ item }) => item!);
  const item = { label: 'item' };
  for (const kind of ['completion', 'completion-resolve']) {
    runtime.guest.dispatch([{ tag: 'provider-request', val: { request: 9n, provider: 'p', kind, snapshot: 3n, document: 1n, version: 1n, length: 11n, range: undefined, cursor: 6n, trigger: undefined, item } } as never]);
    await Promise.resolve(); await Promise.resolve();
  }
  equal(calls.filter((call) => call[0] === 'provideCompletions').length, 2);
  deepStrictEqual(logs, []);
});

test('large typed completion answers use the closed bulk codec without losing byte offsets', async () => {
  const { host, calls } = fakeHost();
  const runtime = createRuntime(host);
  runtime.providers.register('p', 'completion', () => ({ incomplete: true, items: Array.from({ length: 65 }, () => ({ label: '中😀', additionalTextEdits: [], commitCharacters: ['('], textEdit: { range: { start: 9007199254740993n, end: 9007199254740994n }, text: 'value' } })) }));
  runtime.guest.dispatch([{ tag: 'provider-request', val: { request: 9n, provider: 'p', kind: 'completion', snapshot: 3n, document: 1n, version: 1n, length: 11n } } as never]);
  await Promise.resolve(); await Promise.resolve();
  const answer = calls.find(call => call[0] === 'provideCompletionBatch')!;
  equal(answer[1], 9n); equal(answer[3], true);
  const items = JSON.parse(new TextDecoder().decode(answer[2] as Uint8Array));
  equal(items.length, 65); equal(items[0].textEdit.range.start, '9007199254740993');
  equal(items[0].label, '中😀');
  equal(calls.some(call => call[0] === 'provideCompletions'), false);
});

test('optional timing observers cannot prevent typed answers from being delivered', async () => {
  const { host, calls, logs } = fakeHost(); const runtime = createRuntime(host);
  const timings: string[] = [];
  runtime.onTiming((name, elapsed) => { timings.push(name); equal(elapsed, 1n); throw new Error('observer'); });
  runtime.providers.register('p', 'completion', () => []);
  runtime.guest.dispatch([{ tag: 'provider-request', val: { request: 9n, provider: 'p', kind: 'completion', snapshot: 3n, document: 1n, version: 1n, length: 11n } } as never]);
  await Promise.resolve(); await Promise.resolve();
  deepStrictEqual(timings, ['sdk-completion-submit']);
  equal(calls.filter(call => call[0] === 'provideCompletions').length, 1);
  equal(logs.filter(message => message.includes('timing observer failed')).length, 1);
});

test('a cancel message marks the running call', async () => {
  const { host } = fakeHost();
  const runtime = createRuntime(host);
  const seen: boolean[] = [];
  let release: () => void = () => {};
  runtime.commands.register('slow', async (call) => {
    await new Promise<void>((resolve) => (release = resolve));
    seen.push(call.cancelled);
  });
  runtime.guest.dispatch([run('slow')]);
  runtime.guest.dispatch([{ tag: 'cancel', val: 7n }]);
  release();
  await Promise.resolve();
  await Promise.resolve();
  deepStrictEqual(seen, [true]);
});

test('cancelled provider callbacks release snapshots and cannot publish a late answer', async () => {
  const { host,calls } = fakeHost(); const runtime = createRuntime(host);
  let finish: () => void = () => {}; let cancelled = false;
  runtime.providers.register('p','references',async (call) => { await new Promise<void>((resolve)=>{finish=resolve;}); cancelled=call.cancelled; return {tag:'places',val:[]}; });
  runtime.guest.dispatch([{tag:'provider-request',val:{request:9n,provider:'p',kind:'references',snapshot:3n,document:1n,version:1n,length:11n}} as never]);
  runtime.guest.dispatch([{tag:'provider-cancelled',val:9n}]); finish(); await Promise.resolve(); await Promise.resolve();
  equal(cancelled,true); equal(calls.some((call)=>call[0]==='provideLanguage'),false); deepStrictEqual(calls.filter((call)=>call[0]==='release'),[['release',3n]]);
});

test('saved snapshots survive async callbacks and native workspace operations wait for their receipt', async () => {
  const { host,calls } = fakeHost(); const runtime = createRuntime(host);
  let finish: () => void = () => {}; let seen = '';
  runtime.workspace.onDidSave(async ({snapshot}) => { await new Promise<void>((resolve)=>{finish=resolve;}); seen=snapshot.text(); });
  runtime.guest.dispatch([{tag:'document-saved',val:{snapshot:80n,length:11n,document:{document:1n,lifecycle:1n,version:4n,path:'/file',language:'text'}}}]);
  equal(calls.some((call)=>call[0]==='release'),false); finish(); await Promise.resolve(); await Promise.resolve(); equal(seen,'hello world');
  const result = runtime.workspace.applyWorkspaceEdit({title:'edit',edit:{changes:[]}});
  let settled = false; void result.then(()=>{settled=true;}); await Promise.resolve(); equal(settled,false);
  runtime.guest.dispatch([{tag:'operation-complete',val:[101n,{tag:'committed',val:{transaction:0n,version:0n}}]}]);
  equal((await result).tag,'committed'); deepStrictEqual(calls.find((call)=>call[0]==='workspaceOperation'),['workspaceOperation',{tag:'edit',val:{title:'edit',edit:{changes:[]}}}]);
});

test('accepted result commands are dispatched only to their provider', async () => {
  const { host, logs } = fakeHost();
  const runtime = createRuntime(host);
  const seen: unknown[] = [];
  runtime.providers.onCommand('p', async (call) => { seen.push(JSON.parse(call.command.arguments)); });
  runtime.providers.onCommand('other', () => { throw new Error('wrong origin'); });
  runtime.guest.dispatch([{ tag: 'provider-command', val: { provider: 'p', command: { name: 'accepted', arguments: '[42]' } } }]);
  await new Promise((resolve) => setTimeout(resolve, 0));
  deepStrictEqual(seen, [[42]]);
  deepStrictEqual(logs, []);
  equal(runtime.guest.dispatch([]), 'idle');
});

test('binary writes wait for acknowledgement and stdin closes after queued writes', async () => {
  const { host, calls } = fakeHost();
  const runtime = createRuntime(host);
  const output: number[][] = [];
  const child = runtime.processes.spawn({ root: 1n, program: 'tool', args: [], env: [], cwd: undefined, framing: 'content-length' }, (event) => output.push([...event.bytes]));
  const first = child.write(new Uint8Array([0, 255]));
  const second = child.write(new Uint8Array([1]));
  const closed = child.closeStdin();
  await Promise.resolve();
  deepStrictEqual(calls, [['processWrite', 9n, [0, 255]]]);
  runtime.guest.dispatch([{ tag: 'process-writable', val: { process: 9n, bytes: 2 } }]);
  await first;
  await new Promise((resolve) => setTimeout(resolve, 0));
  deepStrictEqual(calls[1], ['processWrite', 9n, [1]]);
  runtime.guest.dispatch([{ tag: 'process-writable', val: { process: 9n, bytes: 1 } }]);
  await second; await closed;
  deepStrictEqual(calls[2], ['processCloseStdin', 9n]);
  runtime.guest.dispatch([{ tag: 'process-output', val: { process: 9n, channel: 'stdout', bytes: new Uint8Array([0, 255]) } }]);
  deepStrictEqual(output, [[0, 255]]);
  runtime.guest.dispatch([{ tag: 'process-exited', val: { process: 9n, code: 0, killed: false, error: undefined } }]);
  equal((await child.exited).code, 0);
});

test('owned process writes detach dedicated buffers and copy partial views without changing ordering', async () => {
  const { host, calls } = fakeHost(); const runtime = createRuntime(host);
  const child = runtime.processes.spawn({ root: 1n, program: 'tool', args: [], env: [], framing: 'raw' });
  const dedicated = new Uint8Array([1, 255]);
  const first = child.writeOwned(dedicated);
  equal(dedicated.byteLength, 0);
  const backing = new Uint8Array([8, 2, 3, 9]);
  const second = child.writeOwned(backing.subarray(1, 3));
  backing[1] = 7;
  const close = child.closeStdin();
  await Promise.resolve(); deepStrictEqual(calls[0], ['processWrite', 9n, [1, 255]]);
  runtime.guest.dispatch([{ tag: 'process-writable', val: { process: 9n, bytes: 2 } }]); await first;
  await new Promise(resolve => setTimeout(resolve, 0));
  deepStrictEqual(calls[1], ['processWrite', 9n, [2, 3]]);
  runtime.guest.dispatch([{ tag: 'process-writable', val: { process: 9n, bytes: 2 } }]); await second; await close;
  deepStrictEqual(calls[2], ['processCloseStdin', 9n]);
});

test('file watch readiness, events and disposal preserve the watch identity', async () => {
  const { host, calls } = fakeHost(); const runtime = createRuntime(host);
  const seen: string[] = [];
  const watch = runtime.workspace.watchFiles({ root: 1n, patterns: ['**/*.rs'] }, (event) => seen.push(event.changes[0].path));
  runtime.guest.dispatch([{ tag: 'watch-ready', val: 12n }]); await watch.ready;
  const event = { tag: 'file-changed' as const, val: { watch: 12n, root: 1n, changes: [{ path: 'src/main.rs', kind: 'create' as const }] } };
  runtime.guest.dispatch([event]); deepStrictEqual(seen, ['src/main.rs']);
  watch.dispose(); deepStrictEqual(calls, [['unwatchFiles', 12n]]);
  runtime.guest.dispatch([event]); deepStrictEqual(seen, ['src/main.rs']);
});

test('UTF-8 text writes share binary ordering, receipts and close semantics', async () => {
  const { host, calls } = fakeHost(); const runtime = createRuntime(host);
  const seen: string[] = [];
  const child = runtime.processes.spawn({ root: 1n, program: 'tool', args: [], env: [], framing: 'content-length-text' }, event => { if (event.text !== undefined) seen.push(event.text); });
  const first = child.write(new Uint8Array([255]));
  const second = child.writeText('中😀');
  const close = child.closeStdin();
  await Promise.resolve(); equal(calls.length, 1);
  runtime.guest.dispatch([{ tag: 'process-writable', val: { process: 9n, bytes: 1 } }]); await first;
  await new Promise(resolve => setTimeout(resolve, 0));
  deepStrictEqual(calls[1], ['processWriteText', 9n, '中😀']);
  runtime.guest.dispatch([{ tag: 'process-output', val: { process: 9n, channel: 'stdout', bytes: new Uint8Array(), text: '中😀' } }]);
  deepStrictEqual(seen, ['中😀']);
  runtime.guest.dispatch([{ tag: 'process-writable', val: { process: 9n, bytes: 7 } }]); await second; await close;
  deepStrictEqual(calls[2], ['processCloseStdin', 9n]);
  await rejects(child.writeText('late'), /closed/);
});

test('widget builders preserve colors, route events and discard removed subtrees', async () => {
  const { host, calls } = fakeHost(); const runtime = createRuntime(host);
  const block = runtime.ui.create('statusbar.extensions', runtime.ui.block({ id: 'color', background: 0x123abc, width: 16 }));
  const label = runtime.ui.create(block, runtime.ui.text('Starting', { id: 'label' }));
  const events: unknown[] = []; label.onEvent((event) => { events.push(event.payload); });
  block.update({ background: 0x00ff00 });
  equal((calls[2][2] as { background: number }).background, 0x00ff00);
  runtime.guest.dispatch([{ tag: 'ui-event', val: { node: label.node, id: 'ext.example.label', payload: { tag: 'visibility', val: true } } }]);
  deepStrictEqual(events, [{ tag: 'visibility', val: true }]);
  block.remove(); throws(() => label.update({ text: 'stale' }), /removed/);
  deepStrictEqual(calls[3], ['uiRemove', block.node]);
  runtime.guest.dispatch([{ tag: 'ui-event', val: { node: label.node, id: 'old', payload: { tag: 'click' } } }]);
  equal(events.length, 1);
});

test('a captured document snapshot includes version metadata and releases once', () => {
  const { host, calls } = fakeHost(); const runtime = createRuntime(host);
  const captured = runtime.workspace.captureDocument(3n);
  equal(captured.document.document, 3n); equal(captured.document.version, 4n);
  equal(captured.snapshot.text(), 'hello world');
  captured.snapshot.release(); captured.snapshot.release();
  deepStrictEqual(calls, [['release', 80n]]);
});
