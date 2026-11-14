import { useEffect, type RefObject } from "react";
import type { Terminal } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { nativeBridge, nativeTerminalsAvailable, type NativePlacement, type NativeRect } from "./native";
import { mintSocketTicket, withTicket } from "./ws/socketTicket";
import type { TerminalStage } from "./ws/terminal";

/** Wait before attaching again after the native terminal's socket closed. */
const REATTACH_MS = 1_000;
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
  };
}

/** Page content that draws over a terminal, which the native view must show. */
const OVERLAY_SELECTOR = [
  ".popover-menu",
  "[role=menu]",
  "[role=dialog]",
  "[role=tooltip]",
  ".modal-backdrop",
  ".alert-pill",
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

/**
 * In the desktop app, draws the agent's terminal natively over the place the
 * page keeps for it, following that place and the page content over it as
 * the layout moves. The native terminal is the page's only viewer of it.
 */
export function useNativeTerminal(
  sessionId: string,
  terminal: Terminal | undefined,
  hostRef: RefObject<HTMLElement | null>,
  active: boolean,
  stage: TerminalStage,
): void {
  const terminalId = terminal?.id;
  const generation = terminal?.generation;
  useEffect(() => {
    const native = nativeBridge()?.terminal;
    if (!native || !nativeTerminalsAvailable() || !active || terminalId === undefined || generation === undefined) return;
    const id = `s:${sessionId}`;
    const viewer = 1 + Math.floor(Math.random() * VIEWER_ID_SPAN);
    let disposed = false;
    let frame = 0;
    let reattach = 0;
    let shown: NativePlacement | null = null;
    let regions: NativeRegions = { holes: [], passthrough: [] };

    const follow = () => {
      if (disposed) return;
      const host = hostRef.current;
      const next = host ? placementOf(host, stage) : null;
      if (next && (!shown || !samePlacement(shown, next))) {
        shown = next;
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
      const path = `/ws/terminal/${terminalId}?generation=${generation}&viewer=${viewer}`;
      const ticket = await mintSocketTicket(path).catch(() => null);
      if (disposed || !ticket) return;
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
        { url: `${scheme}${location.host}${withTicket(path, ticket)}`, generation: Number(generation), viewer },
        placement,
        (event) => {
          if (disposed) return;
          if (event.type === "closed") {
            reattach = window.setTimeout(() => void attach(), REATTACH_MS);
          } else if (event.type === "link") {
            stage.openLink(sessionId, event.href);
          } else if (event.type === "contextmenu" && hostRef.current) {
            dispatchContextMenu(event.x, event.y, hostRef.current);
          }
        },
      );
      if (disposed) return;
      shown = placement;
      regions = { holes: [], passthrough: [] };
      void native.focus(id).catch(() => {});
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(follow);
    };

    void attach();
    return () => {
      disposed = true;
      cancelAnimationFrame(frame);
      window.clearTimeout(reattach);
      void native.close(id).catch(() => {});
    };
  }, [sessionId, terminalId, generation, hostRef, active, stage]);
}
