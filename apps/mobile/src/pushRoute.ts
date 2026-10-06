// Pure function that resolves a push notification payload into a
// navigation target, or a reason why it cannot. Extracted from App.tsx
// so the decision is unit-testable without React or expo-notifications.

import { sessionDisplayName } from "@puppet-master/client-core/format"
import { TerminalKind, type Session, type Terminal } from "@puppet-master/client-core/gen/pm/v1/pm_pb"

import type { TerminalTarget } from "./screens/SessionsScreen"

/** Storage key for the identifier of the last routed notification
 * response. Must be included in the hydration key list
 * so it is available synchronously before the launch tap check runs. */
export const LAST_ROUTED_NOTIFICATION_KEY = "push.lastRoutedNotification"

/** The routing-relevant slice of a notification response payload.
 *
 * The daemon sends `pm` carrying only `pm_sealed`. The notification
 * service extension replaces it with the routing fields it decrypted,
 * so a payload that still has `pm_sealed` is one the extension never
 * opened. */
export interface NotificationData {
  pm?: {
    sessionId?: unknown
    /** Written by the extension from the sealed approval_id, and from nowhere else. */
    approvalId?: unknown
    controllerId?: unknown
    pm_sealed?: unknown
    [key: string]: unknown
  }
  [key: string]: unknown
}

export type RouteResult =
  | { ok: true; target: TerminalTarget }
  | { ok: false; reason: "no-session-id" | "session-not-found" | "no-terminal" }

/** Resolves a push notification's data payload into a TerminalTarget.
 *
 * The function is pure: it reads sessionId from the payload, looks the
 * session up in the provided maps, and finds its primary terminal
 * (agent first, then any). It never mutates state or calls the network.
 *
 * sessionId arrives as a string. The sealed JSON holds it as a number
 * and the extension stringifies it, because the session maps are keyed
 * by string id.
 *
 * controllerId is deliberately ignored. The app supports a single
 * controller, so every notification that arrives belongs to it. When
 * multi-controller support is added, the controllerId should be checked
 * against the active controller's ref before routing. */
export function resolveNotificationRoute(
  data: NotificationData | undefined,
  sessions: ReadonlyMap<string, Session>,
  terminals: ReadonlyMap<string, Terminal>,
): RouteResult {
  const sessionIdStr = data?.pm?.sessionId
  if (typeof sessionIdStr !== "string" || !sessionIdStr) {
    return { ok: false, reason: "no-session-id" }
  }

  // Sessions are keyed by their string id in the state map.
  const session = sessions.get(sessionIdStr)
  if (!session) {
    return { ok: false, reason: "session-not-found" }
  }

  // Find the primary terminal: prefer agent, fall back to any.
  let agentTerminal: Terminal | undefined
  let anyTerminal: Terminal | undefined
  for (const [, t] of terminals) {
    if (t.sessionId === session.id) {
      if (t.kind === TerminalKind.AGENT) {
        agentTerminal = t
        break
      }
      if (!anyTerminal) anyTerminal = t
    }
  }
  const terminal = agentTerminal ?? anyTerminal
  if (!terminal) {
    return { ok: false, reason: "no-terminal" }
  }

  return {
    ok: true,
    target: {
      sessionId: session.id,
      terminalId: terminal.id,
      generation: terminal.generation,
      title: sessionDisplayName(session),
    },
  }
}

const APPROVAL_ID = /^[A-Za-z0-9_-]{1,128}$/

/** The approval id of a notification whose seal was opened, or null; an id beside an unopened seal is never trusted. */
export function notificationApprovalId(data: NotificationData | undefined): string | null {
  if (sealState(data) !== "decrypted") return null
  const id = data?.pm?.approvalId
  return typeof id === "string" && APPROVAL_ID.test(id) ? id : null
}

/** What the app should do when a notification response arrives. */
export type NotificationAction =
  | { kind: "navigate"; target: TerminalTarget }
  | { kind: "approval"; approvalId: string }
  | { kind: "alert"; title: string; message: string }
  | { kind: "ignore" }

/** The routing-relevant slice of a NotificationResponse. Avoids
 * importing the expo-notifications type into the pure module. */
export interface ResponsePayload {
  data: NotificationData | undefined
  identifier: string
}

/** The trigger shape expo-notifications serialises for a remote push.
 * The full userInfo lives in `payload`; `type` is always "push". */
interface PushTrigger {
  type: "push"
  payload?: Record<string, unknown>
}

function hasRoutingData(value: unknown): value is NotificationData {
  return typeof value === "object" && value !== null
    && typeof (value as NotificationData).pm === "object" && (value as NotificationData).pm !== null
}

