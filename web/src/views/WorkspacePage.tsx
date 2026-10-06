import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { updateWorkspace, type SavedWorkspace } from "../api/workspaces";
import { TerminalKind, TerminalRunState, type Terminal } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { navigate } from "../router";
import { useAppState, useClient } from "../state/hooks";
import { AGENT_TERMINAL_DRAG_TYPE, agentTerminalForSession, assignTerminal, closePane, dropTerminal, movePane, paneDropPositionForRatios, paneDropRegionRect, paneIds, parseWorkspaceLayout, removeTerminalFromLayout, SESSION_DRAG_TYPE, setSplitRatio, splitPane, workspaceTerminalEntries, workspaceTerminalLabel, WORKSPACE_PANE_DRAG_TYPE, type PaneDropPosition, type WorkspaceLayout, type WorkspacePane, type WorkspaceSplit, type WorkspaceTerminalEntry } from "@puppet-master/client-core/state/workspace";
import type { TerminalStage } from "../ws/terminal";
import { WorkspaceTerminal } from "../ws/workspaceTerminal";
import type { TerminalStreamStatus } from "@puppet-master/client-core/ws/terminalSocket";
import type { RendererIssue } from "../ws/webglBudget";
import { useTerminalThemeController } from "../theme/context";

const SAVE_DELAY_MS = 350;
const MAX_TRACKED_SAVED_LAYOUTS = 8;

function nodeId(prefix: string): string {
  return `${prefix}-${crypto.randomUUID()}`;
}

/** Adopts a workspace-record layout unless it is an echo of a local save that newer local edits have already outpaced. */
export function shouldAdoptIncomingLayout(
  sameWorkspace: boolean,
  incomingJson: string,
  currentJson: string,
  recentlySavedJsons: readonly string[],
): boolean {
  if (!sameWorkspace) return true;
  return incomingJson !== currentJson && !recentlySavedJsons.includes(incomingJson);
}

export function togglePaneZoomFromHeader(target: { closest: (selectors: string) => Element | null }, paneId: string, onToggleZoom: (paneId: string) => void): void {
  if (target.closest("button, select, .workspace-renderer-warning")) return;
  onToggleZoom(paneId);
}

const DRAG_GHOST_LABEL_MAX = 16;

export function paneDragGhostLabel(label: string): string {
  const trimmed = label.trim();
  if (trimmed.length <= DRAG_GHOST_LABEL_MAX) return trimmed;
  return `${trimmed.slice(0, DRAG_GHOST_LABEL_MAX - 1).trimEnd()}…`;
}

function createPaneDragGhost(label: string): HTMLElement {
  const ghost = document.createElement("div");
  ghost.className = "pane-drag-ghost";
  const icon = document.createElement("span");
  icon.className = "pane-drag-ghost-icon";
  icon.textContent = ">_";
  const text = document.createElement("span");
  text.className = "pane-drag-ghost-label";
  text.textContent = paneDragGhostLabel(label);
  ghost.append(icon, text);
  document.body.append(ghost);
  return ghost;
}

function TerminalViewport({ terminal, focused, active, onIssue, onCommand }: { terminal: Terminal; focused: boolean; active: boolean; onIssue: (reason: RendererIssue | null) => void; onCommand: (terminalId: bigint, command: string) => void }) {
  const client = useClient();
  const themeController = useTerminalThemeController();
  const hostRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<WorkspaceTerminal | null>(null);
  const [streamStatus, setStreamStatus] = useState<TerminalStreamStatus | null>(null);
  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    onIssue(null);
    const view = new WorkspaceTerminal(client, host, terminal.id, themeController, onIssue, setStreamStatus, (command) => onCommand(terminal.id, command));
    viewRef.current = view;
    view.setActive(active);
    return () => {
      viewRef.current = null;
      view.dispose();
      onIssue(null);
      setStreamStatus(null);
    };
  }, [client, onCommand, onIssue, terminal.id, themeController]);
  useEffect(() => viewRef.current?.setActive(active), [active]);
  useEffect(() => {
    if (focused && active) viewRef.current?.focus();
  }, [active, focused]);
  return (
    <div className="workspace-terminal-wrap">
      <div className="workspace-terminal-host" ref={hostRef} />
      {streamStatus?.phase === "reconnecting" && (
        <div className="terminal-stream-status" role="status">
          <span>{streamStatus.lastError ?? "reconnecting…"}</span>
          {streamStatus.canRetry && <button type="button" onClick={() => viewRef.current?.retry()}>retry</button>}
        </div>
      )}
    </div>
  );
}

