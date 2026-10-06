import { approvalRoutePath, type Route } from "../router";
import {
  restorableView,
  type BoardViewInventory,
  type RestorableView,
} from "./boardNavigation";

export const SETTINGS_RETURN_TARGET_KEY = "pm.settingsReturnTarget";

export type SettingsReturnTarget = Exclude<Route, { name: "settings" }>;

export interface SettingsReturnInventory extends BoardViewInventory {
  retainedViews: readonly RestorableView[];
  recentViews: readonly RestorableView[];
}

export function routePath(route: SettingsReturnTarget): string {
  switch (route.name) {
    case "session": return `/session/${route.id}`;
    case "workspace": return `/workspace/${route.id}`;
    case "board": return `/bucket/${route.bucketId}/board`;
    case "item": return `/bucket/${route.bucketId}/item/${route.id}`;
    case "legacy-item": return `/item/${route.legacyId}`;
    case "review": return `/review/${route.id}`;
    case "approvals": return approvalRoutePath(route.id);
    case "home": return "/";
  }
}

export function serializeSettingsReturnTarget(route: Route): string | null {
  return route.name === "settings" ? null : routePath(route);
}

export function settingsEntryReturnTarget(
  route: Route,
  previousSessionId: string | null,
  liveSessionIds: ReadonlySet<string>,
): Route {
  if (route.name !== "home" || !previousSessionId || !liveSessionIds.has(previousSessionId)) {
    return route;
  }
  return { name: "session", id: previousSessionId };
}

export function parseSettingsReturnTarget(value: string | null): SettingsReturnTarget | null {
  if (!value) return null;
  const session = value.match(/^\/session\/(\d+)$/);
  if (session) return { name: "session", id: session[1] };
  const workspace = value.match(/^\/workspace\/(\d+)$/);
  if (workspace) return { name: "workspace", id: Number(workspace[1]) };
  const board = value.match(/^\/bucket\/(\d+)\/board$/);
  if (board) return { name: "board", bucketId: board[1] };
  const item = value.match(/^\/bucket\/(\d+)\/item\/(\d+)$/);
  if (item) return { name: "item", bucketId: item[1], id: item[2] };
  const legacyItem = value.match(/^\/item\/(\d+)$/);
  if (legacyItem) return { name: "legacy-item", legacyId: legacyItem[1] };
  const approval = value.match(/^\/approvals(?:\/([A-Za-z0-9_-]{1,128}))?$/);
  if (approval) return { name: "approvals", ...(approval[1] ? { id: approval[1] } : {}) };
  if (value === "/") return { name: "home" };
  return null;
}

function validView(view: RestorableView, inventory: BoardViewInventory): boolean {
  return view.name === "session"
    ? inventory.liveSessionIds.has(view.id)
    : inventory.workspaceIds.has(view.id);
}

export function settingsReturnView(
  target: SettingsReturnTarget | null,
  inventory: SettingsReturnInventory,
): RestorableView | null {
  const exact = target ? restorableView(target) : null;
  if (exact && validView(exact, inventory)) return exact;
  return [...inventory.retainedViews, ...inventory.recentViews]
    .find((view) => validView(view, inventory)) ?? null;
}

export function settingsReturnPath(
  target: SettingsReturnTarget | null,
  inventory: SettingsReturnInventory,
): string {
  if (target && !restorableView(target)) return routePath(target);
  const view = settingsReturnView(target, inventory);
  return view ? routePath(view) : "/";
}
