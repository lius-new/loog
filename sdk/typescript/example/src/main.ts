// The reference TypeScript extension (design §10, §11.1 sample 1). It shows the
// SDK end to end: a command that edits the selection, a diagnostics provider and
// a hover provider, all with `async`/`await`.
import { byteOffset, byteRange, commands, guest as sdkGuest, log, providers, workspace } from '@loom/extension';

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

export const guest = sdkGuest;
