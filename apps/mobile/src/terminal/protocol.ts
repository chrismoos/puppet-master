import type { XtermTheme } from "@puppet-master/client-core/theme/terminalTheme";

import type { MouseTrackingMode } from "./scrollRouting";
import type { PanPhase } from "./touchScroll";

// Message contract between the native shell and the bundled xterm.js WebView.
// PTY bytes never cross this bridge: the WebView owns the terminal WebSocket
// and only small control/status messages travel here, as JSON strings.
// Terminal and generation ids are u64 and must stay lossless, so they are
// decimal strings, never JSON numbers.

/** Mobile replay cap: 128 KiB keeps the first paint fast while the snapshot
 *  frame still arrives first and the screen is correct. */
export const TERMINAL_REPLAY_CAP_BYTES = 128 * 1024;

export interface TerminalInitMessage {
  type: "init";
  /** ws(s) origin of the daemon, no trailing slash. */
  socketBaseUrl: string;
  terminalId: string;
  generation: string;
  /** Single-use attach ticket; absent until the daemon endpoint exists. */
  ticket?: string;
  /** Bearer access token for header-capable platforms (iOS). When present the
   *  socket authenticates via an Authorization header and no ticket is needed. */
  accessToken?: string;
  /** Initial terminal size so the first replay arrives at the phone's dimensions. */
  initialCols?: number;
  initialRows?: number;
  replayCapBytes: number;
  fontSize: number;
  fontFamily?: string;
  theme?: XtermTheme;
}

/**
 * A touch pan event captured by the native host over the terminal area and
 * forwarded for the WebView's gesture engine to apply. Native capture exists
 * because WKWebView's own recognizers consume drags before page JS ever sees
 * a touchmove.
 */
export interface PanHostMessage {
  type: "pan";
  phase: PanPhase;
  x: number;
  y: number;
  timeMs: number;
}

/**
 * One raw touch event over the terminal area, forwarded whether or not the
 * native layer claimed the touch as a pan. The WebView's mouse reporter
 * turns unclaimed touches into the press, motion, and release reports a
 * mouse-tracking application expects, which DOM events cannot provide:
 * WKWebView synthesizes no mouse events for drags and its tap synthesis is
 * unreliable.
 */
export interface TouchHostMessage {
  type: "touch";
  phase: PanPhase;
  x: number;
  y: number;
  timeMs: number;
  touchCount: number;
}

export type HostMessage =
  | TerminalInitMessage
  | PanHostMessage
  | TouchHostMessage
  | { type: "write"; dataBase64: string }
  /** The native host shows or hides the terminal; hidden disables input and
   * keeps the page from asserting its size to the PTY. */
  | { type: "setVisible"; visible: boolean }
  /** The native layout around the WebView changed size; refit and resize the PTY. */
  | { type: "refit" }
  | { type: "updateSize" }
  | { type: "dismissSize" }
  /** Request the page to report its current viewport diagnostics. */
  | { type: "requestDiag" }
  /** The native shell requests the terminal scroll to the bottom of scrollback. */
  | { type: "scrollToBottom" }
  /** Ask the WebView to return the current xterm selection text. */
  | { type: "getSelection" }
  /** A native pan claimed the gesture; cancel any pending tap/long-press state. */
  | { type: "cancelTouch" }
  /** A WebKit-owned selection touch ended; release editable focus if it dismissed selection. */
  | { type: "finishSelectionTouch" }
  | { type: "shutdown" };

export type TerminalViewPhase = "connecting" | "replaying" | "online" | "reconnecting" | "closed";

