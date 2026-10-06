// Routes pan-driven scroll gestures to whatever the running application can
// respond to, mirroring xterm's wheel convention. On the normal screen the
// terminal owns viewport scrollback, so pans always scroll it locally and
// write nothing to the PTY, even when the application tracks the mouse. The
// alternate screen has no scrollback to move: with mouse tracking on, each
// line step is delivered as a wheel button report at the touch cell, and
// with tracking off it becomes arrow keys (alternateScrollMode). Nothing
// here filters the terminal's own output: every byte xterm emits reaches
// the PTY.

import {
  arrowKeySequence,
  type GestureAction,
  type GestureContext,
  type ScrollDeliveryMode,
} from "./touchScroll";

export type MouseReportEncoding = "sgr" | "default";

/** Mouse tracking mode the application negotiated, as xterm reports it. */
export type MouseTrackingMode = "none" | "x10" | "vt200" | "drag" | "any";

export interface ScrollModeSnapshot {
  bufferType: "normal" | "alternate";
  applicationCursorKeys: boolean;
  mouseTracking: MouseTrackingMode;
  mouseEncoding: MouseReportEncoding;
}

export interface TerminalCell {
  /** 1-based column. */
  col: number;
  /** 1-based row. */
  row: number;
}

export interface ScrollTerminal {
  scrollLines(lines: number): void;
  /** Delivers a PTY-bound sequence the router injects (arrows, wheel reports). */
  input(data: string): void;
  lineHeightPx(): number;
  /** Maps a viewport point in CSS pixels to a cell, clamped to the grid. */
  cellFromPoint(xPx: number, yPx: number): TerminalCell;
  modes(): ScrollModeSnapshot;
}

// xterm button codes: wheel steps are buttons 64 (up) and 65 (down),
// reported as presses only.
const WHEEL_UP_BUTTON = 64;
const WHEEL_DOWN_BUTTON = 65;
// X10-style payload bytes are offset by 32 and capped at one byte.
const X10_BYTE_OFFSET = 32;
const X10_COORD_MAX = 223;

/** One wheel step report in the encoding the application negotiated. */
export function wheelReport(
  direction: "up" | "down",
  cell: TerminalCell,
  encoding: MouseReportEncoding,
): string {
  const button = direction === "up" ? WHEEL_UP_BUTTON : WHEEL_DOWN_BUTTON;
  if (encoding === "sgr") return `\x1b[<${button};${cell.col};${cell.row}M`;
  const byte = (value: number) => String.fromCharCode(X10_BYTE_OFFSET + Math.min(value, X10_COORD_MAX));
  return `\x1b[M${byte(button)}${byte(cell.col)}${byte(cell.row)}`;
}

/** Delivery mode for a line step of pan scrolling, per xterm's convention. */
export function scrollDeliveryMode(modes: ScrollModeSnapshot): ScrollDeliveryMode {
  if (modes.bufferType !== "alternate") return "viewport";
  return modes.mouseTracking !== "none" ? "wheel" : "arrows";
}

export class PanScrollRouter {
  constructor(private term: ScrollTerminal) {}

  context(): GestureContext {
    return {
      lineHeightPx: this.term.lineHeightPx(),
      mode: scrollDeliveryMode(this.term.modes()),
    };
  }

  apply(action: GestureAction): void {
    if (action.kind === "scroll") {
      this.term.scrollLines(action.lines);
    } else if (action.kind === "wheel") {
      const cell = this.term.cellFromPoint(action.xPx, action.yPx);
      const report = wheelReport(action.direction, cell, this.term.modes().mouseEncoding);
      this.term.input(report.repeat(action.count));
    } else if (action.kind === "arrows") {
      const sequence = arrowKeySequence(action.arrow, this.term.modes().applicationCursorKeys);
      this.term.input(sequence.repeat(action.count));
    }
  }
}
