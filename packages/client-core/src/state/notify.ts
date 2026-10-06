import { sessionDisplayName, unescapeHtml } from "../format";
import { SessionAlertKind, type Project, type Session } from "../gen/pm/v1/pm_pb";

export interface NeedsInputNotification {
  title: string;
  body?: string;
}

/** Longest notification body in chars, ellipsis included. */
export const NOTIFY_BODY_MAX_CHARS = 180;

/** Truncates on a code point boundary, ending in an ellipsis when anything was cut. */
function truncateBody(text: string): string {
  const chars = Array.from(text);
  if (chars.length <= NOTIFY_BODY_MAX_CHARS) return text;
  return `${chars.slice(0, NOTIFY_BODY_MAX_CHARS - 1).join("").trimEnd()}…`;
}

/** What the person has to act on: the state detail, else the headline. */
function actionableBody(session: Session): string | undefined {
  const text = session.stateDetail.trim() || session.headline.trim();
  return text ? truncateBody(unescapeHtml(text)) : undefined;
}

/** User-facing copy for a session that needs input: the question as the body. */
export function needsInputNotification(
  session: Session,
  project?: Project,
): NeedsInputNotification {
  const name = sessionDisplayName(session);
  const title = project ? `${name} · ${project.name}` : name;
  const body = actionableBody(session);
  return body ? { title, body } : { title };
}

/**
 * User-facing copy for an alert the daemon raised. needs-input is worded
 * by needsInputNotification, which also names the project.
 */
export function sessionAlertNotification(
  session: Session,
  kind: SessionAlertKind,
): NeedsInputNotification {
  const title = sessionDisplayName(session);
  switch (kind) {
    case SessionAlertKind.FAILED: {
      const detail = session.stateDetail.trim();
      return { title, body: detail ? truncateBody(unescapeHtml(detail)) : "failed" };
    }
    default:
      return { title, body: "finished" };
  }
}

/** Matches the web NotificationPermission states; other platforms map into it. */
export type NotifyPermission = "default" | "denied" | "granted";

/**
 * Why a platform cannot deliver notifications: the API is absent, or
 * the page is on an insecure origin where browsers refuse permission
 * outright (plain HTTP anywhere other than localhost).
 */
export type NotifyUnsupportedReason = "unavailable" | "insecure-context";

export type NotifySupport =
  | { supported: true }
  | { supported: false; reason: NotifyUnsupportedReason };

/**
 * Maps a platform's raw capability signals to support-or-why-not.
 * `secureContext` is undefined on platforms without the concept, which
 * only leaves the API check.
 */
export function notifySupport(hasApi: boolean, secureContext: boolean | undefined): NotifySupport {
  if (!hasApi) return { supported: false, reason: "unavailable" };
  if (secureContext === false) return { supported: false, reason: "insecure-context" };
  return { supported: true };
}

/** Short enough for the sidebar row, which sits beside a terse toggle. The
 * sentence explaining it belongs in a tooltip, not in the row. */
export function notifyUnsupportedLabel(reason: NotifyUnsupportedReason): string {
  switch (reason) {
    case "insecure-context":
      return "notifications need HTTPS";
    case "unavailable":
      return "notifications unavailable";
  }
}

/** User-facing copy for a reason notifications cannot be enabled. */
export function notifyUnsupportedMessage(reason: NotifyUnsupportedReason): string {
  switch (reason) {
    case "insecure-context":
      return "Notifications need HTTPS or localhost. This page was opened over plain HTTP, so the browser will not allow them.";
    case "unavailable":
      return "This browser does not support notifications.";
  }
}

/**
 * Whether to auto-request notification permission on load. Only when
 * the platform supports notifications, we have not asked before, and
 * the user has not already decided (permission still "default"), so a
 * decline is never re-prompted automatically.
 */
export function shouldAutoRequestNotify(
  supported: boolean,
  alreadyAsked: boolean,
  permission: NotifyPermission,
): boolean {
  return supported && !alreadyAsked && permission === "default";
}
