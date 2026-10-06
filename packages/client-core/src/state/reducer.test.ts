import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import {
  AgentKind,
  BucketBriefingSchema,
  BucketSchema,
  ContextFieldSchema,
  ContextKind,
  EventSchema,
  ItemRefSchema,
  ItemSchema,
  ItemStatus,
  ProjectSchema,
  SessionContextSchema,
  SessionForwardSchema,
  SessionSchema,
  SessionState,
  SnapshotSchema,
  UserSettingChangedSchema,
  UserSettingSchema,
  WorkerSchema,
  type Event,
} from "../gen/pm/v1/pm_pb";
import { initialState, reduce, type AppState } from "./reducer";

function bucket(id: bigint, name: string) {
  return create(BucketSchema, { id, name, position: Number(id) });
}

function project(id: bigint, bucketId: bigint, name: string) {
  return create(ProjectSchema, { id, bucketId, name, path: `/src/${name}` });
}

function session(id: bigint, projectId: bigint, state = SessionState.WORKING) {
  return create(SessionSchema, {
    id,
    projectId,
    agent: AgentKind.CLAUDE_CODE,
    state,
    taskTitle: `task ${id}`,
    createdAtUnixMs: 1_000n * id,
    endedAtUnixMs: state === SessionState.EXITED || state === SessionState.FAILED ? 1_000n : undefined,
  });
}

function worker(id: bigint, name: string, online = true) {
  return create(WorkerSchema, { id, name, hostname: `${name}.local`, online });
}

function event(ev: Event["event"]): Event {
  return create(EventSchema, { event: ev });
}

function glanceField(key: string, value: string) {
  return create(ContextFieldSchema, { key, label: key, value, kind: ContextKind.BADGE });
}

function context(sessionId: bigint, glanceValue: string) {
  return create(SessionContextSchema, {
    sessionId,
    glance: [glanceField("status", glanceValue)],
  });
}

function forward(id: bigint, sessionId: bigint, targetReachable?: boolean) {
  return create(SessionForwardSchema, {
    id,
    sessionId,
    workerPort: 5173,
    listenerPort: 41000 + Number(id),
    label: "vite",
    scheme: "http",
    url: `http://pm-box:${41000 + Number(id)}`,
    targetReachable,
  });
}

function hydrated(): AppState {
  const snapshot = create(SnapshotSchema, {
    buckets: [bucket(1n, "work")],
    projects: [project(10n, 1n, "api")],
    sessions: [session(100n, 10n)],
    workers: [worker(0n, "local"), worker(5n, "builder")],
    contexts: [context(100n, "building")],
    forwards: [forward(7n, 100n, true)],
  });
  return reduce(initialState, { type: "snapshot", snapshot });
}