export function WorkspacePage({
  workspace,
  stage,
  onUpdated,
  addTerminalId,
  onTerminalAdded,
  active = true,
}: {
  workspace: SavedWorkspace;
  stage: TerminalStage;
  onUpdated: (workspace: SavedWorkspace) => void;
  addTerminalId?: string;
  onTerminalAdded: () => void;
  active?: boolean;
}) {
  const state = useAppState();
  const parsed = useMemo(() => parseWorkspaceLayout(workspace.layout), [workspace.id, workspace.layout]);
  const [layout, setLayout] = useState<WorkspaceLayout>(() => parsed ?? { kind: "pane", paneId: nodeId("pane"), terminalId: null });
  const [focusedPane, setFocusedPane] = useState(() => paneIds(layout)[0]);
  const [zoomed, setZoomed] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [liveCommands, setLiveCommands] = useState<ReadonlyMap<string, string>>(new Map());
  const skipSave = useRef(true);
  const workspaceRef = useRef(workspace);
  const layoutRef = useRef(layout);
  const workspaceIdRef = useRef(workspace.id);
  const recentlySavedLayouts = useRef<string[]>([]);
  workspaceRef.current = workspace;
  layoutRef.current = layout;

  useEffect(() => {
    const next = parseWorkspaceLayout(workspace.layout) ?? { kind: "pane" as const, paneId: nodeId("pane"), terminalId: null };
    const sameWorkspace = workspaceIdRef.current === workspace.id;
    if (!sameWorkspace) recentlySavedLayouts.current = [];
    if (!shouldAdoptIncomingLayout(
      sameWorkspace,
      JSON.stringify(next),
      JSON.stringify(layoutRef.current),
      recentlySavedLayouts.current,
    )) return;
    workspaceIdRef.current = workspace.id;
    skipSave.current = true;
    setLayout(next);
    setFocusedPane(paneIds(next)[0]);
    setZoomed(false);
  }, [workspace.id, workspace.layout]);

  useEffect(() => {
    if (!addTerminalId) return;
    const existing = findTerminalPane(layout, addTerminalId);
    if (existing) {
      setFocusedPane(existing);
      onTerminalAdded();
      return;
    }
    const newPaneId = nodeId("pane");
    const split = splitPane(layout, focusedPane, "columns", nodeId("split"), newPaneId);
    setLayout(assignTerminal(split, newPaneId, addTerminalId));
    setFocusedPane(newPaneId);
    onTerminalAdded();
  }, [addTerminalId]);

  useEffect(() => {
    if (skipSave.current) {
      skipSave.current = false;
      return;
    }
    const timer = setTimeout(() => {
      // Reparsing canonicalizes the JSON so the server's echo of this save compares equal.
      recentlySavedLayouts.current.push(JSON.stringify(parseWorkspaceLayout(layout) ?? layout));
      if (recentlySavedLayouts.current.length > MAX_TRACKED_SAVED_LAYOUTS) recentlySavedLayouts.current.shift();
      void updateWorkspace({ ...workspaceRef.current, layout })
        .then(onUpdated)
        .catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason)));
    }, SAVE_DELAY_MS);
    return () => clearTimeout(timer);
  }, [layout, onUpdated]);

  useEffect(() => stage.onTerminalCommand((terminalId, command) => {
    setLiveCommands((current) => updateLiveCommand(current, terminalId, command));
  }), [stage]);

  const previousTerminals = useRef(state.terminals);
  useEffect(() => {
    const previous = previousTerminals.current;
    previousTerminals.current = state.terminals;
    const removedShellIds = [...previous]
      .filter(([id, terminal]) => terminal.kind === TerminalKind.SHELL && !state.terminals.has(id))
      .map(([id]) => id);
    if (removedShellIds.length === 0) return;
    const next = removedShellIds.reduce(removeTerminalFromLayout, layoutRef.current);
    if (next === layoutRef.current) return;
    setLayout(next);
    const remainingPanes = paneIds(next);
    if (!remainingPanes.includes(focusedPane)) {
      setFocusedPane(remainingPanes[0]);
      setZoomed(false);
    }
  }, [focusedPane, state.terminals]);

  const terminalCommands = useMemo(() => {
    const commands = new Map(liveCommands);
    for (const terminal of state.terminals.values()) {
      const command = stage.terminalCommand(terminal.id);
      if (command && !commands.has(terminal.id.toString())) {
        commands.set(terminal.id.toString(), command);
      }
    }
    return commands;
  }, [liveCommands, stage, state.terminals]);
  const terminalEntries = useMemo(
    () => workspaceTerminalEntries(state.terminals.values(), state.sessions, terminalCommands),
    [state.sessions, state.terminals, terminalCommands],
  );
  const terminals = useMemo(
    () => terminalEntries.map((entry) => entry.terminal),
    [terminalEntries],
  );
  const onTerminalCommand = useCallback((terminalId: bigint, command: string) => {
    setLiveCommands((current) => updateLiveCommand(current, terminalId, command));
  }, []);

  const mutate = (next: WorkspaceLayout | null) => {
    if (!next) return;
    setLayout(next);
    const ids = paneIds(next);
    if (!ids.includes(focusedPane)) setFocusedPane(ids[0]);
  };

  const dropSession = (paneId: string, sessionId: string, agentTerminalId: string, position: PaneDropPosition) => {
    const agent = agentTerminalForSession(terminals, sessionId, agentTerminalId);
    if (!agent) {
      setError(`session ${sessionId} has no agent terminal`);
      return;
    }
    const newPaneId = nodeId("pane");
    setLayout(dropTerminal(layout, paneId, agent.id.toString(), position, nodeId("split"), newPaneId));
    setFocusedPane(position === "replace" ? paneId : newPaneId);
  };

  const dropPane = (targetPaneId: string, sourcePaneId: string, position: PaneDropPosition) => {
    setLayout(movePane(layout, sourcePaneId, targetPaneId, position, nodeId("split")));
    setFocusedPane(position === "replace" ? targetPaneId : sourcePaneId);
  };

  return (
    <section className={`workspace-page ${zoomed ? "is-zoomed" : ""}`}>
      {error && <div className="flash-error" role="alert">{error}</div>}
      <div className="workspace-layout">
        <LayoutNode
          node={layout}
          terminalEntries={terminalEntries}
          terminalById={state.terminals}
          focusedPane={focusedPane}
          zoomedPane={zoomed ? focusedPane : null}
          onFocus={setFocusedPane}
          onToggleZoom={(paneId) => {
            setFocusedPane(paneId);
            setZoomed((value) => !value);
          }}
          onChange={mutate}
          root={layout}
          sessions={state.sessions}
          onDropSession={dropSession}
          onDropPane={dropPane}
          onTerminalCommand={onTerminalCommand}
          active={active}
        />
      </div>
    </section>
  );
}

