export type Route =
  | { name: "home" }
  | { name: "session"; id: string; tab?: string; focus?: boolean }
  | { name: "workspace"; id: number }
  | { name: "board"; bucketId: string }
  | { name: "item"; bucketId: string; id: string }
  | { name: "legacy-item"; legacyId: string }
  | { name: "approvals"; id?: string }
  | { name: "review"; id: number; view?: string; file?: string; thread?: number }
  | ({ name: "settings"; section: SettingsSection } & SettingsTarget);

export const SETTINGS_GROUPS = [
  { label: "Your account", sections: ["appearance", "terminal-theme", "notifications", "password"] },
  { label: "Workspace", sections: ["projects", "connections", "models", "instructions"] },
  { label: "Controller", sections: ["workers", "mobile", "daemon"] },
] as const;
export type SettingsSection = typeof SETTINGS_GROUPS[number]["sections"][number];
export const SETTINGS_SECTIONS: readonly SettingsSection[] =
  SETTINGS_GROUPS.flatMap((group) => [...group.sections]);

/** Where a reader is inside the Connections page: one connection, its setup, or one call open over the list. */
export type ConnectionView =
  | { view: "new" }
  | { view: "detail"; id: number }
  | { view: "setup"; id: number }
  | { view: "call"; callId: string };

export const SETTINGS_CATALOGS = ["buckets", "projects"] as const;
export type SettingsCatalog = typeof SETTINGS_CATALOGS[number];

/** What a Settings page has open: on Projects a catalog or one bucket or project, on Connections a place inside it. */
export interface SettingsTarget {
  catalog?: SettingsCatalog;
  bucketId?: string;
  projectId?: string;
  connection?: ConnectionView;
}

// Settings once lived at two addresses: account pages under /settings and
// everything else under /manage. Each old prefix keeps its own default page.
const SETTINGS_PREFIX_DEFAULTS: Record<string, SettingsSection> = {
  settings: "appearance",
  manage: "projects",
};

/** The one address of a Settings page. Only Projects and Connections carry a target. */
export function settingsRoutePath(section: SettingsSection, target: SettingsTarget = {}): string {
  if (section === "connections") return connectionRoutePath(target.connection);
  const params: string[] = [];
  if (section === "projects") {
    if (target.bucketId) params.push(`bucket=${encodeURIComponent(target.bucketId)}`);
    if (target.projectId) params.push(`project=${encodeURIComponent(target.projectId)}`);
    if (target.catalog) params.push(`catalog=${target.catalog}`);
  }
  return `/settings/${section}${params.length > 0 ? `?${params.join("&")}` : ""}`;
}

const APPROVAL_ID = /^[A-Za-z0-9_-]{1,128}$/;

/** The address of the approvals list, or of one approval open over it. */
export function approvalRoutePath(id?: string): string {
  return id ? `/approvals/${encodeURIComponent(id)}` : "/approvals";
}

/** The address of the connections list, or of a place inside it. */
export function connectionRoutePath(at?: ConnectionView): string {
  const list = "/settings/connections";
  if (!at) return list;
  if (at.view === "new") return `${list}/new`;
  if (at.view === "call") return `${list}/calls/${encodeURIComponent(at.callId)}`;
  return at.view === "setup" ? `${list}/${at.id}/setup` : `${list}/${at.id}`;
}

function parseConnectionView(rest: string): ConnectionView | undefined {
  if (rest === "new") return { view: "new" };
  const call = rest.match(/^calls\/([^/?]+)$/);
  if (call) return APPROVAL_ID.test(call[1]) ? { view: "call", callId: call[1] } : undefined;
  const connection = rest.match(/^(\d+)(\/setup)?$/);
  if (!connection) return undefined;
  const id = Number(connection[1]);
  if (!Number.isSafeInteger(id) || id < 1) return undefined;
  return { view: connection[2] ? "setup" : "detail", id };
}

