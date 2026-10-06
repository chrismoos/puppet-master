import { authedFetch } from "./token";

/** Interaction, not presence.
 *
 * The daemon holds back push to a user's phone while that user is
 * working in the browser. A tab that is merely open — visible, focused,
 * connected, repainting a terminal on a second monitor — is not someone
 * at the keyboard, so only these four events count, and only while the
 * page is visible.
 */

const ACTIVITY_ENDPOINT = "/api/user/activity";

/** Capture phase, because xterm consumes keystrokes before they bubble. */
const LISTENER_OPTIONS: AddEventListenerOptions = { capture: true, passive: true };

export const ACTIVITY_EVENTS = ["pointerdown", "keydown", "wheel", "touchstart"] as const;

/** One report stands for this long, so a typing burst is one request. */
export const ACTIVITY_REPORT_INTERVAL_MS = 30_000;

interface ActivityTarget {
  addEventListener(type: string, listener: () => void, options: AddEventListenerOptions): void;
  removeEventListener(type: string, listener: () => void, options: EventListenerOptions): void;
}

export interface ActivityReporterOptions {
  target: ActivityTarget;
  visible: () => boolean;
  report: () => void;
  now?: () => number;
  intervalMs?: number;
}

/** Starts reporting interaction; the returned function stops it. */
export function startActivityReporter({
  target,
  visible,
  report,
  now = () => Date.now(),
  intervalMs = ACTIVITY_REPORT_INTERVAL_MS,
}: ActivityReporterOptions): () => void {
  let reportedAt: number | null = null;
  const interacted = () => {
    if (!visible()) return;
    const at = now();
    if (reportedAt !== null && at - reportedAt < intervalMs) return;
    reportedAt = at;
    report();
  };
  for (const event of ACTIVITY_EVENTS) {
    target.addEventListener(event, interacted, LISTENER_OPTIONS);
  }
  return () => {
    for (const event of ACTIVITY_EVENTS) {
      target.removeEventListener(event, interacted, LISTENER_OPTIONS);
    }
  };
}

export function startWebActivityReporting(): () => void {
  return startActivityReporter({
    target: document,
    visible: () => document.visibilityState === "visible",
    // A dropped heartbeat costs at most one early notification, and the
    // next interaction reports again, so a failure is not worth raising.
    report: () => void authedFetch(ACTIVITY_ENDPOINT, { method: "PUT" }).catch(() => {}),
  });
}