function updateLiveCommand(current: ReadonlyMap<string, string>, terminalId: bigint, command: string): ReadonlyMap<string, string> {
  const next = new Map(current);
  const normalized = command.trim();
  if (normalized) next.set(terminalId.toString(), normalized);
  else next.delete(terminalId.toString());
  return next;
}

function findTerminalPane(layout: WorkspaceLayout, terminalId: string): string | null {
  if (layout.kind === "pane") return layout.terminalId === terminalId ? layout.paneId : null;
  return findTerminalPane(layout.first, terminalId) ?? findTerminalPane(layout.second, terminalId);
}

function LayoutNode({
  node,
  root,
  terminalEntries,
  terminalById,
  sessions,
  focusedPane,
  zoomedPane,
  onFocus,
  onToggleZoom,
  onChange,
  onDropSession,
  onDropPane,
  onTerminalCommand,
  active,
}: {
  node: WorkspaceLayout;
  root: WorkspaceLayout;
  terminalEntries: WorkspaceTerminalEntry[];
  terminalById: ReadonlyMap<string, Terminal>;
  sessions: ReturnType<typeof useAppState>["sessions"];
  focusedPane: string;
  zoomedPane: string | null;
  onFocus: (id: string) => void;
  onToggleZoom: (id: string) => void;
  onChange: (layout: WorkspaceLayout | null) => void;
  onDropSession: (paneId: string, sessionId: string, agentTerminalId: string, position: PaneDropPosition) => void;
  onDropPane: (targetPaneId: string, sourcePaneId: string, position: PaneDropPosition) => void;
  onTerminalCommand: (terminalId: bigint, command: string) => void;
  active: boolean;
}) {
  if (zoomedPane && node.kind === "pane" && node.paneId !== zoomedPane) return null;
  if (node.kind === "pane") {
    return <WorkspacePaneView pane={node} root={root} terminalEntries={terminalEntries} terminalById={terminalById} sessions={sessions} focused={node.paneId === focusedPane} active={active} onFocus={onFocus} onToggleZoom={onToggleZoom} onChange={onChange} onDropSession={onDropSession} onDropPane={onDropPane} onTerminalCommand={onTerminalCommand} />;
  }
  if (zoomedPane) {
    return (
      <>
        <LayoutNode node={node.first} root={root} terminalEntries={terminalEntries} terminalById={terminalById} sessions={sessions} focusedPane={focusedPane} zoomedPane={zoomedPane} onFocus={onFocus} onToggleZoom={onToggleZoom} onChange={onChange} onDropSession={onDropSession} onDropPane={onDropPane} onTerminalCommand={onTerminalCommand} active={active} />
        <LayoutNode node={node.second} root={root} terminalEntries={terminalEntries} terminalById={terminalById} sessions={sessions} focusedPane={focusedPane} zoomedPane={zoomedPane} onFocus={onFocus} onToggleZoom={onToggleZoom} onChange={onChange} onDropSession={onDropSession} onDropPane={onDropPane} onTerminalCommand={onTerminalCommand} active={active} />
      </>
    );
  }
  return <SplitView split={node} root={root} terminalEntries={terminalEntries} terminalById={terminalById} sessions={sessions} focusedPane={focusedPane} onFocus={onFocus} onToggleZoom={onToggleZoom} onChange={onChange} onDropSession={onDropSession} onDropPane={onDropPane} onTerminalCommand={onTerminalCommand} active={active} />;
}

