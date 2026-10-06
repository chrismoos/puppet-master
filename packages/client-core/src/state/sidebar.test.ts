import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import {
  AgentKind,
  BucketSchema,
  ProjectSchema,
  SessionSchema,
  SessionState,
  SessionRole,
  SnapshotSchema,
} from "../gen/pm/v1/pm_pb";
import { initialState, reduce, type AppState } from "./reducer";
import { buildSidebar, visibleSessions } from "./sidebar";

function session(id: bigint, projectId: bigint, state: SessionState) {
  return create(SessionSchema, {
    id,
    projectId,
    agent: AgentKind.CLAUDE_CODE,
    state,
    taskTitle: `task ${id}`,
    createdAtUnixMs: 1_000n * id,
  });
}

function activeAt(id: bigint, state: SessionState, at: bigint) {
  const s = session(id, 10n, state);
  s.lastActivityAtUnixMs = at;
  return s;
}

function stateWith(...sessions: ReturnType<typeof session>[]): AppState {
  const snapshot = create(SnapshotSchema, {
    buckets: [create(BucketSchema, { id: 1n, name: "work", position: 0 })],
    projects: [
      create(ProjectSchema, { id: 10n, bucketId: 1n, name: "api", path: "/src/api" }),
      create(ProjectSchema, { id: 11n, bucketId: 1n, name: "web", path: "/src/web" }),
    ],
    sessions,
  });
  return reduce(initialState, { type: "snapshot", snapshot });
}

describe("visibleSessions", () => {
  const all = [
    session(1n, 10n, SessionState.WORKING),
    session(2n, 10n, SessionState.EXITED),
    session(3n, 10n, SessionState.FAILED),
    session(4n, 10n, SessionState.NEEDS_INPUT),
  ];

  it("hides exited and failed sessions by default", () => {
    const ids = visibleSessions(all, false).map((s) => s.id);
    expect(ids).toEqual([1n, 4n]);
  });

  it("reveals ended sessions when showEnded is on", () => {
    expect(visibleSessions(all, true)).toHaveLength(4);
  });

  it("always shows the selected session even when it is ended and hidden", () => {
    const ids = visibleSessions(all, false, "2").map((s) => s.id);
    expect(ids).toEqual([1n, 2n, 4n]);
  });
});

describe("awaiting-worker sessions", () => {
  it("keeps awaiting-worker sessions visible without calling them working or idle", () => {
    const awaiting = session(8n, 10n, SessionState.AWAITING_WORKER);
    const state = stateWith(awaiting);
    expect(visibleSessions(state.sessions.values(), false)).toEqual([awaiting]);
    expect(buildSidebar(state, false).buckets[0].sessions).toEqual([awaiting]);
  });
});

describe("buildSidebar", () => {
  it("keeps recently ended subscription sessions visible by default", () => {
    const state = stateWith(
      session(1n, 10n, SessionState.WORKING),
      session(2n, 10n, SessionState.EXITED),
    );
    const model = buildSidebar(state, false);
    expect(model.buckets[0].sessions.map((s) => s.id)).toEqual([1n, 2n]);
  });

  it("includes ended sessions when showEnded is on", () => {
    const state = stateWith(
      session(1n, 10n, SessionState.WORKING),
      session(2n, 10n, SessionState.EXITED),
    );
    const model = buildSidebar(state, true);
    expect(model.buckets[0].sessions).toHaveLength(2);
  });

  it("preserves server relevance order within grouped search results", () => {
    const state = stateWith(
      session(1n, 10n, SessionState.EXITED),
      session(2n, 10n, SessionState.EXITED),
    );
    const included = new Set(["2", "1"]);
    const order = new Map([["2", 0], ["1", 1]]);
    const model = buildSidebar(state, true, null, included, order);
    expect(model.buckets[0].sessions.map((s) => s.id)).toEqual([2n, 1n]);
  });

  it("keeps multiple needs-input sessions once in their normal order", () => {
    const state = stateWith(
      session(1n, 10n, SessionState.WORKING),
      session(2n, 10n, SessionState.NEEDS_INPUT),
      session(3n, 10n, SessionState.NEEDS_INPUT),
    );
    const model = buildSidebar(state, false);
    expect(model.buckets[0].sessions.map((s) => s.id)).toEqual([3n, 2n, 1n]);
    expect(model.orphans).toHaveLength(0);
  });

  it("pools every project in the bucket into one list", () => {
    const state = stateWith(
      session(1n, 10n, SessionState.WORKING),
      session(2n, 11n, SessionState.WORKING),
    );
    expect(buildSidebar(state, false).buckets[0].sessions.map((s) => s.id).sort()).toEqual([1n, 2n]);
  });

  it("collects sessions whose project is unknown as orphans", () => {
    const state = stateWith(session(9n, 999n, SessionState.WORKING));
    const model = buildSidebar(state, false);
    expect(model.orphans.map((s) => s.id)).toEqual([9n]);
    expect(model.buckets[0].sessions).toHaveLength(0);
  });

  it("exposes the order it applied so callers that regroup sessions keep it", () => {
    const state = stateWith(
      session(1n, 10n, SessionState.WORKING),
      session(2n, 10n, SessionState.NEEDS_INPUT),
    );
    const model = buildSidebar(state, false);
    const regrouped = [...state.sessions.values()].sort(model.order);
    expect(regrouped.map((s) => s.id)).toEqual([2n, 1n]);
  });

  it("orders regrouped sessions by search relevance while searching", () => {
    const state = stateWith(
      session(1n, 10n, SessionState.WORKING),
      session(2n, 10n, SessionState.NEEDS_INPUT),
    );
    const model = buildSidebar(state, true, null, new Set(["1", "2"]), new Map([["1", 0], ["2", 1]]));
    const regrouped = [...state.sessions.values()].sort(model.order);
    expect(regrouped.map((s) => s.id)).toEqual([1n, 2n]);
  });

  it("places Supervisors and Workers alike at bucket level",()=>{
    const worker=session(1n,10n,SessionState.WORKING); worker.role=SessionRole.WORKER;
    const supervisor=session(2n,10n,SessionState.IDLE); supervisor.role=SessionRole.SUPERVISOR;
    const model=buildSidebar(stateWith(worker,supervisor),false);
    expect(model.buckets[0].supervisors.map(s=>s.id)).toEqual([2n]);
    expect(model.buckets[0].sessions.map(s=>s.id)).toEqual([1n]);
  });
});

