import { create, type MessageInitShape } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import {
  BucketSchema,
  ProjectSchema,
  SessionRole,
  SessionSchema,
  SessionState,
} from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import type { SidebarModel } from "@puppet-master/client-core/state/sidebar";
import {
  ALL_SCOPE,
  bucketOfSession,
  bucketTallies,
  effectiveScope,
  isStaleRow,
  parseScope,
  scopeKey,
  scopeSidebar,
  STALE_IDLE_MS,
  type SidebarScope,
} from "./sidebarScope";

const NOW = 10 * STALE_IDLE_MS;
const session = (id: bigint, fields: MessageInitShape<typeof SessionSchema> = {}) =>
  create(SessionSchema, { id, projectId: 1n, state: SessionState.WORKING, ...fields });

function model(): SidebarModel {
  return {
    buckets: [
      {
        bucket: create(BucketSchema, { id: 1n, name: "puppet-master" }),
        supervisors: [session(10n, { role: SessionRole.SUPERVISOR })],
        sessions: [
          session(11n, { state: SessionState.NEEDS_INPUT, spawnedBySessionId: 10n }),
          session(12n),
        ],
      },
      {
        bucket: create(BucketSchema, { id: 2n, name: "bladerun" }),
        supervisors: [],
        sessions: [session(20n, { projectId: 2n })],
      },
    ],
    orphans: [session(30n, { state: SessionState.NEEDS_INPUT, projectId: 99n })],
    order: () => 0,
  };
}

describe("the stored sidebar scope", () => {
  it("round-trips every scope through its storage key", () => {
    const scopes: SidebarScope[] = [ALL_SCOPE, { kind: "needs" }, { kind: "bucket", bucketId: "7" }];
    for (const scope of scopes) expect(parseScope(scopeKey(scope))).toEqual(scope);
  });

  it("reads anything unrecognized as every bucket", () => {
    for (const raw of [null, "", "bucket:", "Needs", "project:3"]) expect(parseScope(raw)).toEqual(ALL_SCOPE);
  });

  it("falls back to every bucket when the stored bucket was deleted", () => {
    expect(effectiveScope({ kind: "bucket", bucketId: "9" }, new Set(["1"]))).toEqual(ALL_SCOPE);
    expect(effectiveScope({ kind: "bucket", bucketId: "1" }, new Set(["1"]))).toEqual({ kind: "bucket", bucketId: "1" });
    expect(effectiveScope({ kind: "needs" }, new Set())).toEqual({ kind: "needs" });
  });
});

describe("scopeSidebar", () => {
  it("lists only the chosen bucket and drops unfiled sessions", () => {
    const scoped = scopeSidebar(model(), { kind: "bucket", bucketId: "2" });
    expect(scoped.buckets.map(({ bucket }) => bucket.name)).toEqual(["bladerun"]);
    expect(scoped.orphans).toEqual([]);
  });

  it("keeps sessions needing input in every bucket, with the supervisor they hang off", () => {
    const scoped = scopeSidebar(model(), { kind: "needs" });
    expect(scoped.buckets.map(({ bucket }) => bucket.name)).toEqual(["puppet-master"]);
    expect(scoped.buckets[0].sessions.map((s) => s.id)).toEqual([11n]);
    expect(scoped.buckets[0].supervisors.map((s) => s.id)).toEqual([10n]);
    expect(scoped.orphans.map((s) => s.id)).toEqual([30n]);
  });

  it("leaves the whole sidebar alone for every bucket", () => {
    const full = model();
    expect(scopeSidebar(full, ALL_SCOPE)).toBe(full);
  });
});

describe("isStaleRow", () => {
  const idle = (id: bigint, ageMs: number) =>
    session(id, { state: SessionState.IDLE, lastActivityAtUnixMs: BigInt(NOW - ageMs) });

  it("folds a row only when every member has been idle for a day", () => {
    expect(isStaleRow([idle(1n, STALE_IDLE_MS)], NOW, null)).toBe(true);
    expect(isStaleRow([idle(1n, STALE_IDLE_MS - 1)], NOW, null)).toBe(false);
    expect(isStaleRow([idle(1n, 2 * STALE_IDLE_MS), idle(2n, 60_000)], NOW, null)).toBe(false);
  });

  it("never folds a session that is working, waiting on the user, or selected", () => {
    const old = BigInt(NOW - 2 * STALE_IDLE_MS);
    expect(isStaleRow([session(1n, { lastActivityAtUnixMs: old })], NOW, null)).toBe(false);
    expect(isStaleRow([session(1n, { state: SessionState.NEEDS_INPUT, lastActivityAtUnixMs: old })], NOW, null)).toBe(false);
    expect(isStaleRow([idle(1n, 2 * STALE_IDLE_MS)], NOW, "1")).toBe(false);
  });

  it("dates an ended session by when it ended and a silent one by when it started", () => {
    expect(isStaleRow([session(1n, { state: SessionState.EXITED, endedAtUnixMs: BigInt(NOW - STALE_IDLE_MS) })], NOW, null)).toBe(true);
    expect(isStaleRow([session(1n, { state: SessionState.EXITED, endedAtUnixMs: BigInt(NOW - 1000) })], NOW, null)).toBe(false);
    expect(isStaleRow([session(1n, { state: SessionState.IDLE, createdAtUnixMs: BigInt(NOW - 1000) })], NOW, null)).toBe(false);
  });
});

describe("bucket tallies", () => {
  const state = {
    projects: new Map([
      ["1", create(ProjectSchema, { id: 1n, bucketId: 1n })],
      ["2", create(ProjectSchema, { id: 2n, bucketId: 2n })],
    ]),
    sessions: new Map([
      ["1", session(1n, { state: SessionState.NEEDS_INPUT })],
      ["2", session(2n, { role: SessionRole.SUPERVISOR })],
      ["3", session(3n, { state: SessionState.EXITED })],
      ["4", session(4n, { projectId: 2n, state: SessionState.IDLE })],
      ["5", session(5n, { projectId: 99n })],
    ]),
  };

  it("counts live sessions and those needing input, skipping ended and unfiled ones", () => {
    expect(Object.fromEntries(bucketTallies(state))).toEqual({
      "1": { live: 2, needsInput: 1 },
      "2": { live: 1, needsInput: 0 },
    });
  });

  it("finds the bucket a session belongs to", () => {
    expect(bucketOfSession(state, "4")).toBe("2");
    expect(bucketOfSession(state, "5")).toBeNull();
    expect(bucketOfSession(state, "404")).toBeNull();
  });
});
