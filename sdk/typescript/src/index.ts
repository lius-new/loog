// @loom/extension: the TypeScript SDK for Loom behaviour extensions.
//
// An extension registers commands and providers at the top of its module and
// exports the `guest` this file creates:
//
//   import { commands, workspace, guest as sdkGuest } from '@loom/extension';
//   commands.register('acme.tools.upper', async (call) => { ... });
//   export const guest = sdkGuest;
//
// The host contract (`wit/loom-extension/world.wit`) is not frozen; the types in
// `generated/` come from it and are regenerated with `node sdk/typescript/generate.mjs`.
import * as host from 'loom:extension/host@0.1.0';
import { createRuntime } from './runtime.ts';

const runtime = createRuntime(host);

export const commands = runtime.commands;
export const providers = runtime.providers;
export const workspace = runtime.workspace;
export const tasks = runtime.tasks;
export const onMessage = runtime.onMessage;
export const guest = runtime.guest;

/** Write to the extension's own log (0 debug, 1 warning, 2 error). Never put document text in it. */
export function log(level: number, message: string): void {
  host.log(level, message);
}

export { byteOffset, byteRange, bounds, slice, stringIndex, utf8Length } from './text.ts';
export type {
  CommandCall,
  CommandHandler,
  EditRequest,
  ProviderCall,
  ProviderResults,
  Snapshot,
} from './runtime.ts';
export type {
  CodeAction,
  CompletionItem,
  Diagnostic,
  DocumentSymbol,
  HoverAnswer,
  InlayHint,
  Location,
  ProviderKind,
  SignatureAnswer,
  TextEdit,
} from '../generated/interfaces/loom-extension-types.js';
