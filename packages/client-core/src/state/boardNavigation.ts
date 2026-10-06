import type { Route } from "../router";

export type RestorableView = Extract<Route, { name: "session" | "workspace" }>;
export type BoardViewMemory = ReadonlyMap<string, RestorableView>;

export interface BoardViewInventory {
  liveSessionIds: ReadonlySet<string>;
  workspaceIds: ReadonlySet<number>;
}

export function restorableView(route: Route): RestorableView | null {
  return route.name === "session" || route.name === "workspace" ? route : null;
}

export function rememberBoardView(
  memory: BoardViewMemory,
  bucketId: string,
  route: Route,
): BoardViewMemory {
  const view = restorableView(route);
  if (!view) return memory;
  const current = memory.get(bucketId);
  if (current?.name === view.name && current.id === view.id) return memory;
  const next = new Map(memory);
  next.set(bucketId, view);
  return next;
}

export function validBoardView(
  memory: BoardViewMemory,
  bucketId: string,
  inventory: BoardViewInventory,
): RestorableView | null {
  const view = memory.get(bucketId);
  if (!view) return null;
  if (view.name === "session") {
    return inventory.liveSessionIds.has(view.id) ? view : null;
  }
  return inventory.workspaceIds.has(view.id) ? view : null;
}

export function boardReturnPath(
  memory: BoardViewMemory,
  bucketId: string,
  inventory: BoardViewInventory,
): string {
  const view = validBoardView(memory, bucketId, inventory);
  if (!view) return "/";
  return view.name === "session" ? `/session/${view.id}` : `/workspace/${view.id}`;
}
