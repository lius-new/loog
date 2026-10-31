# Loom extension SDK for TypeScript

Status: candidate. It follows `wit/loom-extension/world.wit`, which is not frozen.

An extension is a TypeScript module that registers commands and providers and
exports the SDK's `guest`:

```ts
import { commands, guest as sdkGuest, providers, workspace } from '@loom/extension';

commands.register('acme.tools.uppercase', async (call) => {
  const snapshot = call.snapshot;
  if (!snapshot) return;
  const edits = call.selections.map((s) => ({ range: s.range, text: snapshot.read(s.range).toUpperCase() }));
  await workspace.applyEdit({
    title: 'Uppercase Selection',
    selectionPrecondition: call.selectionPrecondition,
    documents: [{ snapshot, edits }],
  }, call.call);
});

providers.register('acme.tools.hover', 'hover', ({ cursor }) =>
  cursor === undefined ? undefined : { contents: 'Hello', range: undefined });

export const guest = sdkGuest;
```

`async`/`await` works across the host's rounds: a host operation returns a Promise
that a later round settles, and the SDK reports to the host whether work is still
pending. Handlers may throw; the failure is logged and does not stall the host.
The snapshot of a command or provider is released when the handler ends.

## What the SDK takes care of

- **Offsets.** The contract counts UTF-8 bytes; JavaScript counts UTF-16 units.
  `byteOffset`, `stringIndex`, `slice` and `byteRange` convert without ever
  producing an offset inside a character. `Snapshot.textIn(range)` reads a byte
  range out of the whole text.
- **Provider kinds.** `providers.register(id, kind, handler)` types the result by
  kind (`diagnostics`, `formatting`, `save`, `completion`, `completion-resolve`,
  `hover`, `definition`, `document-symbols`, `signature-help`, `inlay-hints`,
  `code-actions`) and sends it to the matching host call. A request whose id or
  kind has no handler is logged.
- **Cancellation.** `call.cancelled` turns true after the host cancels a command.

## Building

```sh
node sdk/typescript/build.mjs [entry.ts] [out.wasm]
```

It checks that `generated/` matches the contract, type-checks the sources with
`tsc`, bundles the entry with the SDK into one file and turns it into a component
with the pinned jco. With no arguments it builds `example/src/main.ts` into
`target/extension-guest/ts_guest.wasm`. The toolchain is the one pinned in
`tests/extensions/js-guest/package.json` (installed on first use).

`generated/` holds the TypeScript types of the contract. After changing the
contract run `node sdk/typescript/generate.mjs`; `--check` fails when they differ.

## Tests

```sh
node --test sdk/typescript/test/sdk.test.ts          # offsets and the round protocol, with a fake host
cargo test --release -p loom-extension-host --test ts_guest   # the built component in the real host
```

## Known gaps

No published package yet (`@loom/extension` is resolved by path), no source maps
from errors to TypeScript lines, and a component is about 12 MiB because it
carries the JavaScript engine: build and cache it ahead of activation.
