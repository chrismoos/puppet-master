import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { SessionAlertKind, SessionRole, SessionSchema, SessionState, type Session } from "../gen/pm/v1/pm_pb";
import { AlertCatchUp } from "./alertCatchUp";

function session(
  id: bigint,
  fields: Partial<Pick<Session, "state" | "needsInputUnseen" | "idleUnseen" | "role" | "spawnedBySessionId">> = {},
) {
  return create(SessionSchema, { id, state: SessionState.WORKING, ...fields });
}

function byId(...sessions: Session[]): Map<string, Session> {
  return new Map(sessions.map((s) => [s.id.toString(), s]));
}

const kinds = (raised: { session: Session; kind: SessionAlertKind }[]) =>
  raised.map(({ session: s, kind }) => [s.id, kind]);

describe("AlertCatchUp", () => {
  it("treats every flag on the first hydration as already known", () => {
    const catchUp = new AlertCatchUp();
    const unseen = session(1n, { state: SessionState.IDLE, idleUnseen: true });

    expect(catchUp.snapshot(null, [unseen])).toEqual([]);
    expect(catchUp.snapshot(byId(unseen), [unseen])).toEqual([]);
  });

  it("alerts a session that appeared while disconnected already unseen", () => {
    const catchUp = new AlertCatchUp();
    const fresh = session(2n, { state: SessionState.NEEDS_INPUT, needsInputUnseen: true });

    expect(kinds(catchUp.snapshot(byId(), [fresh]))).toEqual([[2n, SessionAlertKind.NEEDS_INPUT]]);
  });

  it("suppresses an episode the live alert reported until its flag clears", () => {
    const catchUp = new AlertCatchUp();
    const working = session(3n);
    const idle = session(3n, { state: SessionState.IDLE, idleUnseen: true });

    catchUp.noteAlert(3n, SessionAlertKind.COMPLETED);
    expect(catchUp.snapshot(byId(working), [idle])).toEqual([]);

    catchUp.observe(working);
    expect(kinds(catchUp.snapshot(byId(working), [idle]))).toEqual([[3n, SessionAlertKind.COMPLETED]]);
  });

  it("keeps the two kinds of one session independent", () => {
    const catchUp = new AlertCatchUp();
    catchUp.noteAlert(4n, SessionAlertKind.NEEDS_INPUT);
    const idle = session(4n, { state: SessionState.IDLE, idleUnseen: true });

    expect(kinds(catchUp.snapshot(byId(session(4n)), [idle]))).toEqual([[4n, SessionAlertKind.COMPLETED]]);
  });

  it("stays quiet for supervised workers, as the daemon's default scope does", () => {
    const catchUp = new AlertCatchUp();
    const worker = session(5n, { state: SessionState.IDLE, idleUnseen: true, spawnedBySessionId: 1n });
    const supervisor = session(6n, {
      state: SessionState.IDLE,
      idleUnseen: true,
      role: SessionRole.SUPERVISOR,
      spawnedBySessionId: 1n,
    });

    expect(kinds(catchUp.snapshot(byId(), [worker, supervisor]))).toEqual([[6n, SessionAlertKind.COMPLETED]]);
  });
});
