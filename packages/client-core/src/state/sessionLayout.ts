/** A session's pane layout, as the daemon stores it: the agent and the
 * panes beside it, arranged in rows and columns. */

export type LayoutNode = { pane?: string; row?: LayoutNode[]; column?: LayoutNode[]; size?: number };

export interface PaneMeta {
  id: string;
  kind: "program" | "shell" | "page" | "browser";
  title: string;
  terminal_id?: number;
  command?: string;
  page_token?: string;
  version: number;
  user_input: boolean;
  exit_code?: number;
  collapsed?: boolean;
  /** False when nobody named the pane, which then shows its running program. */
  titled?: boolean;
}

export interface SessionPaneLayout {
  rev: number;
  tree: LayoutNode;
  panes: Record<string, PaneMeta>;
  unplaced: string[];
  /** Shell tabs split into panes, by the tab's shell terminal id. */
  tabs?: Record<string, LayoutNode>;
}

/** The pane id the agent's own terminal has in every layout. */
export const AGENT_PANE = "agent";

export function parseSessionLayout(json: string | undefined): SessionPaneLayout | null {
  if (!json) return null;
  try {
    const layout = JSON.parse(json) as SessionPaneLayout;
    return layout && layout.tree ? layout : null;
  } catch {
    return null;
  }
}

/** A live session's layout before anything was ever placed: the agent alone. */
export const AGENT_ONLY_LAYOUT: SessionPaneLayout = { rev: 0, tree: { pane: AGENT_PANE }, panes: {}, unplaced: [] };

/** True when the layout shows anything besides the agent. */
export function layoutHasPanes(layout: SessionPaneLayout | null): boolean {
  return Boolean(layout && !layout.tree.pane);
}

/** Terminal ids that belong to panes, so a shell tab list leaves them out. */
export function paneTerminalIds(layout: SessionPaneLayout | null): Set<string> {
  const ids = new Set<string>();
  for (const pane of Object.values(layout?.panes ?? {})) {
    if (pane.terminal_id !== undefined) ids.add(String(pane.terminal_id));
  }
  return ids;
}

/** One stop when swiping through a session's panes on a phone. */
export type SwipePane =
  | { kind: "agent" }
  | { kind: "terminal"; paneId: string; terminalId: string; title: string }
  | { kind: "page"; paneId: string; pageToken: string; version: number; title: string };

function leaves(node: LayoutNode, out: string[]): string[] {
  if (node.pane) out.push(node.pane);
  for (const child of node.row ?? node.column ?? []) leaves(child, out);
  return out;
}

/** The panes a phone swipes through: the agent first, then every live pane
 * in the layout's reading order, left to right and top to bottom. Panes whose
 * program ended, and panes not yet placed, are left out. */
export function swipePanes(layout: SessionPaneLayout | null): SwipePane[] {
  const stops: SwipePane[] = [{ kind: "agent" }];
  if (!layout) return stops;
  for (const id of leaves(layout.tree, [])) {
    if (id === AGENT_PANE) continue;
    const meta = layout.panes[id];
    if (!meta || meta.exit_code !== undefined) continue;
    const title = meta.title || meta.command || id;
    if (meta.kind === "page") {
      if (meta.page_token) stops.push({ kind: "page", paneId: id, pageToken: meta.page_token, version: meta.version, title });
    } else if (meta.terminal_id !== undefined) {
      stops.push({ kind: "terminal", paneId: id, terminalId: String(meta.terminal_id), title });
    }
  }
  return stops;
}

/** Where a page pane's files are served, relative to the controller's origin.
 * The token in the path is the only credential it needs. */
export function pagePanePath(pageToken: string, version: number): string {
  return `pane-pages/${pageToken}/?v=${version}`;
}
