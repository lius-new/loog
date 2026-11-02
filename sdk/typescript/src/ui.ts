// General widget descriptions; no editor, language or server-specific vocabulary.
import type { HostApi } from './runtime.ts';
import type { UiDescription, UiEvent, UiKind } from '../generated/interfaces/loom-extension-types.js';
export type UiOptions = Partial<Omit<UiDescription, 'id' | 'kind'>> & { id: string };
export interface UiNode {
  readonly node: bigint;
  readonly id: string;
  update(patch: Partial<Omit<UiDescription, 'id'>>): void;
  remove(): void;
  onEvent(listener: (event: UiEvent) => void | Promise<void>): () => void;
}
export interface Ui {
  create(parent: string | UiNode, description: UiDescription): UiNode;
  block(options: UiOptions): UiDescription;
  row(options: UiOptions): UiDescription;
  column(options: UiOptions): UiDescription;
  button(text: string, options: UiOptions): UiDescription;
  text(text: string, options: UiOptions): UiDescription;
  icon(icon: string, options: UiOptions): UiDescription;
  input(value: string, options: UiOptions): UiDescription;
}
const description = (kind: UiKind, options: UiOptions): UiDescription => ({
  text: undefined, icon: undefined, foreground: undefined, background: undefined,
  width: undefined, height: undefined, padding: 0, gap: 0, visible: true, label: undefined,
  ...options, kind,
});
export function createUi(host: HostApi, track: (work: () => void | Promise<unknown>, what: string) => void): { api: Ui; deliver(event: UiEvent): void } {
  type Record = { handle: UiNode; description: UiDescription; parent?: bigint; listeners: Set<(event: UiEvent) => void | Promise<void>> };
  const nodes = new Map<bigint, Record>();
  const api: Ui = {
    block: (options) => description('block', options),
    row: (options) => description('row', options),
    column: (options) => description('column', options),
    button: (text, options) => description('button', { ...options, text }),
    text: (text, options) => description('text', { ...options, text }),
    icon: (icon, options) => description('icon', { ...options, icon }),
    input: (text, options) => description('text-input', { ...options, text }),
    create(parent, props) {
      if (typeof parent !== 'string' && nodes.get(parent.node)?.handle !== parent) throw new Error('the parent belongs to another UI instance');
      const parentId = typeof parent === 'string' ? undefined : parent.node;
      const node = host.uiCreate(typeof parent === 'string' ? { tag: 'mount', val: parent } : { tag: 'node', val: parent.node }, props);
      const record: Record = { handle: undefined as never, description: { ...props }, parent: parentId, listeners: new Set() };
      const owned = () => { if (nodes.get(node) !== record) throw new Error('UI node has been removed'); };
      const handle: UiNode = {
        node, id: props.id,
        update(patch) {
          owned(); const next = { ...record.description, ...patch, id: props.id };
          host.uiUpdate(node, next); record.description = next;
        },
        remove() {
          owned(); host.uiRemove(node);
          const gone = new Set([node]);
          for (;;) {
            const before = gone.size;
            for (const [id, child] of nodes) if (child.parent !== undefined && gone.has(child.parent)) gone.add(id);
            if (gone.size === before) break;
          }
          for (const id of gone) nodes.delete(id);
        },
        onEvent(listener) { owned(); record.listeners.add(listener); return () => record.listeners.delete(listener); },
      };
      record.handle = handle; nodes.set(node, record); return handle;
    },
  };
  return { api, deliver(event) { for (const listener of nodes.get(event.node)?.listeners ?? []) track(() => listener(event), `UI event ${event.id}`); } };
}
