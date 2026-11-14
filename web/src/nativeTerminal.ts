import { useEffect, useRef, useState, type RefObject } from "react";
import { workerUnavailableReason } from "@puppet-master/client-core/state/worker";
import type { TerminalStreamStatus } from "@puppet-master/client-core/ws/terminalSocket";
import { useAppState } from "./state/hooks";
import {
  ATTACHING,
  MANUAL_RETRY_DELAY_MS,
  STATUS_DELAY_MS,
  closedLink,
  nativeStreamStatus,
  onlineLink,
  reattachDelay,
  type NativeLink,
} from "./nativeStatus";
import type { Terminal } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import {
  nativeBridge,
  nativeTerminalsAvailable,
  type NativeColor,
  type NativePlacement,
  type NativeRect,
  type NativeTerminalEvent,
  type NativeTerminals,
  type NativeTheme,
} from "./native";
import type { GhosttyTheme } from "./ws/ghostty/core";
import { mintSocketTicket, withTicket } from "./ws/socketTicket";
import { TERMINAL_PM_LINK_ERROR_EVENT } from "./ws/terminalLinks";
import type { TerminalStage } from "./ws/terminal";

/** Viewer ids are random below this, clear of the page's own counter. */
const VIEWER_ID_SPAN = 2 ** 48;

export function placementOf(host: HTMLElement, stage: Pick<TerminalStage, "visibleCellLayout">): NativePlacement | null {
  const layout = stage.visibleCellLayout();
  if (!layout) return null;
  const rect = host.getBoundingClientRect();
  return {
    x: rect.left,
    y: rect.top,
    width: rect.width,
    height: rect.height,
    ...layout,
    visible:
      rect.width > 0
      && rect.height > 0
      && document.visibilityState === "visible"
      && getComputedStyle(host).visibility !== "hidden",
    scale: window.devicePixelRatio || 1,
    viewportWidth: window.innerWidth,
  };
}

/** A theme color in `#RRGGBB` or `#RRGGBBAA` form. */
export function parseThemeColor(value: string | undefined): NativeColor | undefined {
  const match = value ? /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})?$/i.exec(value) : null;
  if (!match) return undefined;
  const [, r, g, b, a] = match;
  return {
    r: parseInt(r, 16),
    g: parseInt(g, 16),
    b: parseInt(b, 16),
    ...(a ? { a: parseInt(a, 16) / 255 } : {}),
  };
}

export function nativeThemeOf(theme: GhosttyTheme): NativeTheme {
  const selection = parseThemeColor(theme.selectionBackground);
  return {
    foreground: { ...theme.foreground },
    background: { ...theme.background },
    cursor: { ...theme.cursor },
    palette: (theme.palette ?? []).map((color) => ({ ...color })),
    ...(selection ? { selection } : {}),
  };
}

/** Page content that draws over a terminal, which the native view must show. */
const OVERLAY_SELECTOR = [
  ".popover-menu",
  "[role=menu]",
  "[role=dialog]",
  "[role=tooltip]",
  ".modal-backdrop",
  ".alert-card",
  ".alert-stack-footer",
  ".terminal-stream-status:not([hidden])",
  ".term-drop-overlay",
  ".ap-drop-target",
  ".ap-move-ghost",
].join(",");
/** The agent pane's hover header: it takes the mouse always, and shows once open. */
const GRIP_SELECTOR = ".ap-agent-grip";

export interface NativeRegions {
  holes: NativeRect[];
  passthrough: NativeRect[];
}

function clip(rect: DOMRect, area: DOMRect): NativeRect | null {
  const x = Math.max(rect.left, area.left);
  const y = Math.max(rect.top, area.top);
  const right = Math.min(rect.right, area.right);
  const bottom = Math.min(rect.bottom, area.bottom);
  return right > x && bottom > y ? { x, y, width: right - x, height: bottom - y } : null;
}

/** The parts of the page over `host` that a native terminal there must let through. */
export function overlayRegions(host: HTMLElement, root: ParentNode = document): NativeRegions {
  const area = host.getBoundingClientRect();
  const holes: NativeRect[] = [];
  const passthrough: NativeRect[] = [];
  for (const element of root.querySelectorAll<HTMLElement>(OVERLAY_SELECTOR)) {
    const rect = clip(element.getBoundingClientRect(), area);
    if (rect) holes.push(rect);
  }
  for (const grip of root.querySelectorAll<HTMLElement>(GRIP_SELECTOR)) {
    const rect = clip(grip.getBoundingClientRect(), area);
    if (!rect) continue;
    const open = grip.matches(":hover") || grip.classList.contains("is-moving");
    (open ? holes : passthrough).push(rect);
  }
  return { holes, passthrough };
}

function sameRegions(a: NativeRegions, b: NativeRegions): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

/** Gives a right click the native terminal passed up to the page element under it. */
export function dispatchContextMenu(x: number, y: number, fallback: Element): void {
  const target = document.elementFromPoint(x, y) ?? fallback;
  target.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: x, clientY: y, button: 2 }));
}

