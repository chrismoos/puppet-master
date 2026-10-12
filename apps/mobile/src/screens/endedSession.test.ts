import { create, type MessageInitShape } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { SessionSchema, SessionState } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { initialState, type AppState } from "@puppet-master/client-core/state/reducer";
import { canResumeSession, endedSummary, resumeLabel, sessionListed, sessionRowTime } from "./endedSession";

const NOW = 1_700_000_000_000;

function ended(fields: MessageInitShape<typeof SessionSchema> = {}) {
  return create(SessionSchema, {
    id: 5n,
    state: SessionState.EXITED,
    endedAtUnixMs: BigInt(NOW - 20_000),
    taskPrompt: "do the thing",
    ...fields,
  });
}

function stateWith(sources: number): AppState {
  const session = ended();
  return {
    ...initialState,
    sessions: new Map([["5", session]]),
    sessionSources: new Map([["5", sources]]),
  };
}

describe("canResumeSession", () => {
  it("resumes a saved conversation and restarts a promptless session", () => {
    expect(canResumeSession(ended({ resumable: true }))).toBe(true);
    expect(canResumeSession(ended({ resumable: false, taskPrompt: "" }))).toBe(true);
    expect(resumeLabel(ended({ resumable: true }))).toBe("Resume conversation");
    expect(resumeLabel(ended({ resumable: false, taskPrompt: "" }))).toBe("Restart session");
  });

  it("refuses a live session and a task with no transcript", () => {
    expect(canResumeSession(ended({ state: SessionState.WORKING, resumable: true }))).toBe(false);
    expect(canResumeSession(ended({ resumable: false }))).toBe(false);
  });
});

describe("sessionListed", () => {
  it("keeps a just-ended subscription session listed while ended ones are hidden", () => {
    const state = stateWith(1);
    expect(sessionListed(state, state.sessions.get("5")!, false)).toBe(true);
  });

  it("hides an ended session the subscription no longer holds unless asked", () => {
    const state = stateWith(2);
    expect(sessionListed(state, state.sessions.get("5")!, false)).toBe(false);
    expect(sessionListed(state, state.sessions.get("5")!, true)).toBe(true);
  });
});

describe("sessionRowTime", () => {
  it("says when an ended session ended", () => {
    expect(sessionRowTime(ended(), NOW)).toBe("ended 20s ago");
    expect(sessionRowTime(ended({ state: SessionState.FAILED }), NOW)).toBe("failed 20s ago");
  });

  it("names the exit code in the banner line", () => {
    expect(endedSummary(ended({ exitCode: 1 }))).toBe("Session ended with exit code 1");
    expect(endedSummary(ended())).toBe("Session ended");
  });
});
