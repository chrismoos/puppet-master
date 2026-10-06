// Synthesizes the mouse reports a mouse-tracking TUI expects from the raw
// touch stream the native host forwards. Under a React Native host the page
// cannot rely on DOM mouse events for touches: WKWebView synthesizes none
// for drags and its tap synthesis is unreliable, so this reporter is the
// authoritative source for touch-derived reports. A tap becomes a
// press/release pair at the tapped cell. A long-press becomes a button-held
// selection: the press report goes out when the long-press threshold
// elapses, motion reports follow the finger per cell, and the release goes
// out on lift. Every other one-finger drag is a scroll owned by the gesture
// engine, so the reporter emits nothing for it.

import type {
  MouseReportEncoding,
  MouseTrackingMode,
  ScrollModeSnapshot,
  TerminalCell,
} from "./scrollRouting";
import {
  DEFAULT_TOUCH_SCROLL_CONFIG,
  type PanPhase,
  type TouchPoint,
  type TouchScrollConfig,
} from "./touchScroll";

// xterm button codes: the left button is 0, an X10-style release is 3, and
// motion reports set bit 5 on the pressed button.
const LEFT_BUTTON = 0;
const X10_RELEASE_BUTTON = 3;
const MOTION_FLAG = 32;
// X10-style payload bytes are offset by 32 and capped at one byte.
const X10_BYTE_OFFSET = 32;
const X10_COORD_MAX = 223;

export type ButtonAction = "press" | "release" | "motion";

/** One left-button report in the encoding the application negotiated. */
export function buttonReport(
  action: ButtonAction,
  cell: TerminalCell,
  encoding: MouseReportEncoding,
): string {
  const button = action === "motion" ? LEFT_BUTTON | MOTION_FLAG : LEFT_BUTTON;
  if (encoding === "sgr") {
    return `\x1b[<${button};${cell.col};${cell.row}${action === "release" ? "m" : "M"}`;
  }
  const x10Button = action === "release" ? X10_RELEASE_BUTTON : button;
  const byte = (value: number) => String.fromCharCode(X10_BYTE_OFFSET + Math.min(value, X10_COORD_MAX));
  return `\x1b[M${byte(x10Button)}${byte(cell.col)}${byte(cell.row)}`;
}

/** Whether the negotiated tracking mode delivers this report at all. */
export function trackingDelivers(tracking: MouseTrackingMode, action: ButtonAction): boolean {
  switch (action) {
    case "press":
      return tracking !== "none";
    case "release":
      return tracking === "vt200" || tracking === "drag" || tracking === "any";
    case "motion":
      return tracking === "drag" || tracking === "any";
  }
}

export interface MouseReportTarget {
  /** Maps a viewport point in CSS pixels to a cell, clamped to the grid. */
  cellFromPoint(xPx: number, yPx: number): TerminalCell;
  modes(): ScrollModeSnapshot;
  /** Delivers a PTY-bound report directly to the socket. */
  input(data: string): void;
  /** Focuses the terminal so a tap still summons the keyboard. */
  focus(): void;
  /** True while the pan scroll gesture or its momentum owns the touch. */
  scrollActive(): boolean;
}

type ReporterPhase = "idle" | "pending" | "dragging" | "passed";

export type CancelLongPress = () => void;

/** Schedules the long-press trigger; returns a cancel handle. */
export type LongPressScheduler = (cb: () => void, delayMs: number) => CancelLongPress;

const defaultScheduler: LongPressScheduler = (cb, delayMs) => {
  const id = setTimeout(cb, delayMs);
  return () => clearTimeout(id);
};

export class TouchMouseReporter {
  private phase: ReporterPhase = "idle";
  private origin: TouchPoint | null = null;
  private lastCell: TerminalCell | null = null;
  private cancelLongPress: CancelLongPress | null = null;

  constructor(
    private target: MouseReportTarget,
    private config: TouchScrollConfig = DEFAULT_TOUCH_SCROLL_CONFIG,
    private schedule: LongPressScheduler = defaultScheduler,
  ) {}

  handle(phase: PanPhase, point: TouchPoint, touchCount: number): void {
    switch (phase) {
      case "start":
        this.start(point, touchCount);
        break;
      case "move":
        this.move(point, touchCount);
        break;
      case "end":
        this.end(point);
        break;
      case "cancel":
        this.cancel();
        break;
    }
  }