function SplitView({ split, root, terminalEntries, terminalById, sessions, focusedPane, onFocus, onToggleZoom, onChange, onDropSession, onDropPane, onTerminalCommand, active }: { split: WorkspaceSplit; root: WorkspaceLayout; terminalEntries: WorkspaceTerminalEntry[]; terminalById: ReadonlyMap<string, Terminal>; sessions: ReturnType<typeof useAppState>["sessions"]; focusedPane: string; onFocus: (id: string) => void; onToggleZoom: (id: string) => void; onChange: (layout: WorkspaceLayout | null) => void; onDropSession: (paneId: string, sessionId: string, agentTerminalId: string, position: PaneDropPosition) => void; onDropPane: (targetPaneId: string, sourcePaneId: string, position: PaneDropPosition) => void; onTerminalCommand: (terminalId: bigint, command: string) => void; active: boolean }) {
  const hostRef = useRef<HTMLDivElement>(null);
  const [resizing, setResizing] = useState(false);
  const startResize = (event: React.PointerEvent) => {
    event.preventDefault();
    const host = hostRef.current;
    if (!host) return;
    setResizing(true);
    let frame = 0;
    let latestRatio = split.ratio;
    const move = (pointer: PointerEvent) => {
      const bounds = host.getBoundingClientRect();
      latestRatio = split.axis === "columns"
        ? (pointer.clientX - bounds.left) / bounds.width
        : (pointer.clientY - bounds.top) / bounds.height;
      if (frame) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        onChange(setSplitRatio(root, split.splitId, latestRatio));
      });
    };
    const stop = () => {
      if (frame) {
        cancelAnimationFrame(frame);
        frame = 0;
        onChange(setSplitRatio(root, split.splitId, latestRatio));
      }
      requestAnimationFrame(() => setResizing(false));
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", stop);
      window.removeEventListener("pointercancel", stop);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", stop);
    window.addEventListener("pointercancel", stop);
  };
  const firstStyle = { flex: `${split.ratio} 1 0` };
  const secondStyle = { flex: `${1 - split.ratio} 1 0` };
  return (
    <div className={`workspace-split split-${split.axis} ${resizing ? "is-resizing" : ""}`} ref={hostRef}>
      <div className="workspace-split-child" style={firstStyle}><LayoutNode node={split.first} root={root} terminalEntries={terminalEntries} terminalById={terminalById} sessions={sessions} focusedPane={focusedPane} zoomedPane={null} onFocus={onFocus} onToggleZoom={onToggleZoom} onChange={onChange} onDropSession={onDropSession} onDropPane={onDropPane} onTerminalCommand={onTerminalCommand} active={active} /></div>
      <div className="workspace-divider" role="separator" aria-orientation={split.axis === "columns" ? "vertical" : "horizontal"} onPointerDown={startResize} />
      <div className="workspace-split-child" style={secondStyle}><LayoutNode node={split.second} root={root} terminalEntries={terminalEntries} terminalById={terminalById} sessions={sessions} focusedPane={focusedPane} zoomedPane={null} onFocus={onFocus} onToggleZoom={onToggleZoom} onChange={onChange} onDropSession={onDropSession} onDropPane={onDropPane} onTerminalCommand={onTerminalCommand} active={active} /></div>
    </div>
  );
}