describe("reduce", () => {
  it("populates state from a snapshot", () => {
    const state = hydrated();
    expect(state.hydrated).toBe(true);
    expect(state.buckets.get("1")?.name).toBe("work");
    expect(state.projects.get("10")?.name).toBe("api");
    expect(state.sessions.get("100")?.taskTitle).toBe("task 100");
    expect(state.workers.get("0")?.name).toBe("local");
    expect(state.workers.get("5")?.name).toBe("builder");
    expect(state.forwards.get("7")?.url).toBe("http://pm-box:41007");
  });

  it("applies forward change/remove events", () => {
    let state = hydrated();
    expect(state.forwards.get("7")?.targetReachable).toBe(true);

    state = reduce(state, {
      type: "event",
      event: event({ case: "forwardChanged", value: forward(7n, 100n, false) }),
    });
    expect(state.forwards.get("7")?.targetReachable).toBe(false);

    state = reduce(state, {
      type: "event",
      event: event({ case: "forwardChanged", value: forward(8n, 100n) }),
    });
    expect(state.forwards.size).toBe(2);
    expect(state.forwards.get("8")?.targetReachable).toBeUndefined();

    state = reduce(state, {
      type: "event",
      event: event({ case: "forwardRemoved", value: 7n }),
    });
    expect(state.forwards.size).toBe(1);
    expect(state.forwards.has("7")).toBe(false);
  });

  it("applies worker change/remove events", () => {
    let state = hydrated();
    expect(state.workers.size).toBe(2);

    state = reduce(state, {
      type: "event",
      event: event({ case: "workerChanged", value: worker(6n, "runner") }),
    });
    expect(state.workers.size).toBe(3);
    expect(state.workers.get("6")?.name).toBe("runner");

    state = reduce(state, {
      type: "event",
      event: event({ case: "workerChanged", value: worker(5n, "builder", false) }),
    });
    expect(state.workers.size).toBe(3);
    expect(state.workers.get("5")?.online).toBe(false);

    state = reduce(state, {
      type: "event",
      event: event({ case: "workerRemoved", value: 5n }),
    });
    expect(state.workers.size).toBe(2);
    expect(state.workers.has("5")).toBe(false);

    state = reduce(state, {
      type: "event",
      event: event({ case: "workerRemoved", value: 9999n }),
    });
    expect(state.workers.size).toBe(2);
  });

  it("upserts on sessionChanged", () => {
    let state = hydrated();
    state = reduce(state, {
      type: "event",
      event: event({ case: "sessionChanged", value: session(101n, 10n) }),
    });
    expect(state.sessions.size).toBe(2);

    const updated = session(100n, 10n, SessionState.NEEDS_INPUT);
    state = reduce(state, {
      type: "event",
      event: event({ case: "sessionChanged", value: updated }),
    });
    expect(state.sessions.size).toBe(2);
    expect(state.sessions.get("100")?.state).toBe(SessionState.NEEDS_INPUT);
  });

  it("removes on sessionRemoved and ignores unknown ids", () => {
    let state = hydrated();
    state = reduce(state, {
      type: "event",
      event: event({ case: "sessionRemoved", value: 100n }),
    });
    expect(state.sessions.size).toBe(0);

    state = reduce(state, {
      type: "event",
      event: event({ case: "sessionRemoved", value: 9999n }),
    });
    expect(state.sessions.size).toBe(0);
  });

  it("expires subscription ownership after sixty seconds but retains selected/history rows", () => {
    const ended = session(300n, 10n, SessionState.EXITED);
    let state = reduce(initialState, { type: "snapshot", snapshot: create(SnapshotSchema, { sessions: [ended] }) });
    state = reduce(state, { type: "selectedSession", id: "300" });
    state = reduce(state, { type: "expireSessions", nowUnixMs: 61_001 });
    expect(state.sessions.has("300")).toBe(true);
    state = reduce(state, { type: "selectedSession", id: null });
    expect(state.sessions.has("300")).toBe(false);

    state = reduce(initialState, { type: "snapshot", snapshot: create(SnapshotSchema, { sessions: [ended] }) });
    state = reduce(state, { type: "sessionPage", source: "history", sessions: [ended], replace: true });
    state = reduce(state, { type: "expireSessions", nowUnixMs: 61_001 });
    expect(state.sessions.has("300")).toBe(true);
    state = reduce(state, { type: "sessionPage", source: "history", sessions: [], replace: true });
    expect(state.sessions.has("300")).toBe(false);
  });

  it("updates one canonical row from events without retaining cleared search ownership", () => {
    const ended = session(301n, 10n, SessionState.EXITED);
    let state = reduce(initialState, { type: "sessionPage", source: "search", sessions: [ended], replace: true });
    const updated = { ...ended, headline: "fresh" };
    state = reduce(state, { type: "event", event: event({ case: "sessionChanged", value: updated }) });
    expect(state.sessions.get("301")?.headline).toBe("fresh");
    state = reduce(state, { type: "sessionPage", source: "search", sessions: [], replace: true });
    expect(state.sessions.has("301")).toBe(true); // the lifecycle event owns grace retention
    state = reduce(state, { type: "expireSessions", nowUnixMs: 61_001 });
    expect(state.sessions.has("301")).toBe(false);
  });

  it("applies bucket and project change/remove events", () => {
    let state = hydrated();
    state = reduce(state, {
      type: "event",
      event: event({ case: "bucketChanged", value: bucket(2n, "oss") }),
    });
    state = reduce(state, {
      type: "event",
      event: event({ case: "projectChanged", value: project(11n, 2n, "cli") }),
    });
    expect(state.buckets.size).toBe(2);
    expect(state.projects.size).toBe(2);

    state = reduce(state, {
      type: "event",
      event: event({ case: "projectRemoved", value: 11n }),
    });
    state = reduce(state, {
      type: "event",
      event: event({ case: "bucketRemoved", value: 2n }),
    });
    expect(state.buckets.size).toBe(1);
    expect(state.projects.size).toBe(1);
  });

  it("replaces all state on a fresh snapshot after reconnect", () => {
    let state = hydrated();
    state = reduce(state, {
      type: "event",
      event: event({ case: "sessionChanged", value: session(101n, 10n) }),
    });
    state = reduce(state, { type: "conn", phase: "offline" });
    state = reduce(state, { type: "conn", phase: "online" });

    const fresh = create(SnapshotSchema, {
      buckets: [bucket(2n, "oss")],
      projects: [project(20n, 2n, "tool")],
      sessions: [session(200n, 20n)],
    });
    state = reduce(state, { type: "snapshot", snapshot: fresh });

    expect([...state.sessions.keys()]).toEqual(["200"]);
    expect([...state.buckets.keys()]).toEqual(["2"]);
    expect([...state.projects.keys()]).toEqual(["20"]);
    expect(state.sessions.has("100")).toBe(false);
  });

  it("hydrates contexts and upserts/removes on contextChanged", () => {
    let state = hydrated();
    expect(state.contexts.get("100")?.glance[0]?.value).toBe("building");

    state = reduce(state, {
      type: "event",
      event: event({ case: "contextChanged", value: context(100n, "testing") }),
    });
    expect(state.contexts.get("100")?.glance[0]?.value).toBe("testing");

    // Empty bags mean the context was cleared and should be dropped.
    state = reduce(state, {
      type: "event",
      event: event({ case: "contextChanged", value: create(SessionContextSchema, { sessionId: 100n }) }),
    });
    expect(state.contexts.has("100")).toBe(false);
  });

  it("tracks connection phase", () => {
    let state = reduce(initialState, { type: "conn", phase: "online" });
    expect(state.conn).toBe("online");
    state = reduce(state, { type: "conn", phase: "offline" });
    expect(state.conn).toBe("offline");
  });

  it("hydrates and upserts items and briefings", () => {
    const item = create(ItemSchema, {
      id: 21n,
      bucketId: 1n,
      title: "review PR",
      status: ItemStatus.INBOX,
    });
    const briefing = create(BucketBriefingSchema, {
      id: 5n,
      bucketId: 1n,
      tsUnixMs: 1_000n,
      markdown: "quiet",
    });
    let state = reduce(initialState, {
      type: "snapshot",
      snapshot: create(SnapshotSchema, { items: [item], briefings: [briefing] }),
    });
    expect(state.items.get("1/21")?.title).toBe("review PR");
    expect(state.briefings.get("1")?.markdown).toBe("quiet");

    state = reduce(state, {
      type: "event",
      event: event({
        case: "itemChanged",
        value: create(ItemSchema, {
          id: 21n,
          bucketId: 1n,
          title: "review PR",
          status: ItemStatus.DONE,
        }),
      }),
    });
    expect(state.items.get("1/21")?.status).toBe(ItemStatus.DONE);

    // A newer briefing for the bucket replaces the latest.
    state = reduce(state, {
      type: "event",
      event: event({
        case: "briefingChanged",
        value: create(BucketBriefingSchema, { id: 6n, bucketId: 1n, markdown: "busy" }),
      }),
    });
    expect(state.briefings.get("1")?.markdown).toBe("busy");

    state = reduce(state, {
      type: "event",
      event: event({ case: "itemRemoved", value: create(ItemRefSchema, { bucketId: 1n, itemId: 21n }) }),
    });
    expect(state.items.has("1/21")).toBe(false);
  });

  it("hydrates and converges only the user settings delivered by the scoped socket", () => {
    let state = reduce(initialState, {
      type: "snapshot",
      snapshot: create(SnapshotSchema, {
        userSettings: [create(UserSettingSchema, {
          key: "terminal.theme",
          valueJson: "{\"name\":\"first\"}",
        })],
      }),
    });
    expect(state.userSettings.get("terminal.theme")).toContain("first");

    state = reduce(state, {
      type: "event",
      event: event({
        case: "userSettingChanged",
        value: create(UserSettingChangedSchema, {
          key: "terminal.theme",
          valueJson: "{\"name\":\"same-user update\"}",
        }),
      }),
    });
    expect(state.userSettings.get("terminal.theme")).toContain("same-user update");

    state = reduce(state, {
      type: "event",
      event: event({
        case: "userSettingChanged",
        value: create(UserSettingChangedSchema, { key: "terminal.theme" }),
      }),
    });
    expect(state.userSettings.has("terminal.theme")).toBe(false);
  });
});