describe("session ordering within a bucket", () => {
  const MINUTE = 60_000n;
  const ids = (state: AppState) =>
    buildSidebar(state, false).buckets[0].sessions.map((s) => s.id);

  it("puts the more recently active session first, not the more recently created", () => {
    // Session 1 is the older of the two but did something more recently.
    const state = stateWith(
      activeAt(1n, SessionState.WORKING, 9n * MINUTE),
      activeAt(2n, SessionState.WORKING, 3n * MINUTE),
    );
    expect(ids(state)).toEqual([1n, 2n]);
  });

  it("leaves sessions active within the same minute in a stable order", () => {
    // Session 1 acted later, but not by enough to move a row.
    const state = stateWith(
      activeAt(1n, SessionState.WORKING, 3n * MINUTE + 59_000n),
      activeAt(2n, SessionState.WORKING, 3n * MINUTE),
    );
    expect(ids(state)).toEqual([2n, 1n]);
  });

  it("still ranks by state before recency", () => {
    const state = stateWith(
      activeAt(1n, SessionState.NEEDS_INPUT, 1n * MINUTE),
      activeAt(2n, SessionState.WORKING, 9n * MINUTE),
    );
    expect(ids(state)).toEqual([1n, 2n]);
  });

  it("falls back to creation time for a session that has not reported activity", () => {
    const fresh = session(2n, 10n, SessionState.WORKING);
    fresh.createdAtUnixMs = 9n * MINUTE;
    expect(ids(stateWith(activeAt(1n, SessionState.WORKING, 1n * MINUTE), fresh))).toEqual([2n, 1n]);
  });

  it("orders on the clock the row shows, not on one it does not", () => {
    // The row reports the derived activity clock. Ordering on the raw
    // typing or output clocks it never shows put a session reading "now"
    // below older-looking rows, which is the list disagreeing with itself.
    const interacted = session(2n, 10n, SessionState.WORKING);
    interacted.lastUserInteractionAtUnixMs = 9n * MINUTE;
    interacted.lastAgentActivityAtUnixMs = 9n * MINUTE;
    expect(ids(stateWith(activeAt(1n, SessionState.WORKING, 5n * MINUTE), interacted))).toEqual([1n, 2n]);
  });

  it("keeps the newest activity first", () => {
    expect(ids(stateWith(
      activeAt(1n, SessionState.WORKING, 2n * MINUTE),
      activeAt(2n, SessionState.WORKING, 9n * MINUTE),
    ))).toEqual([2n, 1n]);
  });

  it("orders an ended session by when it ended, which is what its row says", () => {
    const ended = session(2n, 10n, SessionState.EXITED);
    ended.lastActivityAtUnixMs = 1n * MINUTE;
    ended.endedAtUnixMs = 9n * MINUTE;
    const older = session(1n, 10n, SessionState.EXITED);
    older.lastActivityAtUnixMs = 8n * MINUTE;
    older.endedAtUnixMs = 2n * MINUTE;
    const shown = buildSidebar(stateWith(older, ended), true).buckets[0].sessions;
    expect(shown.map((session) => session.id)).toEqual([2n, 1n]);
  });
});
