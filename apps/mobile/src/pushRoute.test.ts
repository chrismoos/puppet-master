import { create } from "@bufbuild/protobuf"
import { describe, expect, it } from "vitest"

import {
  SessionSchema,
  TerminalSchema,
  TerminalKind,
  type Session,
  type Terminal,
} from "@puppet-master/client-core/gen/pm/v1/pm_pb"

import {
  resolveNotificationRoute,
  shouldRouteColdStart,
  notificationResponseAction,
  launchTapDecision,
  tapDecision,
  describePayload,
  extractPayload,
  sealState,
  notificationApprovalId,
  type NotificationData,
  type ResponsePayload,
} from "./pushRoute"

function session(id: bigint, headline = ""): Session {
  return create(SessionSchema, { id, headline })
}

function terminal(id: bigint, sessionId: bigint, kind = TerminalKind.AGENT): Terminal {
  return create(TerminalSchema, { id, sessionId, kind, generation: 1n })
}

function sessions(...list: Session[]): ReadonlyMap<string, Session> {
  return new Map(list.map((s) => [s.id.toString(), s]))
}

function terminals(...list: Terminal[]): ReadonlyMap<string, Terminal> {
  return new Map(list.map((t) => [t.id.toString(), t]))
}

describe("resolveNotificationRoute", () => {
  it("routes a well-formed payload to the session terminal", () => {
    const data: NotificationData = { pm: { sessionId: "42" } }
    const result = resolveNotificationRoute(
      data,
      sessions(session(42n, "My session")),
      terminals(terminal(100n, 42n)),
    )
    expect(result).toEqual({
      ok: true,
      target: {
        sessionId: 42n,
        terminalId: 100n,
        generation: 1n,
        title: "My session",
      },
    })
  })

  it("titles the terminal with the session goal before the headline", () => {
    const data: NotificationData = { pm: { sessionId: "7" } }
    const named = create(SessionSchema, { id: 7n, goal: "Moving auth to JWTs", headline: "swapping cookies" })
    const result = resolveNotificationRoute(
      data,
      sessions(named),
      terminals(terminal(10n, 7n)),
    )
    expect(result.ok && result.target.title).toBe("Moving auth to JWTs")
  })

  it("uses the session id as fallback title when the session has no name", () => {
    const data: NotificationData = { pm: { sessionId: "7" } }
    const result = resolveNotificationRoute(
      data,
      sessions(session(7n)),
      terminals(terminal(10n, 7n)),
    )
    expect(result.ok && result.target.title).toBe("session 7")
  })

  it("prefers the agent terminal over a shell terminal", () => {
    const data: NotificationData = { pm: { sessionId: "1" } }
    const result = resolveNotificationRoute(
      data,
      sessions(session(1n)),
      terminals(
        terminal(10n, 1n, TerminalKind.SHELL),
        terminal(20n, 1n, TerminalKind.AGENT),
      ),
    )
    expect(result.ok && result.target.terminalId).toBe(20n)
  })

  it("falls back to any terminal when no agent terminal exists", () => {
    const data: NotificationData = { pm: { sessionId: "1" } }
    const result = resolveNotificationRoute(
      data,
      sessions(session(1n)),
      terminals(terminal(10n, 1n, TerminalKind.SHELL)),
    )
    expect(result.ok && result.target.terminalId).toBe(10n)
  })

  it("returns no-session-id for undefined data", () => {
    const result = resolveNotificationRoute(undefined, sessions(), terminals())
    expect(result).toEqual({ ok: false, reason: "no-session-id" })
  })

  it("returns no-session-id for missing pm key", () => {
    const result = resolveNotificationRoute({}, sessions(), terminals())
    expect(result).toEqual({ ok: false, reason: "no-session-id" })
  })

  it("returns no-session-id for non-string sessionId", () => {
    const data: NotificationData = { pm: { sessionId: 42 } }
    const result = resolveNotificationRoute(data, sessions(), terminals())
    expect(result).toEqual({ ok: false, reason: "no-session-id" })
  })

  it("returns no-session-id for empty string sessionId", () => {
    const data: NotificationData = { pm: { sessionId: "" } }
    const result = resolveNotificationRoute(data, sessions(), terminals())
    expect(result).toEqual({ ok: false, reason: "no-session-id" })
  })

  it("returns session-not-found when session id is not in the map", () => {
    const data: NotificationData = { pm: { sessionId: "999" } }
    const result = resolveNotificationRoute(data, sessions(), terminals())
    expect(result).toEqual({ ok: false, reason: "session-not-found" })
  })

  it("returns no-terminal when the session has no terminals", () => {
    const data: NotificationData = { pm: { sessionId: "1" } }
    const result = resolveNotificationRoute(
      data,
      sessions(session(1n)),
      terminals(),
    )
    expect(result).toEqual({ ok: false, reason: "no-terminal" })
  })

  it("ignores terminals belonging to other sessions", () => {
    const data: NotificationData = { pm: { sessionId: "1" } }
    const result = resolveNotificationRoute(
      data,
      sessions(session(1n)),
      terminals(terminal(10n, 2n)),
    )
    expect(result).toEqual({ ok: false, reason: "no-terminal" })
  })

  it("ignores controllerId without failing", () => {
    const data: NotificationData = {
      pm: { sessionId: "1", controllerId: "opaque-ref" },
    }
    const result = resolveNotificationRoute(
      data,
      sessions(session(1n)),
      terminals(terminal(10n, 1n)),
    )
    expect(result.ok).toBe(true)
  })
})