export type ViewMessage =
  | { type: "ready" }
  | { type: "sizeMismatch"; show: boolean }
  | { type: "nativeResized" }
  | { type: "status"; phase: TerminalViewPhase; detail?: string; canRetry?: boolean }
  | { type: "replay"; bytes: number; durationMs: number; snapshot: boolean; capped: boolean }
  | {
      type: "metrics";
      outputBytes: number;
      firstFrameMs: number | null;
      lastEchoMs: number | null;
      renderer: "webgl" | "dom";
      /** Input bytes handed to the terminal socket, absent from older bundles. */
      inputBytesSent?: number;
      /** Input bytes the socket dropped, absent from older bundles. */
      inputBytesDropped?: number;
      /** Active screen buffer, absent from older bundles. */
      bufferType?: "normal" | "alternate";
      /** Mouse tracking mode the application negotiated, absent from older bundles. */
      mouseTracking?: MouseTrackingMode;
      /** Width diagnostic: xterm's current grid cols. */
      termCols?: number;
      /** Width diagnostic: xterm's current grid rows. */
      termRows?: number;
      /** Width diagnostic: cols last passed to session.sendResize, null if never sent. */
      lastSentCols?: number | null;
      /** Width diagnostic: rows last passed to session.sendResize, null if never sent. */
      lastSentRows?: number | null;
      /** Width diagnostic: PTY cols as echoed by the daemon, null until first echo. */
      ptyCols?: number | null;
      /** Width diagnostic: PTY rows as echoed by the daemon, null until first echo. */
      ptyRows?: number | null;
      /** Width diagnostic: recent resize messages sent to daemon. */
      resizesSent?: Array<{ cols: number; rows: number; at: number }>;
      /** Width diagnostic: whether the last replay was a state snapshot. */
      lastReplaySnapshot?: boolean;
    }
  | { type: "selection"; active: boolean; bracketedPasteMode?: boolean }
  | { type: "selectionText"; text: string }
  | { type: "title"; title: string }
  | { type: "link"; url: string }
  | { type: "error"; message: string }
  /** Whether the viewport is following the bottom of the scrollback. */
  | { type: "replayPainted" }
  | { type: "following"; following: boolean }
  | {
      type: "keyboardDiag";
      /** window.innerHeight */
      layoutHeightPx: number;
      /** visualViewport.height, or -1 if unavailable */
      visualHeightPx: number;
      /** The container element's clientHeight */
      containerHeightPx: number;
      /** Terminal cols x rows */
      cols: number;
      rows: number;
      /** Terminal's pixel height from xterm screen element */
      terminalPixelHeight: number;
    };

const DECIMAL_U64 = /^(?:0|[1-9][0-9]*)$/;

export function isDecimalU64(value: unknown): value is string {
  return typeof value === "string" && DECIMAL_U64.test(value) && BigInt(value) < 1n << 64n;
}

export function encodeHostMessage(msg: HostMessage): string {
  return JSON.stringify(msg);
}

export function encodeViewMessage(msg: ViewMessage): string {
  return JSON.stringify(msg);
}

function record(raw: unknown): Record<string, unknown> | null {
  if (typeof raw !== "string") return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return null;
  return parsed as Record<string, unknown>;
}

function optionalString(value: unknown): value is string | undefined {
  return value === undefined || typeof value === "string";
}

