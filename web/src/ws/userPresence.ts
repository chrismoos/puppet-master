/** How long without input before the next input counts as coming back. */
export const RETURN_IDLE_MS = 60_000;

/** Pointer travel that counts as the user moving the mouse. Browsers repeat
 * a move at the same position when content changes under a resting cursor. */
export const POINTER_MOVE_MIN_PX = 4;

/**
 * Decides when the user comes back to this page. Away starts when the page is
 * hidden, the window loses focus, another viewer takes a terminal, or no input
 * arrives for RETURN_IDLE_MS. The first input while away is the return.
 */
export class PresenceTracker {
  private away = false;
  private lastInputAt: number;
  private pointerX = Number.NaN;
  private pointerY = Number.NaN;

  constructor(
    private now: () => number,
    private returned: () => void,
  ) {
    this.lastInputAt = now();
  }

  leave(): void {
    this.away = true;
  }

  input(): void {
    const now = this.now();
    const back = this.away || now - this.lastInputAt >= RETURN_IDLE_MS;
    this.lastInputAt = now;
    if (!back) return;
    this.away = false;
    this.returned();
  }

  pointerMoved(x: number, y: number): void {
    const moved = Math.abs(x - this.pointerX) >= POINTER_MOVE_MIN_PX
      || Math.abs(y - this.pointerY) >= POINTER_MOVE_MIN_PX;
    const first = Number.isNaN(this.pointerX);
    this.pointerX = x;
    this.pointerY = y;
    if (moved && !first) this.input();
  }
}

type ReturnListener = () => void;

const listeners = new Set<ReturnListener>();
let tracker: PresenceTracker | null = null;

function install(): PresenceTracker {
  const presence = new PresenceTracker(() => performance.now(), () => {
    for (const listener of listeners) listener();
  });
  const options = { capture: true, passive: true } as const;
  const input = () => presence.input();
  for (const type of ["pointerdown", "keydown", "wheel", "touchstart"] as const) {
    window.addEventListener(type, input, options);
  }
  window.addEventListener("pointermove", (event) => presence.pointerMoved(event.screenX, event.screenY), options);
  window.addEventListener("blur", () => presence.leave());
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") presence.leave();
  });
  return presence;
}

/** Calls `listener` each time the user comes back to the page. Returns the unsubscribe function. */
export function onUserReturn(listener: ReturnListener): () => void {
  tracker ??= install();
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Records that the user's attention went elsewhere, such as another viewer taking a terminal. */
export function markUserAway(): void {
  tracker?.leave();
}
