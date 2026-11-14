/** Where the page keeps a terminal, in CSS pixels, and the grid it draws. */
export interface NativePlacement {
  x: number;
  y: number;
  width: number;
  height: number;
  cellWidth: number;
  cellHeight: number;
  fontSize: number;
  padding: number;
  visible: boolean;
}

export type NativeTerminalEvent = { type: "closed" | "failed"; id: string; reason: string };

/** Terminals the desktop app draws natively over the page. */
export interface NativeTerminals {
  supported(): Promise<boolean>;
  open(
    id: string,
    attach: { url: string; generation: number; viewer: number },
    placement: NativePlacement,
    onEvent: (event: NativeTerminalEvent) => void,
  ): Promise<void>;
  place(id: string, placement: NativePlacement): Promise<void>;
  focus(id: string): Promise<void>;
  close(id: string): Promise<void>;
}

/** What the desktop app adds to the page it loads. Absent in a browser. */
export interface NativeBridge {
  notify(title: string, body?: string): Promise<void>;
  setBadge(count: number): Promise<void>;
  openWindow(url: string): Promise<void>;
  terminal?: NativeTerminals;
}

declare global {
  interface Window {
    __pmNative?: NativeBridge;
  }
}

export function nativeBridge(): NativeBridge | null {
  return typeof window !== "undefined" ? window.__pmNative ?? null : null;
}
