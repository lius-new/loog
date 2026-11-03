// General widget descriptions; no editor, language or server-specific vocabulary.
import type { HostApi } from './runtime.ts';
import type { UiDescription, UiEvent, UiKind } from '../generated/interfaces/loom-extension-types.js';
import type { Placement, Relation } from '../generated/interfaces/loom-ui-tree.js';
export interface UiTarget { readonly publicId: string }
export interface PlacementOptions { order?: number; whenMissing?: 'hide' }
export interface UiTreeHost { place(node: bigint, placement: Placement): void }
export type UiOptions = Partial<Omit<UiDescription, 'id' | 'kind'>> & { id: string };
export interface UiNode {
  readonly node: bigint;
  readonly id: string;
  update(patch: Partial<Omit<UiDescription, 'id'>>): void;
  remove(): void;
  onEvent(listener: (event: UiEvent) => void | Promise<void>): () => void;
}
export interface Ui {
  target(publicId: string): UiTarget;
  createElement(description: UiDescription): UiNode;
  insertBefore(target: UiTarget | UiNode, node: UiNode, options?: PlacementOptions): void;
  insertAfter(target: UiTarget | UiNode, node: UiNode, options?: PlacementOptions): void;
  appendChild(target: UiTarget | UiNode, node: UiNode, options?: PlacementOptions): void;
  move(node: UiNode, target: UiTarget | UiNode, options: PlacementOptions & { relation: Relation }): void;
  // COMPAT(legacy-ui-mount): The statusbar.extensions string remains accepted for old callers.
  // New roots use createElement and relative placement. Remove that legacy string usage
  // when mount support ends; retain owned-node creation and internal ui.detached staging.
  // No removal version has been scheduled.
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
// COMPAT(legacy-ui-runtime): The optional tree keeps existing createUi callers working.
// New callers supply loom:ui/tree; see createRuntime's matching compatibility boundary.
// Make tree mandatory after old embeddings/tests migrate; no removal is scheduled.
export function createUi(host: HostApi, track: (work: () => void | Promise<unknown>, what: string) => void, tree?: UiTreeHost): { api: Ui; deliver(event: UiEvent): void } {
  type Record = { handle: UiNode; description: UiDescription; parent?: bigint; listeners: Set<(event: UiEvent) => void | Promise<void>> };
  const nodes = new Map<bigint, Record>();
  const owned = (node: UiNode): Record => {
    const record = nodes.get(node.node);
    if (record?.handle !== node) throw new Error('UI node does not belong to this instance or has been removed');
    return record;
  };
  const place = (target: UiTarget | UiNode, node: UiNode, relation: Relation, options: PlacementOptions = {}) => {
    // COMPAT(legacy-ui-runtime): Base-only callers can still create/update owned widgets.
    // Relative placement requires the new tree interface. Remove this guard with the
    // optional tree parameter after caller migration; no removal version is scheduled.
    if (!tree) throw new Error('This runtime does not support relative UI placement');
    const record = owned(node);
    const order = options.order ?? 0;
    if (!Number.isInteger(order) || order < -2147483648 || order > 2147483647) throw new Error('UI order must be a signed 32-bit integer');
    if (options.whenMissing !== undefined && options.whenMissing !== 'hide') throw new Error('Unsupported missing-target behavior');
    const parent = 'node' in target ? owned(target) : undefined;
    tree.place(node.node, { target: 'node' in target ? { tag: 'node', val: target.node } : { tag: 'public-id', val: target.publicId }, relation, order });
    record.parent = 'node' in target ? relation === 'append' ? target.node : parent!.parent : undefined;
  };
  const api: Ui = {
    target(publicId) {
      if (!publicId || publicId.length > 256 || publicId.includes('\0') || publicId.startsWith('ext.') || publicId === 'ui.detached') throw new Error('Invalid public UI target');
      return Object.freeze({ publicId });
    },
    createElement: props => api.create('ui.detached', props),
    insertBefore: (target, node, options) => place(target, node, 'before', options),
    insertAfter: (target, node, options) => place(target, node, 'after', options),
    appendChild: (target, node, options) => place(target, node, 'append', options),
    move: (node, target, options) => place(target, node, options.relation, options),
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
      // COMPAT(legacy-ui-mount): Keep string parents for existing statusbar callers.
      // Prefer createElement plus relative placement for new roots. Retire only the
      // legacy statusbar path with host mount support; ui.detached and owned parents
      // remain in use. No removal version has been scheduled.
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
