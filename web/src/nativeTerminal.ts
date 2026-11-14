import { useEffect, type RefObject } from "react";
import type { Terminal } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { nativeBridge, type NativePlacement } from "./native";
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

export function samePlacement(a: NativePlacement, b: NativePlacement): boolean {
  return (Object.keys(a) as (keyof NativePlacement)[]).every((key) => a[key] === b[key]);
}

/**
 * In the desktop app, draws the agent's terminal natively over the place the
 * page keeps for it, following that place as the layout moves. The page's
 * own terminal stays underneath and keeps owning the terminal's size; the
 * native one is a second viewer that follows it.
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
    if (!native || !active || terminalId === undefined || generation === undefined) return;
    const id = `s:${sessionId}`;
    const viewer = 1 + Math.floor(Math.random() * VIEWER_ID_SPAN);
    let disposed = false;
    let frame = 0;
    let reattach = 0;
    let shown: NativePlacement | null = null;

    const follow = () => {
      if (disposed) return;
      const host = hostRef.current;
      const next = host ? placementOf(host, stage) : null;
      if (next && (!shown || !samePlacement(shown, next))) {
        shown = next;
        void native.place(id, next).catch(() => {});
      }
      frame = requestAnimationFrame(follow);
    };

    const attach = async () => {
      if (!(await native.supported()) || disposed) return;
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
          if (event.type === "closed" && !disposed) {
            reattach = window.setTimeout(() => void attach(), REATTACH_MS);
          }
        },
      );
      if (disposed) return;
      shown = placement;
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
