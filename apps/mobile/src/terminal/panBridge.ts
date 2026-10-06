// Native-side half of the terminal pan bridge. WKWebView's own gesture
// recognizers consume drags before page JS sees a touchmove, so the React
// Native layer claims vertical pans over the terminal and forwards them as
// pan host messages for the WebView's gesture engine to apply. Claiming
// reuses the engine's own state machine so slop, long-press selection
// hand-off, and multi-touch pass-through stay identical to the DOM path.

import type { HostMessage } from "./protocol";
import {
  DEFAULT_TOUCH_SCROLL_CONFIG,
  TouchScrollGesture,
  type GestureContext,
  type TouchPoint,
  type TouchScrollConfig,
} from "./touchScroll";

export interface PanSample {
  x: number;
  y: number;
  timeMs: number;
  touchCount: number;
}

// Quantization is the WebView's job; a zero line height keeps the local
// engine claim-only.
const CLAIM_CONTEXT: GestureContext = { lineHeightPx: 0, mode: "viewport" };

export class TerminalPanBridge {
  private claimGesture: TouchScrollGesture;
  private origin: TouchPoint | null = null;
  private claimed = false;

  constructor(
    private send: (msg: HostMessage) => void,
    config: TouchScrollConfig = DEFAULT_TOUCH_SCROLL_CONFIG,
  ) {
    this.claimGesture = new TouchScrollGesture(config);
  }

  /** Responder start over the terminal area. Never claims; seeds the origin. */
  touchStart(sample: PanSample): void {
    if (this.claimed && sample.touchCount > 1) {
      this.abort(sample);
      return;
    }
    this.claimed = false;
    this.origin = { x: sample.x, y: sample.y, timeMs: sample.timeMs };
    this.claimGesture.touchStart(this.origin, sample.touchCount);
  }

  /** Move-should-set decision: claim once the drag reads as a vertical scroll. */
  shouldClaim(sample: PanSample): boolean {
    if (this.claimed) return true;
    this.claimGesture.touchMove({ x: sample.x, y: sample.y, timeMs: sample.timeMs }, CLAIM_CONTEXT);
    return this.claimGesture.scrolling();
  }

  /**
   * Responder granted. Replays the gesture from its origin so the WebView
   * engine sees the full displacement, not just post-claim movement.
   */
  grant(sample: PanSample): void {
    this.claimed = true;
    const origin = this.origin ?? { x: sample.x, y: sample.y, timeMs: sample.timeMs };
    this.pan("start", origin.x, origin.y, origin.timeMs);
    if (sample.x !== origin.x || sample.y !== origin.y || sample.timeMs !== origin.timeMs) {
      this.pan("move", sample.x, sample.y, sample.timeMs);
    }
  }

  move(sample: PanSample): void {
    if (!this.claimed) return;
    if (sample.touchCount > 1) {
      this.abort(sample);
      return;
    }
    this.pan("move", sample.x, sample.y, sample.timeMs);
  }

  release(sample: PanSample): void {
    const wasClaimed = this.claimed;
    this.reset();
    if (wasClaimed) this.pan("end", sample.x, sample.y, sample.timeMs);
  }

  terminate(sample: PanSample): void {
    if (this.claimed) this.abort(sample);
    this.reset();
  }

  private abort(sample: PanSample): void {
    this.reset();
    this.pan("cancel", sample.x, sample.y, sample.timeMs);
  }

  private reset(): void {
    this.claimed = false;
    this.origin = null;
    this.claimGesture.touchCancel();
  }

  private pan(phase: "start" | "move" | "end" | "cancel", x: number, y: number, timeMs: number): void {
    this.send({ type: "pan", phase, x, y, timeMs });
  }
}