describe("shouldRouteColdStart", () => {
  it("routes when no previous identifier is stored", () => {
    expect(shouldRouteColdStart("notif-abc-123", null)).toBe(true)
  })

  it("routes when the identifier differs from the stored one", () => {
    expect(shouldRouteColdStart("notif-new", "notif-old")).toBe(true)
  })

  it("does not route when the identifier matches the stored one", () => {
    expect(shouldRouteColdStart("notif-abc-123", "notif-abc-123")).toBe(false)
  })

  it("does not route with an empty identifier", () => {
    expect(shouldRouteColdStart("", null)).toBe(false)
  })
})

// ── Wiring tests ──────────────────────────────────────────────────────
// These test the extracted effect bodies from App.tsx: the notification
// response callback and the cold-start handler. They cover the decisions
// that live in the wiring layer, not the pure routing logic above.

function payload(sessionId: string, identifier = "notif-1"): ResponsePayload {
  return {
    data: { pm: { sessionId } },
    identifier,
  }
}

describe("notificationResponseAction", () => {
  it("returns navigate for a valid payload", () => {
    const action = notificationResponseAction(
      payload("1"),
      sessions(session(1n, "My session")),
      terminals(terminal(10n, 1n)),
    )
    expect(action.kind).toBe("navigate")
    if (action.kind === "navigate") {
      expect(action.target.sessionId).toBe(1n)
      expect(action.target.terminalId).toBe(10n)
    }
  })

  it("returns ignore for a missing sessionId", () => {
    const action = notificationResponseAction(
      { data: undefined, identifier: "notif-1" },
      sessions(),
      terminals(),
    )
    expect(action).toEqual({ kind: "ignore" })
  })

  it("returns alert for a session that is not found", () => {
    const action = notificationResponseAction(
      payload("999"),
      sessions(),
      terminals(),
    )
    expect(action.kind).toBe("alert")
  })

  it("returns alert for a session with no terminal", () => {
    const action = notificationResponseAction(
      payload("1"),
      sessions(session(1n)),
      terminals(),
    )
    expect(action.kind).toBe("alert")
  })
})

describe("tapDecision", () => {
  const s = sessions(session(1n, "test"))
  const t = terminals(terminal(10n, 1n))

  it("holds every tap until the first snapshot", () => {
    expect(tapDecision(false, payload("1", "notif-a"), new Map(), new Map())).toEqual({ kind: "hold" })
    expect(tapDecision(false, payload("999", "notif-b"), new Map(), new Map())).toEqual({ kind: "hold" })
  })

  it("routes a hydrated tap to its session", () => {
    const decision = tapDecision(true, payload("1", "notif-a"), s, t)
    expect(decision.kind).toBe("navigate")
  })

  it("alerts for a hydrated tap whose session is gone", () => {
    expect(tapDecision(true, payload("999", "notif-c"), s, t).kind).toBe("alert")
  })

  it("ignores a hydrated tap that carries no session id", () => {
    const bare: ResponsePayload = { data: { aps: {} }, identifier: "notif-d" }
    expect(tapDecision(true, bare, s, t)).toEqual({ kind: "ignore" })
  })
})

