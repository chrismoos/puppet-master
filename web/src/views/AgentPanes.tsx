import { useCallback, useEffect, useLayoutEffect, useRef, useState, type RefObject } from "react";
import { createPortal, flushSync } from "react-dom";
import { authedFetch } from "../api/token";
import { useAppState } from "../state/hooks";
import { sessionDisplayName } from "@puppet-master/client-core/format";
import type { TerminalStage } from "../ws/terminal";
import { ConfirmDialog } from "./ConfirmDialog";
import { TerminalViewport } from "./WorkspacePage";
import { paneDropRegionRect } from "@puppet-master/client-core/state/workspace";
import { insertPaneInTree, movePaneInTree, movePositionAt, type LayoutNode, type MovePosition } from "@puppet-master/client-core/state/paneMoves";
import { AGENT_PANE as AGENT, type PaneMeta, type SessionPaneLayout } from "@puppet-master/client-core/state/sessionLayout";

export {
  AGENT_ONLY_LAYOUT,
  layoutHasPanes,
  paneTerminalIds,
  parseSessionLayout,
  type SessionPaneLayout,
} from "@puppet-master/client-core/state/sessionLayout";

/** The root of a shell tab's own layout: the tab's shell. */
const TAB = "tab";

/** A dev-server page pane's own forward, which its pane already shows. */
export function isPaneForward(slug: string, sessionId: string): boolean {
  return slug.startsWith(`pane-${sessionId}-`);
}

async function paneAction(sessionId: string, pane: string, action: string): Promise<void> {
  const response = await authedFetch(`/api/sessions/${sessionId}/panes/${encodeURIComponent(pane)}/${action}`, { method: "POST" });
  if (!response.ok) {
    const body = (await response.json().catch(() => ({}))) as { error?: string };
    throw new Error(body.error ?? `could not ${action} the pane`);
  }
}

function Icon({ name }: { name: "keyboard" | "keyboard-off" | "stop" | "hide" | "reload" | "open" }) {
  const common = { viewBox: "0 0 16 16", width: 14, height: 14, fill: "none", stroke: "currentColor", strokeWidth: 1.4, strokeLinecap: "round" as const, strokeLinejoin: "round" as const, "aria-hidden": true };
  switch (name) {
    case "keyboard":
    case "keyboard-off":
      return (
        <svg {...common}>
          <rect x="1.5" y="4" width="13" height="8" rx="1.5" />
          <path d="M4 6.5h.01M6.3 6.5h.01M8.6 6.5h.01M10.9 6.5h.01M5 9.5h6" />
          {name === "keyboard-off" && <path d="M2 2l12 12" />}
        </svg>
      );
    case "stop":
      return (
        <svg {...common}>
          <rect x="4" y="4" width="8" height="8" rx="1.2" fill="currentColor" stroke="none" />
        </svg>
      );
    case "hide":
      return (
        <svg {...common}>
          <path d="M1.5 8s2.4-4.5 6.5-4.5S14.5 8 14.5 8s-2.4 4.5-6.5 4.5S1.5 8 1.5 8z" />
          <circle cx="8" cy="8" r="1.8" />
          <path d="M2.5 13.5l11-11" />
        </svg>
      );
    case "reload":
      return (
        <svg {...common}>
          <path d="M13 8a5 5 0 1 1-1.5-3.6" />
          <path d="M13 2.5v3h-3" />
        </svg>
      );
    case "open":
      return (
        <svg {...common}>
          <path d="M9 2.5h4.5V7M13.5 2.5 7.5 8.5" />
          <path d="M12 9.5v3.5a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1h3.5" />
        </svg>
      );
  }
}

/** Chips for panes the user hid, in the session's tab strip. */
export function HiddenPaneTray({ sessionId, layout }: { sessionId: string; layout: SessionPaneLayout | null }) {
  if (!layout || layout.unplaced.length === 0) return null;
  return (
    <>
      {layout.unplaced.map((id) => (
        <button
          type="button"
          key={id}
          className="btn ap-hidden-chip"
          title={`Show ${layout.panes[id]?.title ?? id} again`}
          onClick={() => void paneAction(sessionId, id, "restore")}
        >
          ↺ {layout.panes[id]?.title ?? id}
        </button>
      ))}
    </>
  );
}

function children(node: LayoutNode): { axis: "row" | "column"; items: LayoutNode[] } | null {
  if (node.row) return { axis: "row", items: node.row };
  if (node.column) return { axis: "column", items: node.column };
  return null;
}

function withSizes(node: LayoutNode, path: number[], sizes: number[]): LayoutNode {
  const split = children(node);
  if (!split) return node;
  if (path.length === 0) {
    const items = split.items.map((item, index) => ({ ...item, size: sizes[index] }));
    return split.axis === "row" ? { ...node, row: items } : { ...node, column: items };
  }
  const [head, ...rest] = path;
  const items = split.items.map((item, index) => (index === head ? withSizes(item, rest, sizes) : item));
  return split.axis === "row" ? { ...node, row: items } : { ...node, column: items };
}

