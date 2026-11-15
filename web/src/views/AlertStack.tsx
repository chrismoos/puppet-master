import { useEffect, useRef, useState } from "react";
import { AgentNotificationLinkKind, type AgentNotificationLink } from "@puppet-master/client-core/gen/pm/v1/pm_pb";

export type InAppAlertKind = "needs-input" | "finished" | "failed" | "approval" | "message";

export type InAppAlertLinkKind = "page" | "preview" | "item";

export interface InAppAlertLink {
  kind: InAppAlertLinkKind;
  title: string;
  /** A forward's public URL, or a dashboard route for an item. */
  url: string;
}

export interface InAppAlert {
  /** One alert per subject and kind: a newer one replaces it. */
  key: string;
  kind: InAppAlertKind;
  /** The session's name, or what an approval is about. */
  title: string;
  /** The first words of the body in bold, used for an agent message's title. */
  heading?: string;
  body?: string;
  sessionId?: string;
  approvalId?: string;
  link?: InAppAlertLink;
  raisedAtMs: number;
}

/** How long a card stays up untouched, by its kind. */
export const AUTO_HIDE_MS: Record<InAppAlertKind, number> = {
  "needs-input": 9_000,
  approval: 9_000,
  message: 9_000,
  failed: 7_000,
  finished: 5_000,
};

export const MAX_VISIBLE_ALERTS = 3;

const KIND_LABEL: Record<InAppAlertKind, string> = {
  "needs-input": "needs you",
  approval: "approval",
  message: "message",
  failed: "failed",
  finished: "finished",
};

const ICON_PATHS: Record<InAppAlertKind, readonly string[]> = {
  "needs-input": ["M9.1 9.2a3 3 0 1 1 4.3 2.7c-.9.4-1.4 1.1-1.4 2v.6", "M12 17.6h.01"],
  approval: ["M12 3.2l7 2.9v5.1c0 4.4-2.9 7.9-7 9.6-4.1-1.7-7-5.2-7-9.6V6.1l7-2.9z", "M9 12.2l2.1 2.1 4-4.2"],
  failed: ["M7.5 7.5l9 9", "M16.5 7.5l-9 9"],
  finished: ["M5.5 12.6l4.2 4.2 8.8-9.3"],
  message: ["M6.5 16V11a5.5 5.5 0 0 1 11 0v5l1.5 2h-14z", "M10 20.5a2 2 0 0 0 4 0"],
};

function AlertIcon({ kind }: { kind: InAppAlertKind }) {
  return (
    <span className="alert-card-icon" title={KIND_LABEL[kind]} aria-hidden="true">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round">
        {ICON_PATHS[kind].map((d) => <path key={d} d={d} />)}
      </svg>
    </span>
  );
}

const LINK_LABEL: Record<InAppAlertLinkKind, string> = {
  page: "published page",
  preview: "preview",
  item: "board item",
};

const MS_PER_MINUTE = 60_000;
const MINUTES_PER_HOUR = 60;

/** True when the person is looking at this page, so an alert can be drawn in it rather than sent to the desktop. */
export function pageIsWatched(doc: Document = document): boolean {
  return doc.visibilityState === "visible" && doc.hasFocus();
}

export function alertLinkFrom(link: AgentNotificationLink | undefined): InAppAlertLink | undefined {
  if (!link?.url) return undefined;
  const kind: InAppAlertLinkKind | undefined = {
    [AgentNotificationLinkKind.PAGE]: "page" as const,
    [AgentNotificationLinkKind.PREVIEW]: "preview" as const,
    [AgentNotificationLinkKind.ITEM]: "item" as const,
  }[link.kind as number];
  return kind ? { kind, title: link.title, url: link.url } : undefined;
}

export function relativeAge(raisedAtMs: number, nowMs: number): string {
  const minutes = Math.floor((nowMs - raisedAtMs) / MS_PER_MINUTE);
  if (minutes < 1) return "just now";
  if (minutes < MINUTES_PER_HOUR) return `${minutes}m ago`;
  return `${Math.floor(minutes / MINUTES_PER_HOUR)}h ago`;
}

/**
 * The newest alerts as cards stacked at the top right. Clicking a card opens
 * what it is about, × dismisses that card, and each card leaves on its own
 * unless the pointer rests on the stack.
 */
