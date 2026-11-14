import type { LayoutNode } from "./sessionLayout";

export type { LayoutNode };
export type MovePosition = "left" | "right" | "above" | "below" | "swap";

const FULL_SHARE = 100;
/** How far into a pane, as a fraction of its width or height, an edge drop reaches. */
const EDGE_BAND = 0.28;

type Axis = "row" | "column";

function split(node: LayoutNode): { axis: Axis; items: LayoutNode[] } | null {
  if (node.row) return { axis: "row", items: node.row };
  if (node.column) return { axis: "column", items: node.column };
  return null;
}

function build(axis: Axis, items: LayoutNode[], size: number | undefined): LayoutNode {
  const node: LayoutNode = axis === "row" ? { row: items } : { column: items };
  if (size !== undefined) node.size = size;
  return node;
}

function share(node: LayoutNode, count: number): number {
  return node.size ?? FULL_SHARE / count;
}

function round(value: number): number {
  return Math.round(value * 10) / 10;
}

function rescaled(items: LayoutNode[], originalCount: number): LayoutNode[] {
  const total = items.reduce((sum, item) => sum + share(item, originalCount), 0) || 1;
  return items.map((item) => ({ ...item, size: round((share(item, originalCount) / total) * FULL_SHARE) }));
}

/** Where a drop at these fractions of a pane's width and height lands. */
export function movePositionAt(x: number, y: number): MovePosition {
  const edges: Array<[MovePosition, number]> = [["left", x], ["right", 1 - x], ["above", y], ["below", 1 - y]];
  const closest = edges.sort((first, second) => first[1] - second[1])[0];
  return closest[1] < EDGE_BAND ? closest[0] : "swap";
}

function swapLeaves(node: LayoutNode, a: string, b: string): LayoutNode {
  const parts = split(node);
  if (!parts) {
    if (node.pane === a) return { ...node, pane: b };
    if (node.pane === b) return { ...node, pane: a };
    return node;
  }
  return build(parts.axis, parts.items.map((item) => swapLeaves(item, a, b)), node.size);
}

/** Removes a leaf and merges away any split left with one child or nested in a split of its own axis. */
function removeLeaf(node: LayoutNode, id: string): LayoutNode | null {
  const parts = split(node);
  if (!parts) return node.pane === id ? null : node;
  const kept = parts.items.map((item) => removeLeaf(item, id)).filter((item): item is LayoutNode => item !== null);
  if (kept.length === 0) return null;
  if (kept.length === 1) {
    const only = kept[0];
    return node.size === undefined ? { ...only, size: undefined } : { ...only, size: node.size };
  }
  const flattened: LayoutNode[] = [];
  const shares = rescaled(kept, parts.items.length);
  for (const item of shares) {
    const inner = split(item);
    if (inner && inner.axis === parts.axis) {
      const innerShares = rescaled(inner.items, inner.items.length);
      for (const child of innerShares) flattened.push({ ...child, size: round(((child.size ?? 0) * (item.size ?? 0)) / FULL_SHARE) });
    } else {
      flattened.push(item);
    }
  }
  return build(parts.axis, flattened, node.size);
}

function insertBeside(node: LayoutNode, target: string, source: string, axis: Axis, before: boolean): LayoutNode {
  const parts = split(node);
  if (!parts) {
    if (node.pane !== target) return node;
    const moved: LayoutNode = { pane: source, size: FULL_SHARE / 2 };
    const kept: LayoutNode = { pane: target, size: FULL_SHARE / 2 };
    return build(axis, before ? [moved, kept] : [kept, moved], node.size);
  }
  if (parts.axis === axis) {
    const index = parts.items.findIndex((item) => item.pane === target);
    if (index >= 0) {
      const items = rescaled(parts.items, parts.items.length);
      const half = round((items[index].size ?? 0) / 2);
      const next = [...items];
      next[index] = { ...items[index], size: half };
      next.splice(before ? index : index + 1, 0, { pane: source, size: half });
      return build(parts.axis, next, node.size);
    }
  }
  return build(parts.axis, parts.items.map((item) => insertBeside(item, target, source, axis, before)), node.size);
}

/** The tree with a new leaf `id` beside `target`. */
export function insertPaneInTree(tree: LayoutNode, id: string, target: string, position: Exclude<MovePosition, "swap">): LayoutNode {
  const axis: Axis = position === "left" || position === "right" ? "row" : "column";
  return insertBeside(tree, target, id, axis, position === "left" || position === "above");
}

/** The tree with `source` moved beside `target`, or the two exchanged. */
function holds(node: LayoutNode, id: string): boolean {
  const parts = split(node);
  return parts ? parts.items.some((item) => holds(item, id)) : node.pane === id;
}

export function movePaneInTree(tree: LayoutNode, source: string, target: string, position: MovePosition): LayoutNode {
  if (source === target || !holds(tree, source) || !holds(tree, target)) return tree;
  if (position === "swap") return swapLeaves(tree, source, target);
  const removed = removeLeaf(tree, source);
  if (!removed) return tree;
  const axis: Axis = position === "left" || position === "right" ? "row" : "column";
  return insertBeside(removed, target, source, axis, position === "left" || position === "above");
}
