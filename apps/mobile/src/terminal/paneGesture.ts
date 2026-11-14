/** Reads pane gestures from raw touches on a terminal or page pane: a
 * two-finger swipe left moves to the next pane and right to the previous
 * one. One-finger touches are left alone, so the view under it keeps its own
 * scrolling and selection, and a page keeps its own swipes. It only watches
 * touches, never claims them. */

export type PaneGesture = "next" | "previous" | "agent";

export interface TouchPoint {
  id: number;
  x: number;
  y: number;
}

/** Sideways travel a swipe needs, in points. */
const SWIPE_MIN = 60;
/** A swipe's sideways travel must outweigh its vertical travel this much. */
const SWIPE_RATIO = 2;
/** Swipes slower than this are scrolls or drags, not pane changes. */
const SWIPE_MAX_MS = 800;
/** Two taps this close together are a double tap. */
export const DOUBLE_TAP_MS = 350;

interface Track {
  startX: number;
  startY: number;
  x: number;
  y: number;
}

export class PaneGestureRecognizer {
  private tracks = new Map<number, Track>();
  private maxTouches = 0;
  private startedAt = 0;

  start(touches: readonly TouchPoint[], timeMs: number): void {
    if (this.tracks.size === 0) {
      this.maxTouches = 0;
      this.startedAt = timeMs;
    }
    this.follow(touches);
  }

  move(touches: readonly TouchPoint[]): void {
    this.follow(touches);
  }

  /** Call with the touches still down; returns a gesture once the last lifts. */
  end(remaining: readonly TouchPoint[], timeMs: number): PaneGesture | null {
    if (remaining.length > 0) return null;
    const tracks = [...this.tracks.values()];
    const touches = this.maxTouches;
    const duration = timeMs - this.startedAt;
    this.tracks.clear();
    if (touches !== 2 || tracks.length < 2 || duration > SWIPE_MAX_MS) return null;
    const dx = tracks.reduce((sum, t) => sum + (t.x - t.startX), 0) / tracks.length;
    const dy = tracks.reduce((sum, t) => sum + (t.y - t.startY), 0) / tracks.length;
    if (Math.abs(dx) < SWIPE_MIN || Math.abs(dx) < SWIPE_RATIO * Math.abs(dy)) return null;
    return dx < 0 ? "next" : "previous";
  }

  cancel(): void {
    this.tracks.clear();
  }

  private follow(touches: readonly TouchPoint[]): void {
    for (const touch of touches) {
      const track = this.tracks.get(touch.id);
      if (track) {
        track.x = touch.x;
        track.y = touch.y;
      } else {
        this.tracks.set(touch.id, { startX: touch.x, startY: touch.y, x: touch.x, y: touch.y });
      }
    }
    this.maxTouches = Math.max(this.maxTouches, this.tracks.size, touches.length);
  }
}

/** Counts taps, and says when one completes a double tap. */
export class DoubleTap {
  private last: number | null = null;

  tap(timeMs: number): boolean {
    if (this.last !== null && timeMs - this.last <= DOUBLE_TAP_MS) {
      this.last = null;
      return true;
    }
    this.last = timeMs;
    return false;
  }
}

/** The pane a gesture leads to, as an index into the swipe order, or null
 * when it leads nowhere new. Swipes stop at either end rather than wrap. */
export function paneAfter(gesture: PaneGesture, current: number, count: number): number | null {
  const next = gesture === "agent" ? 0 : gesture === "next" ? current + 1 : current - 1;
  return next >= 0 && next < count && next !== current ? next : null;
}