function WorkspacePaneView({ pane, root, terminalEntries, terminalById, sessions, focused, active, onFocus, onToggleZoom, onChange, onDropSession, onDropPane, onTerminalCommand }: { pane: WorkspacePane; root: WorkspaceLayout; terminalEntries: WorkspaceTerminalEntry[]; terminalById: ReadonlyMap<string, Terminal>; sessions: ReturnType<typeof useAppState>["sessions"]; focused: boolean; active: boolean; onFocus: (id: string) => void; onToggleZoom: (id: string) => void; onChange: (layout: WorkspaceLayout | null) => void; onDropSession: (paneId: string, sessionId: string, agentTerminalId: string, position: PaneDropPosition) => void; onDropPane: (targetPaneId: string, sourcePaneId: string, position: PaneDropPosition) => void; onTerminalCommand: (terminalId: bigint, command: string) => void }) {
  const [dropPosition, setDropPosition] = useState<PaneDropPosition | null>(null);
  const [dragSource, setDragSource] = useState(false);
  const [rendererIssue, setRendererIssue] = useState<RendererIssue | null>(null);
  const dragGhost = useRef<HTMLElement | null>(null);
  useEffect(() => () => dragGhost.current?.remove(), []);
  const terminal = pane.terminalId ? terminalById.get(pane.terminalId) : undefined;
  const session = terminal ? sessions.get(terminal.sessionId.toString()) : undefined;
  const terminalLive = terminal?.state === TerminalRunState.RUNNING || terminal?.state === TerminalRunState.STARTING;
  return (
    <article
      className={`workspace-pane ${focused ? "is-focused" : ""} ${dropPosition ? "is-drop-target" : ""} ${dragSource ? "is-drag-source" : ""}`}
      onPointerDown={() => onFocus(pane.paneId)}
      onDragOver={(event) => {
        if (!event.dataTransfer.types.includes(SESSION_DRAG_TYPE) && !event.dataTransfer.types.includes(WORKSPACE_PANE_DRAG_TYPE)) return;
        event.preventDefault();
        const paneMove = event.dataTransfer.types.includes(WORKSPACE_PANE_DRAG_TYPE);
        event.dataTransfer.dropEffect = paneMove ? "move" : "copy";
        setDropPosition(paneDropPosition(event, paneMove));
      }}
      onDragLeave={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
          setDropPosition(null);
        }
      }}
      onDrop={(event) => {
        const sourcePaneId = event.dataTransfer.getData(WORKSPACE_PANE_DRAG_TYPE);
        const sessionId = event.dataTransfer.getData(SESSION_DRAG_TYPE);
        if (!sourcePaneId && !sessionId) return;
        event.preventDefault();
        const paneMove = Boolean(sourcePaneId);
        const finalPosition = paneDropPosition(event, paneMove);
        if (sourcePaneId) onDropPane(pane.paneId, sourcePaneId, finalPosition);
        else onDropSession(pane.paneId, sessionId, event.dataTransfer.getData(AGENT_TERMINAL_DRAG_TYPE), finalPosition);
        setDropPosition(null);
      }}
    >
      <header
        className="workspace-pane-head"
        draggable={Boolean(terminal)}
        title={terminal ? "Drag to move pane. Double-click empty space to zoom." : "Double-click empty space to zoom."}
        onDoubleClick={(event) => togglePaneZoomFromHeader(event.target as HTMLElement, pane.paneId, onToggleZoom)}
        onDragStart={(event) => {
          if (!terminal || (event.target as HTMLElement).closest("button, select")) {
            event.preventDefault();
            return;
          }
          const label = workspaceTerminalLabel(terminal, session);
          event.dataTransfer.effectAllowed = "move";
          event.dataTransfer.setData(WORKSPACE_PANE_DRAG_TYPE, pane.paneId);
          event.dataTransfer.setData("text/plain", label);
          dragGhost.current?.remove();
          dragGhost.current = createPaneDragGhost(label);
          event.dataTransfer.setDragImage(dragGhost.current, 12, 12);
          setDragSource(true);
        }}
        onDragEnd={() => {
          dragGhost.current?.remove();
          dragGhost.current = null;
          setDragSource(false);
        }}
      >
        <select value={pane.terminalId ?? ""} onChange={(event) => onChange(assignTerminal(root, pane.paneId, event.target.value || null))} aria-label="terminal shown in pane">
          <option value="">choose terminal…</option>
          {terminalEntries.map((entry) => <option key={entry.terminal.id.toString()} value={entry.terminal.id.toString()}>{entry.label}</option>)}
        </select>
        <div className="topbar-spacer" />
        {rendererIssue && (
          <span className="workspace-renderer-warning" title={rendererIssue === "budget" ? "GPU terminal limit reached. Waiting for a GPU renderer." : "GPU rendering is recovering."}>ⓘ GPU</span>
        )}
        {session && <button type="button" className="workspace-pane-action" onClick={() => navigate(`/session/${session.id}`)}>⌂ home</button>}
        <button type="button" className="workspace-pane-action" title="split right" onClick={() => onChange(splitPane(root, pane.paneId, "columns", nodeId("split"), nodeId("pane")))}>＋ right</button>
        <button type="button" className="workspace-pane-action" title="split below" onClick={() => onChange(splitPane(root, pane.paneId, "rows", nodeId("split"), nodeId("pane")))}>＋ below</button>
        <button type="button" className="workspace-pane-action danger" title="close pane" disabled={paneIds(root).length === 1} onClick={() => onChange(closePane(root, pane.paneId))}>×</button>
      </header>
      {terminal && terminalLive ? <TerminalViewport terminal={terminal} focused={focused} active={active} onIssue={setRendererIssue} onCommand={onTerminalCommand} /> : <div className="workspace-empty-pane"><span>{terminal ? "Terminal is not running" : "Choose any terminal"}</span><small>{terminal ? "Its place in this layout is preserved." : "Live agents and shells from every session are available here."}</small></div>}
      {dropPosition && <PaneDropOverlay active={dropPosition} />}
    </article>
  );
}

function paneDropPosition(event: React.DragEvent<HTMLElement>, movingPane: boolean): PaneDropPosition {
  const bounds = event.currentTarget.getBoundingClientRect();
  const x = (event.clientX - bounds.left) / bounds.width;
  const y = (event.clientY - bounds.top) / bounds.height;
  return paneDropPositionForRatios(x, y, movingPane);
}

function PaneDropOverlay({ active }: { active: PaneDropPosition }) {
  const region = paneDropRegionRect(active);
  return (
    <div className="pane-drop-overlay" aria-hidden="true">
      <div
        className={`pane-drop-region drop-${active}`}
        style={{
          left: `${region.left}%`,
          top: `${region.top}%`,
          width: `${region.width}%`,
          height: `${region.height}%`,
        }}
      />
    </div>
  );
}
