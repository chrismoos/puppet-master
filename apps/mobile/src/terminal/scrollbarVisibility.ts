// Visibility state machine for the terminal's overlay scrollbar, matching
// native iOS indicator behavior: hidden at rest, visible while a pan scroll
// or its momentum is engaged, then fading out after a short idle delay.
// Time is passed in explicitly so the machine stays deterministic in tests.

export type ScrollbarPhase = "hidden" | "visible" | "fading";

export interface ScrollbarVisibilityConfig {
  /** Idle time after the gesture settles before the fade starts. */
  fadeDelayMs: number;
  /** Duration of the fade from full opacity to hidden. */
  fadeDurationMs: number;
}

export const DEFAULT_SCROLLBAR_VISIBILITY_CONFIG: ScrollbarVisibilityConfig = {
  fadeDelayMs: 600,
  fadeDurationMs: 250,
};

export class ScrollbarVisibility {
  private engaged = false;
  private settledAtMs: number | null = null;

  constructor(private config: ScrollbarVisibilityConfig = DEFAULT_SCROLLBAR_VISIBILITY_CONFIG) {}

  /** A pan scroll or momentum step moved the viewport. */
  activity(): void {
    this.engaged = true;
    this.settledAtMs = null;
  }

  /** The gesture and its momentum have both ended. */
  settle(nowMs: number): void {
    if (!this.engaged) return;
    this.engaged = false;
    this.settledAtMs = nowMs;
  }

  phase(nowMs: number): ScrollbarPhase {
    if (this.engaged) return "visible";
    if (this.settledAtMs === null) return "hidden";
    const idleMs = nowMs - this.settledAtMs;
    if (idleMs < this.config.fadeDelayMs) return "visible";
    if (idleMs < this.config.fadeDelayMs + this.config.fadeDurationMs) return "fading";
    return "hidden";
  }

  opacity(nowMs: number): number {
    switch (this.phase(nowMs)) {
      case "visible":
        return 1;
      case "hidden":
        return 0;
      case "fading": {
        const fadeMs = nowMs - (this.settledAtMs as number) - this.config.fadeDelayMs;
        return 1 - fadeMs / this.config.fadeDurationMs;
      }
    }
  }
}