  private start(point: TouchPoint, touchCount: number): void {
    this.releaseIfDragging();
    this.clearLongPress();
    if (touchCount > 1) {
      this.phase = "passed";
      return;
    }
    this.phase = "pending";
    this.origin = point;
    this.lastCell = null;
    this.cancelLongPress = this.schedule(() => this.longPress(), this.config.longPressMs);
  }

  private move(point: TouchPoint, touchCount: number): void {
    if (touchCount > 1) {
      this.releaseIfDragging();
      this.pass();
      return;
    }
    if (this.phase === "pending") {
      this.pendingMove(point);
    } else if (this.phase === "dragging") {
      this.motion(point);
    }
  }

  /**
   * The press outlived the long-press threshold without dragging: it is a
   * selection. The press report goes out now, before any motion arrives, so
   * the application anchors its selection where the finger landed.
   */
  private longPress(): void {
    this.cancelLongPress = null;
    if (this.phase !== "pending" || !this.origin) return;
    if (this.target.scrollActive()) {
      this.phase = "passed";
      return;
    }
    this.beginDrag(this.origin);
  }

  private pendingMove(point: TouchPoint): void {
    const origin = this.origin;
    if (!origin) return;
    if (this.target.scrollActive()) {
      this.pass();
      return;
    }
    // The threshold decides, not the timer, so a delayed scheduler cannot
    // turn a held press into a scroll.
    if (point.timeMs - origin.timeMs >= this.config.longPressMs) {
      this.clearLongPress();
      this.beginDrag(origin);
      if (this.phase === "dragging") this.motion(point);
      return;
    }
    const dx = point.x - origin.x;
    const dy = point.y - origin.y;
    if (Math.max(Math.abs(dx), Math.abs(dy)) < this.config.dragSlopPx) return;
    // A plain drag is a scroll; the gesture engine owns it whether or not
    // the native claim has landed yet.
    this.pass();
  }

  private beginDrag(origin: TouchPoint): void {
    const modes = this.target.modes();
    if (modes.mouseTracking === "none") {
      this.phase = "passed";
      return;
    }
    this.phase = "dragging";
    const cell = this.target.cellFromPoint(origin.x, origin.y);
    this.lastCell = cell;
    this.emit("press", cell, modes);
  }

  private motion(point: TouchPoint): void {
    const modes = this.target.modes();
    const cell = this.target.cellFromPoint(point.x, point.y);
    if (this.lastCell && cell.col === this.lastCell.col && cell.row === this.lastCell.row) return;
    this.lastCell = cell;
    this.emit("motion", cell, modes);
  }

  private end(point: TouchPoint): void {
    if (this.phase === "pending") {
      this.tap(point);
    } else if (this.phase === "dragging") {
      this.emit("release", this.target.cellFromPoint(point.x, point.y), this.target.modes());
    }
    this.reset();
  }

  private tap(point: TouchPoint): void {
    const modes = this.target.modes();
    const cell = this.target.cellFromPoint(point.x, point.y);
    this.emit("press", cell, modes);
    this.emit("release", cell, modes);
    this.target.focus();
  }

  private cancel(): void {
    this.releaseIfDragging();
    this.reset();
  }

  /** A drag that cannot finish must still release, or the TUI keeps
   * extending its selection from a button it believes is held. */
  private releaseIfDragging(): void {
    if (this.phase !== "dragging" || !this.lastCell) return;
    this.emit("release", this.lastCell, this.target.modes());
    this.phase = "passed";
  }

  private pass(): void {
    this.clearLongPress();
    this.phase = "passed";
  }

  private clearLongPress(): void {
    this.cancelLongPress?.();
    this.cancelLongPress = null;
  }

  private reset(): void {
    this.clearLongPress();
    this.phase = "idle";
    this.origin = null;
    this.lastCell = null;
  }

  private emit(action: ButtonAction, cell: TerminalCell, modes: ScrollModeSnapshot): void {
    if (!trackingDelivers(modes.mouseTracking, action)) return;
    this.target.input(buttonReport(action, cell, modes.mouseEncoding));
  }
}
