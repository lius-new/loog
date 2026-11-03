/** @module Interface loom:ui/tree@0.1.0 **/
/**
 * Missing public targets remain pending and hidden until they appear.
 */
export function place(node: bigint, placement: Placement): void;
/**
 * # Variants
 *
 * ## `"before"`
 *
 * ## `"after"`
 *
 * ## `"append"`
 */
export type Relation = "before" | "after" | "append";
export type Target = TargetPublicId | TargetNode;
export interface TargetPublicId {
  tag: 'public-id',
  val: string,
}
export interface TargetNode {
  tag: 'node',
  val: bigint,
}
export interface Placement {
  target: Target,
  relation: Relation,
  order: number,
}
