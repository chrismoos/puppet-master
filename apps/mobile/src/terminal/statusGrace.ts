/** First-open status grace: suppress intermediate statuses for a short
 *  window so the user sees a quiet indicator rather than "authorizing,
 *  connecting, ..." flashing. After the grace period or after the first
 *  terminal-state status, show normally. */

export const STATUS_GRACE_MS = 750;

export type StatusGraceSetter = (status: string) => void;

export interface StatusGraceController {
  /** Set the status, respecting the grace window. */
  setStatus(next: string): void;
  /** Clean up the grace timer. */
  dispose(): void;
}

export function createStatusGrace(rawSetter: StatusGraceSetter): StatusGraceController {
  let graceActive = true;
  let graceTimer: ReturnType<typeof setTimeout> | null = null;
  let pending = "loading";

  function setStatus(next: string) {
    pending = next;
    if (next === "online" || next === "rejected") {
      graceActive = false;
      if (graceTimer !== null) {
        clearTimeout(graceTimer);
        graceTimer = null;
      }
      rawSetter(next);
      return;
    }
    if (!graceActive) {
      rawSetter(next);
      return;
    }
    if (graceTimer === null) {
      graceTimer = setTimeout(() => {
        graceTimer = null;
        graceActive = false;
        rawSetter(pending);
      }, STATUS_GRACE_MS);
    }
  }

  function dispose() {
    if (graceTimer !== null) {
      clearTimeout(graceTimer);
      graceTimer = null;
    }
  }

  return { setStatus, dispose };
}