/** Extracts routing data from an expo-notifications response.
 *
 * Routing reaches the app only through the notification service
 * extension, which writes the decrypted fields to `userInfo["pm"]` and
 * to `userInfo["body"]["pm"]`. On iOS expo-notifications serialises a
 * remote notification's `content.data` from `userInfo["body"]`, while
 * `trigger.payload` carries the whole userInfo. Either may be missing
 * depending on the notification kind and the library version, so the
 * first candidate that actually carries `pm` wins. */
export function extractPayload(
  contentData: unknown,
  trigger: unknown,
  identifier: string,
): ResponsePayload {
  const pushTrigger = trigger as PushTrigger | null | undefined
  const candidates: unknown[] = [
    pushTrigger?.type === "push" ? pushTrigger.payload : undefined,
    contentData,
    (contentData as { body?: unknown } | null | undefined)?.body,
  ]
  const data = candidates.find(hasRoutingData)
    ?? (candidates.find((c) => c !== undefined && c !== null) as NotificationData | undefined)
  return { data, identifier }
}

/** Whether the notification service extension opened the seal.
 *
 * A payload still holding `pm_sealed` was never decrypted, which is
 * the one reason a tap can carry no session id at all. Distinguishing
 * it from a payload with no `pm` keeps a silent no-op readable in the
 * device log. */
export type SealState = "decrypted" | "undecrypted" | "no-pm"

export function sealState(data: NotificationData | undefined): SealState {
  if (!data?.pm) return "no-pm"
  return typeof data.pm.pm_sealed === "string" ? "undecrypted" : "decrypted"
}

/** One line describing a response for the [push] log, with no content
 * from the notification itself. */
export function describePayload(payload: ResponsePayload): string {
  const keys = payload.data ? Object.keys(payload.data).join(",") : "none"
  const sessionId = payload.data?.pm?.sessionId
  const session = typeof sessionId === "string" && sessionId ? sessionId : "missing"
  const approval = notificationApprovalId(payload.data) ? "present" : "none"
  return `id=${payload.identifier || "?"} keys=${keys} session=${session}`
    + ` approval=${approval} seal=${sealState(payload.data)}`
}

/** Decides what to do when a notification response arrives.
 *
 * This is the extracted body of the routeNotification callback in
 * App.tsx. It resolves the payload against live client state and
 * returns an action the caller applies (setScreen, Alert.alert, or
 * nothing). Extracting it makes the wiring testable without React. */
export function notificationResponseAction(
  payload: ResponsePayload,
  sessions: ReadonlyMap<string, Session>,
  terminals: ReadonlyMap<string, Terminal>,
): NotificationAction {
  const approvalId = notificationApprovalId(payload.data)
  if (approvalId) return { kind: "approval", approvalId }
  const result = resolveNotificationRoute(payload.data, sessions, terminals)
  if (result.ok) return { kind: "navigate", target: result.target }
  if (result.reason === "no-session-id") return { kind: "ignore" }
  if (result.reason === "session-not-found") {
    return { kind: "alert", title: "Session unavailable", message: "The session from this notification is no longer available." }
  }
  return { kind: "alert", title: "Session unavailable", message: "The session has no open terminal." }
}

/** What to do with a response the moment it arrives. Before the first
 * snapshot the session and terminal maps are empty and every route would
 * fail, so the response is held and decided again once state hydrates.
 * That covers a launch from a tap and a tap that lands while the app is
 * still connecting after a launch. */
export type TapDecision = { kind: "hold" } | NotificationAction

export function tapDecision(
  hydrated: boolean,
  payload: ResponsePayload,
  sessions: ReadonlyMap<string, Session>,
  terminals: ReadonlyMap<string, Terminal>,
): TapDecision {
  // An approval is fetched by id, so it needs no session snapshot and opens at once.
  const approvalId = notificationApprovalId(payload.data)
  if (approvalId) return { kind: "approval", approvalId }
  if (!hydrated) return { kind: "hold" }
  return notificationResponseAction(payload, sessions, terminals)
}

/** Whether the response the app launched with is a fresh tap to route,
 * a tap already routed by an earlier process, or absent. */
export function launchTapDecision(
  response: ResponsePayload | null,
  lastRoutedIdentifier: string | null,
): "route" | "duplicate" | "none" {
  if (!response) return "none"
  return shouldRouteColdStart(response.identifier, lastRoutedIdentifier) ? "route" : "duplicate"
}

/** Decides whether the response the app launched with should be routed.
 *
 * The last response is what the user most recently tapped, and a build
 * that keeps it across process lifetimes would re-navigate on every
 * launch. The response has no tap timestamp, so identity is the guard:
 * the request identifier is unique per delivered notification, and one
 * that was already routed is skipped. */
export function shouldRouteColdStart(
  responseIdentifier: string,
  lastRoutedIdentifier: string | null,
): boolean {
  if (!responseIdentifier) return false
  return responseIdentifier !== lastRoutedIdentifier
}
