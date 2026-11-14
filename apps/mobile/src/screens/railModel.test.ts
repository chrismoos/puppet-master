import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { BucketIconSchema, BucketSchema, ProjectSchema, SessionSchema, SessionState } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
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
    create(SessionSchema, { id: 4n, projectId: 30n, state: SessionState.IDLE, idleUnseen: true }),
    create(SessionSchema, { id: 6n, projectId: 40n, state: SessionState.IDLE }),
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
  it("follows the web rail: a count waiting on you, working, and unseen finished turns only", () => {
    expect(railTiles(fleet()).map(({ letters, needs, working, finishedUnseen }) => [letters, needs, working, finishedUnseen])).toEqual([
      ["PM", 1, true, false],
      ["AI", 0, true, false],
      ["FU", 0, false, true],
      ["M6", 0, false, false],
    ]);
  });

  it("shows a bucket's own icon when one is set", () => {
    const state = fleet();
    const bucket = state.buckets.get("2")!;
    const withIcon = { ...state, buckets: new Map(state.buckets).set("2", { ...bucket, icon: create(BucketIconSchema, { text: "🤖", color: "#ff8800" }) }) };
    const tile = railTiles(withIcon).find((t) => t.id === "2");
    expect(tile?.icon).toEqual({ text: "🤖", color: "#ff8800", image: undefined });
    expect(railTiles(fleet()).find((t) => t.id === "1")?.icon).toBeUndefined();
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
