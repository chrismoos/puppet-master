import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import {
  ItemSchema,
  ProjectSchema,
  SessionSchema,
  SessionState,
  type Item,
  type Project,
  type Session,
} from "../gen/pm/v1/pm_pb";
import { buildItemResponseOptions, resolvePrimaryResponseTarget } from "./itemResponse";

const NOW = 1_700_000_000_000;

function item(overrides: Partial<Omit<Item, "$typeName" | "$unknown">> = {}): Item {
  return create(ItemSchema, { id: 1n, bucketId: 1n, title: "Question", ...overrides });
}

function project(id: bigint, bucketId: bigint, name: string): Project {
  return create(ProjectSchema, { id, bucketId, name });
}

function session(
  id: bigint,
  projectId: bigint,
  overrides: Partial<Omit<Session, "$typeName" | "$unknown">> = {},
): Session {
  return create(SessionSchema, {
    id,
    projectId,
    state: SessionState.WORKING,
    supervisorApi: true,
    taskTitle: `session ${id}`,
    lastActivityAtUnixMs: BigInt(NOW - Number(id) * 1_000),
    ...overrides,
  });
}

describe("resolvePrimaryResponseTarget", () => {
  const projects = [project(10n, 1n, "Alpha"), project(20n, 2n, "Other")];

  it("prefers the most recently active linked live supervisor", () => {
    const sessions = [
      session(1n, 10n, { lastActivityAtUnixMs: BigInt(NOW - 100) }),
      session(2n, 10n, { lastActivityAtUnixMs: BigInt(NOW - 2_000) }),
      session(3n, 10n, { lastActivityAtUnixMs: BigInt(NOW - 1_000) }),
    ];
    expect(
      resolvePrimaryResponseTarget(item({ sessionIds: [2n, 3n] }), sessions, projects),
    ).toEqual({ kind: "session", sessionId: 3n });
  });

  it("falls back to the most recently active live supervisor in the bucket", () => {
    const sessions = [
      session(1n, 10n, { lastActivityAtUnixMs: BigInt(NOW - 100) }),
      session(2n, 10n, { lastActivityAtUnixMs: BigInt(NOW - 2_000) }),
      session(3n, 20n, { lastActivityAtUnixMs: BigInt(NOW) }),
    ];
    expect(resolvePrimaryResponseTarget(item(), sessions, projects)).toEqual({
      kind: "session",
      sessionId: 1n,
    });
  });

  it("returns no default when there is no live supervisor", () => {
    const sessions = [
      session(1n, 10n, { state: SessionState.EXITED }),
      session(2n, 10n, { supervisorApi: false }),
    ];
    expect(resolvePrimaryResponseTarget(item(), sessions, projects)).toBeNull();
  });
});

describe("buildItemResponseOptions", () => {
  it("builds live existing supervisors and one new row per bucket project", () => {
    const projects = [
      project(11n, 1n, "Zulu"),
      project(10n, 1n, "Alpha"),
      project(20n, 2n, "Other"),
    ];
    const sessions = [
      session(1n, 10n, { goal: "Alpha lead", lastActivityAtUnixMs: BigInt(NOW - 1_000) }),
      session(2n, 11n, { goal: "Zulu lead", lastActivityAtUnixMs: BigInt(NOW - 2_000) }),
      session(3n, 10n, { state: SessionState.FAILED }),
      session(4n, 10n, { supervisorApi: false }),
      session(5n, 20n, { goal: "Other lead" }),
    ];

    const options = buildItemResponseOptions(item(), sessions, projects, NOW);

    expect(options.existingSupervisors).toEqual([
      {
        target: { kind: "session", sessionId: 1n },
        label: "Alpha lead · Alpha · 1s ago",
      },
      {
        target: { kind: "session", sessionId: 2n },
        label: "Zulu lead · Zulu · 2s ago",
      },
    ]);
    expect(options.newSupervisors).toEqual([
      {
        target: { kind: "newSupervisor", projectId: 10n },
        label: "New supervisor in Alpha",
      },
      {
        target: { kind: "newSupervisor", projectId: 11n },
        label: "New supervisor in Zulu",
      },
    ]);
    expect(options.replyOnly).toEqual({
      target: { kind: "replyOnly" },
      label: "Reply only (no session)",
    });
  });
});
