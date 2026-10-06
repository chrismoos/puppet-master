import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import {
  AgentKind,
  ProjectSchema,
  SessionAlertKind,
  SessionSchema,
  SessionState,
} from "../gen/pm/v1/pm_pb";
import {
  NOTIFY_BODY_MAX_CHARS,
  needsInputNotification,
  notifySupport,
  notifyUnsupportedMessage,
  sessionAlertNotification,
  shouldAutoRequestNotify,
} from "./notify";

function session(id: bigint, state: SessionState, spawnedBy?: bigint) {
  return create(SessionSchema, {
    id,
    projectId: 10n,
    agent: AgentKind.CLAUDE_CODE,
    state,
    taskTitle: `task ${id}`,
    createdAtUnixMs: 1_000n,
    spawnedBySessionId: spawnedBy,
  });
}

describe("sessionAlertNotification", () => {
  it("uses the session display name as title and says finished", () => {
    const currentSession = session(1n, SessionState.IDLE);
    currentSession.goal = "Migrate the auth tables";

    expect(sessionAlertNotification(currentSession, SessionAlertKind.COMPLETED)).toEqual({
      title: "Migrate the auth tables",
      body: "finished",
    });
  });

  it("uses the failure detail as the body for a failure alert", () => {
    const currentSession = session(1n, SessionState.FAILED);
    currentSession.goal = "Migrate the auth tables";
    currentSession.stateDetail = "worker did not reconnect";

    expect(sessionAlertNotification(currentSession, SessionAlertKind.FAILED)).toEqual({
      title: "Migrate the auth tables",
      body: "worker did not reconnect",
    });
  });

  it("truncates a long failure detail", () => {
    const currentSession = session(1n, SessionState.FAILED);
    currentSession.stateDetail = "x".repeat(NOTIFY_BODY_MAX_CHARS + 1);

    const { body } = sessionAlertNotification(currentSession, SessionAlertKind.FAILED);
    expect(body).toBe(`${"x".repeat(NOTIFY_BODY_MAX_CHARS - 1)}…`);
  });

  it("keeps finished for a completed alert even with a state detail", () => {
    const currentSession = session(1n, SessionState.IDLE);
    currentSession.goal = "Migrate the auth tables";
    currentSession.stateDetail = "stale detail";

    expect(sessionAlertNotification(currentSession, SessionAlertKind.COMPLETED)).toEqual({
      title: "Migrate the auth tables",
      body: "finished",
    });
  });

  it("says failed for a failure alert without a detail", () => {
    const currentSession = session(1n, SessionState.IDLE);
    currentSession.goal = "Migrate the auth tables";

    expect(sessionAlertNotification(currentSession, SessionAlertKind.FAILED)).toEqual({
      title: "Migrate the auth tables",
      body: "failed",
    });
  });
});

describe("needsInputNotification", () => {
  it("names the project in the title and uses the question as the body", () => {
    const currentSession = session(1n, SessionState.NEEDS_INPUT);
    currentSession.goal = "Refine notifications";
    currentSession.stateDetail = "Merge the fix into master?";
    const project = create(ProjectSchema, { id: 10n, name: "Puppet Master" });

    expect(needsInputNotification(currentSession, project)).toEqual({
      title: "Refine notifications · Puppet Master",
      body: "Merge the fix into master?",
    });
  });

  it("falls back to the headline when there is no question", () => {
    const currentSession = session(1n, SessionState.NEEDS_INPUT);
    currentSession.goal = "Refine notifications";
    currentSession.headline = "Waiting on a review";
    currentSession.stateDetail = "  ";
    const project = create(ProjectSchema, { id: 10n, name: "Puppet Master" });

    expect(needsInputNotification(currentSession, project)).toEqual({
      title: "Refine notifications · Puppet Master",
      body: "Waiting on a review",
    });
  });

  it("unescapes entities in title and question", () => {
    const currentSession = session(1n, SessionState.NEEDS_INPUT);
    currentSession.goal = "Auth &amp; Permissions";
    currentSession.stateDetail = "Merge &lt;feature&gt; &amp; &apos;fix&apos;?";
    const project = create(ProjectSchema, { id: 10n, name: "Puppet Master" });

    expect(needsInputNotification(currentSession, project)).toEqual({
      title: "Auth & Permissions · Puppet Master",
      body: "Merge <feature> & 'fix'?",
    });
  });

  it("truncates a long question on a code point boundary", () => {
    const currentSession = session(1n, SessionState.NEEDS_INPUT);
    currentSession.stateDetail = "🙂".repeat(NOTIFY_BODY_MAX_CHARS + 5);

    const { body } = needsInputNotification(currentSession);
    expect(body).toBe(`${"🙂".repeat(NOTIFY_BODY_MAX_CHARS - 1)}…`);
    expect(Array.from(body ?? "")).toHaveLength(NOTIFY_BODY_MAX_CHARS);
  });

  it("keeps the session name alone as title when the session has no project", () => {
    const currentSession = session(1n, SessionState.NEEDS_INPUT);
    currentSession.goal = "Refine notifications";
    currentSession.stateDetail = "Which branch?";

    expect(needsInputNotification(currentSession)).toEqual({
      title: "Refine notifications",
      body: "Which branch?",
    });
  });

  it("omits the body when there is neither question nor headline", () => {
    const currentSession = session(1n, SessionState.NEEDS_INPUT);

    expect(needsInputNotification(currentSession)).toEqual({ title: "task 1" });
  });
});

describe("shouldAutoRequestNotify", () => {
  it("asks once when supported, unasked, and undecided", () => {
    expect(shouldAutoRequestNotify(true, false, "default")).toBe(true);
  });
  it("does not ask again after asking", () => {
    expect(shouldAutoRequestNotify(true, true, "default")).toBe(false);
  });
  it("does not ask once the user has decided", () => {
    expect(shouldAutoRequestNotify(true, false, "granted")).toBe(false);
    expect(shouldAutoRequestNotify(true, false, "denied")).toBe(false);
  });
  it("does not ask when unsupported", () => {
    expect(shouldAutoRequestNotify(false, false, "default")).toBe(false);
  });
});

describe("notifySupport", () => {
  it("is supported with the API on a secure origin", () => {
    expect(notifySupport(true, true)).toEqual({ supported: true });
  });
  it("is supported with the API when the platform has no secure-context concept", () => {
    expect(notifySupport(true, undefined)).toEqual({ supported: true });
  });
  it("reports an insecure origin as the reason when the API exists but the page is plain HTTP", () => {
    expect(notifySupport(true, false)).toEqual({ supported: false, reason: "insecure-context" });
  });
  it("reports a missing API regardless of origin", () => {
    expect(notifySupport(false, true)).toEqual({ supported: false, reason: "unavailable" });
    expect(notifySupport(false, false)).toEqual({ supported: false, reason: "unavailable" });
  });
});

describe("notifyUnsupportedMessage", () => {
  it("tells the user what would make an insecure origin work", () => {
    const message = notifyUnsupportedMessage("insecure-context");
    expect(message).toContain("HTTPS");
    expect(message).toContain("localhost");
  });
  it("explains a missing API", () => {
    expect(notifyUnsupportedMessage("unavailable")).toMatch(/does not support/);
  });
});
