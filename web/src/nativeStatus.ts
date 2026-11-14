import type { TerminalStreamStatus } from "@puppet-master/client-core/ws/terminalSocket";

/** A refusal close code from the controller: the worker's terminal engine
 * differs from the controller's, or this page's does. */
export const CLOSE_INCOMPATIBLE = 4412;
export const CLOSE_PAGE_OUTDATED = 4413;
/** Quiet this long before saying anything, so a quick attach shows nothing. */
export const STATUS_DELAY_MS = 750;
/** Offer a manual retry once attaching has failed for this long. */
export const MANUAL_RETRY_DELAY_MS = 10_000;
const REATTACH_MIN_MS = 250;
const REATTACH_MAX_MS = 4_000;

/** What a native terminal's socket has reported, as its status needs it. */
export interface NativeLink {
  online: boolean;
  /** When the current wait for a screen began, or null while online. */
  waitingSince: number | null;
  lastError: string;
  refusal: "incompatible" | "page-outdated" | null;
}

export const ATTACHING: (now: number) => NativeLink = (now) => ({
  online: false,
  waitingSince: now,
  lastError: "",
  refusal: null,
});

export function onlineLink(): NativeLink {
  return { online: true, waitingSince: null, lastError: "", refusal: null };
}

/** The link after its socket closed with `code`, if any. */
export function closedLink(link: NativeLink, reason: string, code: number | null, now: number): NativeLink {
  const refusal = code === CLOSE_INCOMPATIBLE ? "incompatible" : code === CLOSE_PAGE_OUTDATED ? "page-outdated" : null;
  return {
    online: false,
    waitingSince: link.waitingSince ?? now,
    lastError: reason,
    refusal,
  };
}

/** The status a browser terminal would show in the same situation, or null
 * for nothing. `unavailable` names why the worker cannot be reached. */
export function nativeStreamStatus(link: NativeLink, unavailable: string | null, now: number): TerminalStreamStatus | null {
  if (link.online) return null;
  if (unavailable) return { phase: "reconnecting", canRetry: false, lastError: unavailable, problem: "worker" };
  if (link.refusal) {
    return { phase: "reconnecting", canRetry: true, lastError: link.lastError || "terminal engine mismatch", problem: link.refusal };
  }
  const waited = link.waitingSince === null ? 0 : now - link.waitingSince;
  if (waited < STATUS_DELAY_MS) return null;
  if (waited < MANUAL_RETRY_DELAY_MS) return { phase: "reconnecting", canRetry: false };
  return {
    phase: "reconnecting",
    canRetry: true,
    lastError: link.lastError || "terminal connection unavailable",
    problem: "connection",
  };
}

/** The wait before attaching again after the `attempt`-th failure in a row. */
export function reattachDelay(attempt: number): number {
  return Math.min(REATTACH_MAX_MS, REATTACH_MIN_MS * 2 ** Math.max(0, attempt));
}
