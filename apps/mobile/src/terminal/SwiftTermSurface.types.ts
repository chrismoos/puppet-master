import type { HostMessage, ViewMessage } from "./protocol";

export interface SwiftTermSurfaceHandle {
  send(message: HostMessage): void;
  focus(): void;
  /** Current terminal grid size, available after the first native resize. */
  surfaceSize(): { cols: number; rows: number } | null;
  /** Resolves with the first native size, or the fallback after ~300 ms. */
  firstSize(): Promise<{ cols: number; rows: number } | null>;
  /** Install a provider that returns WebSocket headers for every connect.
   *  Called per socket open so the bearer token is always current. */
  setHeaderProvider(provider: (() => Record<string, string>) | null): void;
  /** Scrolls to bottom, forces layout, waits for display refresh, then
   *  hides the veil and resolves. */
  revealAtBottom(): Promise<void>;
}

export interface SwiftTermSurfaceProps {
  onMessage(message: ViewMessage): void;
  /** When true, an opaque overlay hides the terminal content. */
  veiled?: boolean;
}