function stripSizes(node: LayoutNode): LayoutNode {
  const split = children(node);
  if (!split) return { pane: node.pane };
  const items = split.items.map(stripSizes);
  return split.axis === "row" ? { row: items } : { column: items };
}

const HEAD_PX = 24;
const GUTTER_PX = 1;
/** How far the pointer travels on a pane header before a press becomes a move. */
const MOVE_THRESHOLD_PX = 4;
/** Where the pointer holds the move ghost, as the workspace drag image does. */
const GHOST_OFFSET_PX = 12;

interface MoveDrag {
  id: string;
  title: string;
  x: number;
  y: number;
  target: string | null;
  position: MovePosition | null;
}

export interface Rect { x: number; y: number; w: number; h: number }
export interface Placed { rect: Rect; axis: "row" | "column" | null }
export interface Gutter {
  key: string;
  axis: "row" | "column";
  rect: Rect;
  path: number[];
  index: number;
  shares: number[];
  lengths: number[];
}

function withCollapsed(layout: SessionPaneLayout, collapsed: Record<string, boolean>): SessionPaneLayout {
  const ids = Object.keys(collapsed).filter((id) => layout.panes[id] && (layout.panes[id].collapsed === true) !== collapsed[id]);
  if (ids.length === 0) return layout;
  const panes = { ...layout.panes };
  for (const id of ids) panes[id] = { ...panes[id], collapsed: collapsed[id] };
  return { ...layout, panes };
}

function isCollapsed(node: LayoutNode, layout: SessionPaneLayout): boolean {
  return Boolean(node.pane && node.pane !== AGENT && layout.panes[node.pane]?.collapsed);
}

/** Lays the tree out in pixels: a collapsed pane keeps only its header along
 * its parent's axis, and the rest share what remains by their sizes. */
export function measure(
  node: LayoutNode,
  rect: Rect,
  axis: "row" | "column" | null,
  path: number[],
  layout: SessionPaneLayout,
  panes: Map<string, Placed>,
  gutters: Gutter[],
): void {
  const split = children(node);
  if (!split) {
    if (node.pane) panes.set(node.pane, { rect, axis });
    return;
  }
  const horizontal = split.axis === "row";
  const length = (horizontal ? rect.w : rect.h) - GUTTER_PX * (split.items.length - 1);
  const shares = split.items.map((item) => item.size ?? 100 / split.items.length);
  const collapsed = split.items.map((item) => isCollapsed(item, layout));
  const fixed = collapsed.filter(Boolean).length * HEAD_PX;
  const flexShare = shares.reduce((sum, share, index) => (collapsed[index] ? sum : sum + share), 0) || 1;
  const lengths = split.items.map((_, index) =>
    collapsed[index] ? HEAD_PX : Math.max(0, ((length - fixed) * shares[index]) / flexShare),
  );
  let offset = horizontal ? rect.x : rect.y;
  split.items.forEach((item, index) => {
    if (index > 0) {
      const gutterRect = horizontal
        ? { x: offset, y: rect.y, w: GUTTER_PX, h: rect.h }
        : { x: rect.x, y: offset, w: rect.w, h: GUTTER_PX };
      if (!collapsed[index - 1] && !collapsed[index]) {
        gutters.push({ key: `${path.join(".")}:${index}`, axis: split.axis, rect: gutterRect, path, index: index - 1, shares, lengths });
      }
      offset += GUTTER_PX;
    }
    const size = lengths[index];
    const childRect = horizontal
      ? { x: offset, y: rect.y, w: size, h: rect.h }
      : { x: rect.x, y: offset, w: rect.w, h: size };
    measure(item, childRect, split.axis, [...path, index], layout, panes, gutters);
    offset += size;
  });
}

function rectStyle(rect: Rect): React.CSSProperties {
  return { left: rect.x, top: rect.y, width: rect.w, height: rect.h };
}

/**
 * The agent's split layout over the session view. Every pane is absolutely
 * placed from a rectangle computed from the tree and rendered in a fixed order
 * keyed by its id, so a layout change only moves elements and never remounts a
 * terminal or reloads a page. The stage host is placed onto the agent's
 * rectangle the same way.
 */
