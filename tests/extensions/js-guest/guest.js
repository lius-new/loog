// Reference JavaScript extension for the host tests (design §7.1, §11.1 sample 1).
// A minimal SDK shim turns host operations into Promises that are settled by a
// later `dispatch`, so `async`/`await` works across rounds without any export
// returning a Promise.
import * as host from 'loom:extension/host@0.1.0';

const waiting = new Map(); // operation id -> resolve(completion)
let running = 0; // callbacks that have started and not finished

function applyEdit(request) {
  const operation = host.applyEdit(request);
  return new Promise((resolve) => waiting.set(operation, resolve));
}

let sequence = 0n;

async function uppercase(context) {
  const snapshot = context.snapshot;
  if (snapshot === undefined) return;
  try {
    const edits = context.selections.map((selection) => ({
      range: selection.range,
      text: host.readRange(snapshot, selection.range).toUpperCase(),
    }));
    sequence += 1n;
    const completion = await applyEdit({
      title: 'Uppercase Selection',
      call: context.call,
      sequence,
      selectionPrecondition: context.selectionPrecondition,
      documents: [{ snapshot, edits }],
    });
    host.log(0, completion.tag === 'committed'
      ? `operation committed as ${completion.val.transaction}`
      : `operation failed: ${completion.val.message}`);
  } finally {
    host.releaseSnapshot(snapshot);
  }
}

const commands = {
  'example.tools.uppercase': uppercase,
  'example.tools.spin': async () => { for (let i = 0; ; i++) { if (i < 0) break; } },
  'example.tools.flood': async () => { for (let i = 0; ; i++) await Promise.resolve(); },
  'example.tools.trap': async () => { throw new Error('deliberate failure'); },
};

function start(context) {
  const handler = commands[context.command];
  if (!handler) { host.log(1, `unknown command ${context.command}`); return; }
  running += 1;
  handler(context)
    .catch((error) => host.log(2, `command failed: ${error}`))
    .finally(() => { running -= 1; });
}

export const guest = {
  dispatch(batch) {
    for (const message of batch) {
      switch (message.tag) {
        case 'run-command': start(message.val); break;
        case 'operation-complete': {
          const [operation, completion] = message.val;
          const resolve = waiting.get(operation);
          waiting.delete(operation);
          if (resolve) resolve(completion);
          break;
        }
        case 'document-opened': host.log(0, `opened ${message.val.document} v${message.val.version} life ${message.val.lifecycle} ${message.val.language}`); break;
        case 'document-changed': host.log(0, `changed ${message.val.document} ${message.val.fromVersion}->${message.val.toVersion} x${message.val.edits}`); break;
        case 'document-resync': host.log(0, `resync ${message.val.document} v${message.val.version}`); break;
        case 'document-closed': host.log(0, `closed ${message.val.document} life ${message.val.lifecycle}`); break;
        case 'provider-request': {
          const request = message.val;
          if (request.kind === 'completion') {
            host.provideCompletions(request.request, [
              { label: 'loom_hello', detail: 'from the extension', kind: 3, insertText: 'loom_hello()', data: 'hello' },
              { label: 'loom_world', kind: 3, data: 'world' },
            ], false);
            break;
          }
          if (request.kind === 'completion-resolve') {
            const item = { ...request.item, documentation: 'docs for ' + (request.item.data ?? '') };
            host.provideCompletions(request.request, [item], false);
            break;
          }
          const text = host.readRange(request.snapshot, { start: 0n, end: request.length });
          if (request.kind === 'diagnostics') {
            const items = [];
            for (let at = text.indexOf('TODO'); at >= 0; at = text.indexOf('TODO', at + 4)) {
              items.push({ range: { start: BigInt(at), end: BigInt(at + 4) }, severity: 2, message: 'TODO found', source: request.provider });
            }
            host.provideDiagnostics(request.request, items);
          } else {
            const edits = [];
            let offset = 0;
            for (const line of text.split(/(?<=\n)/)) {
              const body = line.replace(/[\r\n]+$/, '');
              const trimmed = body.replace(/[ \t]+$/, '');
              if (trimmed.length !== body.length) {
                edits.push({ range: { start: BigInt(offset + trimmed.length), end: BigInt(offset + body.length) }, text: '' });
              }
              offset += line.length;
            }
            host.provideEdits(request.request, edits);
          }
          host.releaseSnapshot(request.snapshot);
          break;
        }
        case 'settings-changed':
          for (const key of message.val) host.log(0, `setting ${key}=${host.getSetting(key) ?? ''}`);
          break;
        case 'cancel': host.log(0, `call ${message.val} cancelled`); break;
        default: break;
      }
    }
    return running > 0 || waiting.size > 0 ? 'pending' : 'idle';
  },
};
