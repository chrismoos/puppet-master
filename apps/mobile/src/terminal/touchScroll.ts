// Platform-neutral touch-drag-to-scroll gesture engine for the terminal
// WebView. Consumes touch point sequences and produces quantized scroll or
// arrow-key actions so the logic stays testable in Node. The DOM layer feeds
// it events and applies the returned actions to xterm.

export interface TouchPoint {
  x: number;
  y: number;
  timeMs: number;
}

/**
 * What a line step of pan scrolling turns into, following xterm's wheel
 * convention: "viewport" scrolls the terminal's own scrollback, "wheel"
 * delivers mouse wheel button reports to a mouse-tracking application, and
 * "arrows" emulates cursor keys for alternate-screen applications that have
 * mouse tracking off (alternateScrollMode).
 */
export type ScrollDeliveryMode = "viewport" | "wheel" | "arrows";

export interface GestureContext {
  /** Rendered cell height in CSS pixels, used to quantize drags to lines. */
  lineHeightPx: number;
  mode: ScrollDeliveryMode;
}

export type GestureAction =
  | { kind: "none" }
  | { kind: "scroll"; lines: number }
  | { kind: "wheel"; direction: "up" | "down"; count: number; xPx: number; yPx: number }
  | { kind: "arrows"; arrow: "up" | "down"; count: number };

export interface TouchScrollConfig {
  /** Movement below this distance is a tap or press, never a drag. */
  dragSlopPx: number;
  /** A press held this long before dragging is selection, not scrolling. */
  longPressMs: number;
  /** Velocity samples older than this window are ignored. */
  velocityWindowMs: number;
  /** A pause longer than this between the last move and lift kills the fling. */
  flingMaxPauseMs: number;
  /** Minimum lift velocity, in px/ms, for momentum to start. */
  flingMinStartVelocityPxMs: number;
  /** Momentum stops once decayed velocity falls below this, in px/ms. */
  flingStopVelocityPxMs: number;
  /** Per-millisecond exponential velocity decay factor. */
  flingDecayPerMs: number;
}

export const DEFAULT_TOUCH_SCROLL_CONFIG: TouchScrollConfig = {
  dragSlopPx: 8,
  longPressMs: 500,
  velocityWindowMs: 100,
  flingMaxPauseMs: 80,
  flingMinStartVelocityPxMs: 0.25,
  flingStopVelocityPxMs: 0.05,
  flingDecayPerMs: 0.995,
};

/**
 * Maximum fling velocity in px/ms. A tiny drag span with a large dy can
 * produce an arbitrarily large velocity; capping it prevents a single
 * quick flick from scrolling the entire buffer.
 */
const MAX_FLING_VELOCITY_PX_MS = 8;

/**
 * Maximum elapsed milliseconds consumed in a single fling step. Larger
 * values indicate a stall or a clock-domain mismatch and are evidence
 * the fling should stop rather than silently apply a huge displacement.
 */
const MAX_FLING_STEP_MS = 100;

const ARROW_UP_NORMAL = "\x1b[A";
const ARROW_DOWN_NORMAL = "\x1b[B";
const ARROW_UP_APPLICATION = "\x1bOA";
const ARROW_DOWN_APPLICATION = "\x1bOB";

export function arrowKeySequence(arrow: "up" | "down", applicationCursorKeys: boolean): string {
  if (arrow === "up") return applicationCursorKeys ? ARROW_UP_APPLICATION : ARROW_UP_NORMAL;
  return applicationCursorKeys ? ARROW_DOWN_APPLICATION : ARROW_DOWN_NORMAL;
}

const NO_ACTION: GestureAction = { kind: "none" };

type Phase = "idle" | "pending" | "scrolling" | "released";

export class TouchScrollGesture {
  private phase: Phase = "idle";
  private origin: TouchPoint | null = null;
  private last: TouchPoint | null = null;
  private accumPx = 0;
  private samples: TouchPoint[] = [];
  private flingVelocityPxMs = 0;
  private flingLastMs = 0;
  private flingFirstFrame = false;

  constructor(private config: TouchScrollConfig = DEFAULT_TOUCH_SCROLL_CONFIG) {}