describe("launchTapDecision", () => {
  it("routes a launch tap when nothing was routed before", () => {
    expect(launchTapDecision(payload("1", "notif-abc"), null)).toBe("route")
  })

  it("routes a launch tap whose identifier differs from the last routed one", () => {
    expect(launchTapDecision(payload("1", "notif-new"), "notif-old")).toBe("route")
  })

  it("skips a launch tap an earlier process already routed", () => {
    expect(launchTapDecision(payload("1", "notif-abc"), "notif-abc")).toBe("duplicate")
  })

  it("reports a launch without a tap", () => {
    expect(launchTapDecision(null, null)).toBe("none")
  })
})

describe("describePayload", () => {
  it("names the identifier, the payload keys, and the session without content", () => {
    const p: ResponsePayload = {
      data: { aps: { alert: { title: "secret" } }, pm: { sessionId: "42" } },
      identifier: "notif-1",
    }
    const line = describePayload(p)
    expect(line).toBe("id=notif-1 keys=aps,pm session=42 approval=none seal=decrypted")
    expect(line).not.toContain("secret")
  })

  it("says when the session id is missing", () => {
    expect(describePayload({ data: { aps: {} }, identifier: "notif-2" }))
      .toBe("id=notif-2 keys=aps session=missing approval=none seal=no-pm")
    expect(describePayload({ data: undefined, identifier: "" }))
      .toBe("id=? keys=none session=missing approval=none seal=no-pm")
  })

  // A tap that resolves nothing is indistinguishable from a tap that
  // never arrived unless the line says the seal was never opened.
  it("names an unopened seal so a silent tap is not a mystery", () => {
    const line = describePayload({
      data: { aps: {}, pm: { pm_sealed: "c2VhbGVk" } },
      identifier: "notif-3",
    })
    expect(line).toBe("id=notif-3 keys=aps,pm session=missing approval=none seal=undecrypted")
  })
})

describe("sealState", () => {
  it("reports the sealed blob the daemon sends before the extension runs", () => {
    expect(sealState({ pm: { pm_sealed: "c2VhbGVk" } })).toBe("undecrypted")
  })

  it("reports routing fields the extension wrote in place of the blob", () => {
    expect(sealState({ pm: { sessionId: "42", controllerId: "ref" } })).toBe("decrypted")
  })

  it("reports a payload with no pm dict at all", () => {
    expect(sealState({ aps: {} })).toBe("no-pm")
    expect(sealState(undefined)).toBe("no-pm")
  })
})

// ── The shape the notification service extension writes ──────────────
// The daemon sends only the sealed blob. Everything the app routes on
// is written to userInfo by the extension after it decrypts, at both
// "pm" and "body"."pm", so either expo read path finds it.

/** userInfo as the extension leaves it for a notification it opened. */
function nseWrittenUserInfo(sessionId: string) {
  const routing = {
    sessionId,
    controllerId: "a45c1a8a796c2ba05e852ba0ba4b9037",
    state: "needs-input",
    eventId: "inst:1:1:needs-input:1",
    url: `puppetmaster://controller/a45c1a8a796c2ba05e852ba0ba4b9037/session/${sessionId}`,
  }
  return {
    aps: { alert: { title: "Session needs input", body: "Merge now?" } },
    pm: routing,
    body: { pm: routing },
  }
}

describe("a notification the extension opened", () => {
  it("routes through trigger.payload", () => {
    const trigger = { type: "push" as const, payload: nseWrittenUserInfo("42") }
    const p = extractPayload(null, trigger, "notif-1")

    expect(sealState(p.data)).toBe("decrypted")
    const action = notificationResponseAction(
      p,
      sessions(session(42n, "My session")),
      terminals(terminal(100n, 42n)),
    )
    expect(action.kind).toBe("navigate")
    if (action.kind === "navigate") expect(action.target.sessionId).toBe(42n)
  })

  it("routes through content.data when trigger.payload is absent", () => {
    const contentData = { pm: nseWrittenUserInfo("42").pm }
    const p = extractPayload(contentData, { type: "push" as const }, "notif-2")

    const action = notificationResponseAction(
      p,
      sessions(session(42n, "My session")),
      terminals(terminal(100n, 42n)),
    )
    expect(action.kind).toBe("navigate")
  })

  it("carries no content from the session in the log line", () => {
    const trigger = { type: "push" as const, payload: nseWrittenUserInfo("42") }
    const line = describePayload(extractPayload(null, trigger, "notif-3"))
    expect(line).not.toContain("Merge now?")
    expect(line).not.toContain("Session needs input")
  })
})

