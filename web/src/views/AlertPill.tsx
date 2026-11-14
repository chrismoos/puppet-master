import { useEffect, useRef, useState } from "react";

export type InAppAlertKind = "needs-input" | "finished" | "failed" | "approval";

export interface InAppAlert {
  /** One alert per subject and kind: a newer one replaces it. */
  key: string;
  kind: InAppAlertKind;
  title: string;
  body?: string;
  sessionId?: string;
  approvalId?: string;
}

/** How long the pill stays up untouched, by the kind of its newest alert. */
const AUTO_HIDE_MS: Record<InAppAlertKind, number> = {
  "needs-input": 9_000,
  approval: 9_000,
  failed: 7_000,
  finished: 5_000,
};

const KIND_LABEL: Record<InAppAlertKind, string> = {
  "needs-input": "needs you",
  approval: "approval",
  failed: "failed",
  finished: "finished",
};

/** True when the person is looking at this page, so an alert can be drawn in it rather than sent to the desktop. */
export function pageIsWatched(doc: Document = document): boolean {
  return doc.visibilityState === "visible" && doc.hasFocus();
}

/**
 * The newest alert as a slim pill at the top right, with a count of the rest.
 * Clicking it opens what it is about, × dismisses everything, and it hides on
 * its own unless the pointer rests on it.
 */
export function AlertPill({
  alerts,
  onOpen,
  onDismiss,
}: {
  /** Newest first. */
  alerts: readonly InAppAlert[];
  onOpen: (alert: InAppAlert) => void;
  onDismiss: () => void;
}) {
  const newest = alerts[0];
  const timer = useRef<number | null>(null);
  // Restarts the countdown line together with the timer.
  const [cycle, setCycle] = useState(0);
  const [paused, setPaused] = useState(false);
  const dismiss = useRef(onDismiss);
  dismiss.current = onDismiss;
  const stop = () => {
    if (timer.current !== null) window.clearTimeout(timer.current);
    timer.current = null;
  };
  const start = () => {
    stop();
    setPaused(false);
    setCycle((n) => n + 1);
    if (newest) timer.current = window.setTimeout(() => dismiss.current(), AUTO_HIDE_MS[newest.kind]);
  };
  const pause = () => {
    stop();
    setPaused(true);
  };
  useEffect(() => {
    start();
    return stop;
  }, [newest?.key, newest?.kind]);
  if (!newest) return null;
  const rest = alerts.length - 1;
  return (
    <div className={`alert-pill alert-pill-${newest.kind}`} role="status" aria-live="polite" onMouseEnter={pause} onMouseLeave={start}>
      <button type="button" className="alert-pill-main" title="Open" onClick={() => onOpen(newest)}>
        <span className="alert-pill-dot" aria-hidden="true" />
        <span className="alert-pill-kind">{KIND_LABEL[newest.kind]}</span>
        <span className="alert-pill-title">{newest.title}</span>
        {newest.body && <span className="alert-pill-body">{newest.body}</span>}
        {rest > 0 && <span className="alert-pill-more">+{rest}</span>}
      </button>
      <button type="button" className="alert-pill-close" aria-label="Dismiss" title="Dismiss" onClick={onDismiss}>×</button>
      {!paused && <span key={cycle} className="alert-pill-timer" style={{ animationDuration: `${AUTO_HIDE_MS[newest.kind]}ms` }} aria-hidden="true" />}
    </div>
  );
}
