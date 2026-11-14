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
  /** The muted line under the title, such as the session's project. */
  detail?: string;
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
  const [paused, setPaused] = useState(false);
  if (alerts.length === 0) return null;
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
      <div className="alert-card-head">
        <span className="alert-card-chip">{KIND_LABEL[alert.kind]}</span>
        <span className="alert-card-title">{alert.title}</span>
        <span className="alert-card-age">{age}</span>
        <button type="button" className="alert-card-close" aria-label="Dismiss" title="Dismiss" onClick={() => onDismiss(alert.key)}>×</button>
      </div>
      {alert.detail && <div className="alert-card-detail">{alert.detail}</div>}
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
      <span
        className="alert-card-timer"
        style={{ animationDuration: `${duration}ms`, animationPlayState: paused ? "paused" : "running" }}
        aria-hidden="true"
      />
    </article>
  );
}
