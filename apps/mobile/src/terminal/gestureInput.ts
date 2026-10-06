// Wires the scroll gesture engine, the scroll router, and the touch mouse
// reporter to one terminal and owns the exclusivity rules between them: a
// plain one-finger drag scrolls while the reporter stays silent, a long-press
// hands the touch to mouse selection, and a finger landing during momentum
// stops the scroll. The WebView entry point feeds host messages and DOM
// touches through this class, so tests can drive the exact pipeline the
// WebView runs and assert on PTY bytes and viewport movement.

import { TouchMouseReporter, type LongPressScheduler } from "./mouseReports";
import { PanScrollRouter, type ScrollTerminal } from "./scrollRouting";
import {
  BridgedPanSink,
  DEFAULT_TOUCH_SCROLL_CONFIG,
  TouchScrollGesture,
  type GestureAction,
  type PanPhase,
  type TouchPoint,
  type TouchScrollConfig,
} from "./touchScroll";

export interface GestureTerminal extends ScrollTerminal {
  /** Focuses the terminal so a tap still summons the keyboard. */
  focus(): void;
}

export interface GestureInputHooks {
  /** A gesture or momentum step moved the local viewport. */
  onViewportScroll(): void;
  /** Momentum survived the lift; the host drives flingStep from its frame loop. */
  startFling(): void;
}

export class TerminalGestureInput {
  private gesture: TouchScrollGesture;
  private router: PanScrollRouter;
  private reporter: TouchMouseReporter;
  private sink: BridgedPanSink;

  constructor(
    term: GestureTerminal,
    private hooks: GestureInputHooks,
    config: TouchScrollConfig = DEFAULT_TOUCH_SCROLL_CONFIG,
    schedule?: LongPressScheduler,
  ) {
    this.gesture = new TouchScrollGesture(config);
    this.router = new PanScrollRouter(term);
    this.reporter = new TouchMouseReporter(
      {
        cellFromPoint: (xPx, yPx) => term.cellFromPoint(xPx, yPx),
        modes: () => term.modes(),
        input: (data) => term.input(data),
        focus: () => term.focus(),
        scrollActive: () => this.scrollActive(),
      },
      config,
      schedule,
    );
    this.sink = new BridgedPanSink(this.gesture, {
      context: () => this.router.context(),
      apply: (action) => this.apply(action),
      startFling: () => this.hooks.startFling(),
    });
    if ((globalThis as Record<string, unknown>).__DEV__ === true) {
      this.gesture.onFlingDiag = (msg) => console.log(`[fling] ${msg}`);
    }
  }

  /** True while a claimed drag or its momentum owns the touch. */
  scrollActive(): boolean {
    return this.gesture.scrolling() || this.gesture.flingActive();
  }

  /** True while a drag is claimed as a scroll. */
  scrolling(): boolean {
    return this.gesture.scrolling();
  }

  flingActive(): boolean {
    return this.gesture.flingActive();
  }

  /** A pan event from the native bridge or the DOM touch fallback. */
  pan(phase: PanPhase, point: TouchPoint, touchCount = 1): void {
    this.sink.handle(phase, point, touchCount);
  }

  /** A raw touch from the native host, feeding the mouse reporter. */
  touch(phase: PanPhase, point: TouchPoint, touchCount: number): void {
    // A finger landing during momentum stops the scroll, and also clears a
    // scroll claim whose end event was lost.
    if (phase === "start" && this.scrollActive()) this.gesture.touchCancel();
    this.reporter.handle(phase, point, touchCount);
  }

  /** Cancels a pending tap when the native responder claims a scroll. */
  cancelTouch(): void {
    this.reporter.handle("cancel", { x: 0, y: 0, timeMs: 0 }, 1);
  }

  /** One momentum frame, driven from the host's animation loop. */
  flingStep(nowMs: number): void {
    this.apply(this.gesture.flingStep(nowMs, this.router.context()));
  }

  private apply(action: GestureAction): void {
    this.router.apply(action);
    if (action.kind === "scroll") this.hooks.onViewportScroll();
  }
}
