import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { SessionSchema, SessionState } from "../gen/pm/v1/pm_pb";
import { selectedSessionBecameUnavailable, sessionFallbackPath } from "./sessionNavigation";

function session(id: bigint, state: SessionState, createdAtUnixMs = id * 1_000n) {
  return create(SessionSchema, { id, state, createdAtUnixMs });
}

describe("session route fallback", () => {
  it("retains a selected ended session and only falls back on removal", () => {
    const live = session(7n, SessionState.WORKING);
    const ended = session(7n, SessionState.EXITED);
    expect(selectedSessionBecameUnavailable(new Map([["7", live]]), new Map([["7", ended]]), "7")).toBe(false);
    expect(selectedSessionBecameUnavailable(new Map([["7", ended]]), new Map([["7", ended]]), "7")).toBe(false);
    expect(selectedSessionBecameUnavailable(new Map([["7", live]]), new Map(), "7")).toBe(true);
  });

  it("keeps a selected session routed while it awaits its worker", () => {
    const live = session(7n, SessionState.WORKING);
    const awaitingWorker = session(7n, SessionState.AWAITING_WORKER);

    expect(selectedSessionBecameUnavailable(
      new Map([["7", live]]),
      new Map([["7", awaitingWorker]]),
      "7",
    )).toBe(false);
  });

  it("chooses the newest live session, then the first workspace, then Home", () => {
    const sessions = [
      session(7n, SessionState.EXITED),
      session(8n, SessionState.IDLE, 2_000n),
      session(9n, SessionState.WORKING, 3_000n),
    ];
    expect(sessionFallbackPath(sessions, [4, 6], "7")).toBe("/session/9");
    expect(sessionFallbackPath([sessions[0]], [4, 6], "7")).toBe("/workspace/4");
    expect(sessionFallbackPath([sessions[0]], [], "7")).toBe("/");
  });

  it("uses an awaiting-worker session as a live fallback without changing its identity", () => {
    const ended = session(7n, SessionState.EXITED);
    const awaitingWorker = session(8n, SessionState.AWAITING_WORKER, 2_000n);

    expect(sessionFallbackPath([ended, awaitingWorker], [4], "7")).toBe("/session/8");
  });
});
