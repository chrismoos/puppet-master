import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { BucketSchema, ProjectSchema, SessionSchema, SessionState } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { initialState, type AppState } from "@puppet-master/client-core/state/reducer";
import { railSummary, railTiles, tapBucket } from "./railModel";

function fleet(): AppState {
  const buckets = [
    create(BucketSchema, { id: 1n, name: "puppet master", position: 0 }),
    create(BucketSchema, { id: 2n, name: "ai", position: 1 }),
    create(BucketSchema, { id: 3n, name: "fullstack", position: 2 }),
    create(BucketSchema, { id: 4n, name: "m6502", position: 3 }),
  ];
  const projects = [1n, 2n, 3n, 4n].map((id) => create(ProjectSchema, { id: id * 10n, bucketId: id, name: `p${id}` }));
  const sessions = [
    create(SessionSchema, { id: 1n, projectId: 10n, state: SessionState.NEEDS_INPUT }),
    create(SessionSchema, { id: 2n, projectId: 10n, state: SessionState.WORKING }),
    create(SessionSchema, { id: 3n, projectId: 20n, state: SessionState.WORKING }),
    create(SessionSchema, { id: 4n, projectId: 30n, state: SessionState.IDLE }),
    create(SessionSchema, { id: 5n, projectId: 40n, state: SessionState.EXITED, endedAtUnixMs: 1n }),
  ];
  return {
    ...initialState,
    buckets: new Map(buckets.map((b) => [b.id.toString(), b])),
    projects: new Map(projects.map((p) => [p.id.toString(), p])),
    sessions: new Map(sessions.map((s) => [s.id.toString(), s])),
  };
}

describe("the session list's bucket rail", () => {
  it("shows each bucket's letters and its most urgent dot, in bucket order", () => {
    expect(railTiles(fleet()).map(({ letters, dot }) => [letters, dot])).toEqual([
      ["PM", "needs"],
      ["AI", "working"],
      ["FU", "idle"],
      ["M6", null],
    ]);
  });

  it("narrows to a tapped bucket and shows every bucket when the selected one is tapped again", () => {
    expect(tapBucket({}, "2")).toEqual({ bucketId: "2" });
    expect(tapBucket({ bucketId: "2", projectId: "20" }, "2")).toEqual({});
    expect(tapBucket({ bucketId: "2", projectId: "20" }, "3")).toEqual({ bucketId: "3" });
  });

  it("sums what is working and waiting in view", () => {
    expect(railSummary(fleet(), "1")).toBe("1 working · 1 needs you");
    expect(railSummary(fleet(), undefined)).toBe("2 working · 1 needs you");
    expect(railSummary(fleet(), "4")).toBe("");
  });
});