  /** True while a drag has been claimed as a scroll gesture. */
  scrolling(): boolean {
    return this.phase === "scrolling";
  }

  flingActive(): boolean {
    return this.flingVelocityPxMs !== 0;
  }

  touchStart(point: TouchPoint, touchCount: number): void {
    this.stopFling();
    if (touchCount > 1) {
      this.phase = "released";
      return;
    }
    this.phase = "pending";
    this.origin = point;
    this.last = point;
    this.accumPx = 0;
    this.samples = [point];
  }

  touchMove(point: TouchPoint, ctx: GestureContext): GestureAction {
    if (this.phase === "pending") return this.maybeClaim(point, ctx);
    if (this.phase !== "scrolling" || !this.last) return NO_ACTION;
    this.accumPx += point.y - this.last.y;
    this.last = point;
    this.recordSample(point);
    return this.quantize(ctx);
  }

  touchEnd(timeMs: number, ctx: GestureContext): { claimed: boolean } {
    const claimed = this.phase === "scrolling";
    if (claimed && ctx.mode === "viewport") this.maybeStartFling(timeMs);
    this.phase = "idle";
    this.origin = null;
    this.last = null;
    this.accumPx = this.flingActive() ? this.accumPx : 0;
    this.samples = [];
    return { claimed };
  }

  touchCancel(): void {
    this.stopFling();
    this.phase = "idle";
    this.origin = null;
    this.last = null;
    this.accumPx = 0;
    this.samples = [];
  }

  flingStep(nowMs: number, ctx: GestureContext): GestureAction {
    if (!this.flingActive()) return NO_ACTION;
    if (ctx.mode !== "viewport") {
      this.stopFling();
      return NO_ACTION;
    }
    // The first rAF frame after a fling starts adopts the animation clock
    // as the baseline. maybeStartFling records velocity from the sample
    // clock (React Native evt.nativeEvent.timestamp), which may be a
    // different epoch than the rAF clock (ms since page load). Using the
    // sample timestamp as flingLastMs and then subtracting it from the rAF
    // clock produces a nonsensical elapsed time that either clamps to zero
    // (freezing decay) or produces a huge displacement. Setting the
    // baseline here avoids the mismatch entirely.
    if (this.flingFirstFrame) {
      this.flingFirstFrame = false;
      this.flingLastMs = nowMs;
      return NO_ACTION;
    }
    const elapsedMs = nowMs - this.flingLastMs;
    if (elapsedMs < 0 || elapsedMs > MAX_FLING_STEP_MS) {
      this.stopFling();
      return NO_ACTION;
    }
    this.flingLastMs = nowMs;
    this.accumPx += this.flingVelocityPxMs * elapsedMs;
    this.flingVelocityPxMs *= this.config.flingDecayPerMs ** elapsedMs;
    if (Math.abs(this.flingVelocityPxMs) < this.config.flingStopVelocityPxMs) this.stopFling();
    return this.quantize(ctx);
  }

  private maybeClaim(point: TouchPoint, ctx: GestureContext): GestureAction {
    if (!this.origin) return NO_ACTION;
    const dx = point.x - this.origin.x;
    const dy = point.y - this.origin.y;
    if (Math.max(Math.abs(dx), Math.abs(dy)) < this.config.dragSlopPx) return NO_ACTION;
    // A press held past the long-press threshold belongs to mouse selection,
    // so its drag is never claimed as a scroll. Every other one-finger drag
    // is a scroll, whatever its direction; only its vertical travel moves.
    if (point.timeMs - this.origin.timeMs >= this.config.longPressMs) {
      this.phase = "released";
      return NO_ACTION;
    }
    this.phase = "scrolling";
    this.accumPx = dy;
    this.last = point;
    this.recordSample(point);
    return this.quantize(ctx);
  }

