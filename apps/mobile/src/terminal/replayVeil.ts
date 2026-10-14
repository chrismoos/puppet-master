/** Replay veil state machine: hides the terminal during scrollback replay
 *  so the user sees the final position instantly instead of watching the
 *  feed scroll from the top. The veil is set on init, held through replay,
 *  and cleared only after revealAtBottom scrolls to the end, forces layout,
 *  and waits for a display refresh (or on a fallback timeout, an online
 *  socket that no replay follows, or an error — each going through the same
 *  atomic reveal).
 *
 *  Keyboard resizes are held natively (holdForResize), not here. */

export const VEIL_TIMEOUT_MS = 3_000;
/** How long an online socket waits for its replay before showing the
 *  terminal without one. The replay follows online within a round trip;
 *  revealing on online itself showed an empty screen just before it. */
export const ONLINE_GRACE_MS = 800;

export type VeilEvent =
  | "init"
  | "replay"
  | "painted"
  | "online"
  | "error"
  | "timeout";

export interface VeilController {
  /** Process a veil event and return the new veiled state. */
  event(kind: VeilEvent): boolean;
  /** Current veiled state. */
  veiled(): boolean;
  /** Clean up the fallback timer. */
  dispose(): void;
}

/**
 * Pure-ish state machine for the replay veil. The only side effect is the
 * fallback timer, which fires the onTimeout callback after VEIL_TIMEOUT_MS
 * so a stalled replay never leaves a blank screen.
 *
 * The caller is responsible for calling revealAtBottom before advancing the
 * state machine on every unveiling edge (painted, online-without-replay,
 * error, timeout).
 *
 * Transitions:
 *   init      → veiled (start timeout)
 *   replay    → veiled (reset timeout)
 *   painted   → unveiled (clear timeout)
 *   online    → if no replay was received, unveiled after ONLINE_GRACE_MS
 *   error     → unveiled
 *   timeout   → unveiled (via onTimeout callback)
 */
export function createReplayVeil(
  setter: (veiled: boolean) => void,
  onTimeout?: () => void,
): VeilController {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let hadReplay = false;
  let state = false;

  function set(next: boolean) {
    if (next !== state) {
      state = next;
      setter(next);
    }
  }

  function clearTimer() {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  }

  function startTimer(ms: number = VEIL_TIMEOUT_MS) {
    clearTimer();
    timer = setTimeout(() => {
      timer = null;
      if (onTimeout) {
        onTimeout();
      } else {
        set(false);
      }
    }, ms);
  }

  function event(kind: VeilEvent): boolean {
    switch (kind) {
      case "init":
        hadReplay = false;
        set(true);
        startTimer();
        break;
      case "replay":
        hadReplay = true;
        if (!state) set(true);
        startTimer();
        break;
      case "painted":
        clearTimer();
        set(false);
        break;
      case "online":
        if (state && !hadReplay) startTimer(ONLINE_GRACE_MS);
        break;
      case "error":
      case "timeout":
        clearTimer();
        set(false);
        break;
    }
    return state;
  }

  return {
    event,
    veiled: () => state,
    dispose() { clearTimer(); },
  };
}
