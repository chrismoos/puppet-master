import { sessionDisplayName, sessionEnded } from "../format";
import { TerminalKind, TerminalRunState, type Session, type Terminal } from "../gen/pm/v1/pm_pb";

export type WorkspaceLayout = WorkspacePane | WorkspaceSplit;

export const SESSION_DRAG_TYPE = "application/x-puppet-master-session";
export const AGENT_TERMINAL_DRAG_TYPE = "application/x-puppet-master-agent-terminal";
export const WORKSPACE_PANE_DRAG_TYPE = "application/x-puppet-master-workspace-pane";
export type PaneDropPosition = "swap" | "replace" | "left" | "right" | "above" | "below";

export interface WorkspacePane {
  kind: "pane";
  paneId: string;
  terminalId: string | null;
}

export interface WorkspaceSplit {
  kind: "split";
  splitId: string;
  axis: "columns" | "rows";
  ratio: number;
  first: WorkspaceLayout;
  second: WorkspaceLayout;
}

export interface WorkspaceTerminalEntry {
  terminal: Terminal;
  label: string;
}

const MIN_RATIO = 0.15;
const MAX_RATIO = 0.85;
const MAX_LAYOUT_DEPTH = 16;

function terminalIsLive(terminal: Terminal): boolean {
  return terminal.state === TerminalRunState.STARTING
    || terminal.state === TerminalRunState.RUNNING;
}

export function workspaceTerminalLabel(
  terminal: Terminal,
  session: Session | undefined,
  runningCommand = "",
): string {
  const sessionName = session ? sessionDisplayName(session) : `session ${terminal.sessionId}`;
  if (terminal.kind === TerminalKind.AGENT) return `${sessionName} · agent`;
  const configuredTitle = terminal.title.trim();
  const shellName = runningCommand.trim()
    || (configuredTitle.toLowerCase() === "shell" ? `shell ${terminal.id}` : configuredTitle)
    || `shell ${terminal.id}`;
  return `${sessionName} · ${shellName}`;
}

export function workspaceTerminalEntries(
  terminals: Iterable<Terminal>,
  sessions: ReadonlyMap<string, Session>,
  runningCommands: ReadonlyMap<string, string> = new Map(),
): WorkspaceTerminalEntry[] {
  const entries: WorkspaceTerminalEntry[] = [];
  for (const terminal of terminals) {
    const session = sessions.get(terminal.sessionId.toString());
    const knownKind = terminal.kind === TerminalKind.AGENT || terminal.kind === TerminalKind.SHELL;
    if (!session || sessionEnded(session) || !terminalIsLive(terminal) || !knownKind) continue;
    entries.push({
      terminal,
      label: workspaceTerminalLabel(
        terminal,
        session,
        runningCommands.get(terminal.id.toString()),
      ),
    });
  }
  return entries.sort((first, second) => {
    if (first.terminal.id === second.terminal.id) return 0;
    return first.terminal.id < second.terminal.id ? -1 : 1;
  });
}

export function agentTerminalForSession(
  terminals: Iterable<Terminal>,
  sessionId: string,
  preferredId = "",
): Terminal | undefined {
  const matching = [...terminals].filter(
    (terminal) => terminal.sessionId.toString() === sessionId
      && terminal.kind === TerminalKind.AGENT
      && terminalIsLive(terminal),
  );
  return matching.find((terminal) => terminal.id.toString() === preferredId) ?? matching[0];
}

export function paneDropPositionForRatios(
  x: number,
  y: number,
  movingPane: boolean,
): PaneDropPosition {
  const edges: Array<[PaneDropPosition, number]> = [
    ["left", x],
    ["right", 1 - x],
    ["above", y],
    ["below", 1 - y],
  ];
  const closest = edges.sort((first, second) => first[1] - second[1])[0];
  if (closest[1] < 0.28) return closest[0];
  return movingPane && x < 0.5 ? "swap" : "replace";
}

