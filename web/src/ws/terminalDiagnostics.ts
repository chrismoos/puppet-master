import type { GhosttyTerminalAdapter } from "./ghostty/terminalAdapter";
import type { TerminalSize } from "@puppet-master/client-core/ws/pty";
import type { TerminalSocketStats } from "@puppet-master/client-core/ws/terminalSocket";

/** Everything the terminal debug bar shows for one warm layer. */
export interface TerminalLayerDebug {
  key: string;
  visible: boolean;
  cols: number;
  rows: number;
  bufferType: string;
  bufferLines: number;
  baseY: number;
  viewportY: number;
  backgroundBytes: number;
  lastPtyResize: TerminalSize | null;
  socket: TerminalSocketStats | null;
  screenText: string[];
  mouseProtocol: string | null;
  mouseEncoding: string | null;
  bracketedPaste: boolean | null;
  applicationCursorKeys: boolean | null;
  sendFocus: boolean | null;
  cellWidth: number | null;
  cellHeight: number | null;
  canvasWidth: number | null;
  canvasHeight: number | null;
  charSizeValid: boolean | null;
  scrollableHeight: number | null;
  scrollableScrollHeight: number | null;
  scrollableScrollTop: number | null;
}

export function collectTerminalSurfaceDebug(term: GhosttyTerminalAdapter): Pick<
  TerminalLayerDebug,
  | "mouseProtocol"
  | "mouseEncoding"
  | "bracketedPaste"
  | "applicationCursorKeys"
  | "sendFocus"
  | "cellWidth"
  | "cellHeight"
  | "canvasWidth"
  | "canvasHeight"
  | "charSizeValid"
  | "scrollableHeight"
  | "scrollableScrollHeight"
  | "scrollableScrollTop"
> {
  const canvas = term.element.querySelector("canvas");
  const cell = term.cellSize;
  return {
    mouseProtocol: term.modes.mouseTrackingMode,
    mouseEncoding: null,
    bracketedPaste: null,
    applicationCursorKeys: null,
    sendFocus: null,
    cellWidth: term.cellMetrics?.width ?? null,
    cellHeight: term.cellMetrics?.height ?? null,
    canvasWidth: canvas?.width ?? null,
    canvasHeight: canvas?.height ?? null,
    charSizeValid: cell !== null,
    scrollableHeight: null,
    scrollableScrollHeight: null,
    scrollableScrollTop: null,
  };
}

function shortBytes(count: number): string {
  if (count < 1024) return `${count}b`;
  if (count < 1024 * 1024) return `${(count / 1024).toFixed(1)}k`;
  return `${(count / (1024 * 1024)).toFixed(2)}m`;
}

function agoSeconds(at: number | null, now: number): string {
  if (at === null) return "never";
  return `${Math.max(0, Math.round((now - at) / 1000))}s`;
}

/** Renders one layer's diagnostics as terse chip strings for the bar. */
export function debugChips(layer: TerminalLayerDebug, now: number): string[] {
  const chips = [
    `${layer.key}${layer.visible ? "" : " (hidden)"}`,
    `size=${layer.cols}x${layer.rows}`,
    `buf=${layer.bufferType}`,
    `lines=${layer.bufferLines} base=${layer.baseY} view=${layer.viewportY}`,
    `mouse=${layer.mouseProtocol ?? "?"}/${layer.mouseEncoding ?? "?"}`,
    `modes=paste:${flag(layer.bracketedPaste)} appcur:${flag(layer.applicationCursorKeys)} focus:${flag(layer.sendFocus)}`,
    `cell=${num(layer.cellWidth)}x${num(layer.cellHeight)}${layer.charSizeValid === false ? " INVALID" : ""}`,
    `canvas=${num(layer.canvasWidth)}x${num(layer.canvasHeight)}`,
    `scrollable=${num(layer.scrollableScrollHeight)}/${num(layer.scrollableHeight)}@${num(layer.scrollableScrollTop)}`,
    `bg=${shortBytes(layer.backgroundBytes)}`,
    `pty=${layer.lastPtyResize ? `${layer.lastPtyResize.cols}x${layer.lastPtyResize.rows}` : "unknown"}`,
  ];
  const socket = layer.socket;
  if (socket) {
    chips.push(
      `sock=${socket.phase} g${socket.generation} open:${socket.socketsOpened} closed:${socket.socketsClosed}`,
      `out=${shortBytes(socket.outputBytes)} ${agoSeconds(socket.lastOutputAt, now)} ago`,
      `replay=${socket.replayCount}x last:${shortBytes(socket.lastReplayBytes)} ${agoSeconds(socket.lastReplayAt, now)} ago`,
      `in=${shortBytes(socket.inputBytesSent)} held:${shortBytes(socket.inputBytesPending)} dropped:${shortBytes(socket.inputBytesDropped)}`,
      `resizes=${socket.resizesSent.map((entry) => `${entry.cols}x${entry.rows}`).join(",") || "none"}`,
      `ptyEcho=${socket.ptySize ? `${socket.ptySize.cols}x${socket.ptySize.rows}` : "none"}`,
    );
    if (socket.lastError) chips.push(`err=${socket.lastError}`);
  } else {
    chips.push("sock=?");
  }
  return chips;
}

function flag(value: boolean | null): string {
  return value === null ? "?" : value ? "on" : "off";
}

function num(value: number | null): string {
  if (value === null) return "?";
  return Number.isInteger(value) ? String(value) : value.toFixed(1);
}