// ── Connection approvals ──────────────────────────────────────────────

const APPROVAL = "f".repeat(64)

/** userInfo as the extension leaves it for an approval it opened. */
function nseWrittenApproval(approvalId: string | undefined) {
  const base = nseWrittenUserInfo("42")
  const routing = {
    ...base.pm,
    state: "connection-approval",
    eventId: `connection-approval:${approvalId ?? "older-extension"}`,
    ...(approvalId === undefined ? {} : { approvalId }),
  }
  return { ...base, pm: routing, body: { pm: routing } }
}

describe("a connection approval notification", () => {
  const live = [sessions(session(42n, "My session")), terminals(terminal(100n, 42n))] as const

  it("opens the exact approval after decryption, through either expo read path", () => {
    const viaTrigger = extractPayload(null, { type: "push" as const, payload: nseWrittenApproval(APPROVAL) }, "n-1")
    const viaContent = extractPayload({ pm: nseWrittenApproval(APPROVAL).pm }, { type: "push" as const }, "n-2")
    for (const payload of [viaTrigger, viaContent]) {
      expect(notificationResponseAction(payload, ...live)).toEqual({ kind: "approval", approvalId: APPROVAL })
    }
  })

  it("opens at once, without waiting for the session snapshot", () => {
    const p = extractPayload(null, { type: "push" as const, payload: nseWrittenApproval(APPROVAL) }, "n-3")
    expect(tapDecision(false, p, new Map(), new Map())).toEqual({ kind: "approval", approvalId: APPROVAL })
  })

  it("falls back to the session when an older extension dropped the id", () => {
    const p = extractPayload(null, { type: "push" as const, payload: nseWrittenApproval(undefined) }, "n-4")
    const action = notificationResponseAction(p, ...live)
    expect(action.kind).toBe("navigate")
  })

  it("never trusts an approval id beside an unopened seal", () => {
    const forged = { aps: {}, pm: { pm_sealed: "c2VhbGVk", approvalId: APPROVAL, sessionId: "42" } }
    const p = extractPayload(null, { type: "push" as const, payload: forged }, "n-5")
    expect(notificationApprovalId(p.data)).toBeNull()
    expect(tapDecision(true, p, ...live).kind).not.toBe("approval")
  })

  it("rejects an id that is not an approval token", () => {
    for (const approvalId of ["", "../settings", 42, "a".repeat(129)]) {
      expect(notificationApprovalId({ pm: { sessionId: "42", approvalId } })).toBeNull()
    }
  })

  it("logs that an approval is present without logging its id", () => {
    const line = describePayload(extractPayload(null, { type: "push" as const, payload: nseWrittenApproval(APPROVAL) }, "n-6"))
    expect(line).toContain("approval=present")
    expect(line).not.toContain(APPROVAL)
  })
})

// ── The shape when the extension could not decrypt ───────────────────
// A bad key, a replayed counter, or an extension that never ran all
// leave the daemon's payload untouched: the sealed blob and no
// routing. The tap must resolve to nothing, and must say so.

describe("a notification the extension could not open", () => {
  const undecrypted = {
    aps: { alert: { title: "Puppet Master", body: "New notification" } },
    pm: { pm_sealed: "c2VhbGVkLWJsb2I=" },
  }

  it("carries no session id to route on", () => {
    const p = extractPayload(null, { type: "push" as const, payload: undecrypted }, "notif-1")
    expect(resolveNotificationRoute(p.data, sessions(session(42n)), terminals(terminal(1n, 42n))))
      .toEqual({ ok: false, reason: "no-session-id" })
  })

  it("does nothing rather than opening the wrong session", () => {
    const p = extractPayload(null, { type: "push" as const, payload: undecrypted }, "notif-2")
    expect(notificationResponseAction(p, sessions(session(42n)), terminals(terminal(1n, 42n))))
      .toEqual({ kind: "ignore" })
    expect(tapDecision(true, p, sessions(session(42n)), terminals(terminal(1n, 42n))))
      .toEqual({ kind: "ignore" })
  })

  // Doing nothing is the right call, but doing it without a trace is
  // what made this indistinguishable from a tap that never fired.
  it("logs why it did nothing", () => {
    const p = extractPayload(null, { type: "push" as const, payload: undecrypted }, "notif-3")
    const line = describePayload(p)
    expect(line).toContain("session=missing")
    expect(line).toContain("seal=undecrypted")
  })
})