export interface PaneDropRegion {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** Region of the hovered pane, in percentages, highlighted for a drop position. */
export function paneDropRegionRect(position: PaneDropPosition): PaneDropRegion {
  switch (position) {
    case "left": return { left: 0, top: 0, width: 50, height: 100 };
    case "right": return { left: 50, top: 0, width: 50, height: 100 };
    case "above": return { left: 0, top: 0, width: 100, height: 50 };
    case "below": return { left: 0, top: 50, width: 100, height: 50 };
    default: return { left: 0, top: 0, width: 100, height: 100 };
  }
}

export function splitPane(
  layout: WorkspaceLayout,
  paneId: string,
  axis: WorkspaceSplit["axis"],
  splitId: string,
  newPaneId: string,
): WorkspaceLayout {
  if (layout.kind === "pane") {
    if (layout.paneId !== paneId) return layout;
    return {
      kind: "split",
      splitId,
      axis,
      ratio: 0.5,
      first: layout,
      second: { kind: "pane", paneId: newPaneId, terminalId: null },
    };
  }
  return {
    ...layout,
    first: splitPane(layout.first, paneId, axis, splitId, newPaneId),
    second: splitPane(layout.second, paneId, axis, splitId, newPaneId),
  };
}

export function dropTerminal(
  layout: WorkspaceLayout,
  paneId: string,
  terminalId: string,
  position: PaneDropPosition,
  splitId: string,
  newPaneId: string,
): WorkspaceLayout {
  const withoutDuplicate = assignTerminal(layout, "", terminalId);
  if (position === "swap" || position === "replace") {
    return assignTerminal(withoutDuplicate, paneId, terminalId);
  }
  return insertPane(
    withoutDuplicate,
    paneId,
    position === "left" || position === "right" ? "columns" : "rows",
    position === "left" || position === "above",
    splitId,
    { kind: "pane", paneId: newPaneId, terminalId },
  );
}

export function movePane(
  layout: WorkspaceLayout,
  sourcePaneId: string,
  targetPaneId: string,
  position: PaneDropPosition,
  splitId: string,
): WorkspaceLayout {
  if (sourcePaneId === targetPaneId) return layout;
  const source = findPane(layout, sourcePaneId);
  const target = findPane(layout, targetPaneId);
  if (!source || !target) return layout;
  if (position === "swap") {
    return assignTerminal(
      assignTerminal(layout, sourcePaneId, target.terminalId),
      targetPaneId,
      source.terminalId,
    );
  }
  const withoutSource = closePane(layout, sourcePaneId);
  if (!withoutSource) return layout;
  if (position === "replace") {
    return assignTerminal(withoutSource, targetPaneId, source.terminalId);
  }
  return insertPane(
    withoutSource,
    targetPaneId,
    position === "left" || position === "right" ? "columns" : "rows",
    position === "left" || position === "above",
    splitId,
    source,
  );
}

function findPane(layout: WorkspaceLayout, paneId: string): WorkspacePane | null {
  if (layout.kind === "pane") return layout.paneId === paneId ? layout : null;
  return findPane(layout.first, paneId) ?? findPane(layout.second, paneId);
}

function insertPane(
  layout: WorkspaceLayout,
  paneId: string,
  axis: WorkspaceSplit["axis"],
  before: boolean,
  splitId: string,
  pane: WorkspacePane,
): WorkspaceLayout {
  if (layout.kind === "pane") {
    if (layout.paneId !== paneId) return layout;
    return {
      kind: "split",
      splitId,
      axis,
      ratio: 0.5,
      first: before ? pane : layout,
      second: before ? layout : pane,
    };
  }
  return {
    ...layout,
    first: insertPane(layout.first, paneId, axis, before, splitId, pane),
    second: insertPane(layout.second, paneId, axis, before, splitId, pane),
  };
}

export function closePane(layout: WorkspaceLayout, paneId: string): WorkspaceLayout | null {
  if (layout.kind === "pane") return layout.paneId === paneId ? null : layout;
  const first = closePane(layout.first, paneId);
  const second = closePane(layout.second, paneId);
  if (!first) return second;
  if (!second) return first;
  return { ...layout, first, second };
}

/** Removes every pane showing a closed terminal, preserving the normal empty root pane. */
export function removeTerminalFromLayout(
  layout: WorkspaceLayout,
  terminalId: string,
): WorkspaceLayout {
  const remove = (node: WorkspaceLayout): WorkspaceLayout | null => {
    if (node.kind === "pane") return node.terminalId === terminalId ? null : node;
    const first = remove(node.first);
    const second = remove(node.second);
    if (!first) return second;
    if (!second) return first;
    if (first === node.first && second === node.second) return node;
    return { ...node, first, second };
  };
  return remove(layout) ?? {
    kind: "pane",
    paneId: layout.kind === "pane" ? layout.paneId : paneIds(layout)[0],
    terminalId: null,
  };
}

export function assignTerminal(
  layout: WorkspaceLayout,
  paneId: string,
  terminalId: string | null,
): WorkspaceLayout {
  if (layout.kind === "pane") {
    if (layout.paneId === paneId) return { ...layout, terminalId };
    if (terminalId !== null && layout.terminalId === terminalId) {
      return { ...layout, terminalId: null };
    }
    return layout;
  }
  return {
    ...layout,
    first: assignTerminal(layout.first, paneId, terminalId),
    second: assignTerminal(layout.second, paneId, terminalId),
  };
}

export function setSplitRatio(
  layout: WorkspaceLayout,
  splitId: string,
  ratio: number,
): WorkspaceLayout {
  if (layout.kind === "pane") return layout;
  if (layout.splitId === splitId) {
    return { ...layout, ratio: Math.max(MIN_RATIO, Math.min(MAX_RATIO, ratio)) };
  }
  return {
    ...layout,
    first: setSplitRatio(layout.first, splitId, ratio),
    second: setSplitRatio(layout.second, splitId, ratio),
  };
}

export function paneIds(layout: WorkspaceLayout): string[] {
  return layout.kind === "pane"
    ? [layout.paneId]
    : [...paneIds(layout.first), ...paneIds(layout.second)];
}

export function parseWorkspaceLayout(value: unknown, depth = 0): WorkspaceLayout | null {
  if (!value || typeof value !== "object" || depth > MAX_LAYOUT_DEPTH) return null;
  const node = value as Record<string, unknown>;
  if (node.kind === "pane" && typeof node.paneId === "string") {
    return {
      kind: "pane",
      paneId: node.paneId,
      terminalId: typeof node.terminalId === "string" ? node.terminalId : null,
    };
  }
  if (
    node.kind !== "split" ||
    typeof node.splitId !== "string" ||
    (node.axis !== "columns" && node.axis !== "rows") ||
    typeof node.ratio !== "number"
  ) return null;
  const first = parseWorkspaceLayout(node.first, depth + 1);
  const second = parseWorkspaceLayout(node.second, depth + 1);
  if (!first || !second) return null;
  return {
    kind: "split",
    splitId: node.splitId,
    axis: node.axis,
    ratio: Math.max(MIN_RATIO, Math.min(MAX_RATIO, node.ratio)),
    first,
    second,
  };
}
