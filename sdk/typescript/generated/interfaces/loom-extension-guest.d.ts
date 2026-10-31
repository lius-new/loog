/** @module Interface loom:extension/guest@0.1.0 **/
/**
 * Deliver a batch and advance the guest by one bounded round. The call
 * must return; it must never wait for a later host delivery.
 */
export function dispatch(batch: Array<Inbound>): RoundStatus;
export type Inbound = import('./loom-extension-types.js').Inbound;
export type RoundStatus = import('./loom-extension-types.js').RoundStatus;
export type HostError = import('./loom-extension-types.js').HostError;
