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
  /** Device pixels per CSS pixel. It changes as the window moves between
   * displays, which places the terminal again for the new display. */
  scale: number;
  /** The viewport's width in CSS pixels, from which the app reads the page zoom. */
  viewportWidth: number;
}

/** A color for the native terminal, with an opacity from 0 to 1 when it has one. */
export interface NativeColor {
  r: number;
  g: number;
  b: number;
  a?: number;
}

/** The page's terminal theme, as the native terminal draws it. */
export interface NativeTheme {
  foreground: NativeColor;
  background: NativeColor;
  cursor: NativeColor;
  palette: NativeColor[];
  selection?: NativeColor;
}

/** A rectangle in page coordinates, in CSS pixels. */
export interface NativeRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export type NativeTerminalEvent =
  | { type: "closed"; id: string; reason: string; code?: number | null }
  | { type: "failed" | "linkfailed"; id: string; reason: string }
  | { type: "online"; id: string }
  | { type: "link"; id: string; href: string }
  | { type: "contextmenu"; id: string; x: number; y: number }
  | { type: "command"; id: string; command: string }
  | { type: "hover"; id: string; link: boolean };

/** Terminals the desktop app draws natively over the page. */
export interface NativeTerminals {
  supported(): Promise<boolean>;
  open(
    id: string,
    attach: { url: string; generation: number; viewer: number; readOnly: boolean },
    placement: NativePlacement,
    onEvent: (event: NativeTerminalEvent) => void,
  ): Promise<void>;
  place(id: string, placement: NativePlacement): Promise<void>;
  focus(id: string): Promise<void>;
  /** Page content over the terminal: holes show through and take the mouse,
   * passthrough areas only take the mouse. */
  regions(id: string, holes: NativeRect[], passthrough: NativeRect[]): Promise<void>;
  /** Sets the theme every native terminal in the window draws with. */
  theme(theme: NativeTheme): Promise<void>;
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

/** True in a desktop build that draws terminals natively, known before
 * any terminal is shown so the page never opens a socket of its own. */
export function nativeTerminalsAvailable(): boolean {
  if (typeof window === "undefined") return false;
  return Boolean(window.__pmNative?.terminal) && (window as { __PM_NATIVE_TERMINALS__?: boolean }).__PM_NATIVE_TERMINALS__ === true;
}

export function nativeBridge(): NativeBridge | null {
  return typeof window !== "undefined" ? window.__pmNative ?? null : null;
}