export function samePlacement(a: NativePlacement, b: NativePlacement): boolean {
  return (Object.keys(a) as (keyof NativePlacement)[]).every((key) => a[key] === b[key]);
}

interface Handlers {
  onLink(href: string): void;
  onCommand?(command: string): void;
  onContextMenu(x: number, y: number): void;
  onLinkHover(link: boolean): void;
  onOnline(): void;
  onClosed(reason: string, code: number | null): void;
}

/** A native terminal that is open, shown or kept warm while hidden. */
interface Entry {
  handlers: Handlers | null;
  /** The last placement sent, so a hidden one can be shown again as it was. */
  placement: NativePlacement | null;
  hiddenAt: number | null;
  link: NativeLink;
  /** Failed attaches in a row, which sets the wait before the next. */
  attempts: number;
}

/** What the page shows for a native terminal: its connection status, and a
 * retry the status offers. */
export interface NativeTerminalView {
  status: TerminalStreamStatus | null;
  retry(): void;
}

/** Set on a terminal's host while ⌘ is held over a link in it. */
export const LINK_HOVER_CLASS = "pm-native-link";

/** Hidden native terminals kept attached so switching back is instant. */
export const MAX_WARM_NATIVE_TERMINALS = 8;
const entries = new Map<string, Entry>();

function routeEvent(id: string, event: NativeTerminalEvent): void {
  const entry = entries.get(id);
  if (!entry) return;
  if (event.type === "linkfailed") {
    window.dispatchEvent(new CustomEvent<string>(TERMINAL_PM_LINK_ERROR_EVENT, { detail: event.reason }));
    return;
  }
  if (event.type === "closed" || event.type === "failed") {
    const code = event.type === "closed" ? event.code ?? null : null;
    if (entry.handlers) entry.handlers.onClosed(event.reason, code);
    else entries.delete(id);
  } else if (event.type === "online") entry.handlers?.onOnline(); else if (event.type === "link") entry.handlers?.onLink(event.href);
  else if (event.type === "contextmenu") entry.handlers?.onContextMenu(event.x, event.y);
  else if (event.type === "command") entry.handlers?.onCommand?.(event.command);
  else if (event.type === "hover") entry.handlers?.onLinkHover(event.link);
}

/** The hidden terminals to close so at most `limit` stay warm, oldest first. */
export function warmEvictions(hidden: ReadonlyMap<string, number>, limit: number): string[] {
  const order = [...hidden].sort((a, b) => a[1] - b[1]).map(([id]) => id);
  return order.slice(0, Math.max(0, order.length - limit));
}

function evictWarm(native: NativeTerminals): void {
  const hidden = new Map<string, number>();
  for (const [id, entry] of entries) if (entry.hiddenAt !== null) hidden.set(id, entry.hiddenAt);
  for (const id of warmEvictions(hidden, MAX_WARM_NATIVE_TERMINALS)) {
    entries.delete(id);
    void native.close(id).catch(() => {});
  }
}

export interface NativeTerminalOptions {
  terminal: Terminal | undefined;
  hostRef: RefObject<HTMLElement | null>;
  active: boolean;
  /** The page's cell size, font and inset, which the native grid matches. */
  stage: Pick<TerminalStage, "visibleCellLayout">;
  onLink(href: string): void;
  onCommand?(command: string): void;
  /** Takes no keyboard input, as a read-only pane. */
  readOnly?: boolean;
  onOpened?(): void;
}

/**
 * In the desktop app, draws a terminal natively over the place the page
 * keeps for it, following that place and the page content over it as the
 * layout moves. The native terminal is the page's only viewer of it, and it
 * stays attached for a while once hidden.
 */