export function parseHostMessage(raw: unknown): HostMessage | null {
  const msg = record(raw);
  if (!msg) return null;
  switch (msg["type"]) {
    case "init": {
      const socketBaseUrl = msg["socketBaseUrl"];
      const replayCapBytes = msg["replayCapBytes"];
      const fontSize = msg["fontSize"];
      if (typeof socketBaseUrl !== "string" || !/^wss?:\/\//.test(socketBaseUrl)) return null;
      if (!isDecimalU64(msg["terminalId"]) || !isDecimalU64(msg["generation"])) return null;
      if (typeof replayCapBytes !== "number" || !Number.isInteger(replayCapBytes) || replayCapBytes <= 0) {
        return null;
      }
      if (typeof fontSize !== "number" || fontSize <= 0) return null;
      if (!optionalString(msg["ticket"]) || !optionalString(msg["accessToken"]) || !optionalString(msg["fontFamily"])) return null;
      const theme = msg["theme"];
      if (theme !== undefined && (typeof theme !== "object" || theme === null)) return null;
      const initialCols = msg["initialCols"];
      const initialRows = msg["initialRows"];
      return {
        type: "init",
        socketBaseUrl: socketBaseUrl.replace(/\/+$/, ""),
        terminalId: msg["terminalId"],
        generation: msg["generation"],
        ticket: msg["ticket"] as string | undefined,
        accessToken: msg["accessToken"] as string | undefined,
        initialCols: typeof initialCols === "number" && initialCols >= 2 ? initialCols : undefined,
        initialRows: typeof initialRows === "number" && initialRows >= 1 ? initialRows : undefined,
        replayCapBytes,
        fontSize,
        fontFamily: msg["fontFamily"] as string | undefined,
        theme: theme as XtermTheme | undefined,
      };
    }
    case "pan": {
      const { phase, x, y, timeMs } = msg;
      if (phase !== "start" && phase !== "move" && phase !== "end" && phase !== "cancel") return null;
      if (typeof x !== "number" || !Number.isFinite(x)) return null;
      if (typeof y !== "number" || !Number.isFinite(y)) return null;
      if (typeof timeMs !== "number" || !Number.isFinite(timeMs)) return null;
      return { type: "pan", phase, x, y, timeMs };
    }
    case "touch": {
      const { phase, x, y, timeMs, touchCount } = msg;
      if (phase !== "start" && phase !== "move" && phase !== "end" && phase !== "cancel") return null;
      if (typeof x !== "number" || !Number.isFinite(x)) return null;
      if (typeof y !== "number" || !Number.isFinite(y)) return null;
      if (typeof timeMs !== "number" || !Number.isFinite(timeMs)) return null;
      if (typeof touchCount !== "number" || !Number.isInteger(touchCount) || touchCount < 1) return null;
      return { type: "touch", phase, x, y, timeMs, touchCount };
    }
    case "write":
      return typeof msg["dataBase64"] === "string" ? { type: "write", dataBase64: msg["dataBase64"] } : null;
    case "setVisible":
      return typeof msg["visible"] === "boolean" ? { type: "setVisible", visible: msg["visible"] } : null;
    case "refit":
      return { type: "refit" };
    case "updateSize":
      return { type: "updateSize" };
    case "dismissSize":
      return { type: "dismissSize" };
    case "requestDiag":
      return { type: "requestDiag" };
    case "scrollToBottom":
      return { type: "scrollToBottom" };
    case "getSelection":
      return { type: "getSelection" };
    case "cancelTouch":
      return { type: "cancelTouch" };
    case "finishSelectionTouch":
      return { type: "finishSelectionTouch" };
    case "shutdown":
      return { type: "shutdown" };
    default:
      return null;
  }
}

const VIEW_PHASES: readonly TerminalViewPhase[] = [
  "connecting",
  "replaying",
  "online",
  "reconnecting",
  "closed",
];

const TRACKING_MODES: readonly MouseTrackingMode[] = ["none", "x10", "vt200", "drag", "any"];

export function parseViewMessage(raw: unknown): ViewMessage | null {
  const msg = record(raw);
  if (!msg) return null;
  switch (msg["type"]) {
    case "ready":
      return { type: "ready" };
    case "sizeMismatch":
      return typeof msg["show"] === "boolean" ? { type: "sizeMismatch", show: msg["show"] } : null;
    case "status": {
      const phase = msg["phase"];
      if (typeof phase !== "string" || !VIEW_PHASES.includes(phase as TerminalViewPhase)) return null;
      if (!optionalString(msg["detail"])) return null;
      const canRetry = msg["canRetry"];
      if (canRetry !== undefined && typeof canRetry !== "boolean") return null;
      return {
        type: "status",
        phase: phase as TerminalViewPhase,
        detail: msg["detail"] as string | undefined,
        canRetry,
      };
    }
    case "replay": {
      const { bytes, durationMs, snapshot, capped } = msg;
      if (typeof bytes !== "number" || typeof durationMs !== "number") return null;
      if (typeof snapshot !== "boolean" || typeof capped !== "boolean") return null;
      return { type: "replay", bytes, durationMs, snapshot, capped };
    }
    case "metrics": {
      const { outputBytes, firstFrameMs, lastEchoMs, renderer, inputBytesSent, inputBytesDropped } = msg;
      if (typeof outputBytes !== "number") return null;
      if (firstFrameMs !== null && typeof firstFrameMs !== "number") return null;
      if (lastEchoMs !== null && typeof lastEchoMs !== "number") return null;
      if (renderer !== "webgl" && renderer !== "dom") return null;
      if (inputBytesSent !== undefined && typeof inputBytesSent !== "number") return null;
      if (inputBytesDropped !== undefined && typeof inputBytesDropped !== "number") return null;
      const bufferType = msg["bufferType"];
      if (bufferType !== undefined && bufferType !== "normal" && bufferType !== "alternate") return null;
      const mouseTracking = msg["mouseTracking"];
      if (
        mouseTracking !== undefined &&
        !TRACKING_MODES.includes(mouseTracking as MouseTrackingMode)
      ) {
        return null;
      }
      return {
        type: "metrics",
        outputBytes,
        firstFrameMs,
        lastEchoMs,
        renderer,
        inputBytesSent,
        inputBytesDropped,
        bufferType,
        mouseTracking: mouseTracking as MouseTrackingMode | undefined,
        termCols: typeof msg["termCols"] === "number" ? msg["termCols"] : undefined,
        termRows: typeof msg["termRows"] === "number" ? msg["termRows"] : undefined,
        lastSentCols: msg["lastSentCols"] as number | null | undefined,
        lastSentRows: msg["lastSentRows"] as number | null | undefined,
        ptyCols: msg["ptyCols"] as number | null | undefined,
        ptyRows: msg["ptyRows"] as number | null | undefined,
        resizesSent: msg["resizesSent"] as Array<{ cols: number; rows: number; at: number }> | undefined,
        lastReplaySnapshot: typeof msg["lastReplaySnapshot"] === "boolean" ? msg["lastReplaySnapshot"] : undefined,
      };
    }
    case "selection": {
      if (typeof msg["active"] !== "boolean") return null;
      const bpm = msg["bracketedPasteMode"];
      if (bpm !== undefined && typeof bpm !== "boolean") return null;
      return { type: "selection", active: msg["active"], bracketedPasteMode: bpm };
    }
    case "selectionText":
      return typeof msg["text"] === "string" ? { type: "selectionText", text: msg["text"] } : null;
    case "title":
      return typeof msg["title"] === "string" ? { type: "title", title: msg["title"] } : null;
    case "link":
      return typeof msg["url"] === "string" ? { type: "link", url: msg["url"] } : null;
    case "error":
      return typeof msg["message"] === "string" ? { type: "error", message: msg["message"] } : null;
    case "replayPainted":
      return { type: "replayPainted" };
    case "following":
      return typeof msg["following"] === "boolean" ? { type: "following", following: msg["following"] } : null;
    case "keyboardDiag": {
      const { layoutHeightPx, visualHeightPx, containerHeightPx, cols, rows, terminalPixelHeight } = msg;
      if (typeof layoutHeightPx !== "number") return null;
      if (typeof visualHeightPx !== "number") return null;
      if (typeof containerHeightPx !== "number") return null;
      if (typeof cols !== "number") return null;
      if (typeof rows !== "number") return null;
      if (typeof terminalPixelHeight !== "number") return null;
      return { type: "keyboardDiag", layoutHeightPx, visualHeightPx, containerHeightPx, cols, rows, terminalPixelHeight };
    }
    default:
      return null;
  }
}