  private quantize(ctx: GestureContext): GestureAction {
    if (!(ctx.lineHeightPx > 0)) return NO_ACTION;
    const lines = Math.trunc(this.accumPx / ctx.lineHeightPx);
    if (lines === 0) return NO_ACTION;
    this.accumPx -= lines * ctx.lineHeightPx;
    if (ctx.mode === "viewport") return { kind: "scroll", lines: -lines };
    // Finger moving down reveals older content, which is wheel-up.
    const direction = lines > 0 ? "up" : "down";
    const count = Math.abs(lines);
    if (ctx.mode === "arrows") return { kind: "arrows", arrow: direction, count };
    const at = this.last;
    return { kind: "wheel", direction, count, xPx: at?.x ?? 0, yPx: at?.y ?? 0 };
  }

  private recordSample(point: TouchPoint): void {
    this.samples.push(point);
    const cutoff = point.timeMs - this.config.velocityWindowMs;
    // Trim stale samples with splice instead of repeated shift() to avoid
    // O(n²) element shuffling when many samples accumulate in one frame.
    let trimTo = 0;
    while (
      trimTo < this.samples.length - 2 &&
      this.samples[trimTo].timeMs < cutoff
    ) {
      trimTo++;
    }
    if (trimTo > 0) this.samples.splice(0, trimTo);
  }

  onFlingDiag: ((msg: string) => void) | null = null;

  private maybeStartFling(timeMs: number): void {
    const newest = this.samples[this.samples.length - 1];
    const oldest = this.samples[0];
    if (!newest || !oldest || newest === oldest) {
      this.onFlingDiag?.(`reject: samples=${this.samples.length}`);
      return;
    }
    const pause = timeMs - newest.timeMs;
    if (pause > this.config.flingMaxPauseMs) {
      this.onFlingDiag?.(`reject: pause=${pause.toFixed(1)}ms > ${this.config.flingMaxPauseMs}ms`);
      return;
    }
    const spanMs = newest.timeMs - oldest.timeMs;
    if (spanMs <= 0) {
      this.onFlingDiag?.(`reject: spanMs=${spanMs.toFixed(1)}`);
      return;
    }
    const velocity = (newest.y - oldest.y) / spanMs;
    if (Math.abs(velocity) < this.config.flingMinStartVelocityPxMs) {
      this.onFlingDiag?.(`reject: |vel|=${Math.abs(velocity).toFixed(4)} < ${this.config.flingMinStartVelocityPxMs}, samples=${this.samples.length}, spanMs=${spanMs.toFixed(1)}`);
      return;
    }
    const capped = Math.max(-MAX_FLING_VELOCITY_PX_MS, Math.min(MAX_FLING_VELOCITY_PX_MS, velocity));
    this.onFlingDiag?.(`START: vel=${velocity.toFixed(4)}, capped=${capped.toFixed(4)}, pause=${pause.toFixed(1)}ms, samples=${this.samples.length}`);
    this.flingVelocityPxMs = capped;
    // Do not set flingLastMs from the sample clock: the rAF clock used by
    // flingStep may be a different epoch. The first flingStep call adopts
    // the rAF clock as the baseline.
    this.flingFirstFrame = true;
  }

  private stopFling(): void {
    this.flingVelocityPxMs = 0;
    this.flingFirstFrame = false;
  }
}

export type PanPhase = "start" | "move" | "end" | "cancel";

export interface BridgedPanHooks {
  context(): GestureContext;
  apply(action: GestureAction): void;
  startFling(): void;
}

/**
 * Feeds pan events forwarded by the native host into a gesture engine, so
 * host-captured pans run through the same claim, quantize, and fling paths
 * as direct DOM touches.
 */
export class BridgedPanSink {
  constructor(
    private gesture: TouchScrollGesture,
    private hooks: BridgedPanHooks,
  ) {}

  handle(phase: PanPhase, point: TouchPoint, touchCount = 1): void {
    switch (phase) {
      case "start":
        this.gesture.touchStart(point, touchCount);
        break;
      case "move":
        this.hooks.apply(this.gesture.touchMove(point, this.hooks.context()));
        break;
      case "end":
        this.gesture.touchEnd(point.timeMs, this.hooks.context());
        if (this.gesture.flingActive()) this.hooks.startFling();
        break;
      case "cancel":
        this.gesture.touchCancel();
        break;
    }
  }
}