// ── extractPayload ───────────────────────────────────────────────────
// Verifies that the payload extraction reads from the right place
// depending on whether the notification is a remote push or a local
// notification. expo-notifications on iOS serialises content.data for
// remote notifications as userInfo["body"], and the notification
// service extension writes routing to both that key and "pm".
// trigger.payload carries the full userInfo.

describe("extractPayload", () => {
  it("reads from trigger.payload for a push notification", () => {
    // Simulates what expo-notifications gives: content.data is null
    // (because userInfo has no "body" key), but trigger.payload has
    // the full APNs userInfo including the "pm" key.
    const trigger = {
      type: "push" as const,
      payload: {
        aps: { alert: { title: "t", body: "b" } },
        pm: { sessionId: "42", controllerId: "ref" },
      },
    }
    const result = extractPayload(null, trigger, "notif-1")
    expect(result.data?.pm?.sessionId).toBe("42")
    expect(result.identifier).toBe("notif-1")
  })

  it("falls back to content.data for a local notification", () => {
    const contentData = { pm: { sessionId: "7" } }
    const trigger = { type: "calendar", repeats: false }
    const result = extractPayload(contentData, trigger, "notif-2")
    expect(result.data?.pm?.sessionId).toBe("7")
  })

  it("falls back to content.data when trigger is null", () => {
    const contentData = { pm: { sessionId: "3" } }
    const result = extractPayload(contentData, null, "notif-3")
    expect(result.data?.pm?.sessionId).toBe("3")
  })

  it("falls back to content.data when trigger.payload is missing", () => {
    const contentData = { pm: { sessionId: "5" } }
    const trigger = { type: "push" as const }
    const result = extractPayload(contentData, trigger, "notif-4")
    expect(result.data?.pm?.sessionId).toBe("5")
  })

  it("works end-to-end: push payload routes to the correct session", () => {
    const trigger = {
      type: "push" as const,
      payload: {
        aps: { alert: { title: "t", body: "b" } },
        pm: { sessionId: "42", controllerId: "ref" },
      },
    }
    const p = extractPayload(null, trigger, "notif-1")
    const action = notificationResponseAction(
      p,
      sessions(session(42n, "My session")),
      terminals(terminal(100n, 42n)),
    )
    expect(action.kind).toBe("navigate")
    if (action.kind === "navigate") {
      expect(action.target.sessionId).toBe(42n)
    }
  })

  it("reads from body fallback when trigger is not a push", () => {
    // Simulates the "body" key that expo expects at the APNs root.
    // The extension writes "body": { "pm": ... }, so content.data is
    // populated even without trigger.payload.
    const contentData = { pm: { sessionId: "99" } }
    const trigger = null
    const result = extractPayload(contentData, trigger, "notif-5")
    expect(result.data?.pm?.sessionId).toBe("99")
  })

  it("prefers whichever candidate carries pm when trigger.payload lacks it", () => {
    const trigger = { type: "push" as const, payload: { aps: { alert: { title: "t" } } } }
    const contentData = { pm: { sessionId: "11" } }
    expect(extractPayload(contentData, trigger, "notif-6").data?.pm?.sessionId).toBe("11")
  })

  it("reads pm nested under a body key in content.data", () => {
    const contentData = { body: { pm: { sessionId: "12" } } }
    expect(extractPayload(contentData, null, "notif-7").data?.pm?.sessionId).toBe("12")
  })

  it("keeps the raw data when no candidate carries pm", () => {
    const trigger = { type: "push" as const, payload: { aps: {} } }
    const result = extractPayload(null, trigger, "notif-8")
    expect(result.data).toEqual({ aps: {} })
    expect(result.data?.pm).toBeUndefined()
  })
})
