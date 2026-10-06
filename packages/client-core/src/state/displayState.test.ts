import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { SessionRole, SessionSchema, SessionState } from "../gen/pm/v1/pm_pb";
import { displaySessionState } from "./displayState";

function supervisor(state: SessionState) {
  return create(SessionSchema, { id: 1n, role: SessionRole.SUPERVISOR, state });
}

function worker(id: bigint, state: SessionState) {
  return create(SessionSchema, { id, role: SessionRole.WORKER, state, spawnedBySessionId: 1n });
}

describe("displaySessionState", () => {
  it("renders an idle supervisor as running while a spawned session is still going", () => {
    for (const child of [SessionState.WORKING, SessionState.STARTING]) {
      expect(displaySessionState(supervisor(SessionState.IDLE), [worker(2n, child)]))
        .toBe(SessionState.WORKING);
    }
  });

  it("reads as awaiting worker when every running child is stranded on an offline host", () => {
    const children = [
      worker(2n, SessionState.AWAITING_WORKER),
      worker(3n, SessionState.AWAITING_WORKER),
      worker(4n, SessionState.IDLE),
      worker(5n, SessionState.EXITED),
    ];

    expect(displaySessionState(supervisor(SessionState.IDLE), children))
      .toBe(SessionState.AWAITING_WORKER);
  });

  it("still reads as working while one child can make progress", () => {
    const children = [
      worker(2n, SessionState.AWAITING_WORKER),
      worker(3n, SessionState.WORKING),
    ];

    expect(displaySessionState(supervisor(SessionState.IDLE), children)).toBe(SessionState.WORKING);
  });

  it("finds the running child among quiet siblings", () => {
    const children = [
      worker(2n, SessionState.IDLE),
      worker(3n, SessionState.FAILED),
      worker(4n, SessionState.WORKING),
    ];

    expect(displaySessionState(supervisor(SessionState.IDLE), children)).toBe(SessionState.WORKING);
  });

  it("leaves an idle supervisor idle when nothing it spawned is running", () => {
    const children = [
      worker(2n, SessionState.IDLE),
      worker(3n, SessionState.FAILED),
      worker(4n, SessionState.NEEDS_INPUT),
      worker(5n, SessionState.EXITED),
    ];

    expect(displaySessionState(supervisor(SessionState.IDLE), children)).toBe(SessionState.IDLE);
    expect(displaySessionState(supervisor(SessionState.IDLE), [])).toBe(SessionState.IDLE);
  });

  it("keeps the supervisor's own needs-input above a running child", () => {
    expect(displaySessionState(supervisor(SessionState.NEEDS_INPUT), [worker(2n, SessionState.WORKING)]))
      .toBe(SessionState.NEEDS_INPUT);
  });

  it("never recolours a worker because of what it spawned", () => {
    const parent = create(SessionSchema, { id: 1n, role: SessionRole.WORKER, state: SessionState.IDLE });

    expect(displaySessionState(parent, [worker(2n, SessionState.WORKING)])).toBe(SessionState.IDLE);
  });

  it("passes every other supervisor state through untouched", () => {
    for (const state of [
      SessionState.WORKING,
      SessionState.STARTING,
      SessionState.AWAITING_WORKER,
      SessionState.FAILED,
      SessionState.EXITED,
    ]) {
      expect(displaySessionState(supervisor(state), [worker(2n, SessionState.WORKING)])).toBe(state);
    }
  });
});
