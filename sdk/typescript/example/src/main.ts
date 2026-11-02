// The reference TypeScript extension (design §10, §11.1 sample 1). It shows the
// SDK end to end: a command that edits the selection, a diagnostics provider and
// a hover provider, all with `async`/`await`.
import { byteOffset, byteRange, commands, guest as sdkGuest, log, providers, workspace, ui } from '@loom/extension';

commands.register('example.tools.widget', () => {
  const node = ui.create('statusbar.extensions', ui.button('Starting', {
    id: 'example-status', background: 0x334455, foreground: 0xffffff, width: 100, height: 28,
  }));
  let ready = false;
  node.onEvent((event) => {
    if (event.payload.tag !== 'click') return;
    if (ready) node.remove();
    else { ready = true; node.update({ text: 'Ready', background: 0x228844 }); }
  });
});

commands.register('example.tools.uppercase', async (call) => {
  const snapshot = call.snapshot;
  if (snapshot === undefined) return;
  const edits = call.selections.map((selection) => ({
    range: selection.range,
    text: snapshot.read(selection.range).toUpperCase(),
  }));
  const completion = await workspace.applyEdit({
    title: 'Uppercase Selection',
    selectionPrecondition: call.selectionPrecondition,
    documents: [{ snapshot, edits }],
  }, call.call);
  log(0, completion.tag === 'committed' ? `committed as ${completion.val.transaction}` : `refused: ${completion.val.message}`);
});

providers.register('example.tools.todo', 'diagnostics', ({ snapshot, provider }) => {
  const text = snapshot.text();
  const found = [];
  for (let at = text.indexOf('TODO'); at >= 0; at = text.indexOf('TODO', at + 4)) {
    // Offsets are UTF-8 bytes, not the UTF-16 indexes `indexOf` gives.
    const start = byteOffset(text, at);
    found.push({ range: byteRange(start, start + 4), severity: 2, message: 'TODO found', source: provider, code: undefined });
  }
  return found;
});

providers.register('example.tools.hover', 'hover', ({ cursor, snapshot }) => {
  if (cursor === undefined) return undefined;
  return {
    contents: `**${snapshot.length ?? 0n}** bytes, caret at ${cursor}`,
    range: byteRange(cursor, cursor),
  };
});

// References to TODO markers show how unopened-file coordinates keep their unit.
providers.register('example.tools.references', 'references', ({ document, snapshot }) => {
  const captured = workspace.captureDocument(document);
  try {
    const text = snapshot.text();
    const at = text.indexOf('TODO');
    if (at < 0) return { tag: 'places', val: [] };
    const line = text.slice(0, at).split('\n').length - 1;
    const column = at - (text.lastIndexOf('\n', at) + 1);
    return { tag: 'places', val: [{ path: captured.document.path, range: {
      start: { line, column }, end: { line, column: column + 4 }, unit: 'utf16',
    } }] };
  } finally { captured.snapshot.release(); }
});

workspace.onDidSave(({ document, snapshot }) => {
  log(0, `saved ${document.version}: ${snapshot.text()}`);
});

export const guest = sdkGuest;