export function AgentPanes({
  sessionId,
  layout: servedLayout,
  hostRef,
  active,
  stage,
  tab,
}: {
  sessionId: string;
  layout: SessionPaneLayout;
  hostRef: RefObject<HTMLDivElement | null>;
  stage: TerminalStage;
  /** Lays out this shell tab, rooted at its shell, instead of the agent's view. */
  tab?: string;
  /** Shown and placing the agent's terminal. An inactive layout stays mounted
   * and connected but hidden, so returning to its session shows it at once. */
  active: boolean;
}) {
  const state = useAppState();
  const rootRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState<{ w: number; h: number } | null>(null);
  const [draft, setDraft] = useState<LayoutNode | null>(null);
  const rootId = tab ? TAB : AGENT;
  const treeOf = (of: SessionPaneLayout): LayoutNode => (tab ? of.tabs?.[tab] ?? { pane: TAB } : of.tree);
  const tree = draft ?? treeOf(servedLayout);
  // Collapse and expand apply locally on click rather than after the daemon answers.
  const [collapseDraft, setCollapseDraft] = useState<Record<string, boolean>>({});
  useEffect(() => {
    setDraft(null);
    setCollapseDraft({});
  }, [servedLayout.rev]);
  const layout = withCollapsed(servedLayout, collapseDraft);
  const layerKey = tab ? `t:${tab}` : `s:${sessionId}`;

  const agentRectIf = (pane: string, collapsed: boolean) => {
    const root = rootRef.current;
    if (!root) return undefined;
    const placed = new Map<string, Placed>();
    measure(tree, { x: 0, y: 0, w: root.clientWidth, h: root.clientHeight }, null, [], withCollapsed(layout, { [pane]: collapsed }), placed, []);
    return placed.get(rootId)?.rect;
  };
  const pressCollapse = (pane: string, collapsed: boolean) => {
    const rect = agentRectIf(pane, collapsed);
    if (rect) stage.anticipate(layerKey, rect.w, rect.h);
  };
  const toggleCollapse = (pane: string, collapsed: boolean) => {
    setCollapseDraft((current) => ({ ...current, [pane]: collapsed }));
    paneAction(sessionId, pane, collapsed ? "collapse" : "expand").catch(() => {
      setCollapseDraft((current) => {
        const { [pane]: _reverted, ...rest } = current;
        return rest;
      });
    });
  };
  const abandonPress = () => stage.cancelAnticipation(layerKey);

  const [moving, setMoving] = useState<MoveDrag | null>(null);
  const [menu, setMenu] = useState<{ pane: string; x: number; y: number } | null>(null);
  const openMenu = (pane: string) => (event: { clientX: number; clientY: number; preventDefault: () => void }) => {
    event.preventDefault();
    setMenu({ pane, x: event.clientX, y: event.clientY });
  };
  const openShell = (target: string, position: Exclude<MovePosition, "swap">) => {
    const current = latest.current;
    let n = 1;
    while (current.panes[`shell-${n}`]) n += 1;
    const id = `shell-${n}`;
    void authedFetch(`/api/sessions/${sessionId}/panes`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ id, base_rev: current.rev, layout: insertPaneInTree(treeOf(current), id, target, position), tab }),
    });
  };
  // The agent's terminal lives outside this tree, so its right-click is
  // caught on the host. A program reading the mouse keeps its own clicks.
  const openMenuRef = useRef(openMenu);
  openMenuRef.current = openMenu;
  useEffect(() => {
    const host = hostRef.current;
    if (!host || !active) return;
    const onMenu = (event: MouseEvent) => {
      const modified = event.shiftKey || event.altKey || event.ctrlKey || event.metaKey;
      if (!modified && stage.reportsMouse(layerKey)) return;
      openMenuRef.current(rootId)(event);
    };
    host.addEventListener("contextmenu", onMenu);
    return () => host.removeEventListener("contextmenu", onMenu);
  }, [hostRef, active, stage, layerKey, rootId]);
  // Zooming is a view of this browser only, never saved to the layout.
  const [zoomed, setZoomed] = useState<string | null>(null);
  const toggleZoom = (pane: string) => setZoomed((current) => (current === pane ? null : pane));
  useEffect(() => {
    if (!zoomed) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setZoomed(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [zoomed]);

  // Measured again whenever the layout becomes active: its area can differ
  // from the session shown while it was hidden, such as one without a
  // forwards bar, and the agent's terminal must be placed at the real size
  // before it is shown or it claims a stale one and redraws.
  useLayoutEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    const read = () =>
      setSize((current) =>
        current && current.w === root.clientWidth && current.h === root.clientHeight
          ? current
          : { w: root.clientWidth, h: root.clientHeight },
      );
    read();
    // Laid out inside the observer callback, so panes move in the same frame
    // as the area that changed size instead of the frame after.
    const observer = new ResizeObserver(() => flushSync(read));
    observer.observe(root);
    return () => observer.disconnect();
  }, [active]);

  const panes = new Map<string, Placed>();
  const gutters: Gutter[] = [];
  if (size) measure(tree, { x: 0, y: 0, w: size.w, h: size.h }, null, [], layout, panes, gutters);
  const zoomedPane = zoomed && panes.has(zoomed) && !layout.panes[zoomed]?.collapsed ? zoomed : null;
  const agentZoomed = zoomedPane === rootId;
  useEffect(() => {
    if (zoomed && !zoomedPane) setZoomed(null);
  }, [zoomed, zoomedPane]);

  // Placed from the area as it is now, not the last stored measurement: the
  // agent's terminal is shown in this same commit, and a stale height would
  // make it claim the wrong size and redraw.
  useLayoutEffect(() => {
    const host = hostRef.current;
    const root = rootRef.current;
    if (!host || !root || !active) return;
    host.style.visibility = zoomedPane && !agentZoomed ? "hidden" : "";
    // Alone, the terminal keeps its place in the page's own flow.
    if (tree.pane) {
      if (host.style.position === "absolute") {
        for (const key of ["position", "left", "top", "width", "height", "flex"]) host.style.removeProperty(key);
        stage.fitVisibleNow();
      }
      return;
    }
    const fresh = new Map<string, Placed>();
    measure(tree, { x: 0, y: 0, w: root.clientWidth, h: root.clientHeight }, null, [], layout, fresh, []);
    const rect = agentZoomed ? { x: 0, y: 0, w: root.clientWidth, h: root.clientHeight } : fresh.get(rootId)?.rect;
    if (!rect) return;
    const next = { left: `${rect.x}px`, top: `${rect.y}px`, width: `${rect.w}px`, height: `${rect.h}px` };
    if (
      host.style.position === "absolute" && host.style.left === next.left && host.style.top === next.top
      && host.style.width === next.width && host.style.height === next.height
    ) return;
    Object.assign(host.style, { position: "absolute", flex: "none", ...next });
    stage.fitVisibleNow();
  });
  useLayoutEffect(() => {
    const host = hostRef.current;
    if (!active) return;
    return () => {
      if (host) for (const key of ["position", "left", "top", "width", "height", "flex", "visibility"]) host.style.removeProperty(key);
    };
  }, [hostRef, active]);

  // A resize applies to the newest revision when the tree it was dragged on
  // still has the same shape; collapsing or retitling a pane changes the
  // revision but not the tree.
  const latest = useRef(layout);
  latest.current = layout;
  const commit = useCallback(
    async (dragged: LayoutNode, next: LayoutNode) => {
      const current = latest.current;
      if (JSON.stringify(stripSizes(treeOf(current))) !== JSON.stringify(stripSizes(dragged))) {
        setDraft(null);
        return;
      }
      const response = await authedFetch(`/api/sessions/${sessionId}/layout`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ base_rev: current.rev, layout: next, tab }),
      });
      if (!response.ok) setDraft(null);
    },
    [sessionId, tab],
  );

  // A drag resizes live, at most once per animation frame, and commits the
  // final sizes on release.
  const startDrag = (gutter: Gutter) => (event: React.PointerEvent) => {
    event.preventDefault();
    const horizontal = gutter.axis === "row";
    const origin = horizontal ? event.clientX : event.clientY;
    const a = gutter.lengths[gutter.index];
    const b = gutter.lengths[gutter.index + 1];
    const pairShare = gutter.shares[gutter.index] + gutter.shares[gutter.index + 1];
    const base = tree;
    let result = base;
    let frame = 0;
    let pending: LayoutNode | null = null;
    const move = (moveEvent: PointerEvent) => {
      const delta = (horizontal ? moveEvent.clientX : moveEvent.clientY) - origin;
      const first = Math.min(a + b - HEAD_PX * 2, Math.max(HEAD_PX * 2, a + delta));
      const next = [...gutter.shares];
      next[gutter.index] = Math.round((first / (a + b)) * pairShare * 10) / 10;
      next[gutter.index + 1] = Math.round((pairShare - next[gutter.index]) * 10) / 10;
      result = withSizes(base, gutter.path, next);
      pending = result;
      if (!frame) {
        frame = requestAnimationFrame(() => {
          frame = 0;
          if (pending) setDraft(pending);
        });
      }
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", up);
      if (frame) cancelAnimationFrame(frame);
      document.body.classList.remove("ap-resizing", `ap-resizing-${gutter.axis}`);
      if (result === base) return;
      setDraft(result);
      void commit(base, result);
    };
    document.body.classList.add("ap-resizing", `ap-resizing-${gutter.axis}`);
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", up);
  };

  // The header keeps the pointer captured while moving, so the move follows
  // it over page panes' iframes and the agent's terminal alike.
  const startMove = (id: string, title: string) => (event: React.PointerEvent<HTMLElement>) => {
    if (event.button !== 0 || zoomedPane) return;
    if ((event.target as HTMLElement).closest("button, a, select, input")) return;
    const head = event.currentTarget;
    const pointerId = event.pointerId;
    const startX = event.clientX;
    const startY = event.clientY;
    const base = tree;
    let started = false;
    let drop: { target: string; position: MovePosition } | null = null;
    const hit = (clientX: number, clientY: number) => {
      const root = rootRef.current;
      if (!root) return null;
      const bounds = root.getBoundingClientRect();
      const x = clientX - bounds.left;
      const y = clientY - bounds.top;
      const placed = new Map<string, Placed>();
      measure(base, { x: 0, y: 0, w: root.clientWidth, h: root.clientHeight }, null, [], layout, placed, []);
      for (const [target, { rect }] of placed) {
        if (target === id || x < rect.x || x > rect.x + rect.w || y < rect.y || y > rect.y + rect.h) continue;
        return { target, position: movePositionAt((x - rect.x) / (rect.w || 1), (y - rect.y) / (rect.h || 1)) };
      }
      return null;
    };
    const move = (moveEvent: PointerEvent) => {
      if (!started) {
        if (Math.hypot(moveEvent.clientX - startX, moveEvent.clientY - startY) < MOVE_THRESHOLD_PX) return;
        started = true;
        head.setPointerCapture(pointerId);
        document.body.classList.add("ap-moving");
      }
      drop = hit(moveEvent.clientX, moveEvent.clientY);
      setMoving({ id, title, x: moveEvent.clientX, y: moveEvent.clientY, target: drop?.target ?? null, position: drop?.position ?? null });
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", up);
      if (!started) return;
      document.body.classList.remove("ap-moving");
      setMoving(null);
      if (!drop) return;
      const next = movePaneInTree(base, id, drop.target, drop.position);
      if (JSON.stringify(next) === JSON.stringify(base)) return;
      setDraft(next);
      void commit(base, next);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", up);
  };

  const dropTarget = moving?.target && moving.position ? panes.get(moving.target)?.rect : undefined;
  const dropRegion = moving?.position ? paneDropRegionRect(moving.position) : null;

  const ids = Object.keys(layout.panes).sort();
  const full = size ? { x: 0, y: 0, w: size.w, h: size.h } : null;
  const agentRect = agentZoomed ? full : zoomedPane ? null : panes.get(rootId)?.rect ?? null;
  const session = state.sessions.get(sessionId);
  const agentTitle = tab ? "shell" : session ? sessionDisplayName(session) : "Agent";
  return (
    <div className={`ap-root${active ? "" : " is-warm"}${zoomedPane ? " is-zoomed" : ""}`} ref={rootRef} aria-hidden={active ? undefined : true}>
      {ids.map((id) => {
        const placed = panes.get(id);
        if (!placed) return null;
        const isZoomed = zoomedPane === id;
        return (
          <PaneView
            key={id}
            meta={layout.panes[id]}
            sessionId={sessionId}
            stage={stage}
            rect={isZoomed && full ? full : placed.rect}
            axis={isZoomed ? null : placed.axis}
            active={active}
            zoomed={isZoomed}
            behindZoom={Boolean(zoomedPane) && !isZoomed}
            moving={moving?.id === id}
            onPressCollapse={pressCollapse}
            onToggleCollapse={toggleCollapse}
            onAbandonPress={abandonPress}
            onHeadPointerDown={startMove(id, layout.panes[id].title)}
            onHeadContextMenu={openMenu(id)}
            onToggleZoom={toggleZoom}
          />
        );
      })}
      {dropTarget && dropRegion && moving?.position && (
        <div key={moving.target} className="ap-drop-target" style={rectStyle(dropTarget)} aria-hidden="true">
          <div className="pane-drop-overlay">
            <div
              className={`pane-drop-region drop-${moving.position}`}
              style={{ left: `${dropRegion.left}%`, top: `${dropRegion.top}%`, width: `${dropRegion.width}%`, height: `${dropRegion.height}%` }}
            />
          </div>
        </div>
      )}
      {moving && createPortal(
        <div className="pane-drag-ghost ap-move-ghost" style={{ left: moving.x - GHOST_OFFSET_PX, top: moving.y - GHOST_OFFSET_PX }} aria-hidden="true">
          <span className="pane-drag-ghost-icon">&gt;_</span>
          <span className="pane-drag-ghost-label">{moving.title}</span>
        </div>,
        document.body,
      )}
      {agentRect && (
        <AgentGrip
          rect={agentRect}
          title={agentTitle}
          zoomed={agentZoomed}
          kind={tab ? "shell" : "agent"}
          moving={moving?.id === rootId}
          onPointerDown={startMove(rootId, agentTitle)}
          onToggleZoom={() => toggleZoom(rootId)}
          onContextMenu={openMenu(rootId)}
        />
      )}
      {menu && (
        <PaneMenu
          x={menu.x}
          y={menu.y}
          zoomed={zoomedPane === menu.pane}
          canZoom={!tree.pane && (menu.pane === rootId || !layout.panes[menu.pane]?.collapsed)}
          canHide={menu.pane !== rootId && !tab}
          onShell={(position) => openShell(menu.pane, position)}
          onZoom={() => toggleZoom(menu.pane)}
          onHide={() => void paneAction(sessionId, menu.pane, "hide")}
          onClose={() => setMenu(null)}
        />
      )}
      {!zoomedPane && gutters.map((gutter) => (
        <div
          key={gutter.key}
          className={`ap-gutter ap-gutter-${gutter.axis}`}
          style={rectStyle(gutter.rect)}
          onPointerDown={startDrag(gutter)}
        />
      ))}
    </div>
  );
}