export function parseRoute(hash: string): Route {
  const path = hash.replace(/^#/, "");
  // A session route carries where you were inside it, so the back
  // button and a refresh land on the same tab rather than resetting to
  // the agent.
  const session = path.match(/^\/session\/(\d+)(?:\?(.*))?$/);
  if (session) {
    const params = new URLSearchParams(session[2] ?? "");
    const tab = params.get("tab");
    return {
      name: "session",
      id: session[1],
      ...(tab ? { tab } : {}),
      ...(params.get("focus") === "1" ? { focus: true } : {}),
    };
  }
  // A review route carries where you were reading it, so the back
  // button and a shared link land on the same revision, file and
  // comment rather than resetting to the live tree.
  const review = path.match(/^\/review\/(\d+)(?:\?(.*))?$/);
  if (review) {
    const params = new URLSearchParams(review[2] ?? "");
    const view = params.get("view");
    const file = params.get("file");
    const thread = Number(params.get("thread"));
    return {
      name: "review",
      id: Number(review[1]),
      ...(view ? { view } : {}),
      ...(file ? { file } : {}),
      ...(Number.isSafeInteger(thread) && thread > 0 ? { thread } : {}),
    };
  }
  // A call id is an opaque token; anything else opens the list alone.
  const approvals = path.match(/^\/approvals(?:\/([^/?]*))?$/);
  if (approvals) {
    const id = approvals[1];
    return { name: "approvals", ...(id && APPROVAL_ID.test(id) ? { id } : {}) };
  }
  const workspace = path.match(/^\/workspace\/(\d+)$/);
  if (workspace) {
    return { name: "workspace", id: Number(workspace[1]) };
  }
  const board = path.match(/^\/bucket\/(\d+)\/board(?:\?.*)?$/);
  if (board) {
    return { name: "board", bucketId: board[1] };
  }
  // An item route opens the owning bucket's board with the item focused.
  const item = path.match(/^\/bucket\/(\d+)\/item\/(\d+)$/);
  if (item) {
    return { name: "item", bucketId: item[1], id: item[2] };
  }
  const legacyItem = path.match(/^\/item\/(\d+)$/);
  if (legacyItem) {
    return { name: "legacy-item", legacyId: legacyItem[1] };
  }
  // An address inside connections that names nothing known opens the list.
  const connections = path.match(/^\/(?:settings|manage)\/connections\/([^?]+)$/);
  if (connections) {
    const connection = parseConnectionView(connections[1]);
    return { name: "settings", section: "connections", ...(connection ? { connection } : {}) };
  }
  const settings = path.match(/^\/(settings|manage)(?:\/([^?]*))?(?:\?(.*))?$/);
  if (settings) {
    const section = settings[2];
    const params = new URLSearchParams(settings[3] ?? "");
    const bucketId = params.get("bucket");
    const projectId = params.get("project");
    const catalog = params.get("catalog");
    return {
      name: "settings",
      section: SETTINGS_SECTIONS.includes(section as SettingsSection)
        ? section as SettingsSection
        : SETTINGS_PREFIX_DEFAULTS[settings[1]],
      ...(SETTINGS_CATALOGS.includes(catalog as SettingsCatalog)
        ? { catalog: catalog as SettingsCatalog }
        : {}),
      ...(bucketId && /^\d+$/.test(bucketId) ? { bucketId } : {}),
      ...(projectId && /^\d+$/.test(projectId) ? { projectId } : {}),
    };
  }
  return { name: "home" };
}

export function selectedSessionId(route: Route): string | null {
  return route.name === "session" ? route.id : null;
}

export function sessionHomeId(route: Route, previousId: string | null): string | null {
  return route.name === "session" ? route.id : previousId;
}

/// Restores the remembered tab on an address that names a session but
/// no tab. Returning from the Board or Settings builds a bare session
/// path, and without this it would land on the agent rather than where
/// the reader actually was.
///
/// An address that already names a tab is left alone, which is what
/// keeps the URL authoritative and the Back button working.
export function withRememberedTab(
  path: string,
  remembered: Readonly<Record<string, string>>,
): string {
  const bare = /^\/session\/(\d+)$/.exec(path);
  if (!bare) return path;
  return sessionRoutePath(bare[1], remembered[bare[1]]);
}

/// The address of a place inside a session: which tab, and whether it
/// is being read full screen.
/**
 * Where a reader is in a review, as a path.
 *
 * The stored viewer state and this URL are one position in two forms:
 * whichever the reader arrives with is written to the other, so the
 * back button, a reload and a shared link all land in the same place.
 * How they read — layout and context width — stays out, because it
 * belongs to the reader rather than to the place.
 */
export function reviewRoutePath(
  id: number | string,
  at: { view?: string; file?: string; thread?: number } = {},
): string {
  const params = new URLSearchParams();
  // The live working tree is the default, so it stays out of the URL.
  if (at.view) params.set("view", at.view);
  if (at.file) params.set("file", at.file);
  if (at.thread) params.set("thread", String(at.thread));
  const query = params.toString();
  return `/review/${id}${query ? `?${query}` : ""}`;
}

export function sessionRoutePath(
  id: string,
  tab?: string,
  focus?: boolean,
): string {
  const params = new URLSearchParams();
  // The agent tab is the default, so it stays out of the URL.
  if (tab && tab !== "agent") params.set("tab", tab);
  if (focus) params.set("focus", "1");
  const query = params.toString();
  return `/session/${id}${query ? `?${query}` : ""}`;
}