export function AlertStack({
  alerts,
  onOpen,
  onOpenLink,
  onDismiss,
  onDismissAll,
  now = Date.now,
}: {
  /** Newest first. */
  alerts: readonly InAppAlert[];
  onOpen: (alert: InAppAlert) => void;
  onOpenLink: (alert: InAppAlert) => void;
  onDismiss: (key: string) => void;
  onDismissAll: () => void;
  now?: () => number;
}) {
  if (alerts.length === 0) return null;
  return (
    <ShownStack
      alerts={alerts}
      onOpen={onOpen}
      onOpenLink={onOpenLink}
      onDismiss={onDismiss}
      onDismissAll={onDismissAll}
      now={now}
    />
  );
}

/**
 * Mounted only while a card is up, so the pause it holds ends with the stack.
 * A stack emptied under the pointer never receives its mouseleave.
 */
function ShownStack({
  alerts,
  onOpen,
  onOpenLink,
  onDismiss,
  onDismissAll,
  now,
}: {
  alerts: readonly InAppAlert[];
  onOpen: (alert: InAppAlert) => void;
  onOpenLink: (alert: InAppAlert) => void;
  onDismiss: (key: string) => void;
  onDismissAll: () => void;
  now: () => number;
}) {
  const [paused, setPaused] = useState(false);
  const visible = alerts.slice(0, MAX_VISIBLE_ALERTS);
  const hidden = alerts.length - visible.length;
  const nowMs = now();
  return (
    <section
      className="alert-stack"
      aria-label="Alerts"
      aria-live="polite"
      onMouseEnter={() => setPaused(true)}
      onMouseLeave={() => setPaused(false)}
    >
      {visible.map((alert, index) => (
        <AlertCard
          key={`${alert.key}@${alert.raisedAtMs}`}
          alert={alert}
          compact={index > 0}
          paused={paused}
          age={relativeAge(alert.raisedAtMs, nowMs)}
          onOpen={onOpen}
          onOpenLink={onOpenLink}
          onDismiss={onDismiss}
        />
      ))}
      {alerts.length > 1 && (
        <div className="alert-stack-footer">
          {hidden > 0 && <span className="alert-stack-more">+{hidden} more</span>}
          <button type="button" className="alert-stack-clear" onClick={onDismissAll}>Dismiss all</button>
        </div>
      )}
    </section>
  );
}

function AlertCard({
  alert,
  compact,
  paused,
  age,
  onOpen,
  onOpenLink,
  onDismiss,
}: {
  alert: InAppAlert;
  compact: boolean;
  paused: boolean;
  age: string;
  onOpen: (alert: InAppAlert) => void;
  onOpenLink: (alert: InAppAlert) => void;
  onDismiss: (key: string) => void;
}) {
  const duration = AUTO_HIDE_MS[alert.kind];
  const remaining = useRef(duration);
  const startedAt = useRef(0);
  const dismiss = useRef(onDismiss);
  dismiss.current = onDismiss;
  useEffect(() => {
    if (paused) return;
    startedAt.current = Date.now();
    const timer = window.setTimeout(() => dismiss.current(alert.key), remaining.current);
    return () => {
      window.clearTimeout(timer);
      remaining.current = Math.max(0, remaining.current - (Date.now() - startedAt.current));
    };
  }, [paused, alert.key]);
  const link = alert.link;
  const external = link !== undefined && link.kind !== "item";
  return (
    <article className={`alert-card alert-card-${alert.kind}${compact ? " is-compact" : ""}`} aria-label={`${KIND_LABEL[alert.kind]}: ${alert.title}`}>
      <button type="button" className="alert-card-open" aria-label={`Open ${alert.title}`} onClick={() => onOpen(alert)} />
      <AlertIcon kind={alert.kind} />
      <div className="alert-card-head">
        <span className="alert-card-title">{alert.title}</span>
        <span className="alert-card-age">{age}</span>
        <button type="button" className="alert-card-close" aria-label="Dismiss" title="Dismiss" onClick={() => onDismiss(alert.key)}>×</button>
      </div>
      {(alert.heading || alert.body) && (
        <p className="alert-card-body">
          {alert.heading && <strong>{alert.heading}</strong>}
          {alert.heading && alert.body && " "}
          {alert.body}
        </p>
      )}
      {link && (
        <a
          className="alert-card-link"
          href={external ? link.url : `#${link.url}`}
          {...(external ? { target: "_blank", rel: "noopener noreferrer" } : {})}
          onClick={() => onOpenLink(alert)}
        >
          <span className="alert-card-link-icon" aria-hidden="true">↗</span>
          <span className="alert-card-link-title">{link.title}</span>
          <span className="alert-card-link-kind">{LINK_LABEL[link.kind]}</span>
        </a>
      )}
    </article>
  );
}