export function useNativeTerminal(options: NativeTerminalOptions): NativeTerminalView {
  const { terminal, hostRef, active, stage, readOnly = false } = options;
  const terminalId = terminal?.id;
  const generation = terminal?.generation;
  const callbacks = useRef(options);
  callbacks.current = options;
  const state = useAppState();
  const session = terminal ? state.sessions.get(terminal.sessionId.toString()) : undefined;
  const unavailable = session ? workerUnavailableReason(state.workers, session.workerId) : null;
  const unavailableRef = useRef(unavailable);
  unavailableRef.current = unavailable;
  const [status, setStatus] = useState<TerminalStreamStatus | null>(null);
  const controls = useRef<{ retry(): void; recheck(): void } | null>(null);
  useEffect(() => controls.current?.recheck(), [unavailable]);
  useEffect(() => {
    const native = nativeBridge()?.terminal;
    if (!native || !nativeTerminalsAvailable() || !active || terminalId === undefined || generation === undefined) return;
    const id = `t:${terminalId}:${generation}:${readOnly ? "r" : "w"}`;
    let disposed = false;
    let frame = 0;
    let reattach = 0;
    let statusTimers: number[] = [];
    let regions: NativeRegions = { holes: [], passthrough: [] };
    const entry: Entry = entries.get(id) ?? { handlers: null, placement: null, hiddenAt: null, link: ATTACHING(Date.now()), attempts: 0 };
    const warm = entries.has(id) && entry.placement !== null;
    entries.set(id, entry);
    entry.hiddenAt = null;

    const showStatus = () => {
      if (disposed) return;
      setStatus(nativeStreamStatus(entry.link, unavailableRef.current, Date.now()));
      for (const timer of statusTimers) window.clearTimeout(timer);
      statusTimers = [];
      const since = entry.link.waitingSince;
      if (since === null) return;
      for (const at of [STATUS_DELAY_MS, MANUAL_RETRY_DELAY_MS]) {
        const wait = since + at - Date.now();
        if (wait > 0) statusTimers.push(window.setTimeout(showStatus, wait));
      }
    };

    const follow = () => {
      if (disposed) return;
      const host = hostRef.current;
      const next = host ? placementOf(host, stage) : null;
      if (next && (!entry.placement || !samePlacement(entry.placement, next))) {
        entry.placement = next;
        void native.place(id, next).catch(() => {});
      }
      const over = host ? overlayRegions(host) : { holes: [], passthrough: [] };
      if (!sameRegions(regions, over)) {
        regions = over;
        void native.regions(id, over.holes, over.passthrough).catch(() => {});
      }
      frame = requestAnimationFrame(follow);
    };

    const attach = async () => {
      window.clearTimeout(reattach);
      reattach = 0;
      if (unavailableRef.current) return;
      const viewer = 1 + Math.floor(Math.random() * VIEWER_ID_SPAN);
      const path = `/ws/terminal/${terminalId}?generation=${generation}&viewer=${viewer}`;
      const ticket = await mintSocketTicket(path).catch(() => null);
      if (disposed) return;
      if (!ticket) {
        entry.handlers?.onClosed("could not get a ticket for the terminal", null);
        return;
      }
      let placement: NativePlacement | null = null;
      while (!disposed && !placement) {
        await new Promise((resolve) => requestAnimationFrame(resolve));
        const host = hostRef.current;
        placement = host ? placementOf(host, stage) : null;
      }
      if (disposed || !placement) return;
      const scheme = location.protocol === "https:" ? "wss://" : "ws://";
      await native.open(
        id,
        { url: `${scheme}${location.host}${withTicket(path, ticket)}`, generation: Number(generation), viewer, readOnly },
        placement,
        (event) => routeEvent(id, event),
      );
      if (disposed) return;
      entry.placement = placement;
      regions = { holes: [], passthrough: [] };
      callbacks.current.onOpened?.();
      if (!readOnly) void native.focus(id).catch(() => {});
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(follow);
    };

    entry.handlers = {
      onLink: (href) => callbacks.current.onLink(href),
      onCommand: (command) => callbacks.current.onCommand?.(command),
      onContextMenu: (x, y) => {
        if (hostRef.current) dispatchContextMenu(x, y, hostRef.current);
      },
      onLinkHover: (link) => hostRef.current?.classList.toggle(LINK_HOVER_CLASS, link),
      onOnline: () => {
        entry.link = onlineLink();
        entry.attempts = 0;
        showStatus();
      },
      onClosed: (reason, code) => {
        if (disposed) return;
        entry.link = closedLink(entry.link, reason, code, Date.now());
        showStatus();
        // The view keeps its last screen in place while a new socket attaches.
        if (entry.link.refusal || unavailableRef.current || reattach) return;
        reattach = window.setTimeout(() => void attach(), reattachDelay(entry.attempts++));
      },
    };
    controls.current = {
      retry: () => {
        entry.attempts = 0;
        entry.link = { ...entry.link, refusal: null };
        showStatus();
        void attach();
      },
      recheck: () => {
        showStatus();
        if (!unavailableRef.current && !entry.link.online && !reattach && !entry.link.refusal) void attach();
      },
    };

    showStatus();
    if (warm) {
      callbacks.current.onOpened?.();
      if (!readOnly) void native.focus(id).catch(() => {});
      frame = requestAnimationFrame(follow);
    } else {
      entry.link = ATTACHING(Date.now());
      showStatus();
      void attach();
    }
    return () => {
      disposed = true;
      controls.current = null;
      cancelAnimationFrame(frame);
      window.clearTimeout(reattach);
      for (const timer of statusTimers) window.clearTimeout(timer);
      setStatus(null);
      entry.handlers = null;
      hostRef.current?.classList.remove(LINK_HOVER_CLASS);
      if (!entry.placement || !entry.link.online) {
        entries.delete(id);
        void native.close(id).catch(() => {});
        return;
      }
      entry.placement = { ...entry.placement, visible: false };
      entry.hiddenAt = Date.now();
      void native.place(id, entry.placement).catch(() => {});
      evictWarm(native);
    };
  }, [terminalId, generation, hostRef, active, stage, readOnly]);
  return { status, retry: () => controls.current?.retry() };
}
