/** Reads pane gestures from raw touches on a terminal or page pane:
 * a one-finger swipe moves to the next or previous pane, and a two-finger
 * double-tap or two-finger swipe right goes back to the agent. It only
 * watches touches, so the view under it keeps its own scrolling and
 * selection. */

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
/** Swipes slower than this are drags or selections, not pane changes. */
const SWIPE_MAX_MS = 700;
/** Touches starting this close to the left edge belong to the system's back gesture. */
const EDGE_ZONE = 24;
/** A two-finger tap moves less than this and lifts within TAP_MAX_MS. */
const TAP_SLOP = 12;
const TAP_MAX_MS = 300;
/** The second two-finger tap must come within this of the first. */
const DOUBLE_TAP_MS = 400;

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
  private lastTwoFingerTapAt: number | null = null;

  /** False where the view scrolls sideways itself, which leaves only the two-finger gestures. */
  swipes: boolean;

  constructor(options: { swipes: boolean } = { swipes: true }) {
    this.swipes = options.swipes;
  }

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
    if (touches === 1) return this.oneFinger(tracks[0], duration);
    if (touches === 2) return this.twoFingers(tracks, duration, timeMs);
    return null;
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

  private oneFinger(track: Track | undefined, duration: number): PaneGesture | null {
    if (!track || !this.swipes || track.startX < EDGE_ZONE || duration > SWIPE_MAX_MS) return null;
    const dx = track.x - track.startX;
    const dy = track.y - track.startY;
    if (Math.abs(dx) < SWIPE_MIN || Math.abs(dx) < SWIPE_RATIO * Math.abs(dy)) return null;
    return dx < 0 ? "next" : "previous";
  }

  private twoFingers(tracks: Track[], duration: number, timeMs: number): PaneGesture | null {
    if (tracks.length < 2) return null;
    const dx = tracks.reduce((sum, t) => sum + (t.x - t.startX), 0) / tracks.length;
    const dy = tracks.reduce((sum, t) => sum + (t.y - t.startY), 0) / tracks.length;
    if (dx >= SWIPE_MIN && dx >= SWIPE_RATIO * Math.abs(dy)) {
      this.lastTwoFingerTapAt = null;
      return "agent";
    }
    const tapped = duration <= TAP_MAX_MS
      && tracks.every((t) => Math.hypot(t.x - t.startX, t.y - t.startY) < TAP_SLOP);
    if (!tapped) return null;
    if (this.lastTwoFingerTapAt !== null && timeMs - this.lastTwoFingerTapAt <= DOUBLE_TAP_MS) {
      this.lastTwoFingerTapAt = null;
      return "agent";
    }
    this.lastTwoFingerTapAt = timeMs;
    return null;
  }
}

/** The pane a gesture leads to, as an index into the swipe order, or null
 * when it leads nowhere new. Swipes stop at either end rather than wrap. */
export function paneAfter(gesture: PaneGesture, current: number, count: number): number | null {
  const next = gesture === "agent" ? 0 : gesture === "next" ? current + 1 : current - 1;
  return next >= 0 && next < count && next !== current ? next : null;
}
