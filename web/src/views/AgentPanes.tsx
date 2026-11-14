import { useCallback, useEffect, useLayoutEffect, useRef, useState, type RefObject } from "react";
import { createPortal } from "react-dom";
import { authedFetch } from "../api/token";
import { useAppState } from "../state/hooks";
import { ConfirmDialog } from "./ConfirmDialog";
import { TerminalViewport } from "./WorkspacePage";

type LayoutNode = { pane?: string; row?: LayoutNode[]; column?: LayoutNode[]; size?: number };

interface PaneMeta {
  id: string;
  kind: "program" | "shell" | "page";
  title: string;
  terminal_id?: number;
  command?: string;
  page_token?: string;
  version: number;
  user_input: boolean;
  exit_code?: number;
  collapsed?: boolean;
}

export interface SessionPaneLayout {
  rev: number;
  tree: LayoutNode;
  panes: Record<string, PaneMeta>;
  unplaced: string[];
}

const AGENT = "agent";

export function parseSessionLayout(json: string | undefined): SessionPaneLayout | null {
  if (!json) return null;
  try {
    const layout = JSON.parse(json) as SessionPaneLayout;
    return layout && layout.tree ? layout : null;
  } catch {
    return null;
  }
}

/** True when the layout shows anything besides the agent. */
export function layoutHasPanes(layout: SessionPaneLayout | null): boolean {
  return Boolean(layout && !layout.tree.pane);
}

/** A dev-server page pane's own forward, which its pane already shows. */
export function isPaneForward(slug: string, sessionId: string): boolean {
  return slug.startsWith(`pane-${sessionId}-`);
}

/** Terminal ids that belong to panes, so the shell tab strip leaves them out. */
export function paneTerminalIds(layout: SessionPaneLayout | null): Set<string> {
  const ids = new Set<string>();
  for (const pane of Object.values(layout?.panes ?? {})) {
    if (pane.terminal_id !== undefined) ids.add(String(pane.terminal_id));
  }
  return ids;
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
  layout,
  hostRef,
}: {
  sessionId: string;
  layout: SessionPaneLayout;
  hostRef: RefObject<HTMLDivElement | null>;
}) {
  const rootRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState<{ w: number; h: number } | null>(null);
  const [draft, setDraft] = useState<LayoutNode | null>(null);
  const tree = draft ?? layout.tree;
  useEffect(() => setDraft(null), [layout.rev]);

  useLayoutEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    const read = () => setSize({ w: root.clientWidth, h: root.clientHeight });
    read();
    const observer = new ResizeObserver(read);
    observer.observe(root);
    return () => observer.disconnect();
  }, []);

  const panes = new Map<string, Placed>();
  const gutters: Gutter[] = [];
  if (size) measure(tree, { x: 0, y: 0, w: size.w, h: size.h }, null, [], layout, panes, gutters);
  const agentRect = panes.get(AGENT)?.rect;

  useLayoutEffect(() => {
    const host = hostRef.current;
    if (!host || !agentRect) return;
    Object.assign(host.style, {
      position: "absolute",
      left: `${agentRect.x}px`,
      top: `${agentRect.y}px`,
      width: `${agentRect.w}px`,
      height: `${agentRect.h}px`,
      flex: "none",
    });
  }, [hostRef, agentRect?.x, agentRect?.y, agentRect?.w, agentRect?.h]);
  useLayoutEffect(() => {
    const host = hostRef.current;
    return () => {
      if (host) for (const key of ["position", "left", "top", "width", "height", "flex"]) host.style.removeProperty(key);
    };
  }, [hostRef]);

  // A resize applies to the newest revision when the tree it was dragged on
  // still has the same shape; collapsing or retitling a pane changes the
  // revision but not the tree.
  const latest = useRef(layout);
  latest.current = layout;
  const commit = useCallback(
    async (dragged: LayoutNode, next: LayoutNode) => {
      const current = latest.current;
      if (JSON.stringify(stripSizes(current.tree)) !== JSON.stringify(stripSizes(dragged))) {
        setDraft(null);
        return;
      }
      const response = await authedFetch(`/api/sessions/${sessionId}/layout`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ base_rev: current.rev, layout: next }),
      });
      if (!response.ok) setDraft(null);
    },
    [sessionId],
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

  const ids = Object.keys(layout.panes).sort();
  return (
    <div className="ap-root" ref={rootRef}>
      {ids.map((id) => {
        const placed = panes.get(id);
        if (!placed) return null;
        return (
          <PaneView
            key={id}
            meta={layout.panes[id]}
            sessionId={sessionId}
            rect={placed.rect}
            axis={placed.axis}
          />
        );
      })}
      {gutters.map((gutter) => (
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

const NO_ISSUE = () => {};
const NO_COMMAND = () => {};

function PaneView({
  meta,
  sessionId,
  rect,
  axis,
}: {
  meta: PaneMeta;
  sessionId: string;
  rect: Rect;
  axis: "row" | "column" | null;
}) {
  const state = useAppState();
  const collapsed = meta.collapsed === true;
  const expanded = useRef<{ w: number; h: number }>({ w: rect.w, h: rect.h });
  if (!collapsed) expanded.current = { w: rect.w, h: rect.h - HEAD_PX };
  const sideways = collapsed && axis === "row";
  const [localReload, setLocalReload] = useState(0);
  const [confirmingStop, setConfirmingStop] = useState(false);
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
      className={`ap-pane ap-${meta.kind}${collapsed ? " is-collapsed" : ""}${sideways ? " is-sideways" : ""}`}
      style={rectStyle(rect)}
    >
      <div className="ap-head">
        <button
          type="button"
          className={`ap-glyph ap-glyph-${meta.kind}`}
          title={collapsed ? "Expand" : "Collapse to its header"}
          aria-label={collapsed ? "Expand pane" : "Collapse pane"}
          aria-expanded={!collapsed}
          onClick={() => void paneAction(sessionId, meta.id, collapsed ? "expand" : "collapse")}
        >
          <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true" className="ap-glyph-chevron">
            <path d="M5.5 3.5 10 8l-4.5 4.5" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        </button>
        <span className="ap-title" title={meta.command ?? meta.title}>{meta.title}</span>
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
        className="ap-body"
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
          />
        ) : meta.exit_code !== undefined ? (
          <EndedOutput sessionId={sessionId} paneId={meta.id} terminalId={meta.terminal_id} exitCode={meta.exit_code} />
        ) : terminal ? (
          <ReadOnlyGuard enabled={!meta.user_input}>
            <TerminalViewport key={String(terminal.id)} terminal={terminal} focused={false} active={!collapsed} onIssue={NO_ISSUE} onCommand={NO_COMMAND} />
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
