// Run with: node --test sdk/typescript/test
import assert from 'node:test';
import { deepStrictEqual, equal, throws } from 'node:assert/strict';
import { byteOffset, byteRange, slice, stringIndex, utf8Length } from '../src/text.ts';
import { createRuntime } from '../src/runtime.ts';

const { test } = assert;

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

// A host that records what the SDK asks and lets the test answer later.
function fakeHost() {
  const calls: unknown[][] = [];
  const logs: string[] = [];
  let operation = 100n;
  const note = (name: string) => (...args: unknown[]) => {
    calls.push([name, ...args]);
  };
  const host = {
    readRange: (_snapshot: bigint, range: { start: bigint; end: bigint }) =>
      'hello world'.slice(Number(range.start), Number(range.end)),
    releaseSnapshot: note('release'),
    applyEdit: (request: unknown) => {
      calls.push(['applyEdit', request]);
      operation += 1n;
      return operation;
    },
    applyFileOps: () => 1n,
    taskStart: () => 5n,
    log: (_level: number, message: string) => logs.push(message),
    provideDiagnostics: note('provideDiagnostics'),
    provideEdits: note('provideEdits'),
    provideCompletions: note('provideCompletions'),
    provideHover: note('provideHover'),
    provideLocations: note('provideLocations'),
    provideSymbols: note('provideSymbols'),
    provideSignature: note('provideSignature'),
    provideInlayHints: note('provideInlayHints'),
    provideCodeActions: note('provideCodeActions'),
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
