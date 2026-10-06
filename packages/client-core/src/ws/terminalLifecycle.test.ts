import { create } from "@bufbuild/protobuf";
import { describe, expect, it, vi } from "vitest";
import {
  SessionSchema,
  SessionState,
  TerminalKind,
  TerminalSchema,
  type Session,
  type Terminal,
} from "../gen/pm/v1/pm_pb";
import {
  releaseEndedOrRemovedSessions,
  releaseRemovedTerminals,
  type TerminalResourceReleaser,
} from "./terminalLifecycle";

function session(id: bigint, state: SessionState): Session {
  return create(SessionSchema, { id, state });
}

function terminal(id: bigint, sessionId: bigint, kind: TerminalKind): Terminal {
  return create(TerminalSchema, { id, sessionId, kind });
}

function releaser(): TerminalResourceReleaser {
  return { disposeSession: vi.fn(), disposeTerminal: vi.fn() };
}

describe("terminal lifecycle resource release", () => {
  it.each([SessionState.EXITED, SessionState.FAILED])(
    "releases a cached agent layer on terminal session state %s",
    (state) => {
      const stage = releaser();
      releaseEndedOrRemovedSessions(
        new Map([["7", session(7n, SessionState.WORKING)]]),
        new Map([["7", session(7n, state)]]),
        stage,
      );

      expect(stage.disposeSession).toHaveBeenCalledOnce();
      expect(stage.disposeSession).toHaveBeenCalledWith(7n);
    },
  );

  it("releases an ended session whose creation and exit were batched", () => {
    const stage = releaser();
    releaseEndedOrRemovedSessions(
      new Map(),
      new Map([["7", session(7n, SessionState.FAILED)]]),
      stage,
    );

    expect(stage.disposeSession).toHaveBeenCalledOnce();
    expect(stage.disposeSession).toHaveBeenCalledWith(7n);
  });

  it("idempotently releases removed and already-ended session resources", () => {
    const stage = releaser();
    releaseEndedOrRemovedSessions(
      new Map([
        ["7", session(7n, SessionState.WORKING)],
        ["8", session(8n, SessionState.EXITED)],
      ]),
      new Map([["8", session(8n, SessionState.FAILED)]]),
      stage,
    );

    expect(stage.disposeSession).toHaveBeenCalledTimes(2);
    expect(stage.disposeSession).toHaveBeenCalledWith(7n);
    expect(stage.disposeSession).toHaveBeenCalledWith(8n);
  });

  it("releases agent terminals by session cache key and shells by terminal key", () => {
    const stage = releaser();
    releaseRemovedTerminals(
      new Map([
        ["70", terminal(70n, 7n, TerminalKind.AGENT)],
        ["80", terminal(80n, 7n, TerminalKind.SHELL)],
        ["90", terminal(90n, 9n, TerminalKind.SHELL)],
      ]),
      new Map([["90", terminal(90n, 9n, TerminalKind.SHELL)]]),
      stage,
    );

    expect(stage.disposeSession).toHaveBeenCalledWith(7n);
    expect(stage.disposeTerminal).toHaveBeenCalledWith(80n);
    expect(stage.disposeTerminal).not.toHaveBeenCalledWith(70n);
  });
});