const SHELL_PLACES: Array<[Exclude<MovePosition, "swap">, string]> = [
  ["right", "New shell right"],
  ["below", "New shell below"],
  ["left", "New shell left"],
  ["above", "New shell above"],
];
/** Keeps the menu this far inside the window edges. */
const MENU_MARGIN_PX = 8;

function PaneMenu({
  x,
  y,
  zoomed,
  canZoom,
  canHide,
  onShell,
  onZoom,
  onHide,
  onClose,
}: {
  x: number;
  y: number;
  zoomed: boolean;
  canZoom: boolean;
  canHide: boolean;
  onShell: (position: Exclude<MovePosition, "swap">) => void;
  onZoom: () => void;
  onHide: () => void;
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [place, setPlace] = useState({ left: x, top: y });
  useLayoutEffect(() => {
    const menu = ref.current;
    if (!menu) return;
    setPlace({
      left: Math.max(MENU_MARGIN_PX, Math.min(x, window.innerWidth - menu.offsetWidth - MENU_MARGIN_PX)),
      top: Math.max(MENU_MARGIN_PX, Math.min(y, window.innerHeight - menu.offsetHeight - MENU_MARGIN_PX)),
    });
    menu.querySelector<HTMLButtonElement>("button")?.focus();
  }, [x, y]);
  useEffect(() => {
    const away = (event: Event) => {
      if (!ref.current?.contains(event.target as Node)) onClose();
    };
    const key = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("pointerdown", away, true);
    window.addEventListener("contextmenu", away, true);
    window.addEventListener("keydown", key);
    window.addEventListener("blur", onClose);
    window.addEventListener("resize", onClose);
    return () => {
      window.removeEventListener("pointerdown", away, true);
      window.removeEventListener("contextmenu", away, true);
      window.removeEventListener("keydown", key);
      window.removeEventListener("blur", onClose);
      window.removeEventListener("resize", onClose);
    };
  }, [onClose]);
  const pick = (action: () => void) => () => {
    onClose();
    action();
  };
  return createPortal(
    <div ref={ref} className="popover-menu ap-menu" role="menu" style={{ position: "fixed", left: place.left, top: place.top }} onContextMenu={(event) => event.preventDefault()}>
      {SHELL_PLACES.map(([position, label]) => (
        <button key={position} type="button" role="menuitem" className="popover-item" onClick={pick(() => onShell(position))}>{label}</button>
      ))}
      {(canZoom || canHide) && <div className="popover-sep" role="separator" />}
      {canZoom && <button type="button" role="menuitem" className="popover-item" onClick={pick(onZoom)}>{zoomed ? "Restore" : "Zoom"}</button>}
      {canHide && <button type="button" role="menuitem" className="popover-item" onClick={pick(onHide)}>Hide</button>}
    </div>,
    document.body,
  );
}

/** A header over the top edge of the agent's terminal that shows on hover, so
 * the agent can be moved and zoomed like a pane. It overlays the terminal
 * rather than taking a row from it, so showing it never resizes the terminal. */
function AgentGrip({
  rect,
  title,
  kind,
  zoomed,
  moving,
  onPointerDown,
  onToggleZoom,
  onContextMenu,
}: {
  rect: Rect;
  title: string;
  kind: "agent" | "shell";
  zoomed: boolean;
  moving: boolean;
  onPointerDown: (event: React.PointerEvent<HTMLElement>) => void;
  onToggleZoom: () => void;
  onContextMenu: (event: React.MouseEvent<HTMLElement>) => void;
}) {
  return (
    <div
      className={`ap-agent-grip${moving ? " is-moving" : ""}${zoomed ? " is-zoomed" : ""}`}
      style={{ left: rect.x, top: rect.y, width: rect.w }}
    >
      <div
        className="ap-head ap-agent-head"
        title={zoomed ? "Drag to move pane. Double-click empty space or press Esc to restore." : "Drag to move pane. Double-click empty space to zoom."}
        onPointerDown={onPointerDown}
        onDoubleClick={onToggleZoom}
        onContextMenu={onContextMenu}
      >
        <span className="ap-agent-glyph" aria-hidden="true">&gt;_</span>
        <span className="ap-title">{title}</span>
        <span className="ap-status ap-dim">{kind}</span>
      </div>
    </div>
  );
}

/** How long a pane waits for its content before showing whatever it has. */
const REVEAL_FALLBACK_MS = 1500;
const NO_ISSUE = () => {};

/** The program a foreground command runs, as a terminal tab names it: `cargo` for `cargo build --release`. */
export function programName(command: string): string {
  const [program = ""] = command.trim().split(/\s+/);
  return program.replace(/^-/, "");
}

function PaneView({
  meta,
  sessionId,
  stage,
  rect,
  axis,
  active,
  zoomed,
  behindZoom,
  moving,
  onPressCollapse,
  onToggleCollapse,
  onAbandonPress,
  onHeadPointerDown,
  onHeadContextMenu,
  onToggleZoom,
}: {
  meta: PaneMeta;
  sessionId: string;
  stage: TerminalStage;
  rect: Rect;
  axis: "row" | "column" | null;
  active: boolean;
  zoomed: boolean;
  /** Another pane is zoomed over this one. */
  behindZoom: boolean;
  moving: boolean;
  onHeadPointerDown: (event: React.PointerEvent<HTMLElement>) => void;
  onHeadContextMenu: (event: React.MouseEvent<HTMLElement>) => void;
  onToggleZoom: (pane: string) => void;
  /** Pointerdown on the collapse toggle, with the state a click would set. */
  onPressCollapse: (pane: string, collapsed: boolean) => void;
  onToggleCollapse: (pane: string, collapsed: boolean) => void;
  /** The press ended without a click. */
  onAbandonPress: () => void;
}) {
  const state = useAppState();
  const collapsed = meta.collapsed === true;
  const expanded = useRef<{ w: number; h: number }>({ w: rect.w, h: rect.h });
  if (!collapsed) expanded.current = { w: rect.w, h: rect.h - HEAD_PX };
  const sideways = collapsed && axis === "row";
  const [localReload, setLocalReload] = useState(0);
  const [liveCommand, setLiveCommand] = useState("");
  const reportCommand = useCallback((_terminalId: bigint, command: string) => setLiveCommand(command.trim()), []);
  const shownTitle = meta.titled === false && liveCommand ? programName(liveCommand) : meta.title;
  const pressed = useRef<object | null>(null);
  const [confirmingStop, setConfirmingStop] = useState(false);
  // The body stays behind the pane's own background until its content has
  // painted, so a pane appears whole instead of building up on screen.
  const [ready, setReady] = useState(false);
  const contentKey = `${meta.terminal_id ?? ""}:${meta.version}:${localReload}`;
  useEffect(() => {
    setReady(false);
    const timer = window.setTimeout(() => setReady(true), REVEAL_FALLBACK_MS);
    return () => window.clearTimeout(timer);
  }, [contentKey]);
  const terminal = meta.terminal_id !== undefined ? state.terminals.get(String(meta.terminal_id)) : undefined;
  const status =
    meta.kind === "page"
      ? "live"
      : meta.exit_code !== undefined
        ? `exit ${meta.exit_code}`
        : terminal
          ? "running"
          : "starting";
  const statusClass = meta.kind === "page" || status === "running" ? "ok" : status.startsWith("exit") ? (meta.exit_code === 0 ? "dim" : "bad") : "dim";
  const pageUrl = meta.page_token ? `pane-pages/${meta.page_token}/?v=${meta.version}.${localReload}` : "";
  return (
    <div
      className={`ap-pane ap-${meta.kind}${collapsed ? " is-collapsed" : ""}${sideways ? " is-sideways" : ""}${zoomed ? " is-zoomed" : ""}${behindZoom ? " is-behind-zoom" : ""}${moving ? " is-moving" : ""}`}
      style={rectStyle(rect)}
    >
      <div
        className="ap-head"
        title={collapsed ? "Drag to move pane." : zoomed ? "Drag to move pane. Double-click empty space or press Esc to restore." : "Drag to move pane. Double-click empty space to zoom."}
        onPointerDown={onHeadPointerDown}
        onContextMenu={onHeadContextMenu}
        onDoubleClick={(event) => {
          if ((event.target as HTMLElement).closest("button, a")) return;
          if (!collapsed) onToggleZoom(meta.id);
        }}
      >
        <button
          type="button"
          className={`ap-glyph ap-glyph-${meta.kind}`}
          title={collapsed ? "Expand" : "Collapse to its header"}
          aria-label={collapsed ? "Expand pane" : "Collapse pane"}
          aria-expanded={!collapsed}
          onPointerDown={(event) => {
            if (event.button !== 0) return;
            const press = pressed.current = {};
            onPressCollapse(meta.id, !collapsed);
            window.addEventListener("pointerup", () => window.setTimeout(() => {
              if (pressed.current === press) {
                pressed.current = null;
                onAbandonPress();
              }
            }), { once: true, capture: true });
          }}
          onClick={() => {
            pressed.current = null;
            onToggleCollapse(meta.id, !collapsed);
          }}
        >
          <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true" className="ap-glyph-chevron">
            <path d="M5.5 3.5 10 8l-4.5 4.5" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        </button>
        <span className="ap-title" title={liveCommand || meta.command || meta.title}>{shownTitle}</span>
        <span className={`ap-status ap-${statusClass}`}>{status}</span>
        {meta.kind !== "page" && (
          <button
            type="button"
            className={`ap-btn ${meta.user_input ? "on" : ""}`}
            title={meta.user_input ? "Typing is on: your keys go to this program. Click to make it read-only." : "Read-only. Click to type into this program."}
            aria-label={meta.user_input ? "Make read-only" : "Allow typing"}
            aria-pressed={meta.user_input}
            onClick={() => void paneAction(sessionId, meta.id, meta.user_input ? "input-off" : "input-on")}
          >
            <Icon name={meta.user_input ? "keyboard" : "keyboard-off"} />
          </button>
        )}
        {meta.kind === "page" && (
          <>
            <button type="button" className="ap-btn" title="Reload this page" aria-label="Reload" onClick={() => setLocalReload((n) => n + 1)}><Icon name="reload" /></button>
            <a className="ap-btn" title="Open in a new tab" aria-label="Open in a new tab" href={pageUrl} target="_blank" rel="noreferrer"><Icon name="open" /></a>
          </>
        )}
        {meta.kind !== "page" && (
          <button
            type="button"
            className="ap-btn ap-btn-stop"
            title="Stop the program and remove the pane"
            aria-label="Stop"
            onClick={() => setConfirmingStop(true)}
          >
            <Icon name="stop" />
          </button>
        )}
        <button type="button" className="ap-btn" title="Hide the pane. It keeps running; bring it back from the tab strip." aria-label="Hide" onClick={() => void paneAction(sessionId, meta.id, "hide")}><Icon name="hide" /></button>
      </div>
      {confirmingStop && createPortal(
        <ConfirmDialog
          title="stop this program"
          titleId={`ap-stop-${meta.id}`}
          confirmLabel="stop"
          busyLabel="stopping…"
          onClose={() => setConfirmingStop(false)}
          onConfirm={async () => {
            await paneAction(sessionId, meta.id, "stop");
            setConfirmingStop(false);
          }}
        >
          <p>
            <strong>{meta.title}</strong> will be stopped and its pane removed.
            {meta.command && meta.command !== meta.title && <> It runs <code>{meta.command}</code>.</>}
          </p>
          <p>The agent is told and won't start it again unless you ask. To keep it running out of sight, hide the pane instead.</p>
        </ConfirmDialog>,
        document.body,
      )}
      <div
        className={`ap-body${ready || meta.exit_code !== undefined ? "" : " is-curtained"}`}
        style={collapsed ? { position: "absolute", top: HEAD_PX, left: 0, width: expanded.current.w, height: expanded.current.h, visibility: "hidden", pointerEvents: "none" } : undefined}
      >
        {meta.kind === "page" ? (
          <iframe
            key={`${meta.version}.${localReload}`}
            className="ap-frame"
            src={pageUrl}
            sandbox="allow-scripts allow-forms"
            referrerPolicy="no-referrer"
            title={meta.title}
            onLoad={() => setReady(true)}
          />
        ) : meta.exit_code !== undefined ? (
          <EndedOutput sessionId={sessionId} paneId={meta.id} terminalId={meta.terminal_id} exitCode={meta.exit_code} />
        ) : terminal ? (
          <ReadOnlyGuard enabled={!meta.user_input}>
            <TerminalViewport key={String(terminal.id)} terminal={terminal} focused={false} active={active && !collapsed} onIssue={NO_ISSUE} onCommand={reportCommand} onFirstPaint={() => setReady(true)} stage={stage} readOnly={!meta.user_input} />
          </ReadOnlyGuard>
        ) : (
          <div className="ap-wait">starting…</div>
        )}
      </div>
    </div>
  );
}

/** An ended program's last output, since its terminal can no longer be attached. */
function EndedOutput({ sessionId, paneId, terminalId, exitCode }: { sessionId: string; paneId: string; terminalId?: number; exitCode: number }) {
  const [text, setText] = useState<string | null>(null);
  const ref = useRef<HTMLPreElement>(null);
  useEffect(() => {
    let current = true;
    void authedFetch(`/api/sessions/${sessionId}/panes/${encodeURIComponent(paneId)}/output`)
      .then((response) => (response.ok ? response.text() : ""))
      .then((body) => { if (current) setText(body.replace(/\s+$/, "")); })
      .catch(() => { if (current) setText(""); });
    return () => { current = false; };
  }, [sessionId, paneId, terminalId]);
  useLayoutEffect(() => {
    const element = ref.current;
    if (element) element.scrollTop = element.scrollHeight;
  }, [text]);
  return (
    <pre className="ap-output" ref={ref}>
      {text ?? ""}
      <span className={exitCode === 0 ? "ap-dim" : "ap-bad"}>{`\n[exited with status ${exitCode}]`}</span>
    </pre>
  );
}

const BLOCKED_KEYBOARD_EVENTS = ["keydown", "keypress", "beforeinput", "paste", "compositionstart"] as const;

/** Swallows keyboard input while the user has not enabled typing, but leaves
 * scrolling, selection and the terminal's own replies alone. */
function ReadOnlyGuard({ enabled, children }: { enabled: boolean; children: React.ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const host = ref.current;
    if (!host || !enabled) return;
    const block = (event: Event) => {
      event.preventDefault();
      event.stopPropagation();
    };
    for (const type of BLOCKED_KEYBOARD_EVENTS) host.addEventListener(type, block, true);
    return () => {
      for (const type of BLOCKED_KEYBOARD_EVENTS) host.removeEventListener(type, block, true);
    };
  }, [enabled]);
  return <div className="ap-guard" ref={ref}>{children}</div>;
}
