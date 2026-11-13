import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { SessionRole, SessionSchema, SessionState } from "../gen/pm/v1/pm_pb";
import { awaitsSupervisor, awaitsUser, displaySessionState } from "./displayState";

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

describe("who a question waits on", () => {
  const supervisor = create(SessionSchema, { id: 1n, role: SessionRole.SUPERVISOR, state: SessionState.WORKING });
  const worker = create(SessionSchema, { id: 2n, state: SessionState.NEEDS_INPUT, spawnedBySessionId: 1n });
  const own = create(SessionSchema, { id: 3n, state: SessionState.NEEDS_INPUT });
  const lookupWith = (...sessions: (typeof supervisor)[]) => (id: string) =>
    sessions.find((s) => s.id.toString() === id);

  it("sends a worker's question to its running supervisor", () => {
    const lookup = lookupWith(supervisor, worker);
    expect(awaitsUser(worker, lookup)).toBe(false);
    expect(awaitsSupervisor(worker, lookup)).toBe(true);
  });

  it("returns a worker's question to the user once its supervisor ended or is unknown", () => {
    const ended = create(SessionSchema, { ...supervisor, state: SessionState.EXITED });
    expect(awaitsUser(worker, lookupWith(ended, worker))).toBe(true);
    expect(awaitsUser(worker, lookupWith(worker))).toBe(true);
    expect(awaitsSupervisor(worker, lookupWith(ended, worker))).toBe(false);
  });

  it("sends an unsupervised session's question to the user and ignores other states", () => {
    expect(awaitsUser(own, lookupWith(own))).toBe(true);
    expect(awaitsUser(supervisor, lookupWith(supervisor))).toBe(false);
  });
});
