import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { CommandResultSchema, PermissionMode } from "../gen/pm/v1/pm_pb";
import {
  CommandTracker,
  FIRE_AND_FORGET_SEQ,
  createProjectMsg,
  setBucketPermissionModeMsg,
  setProjectPermissionModeMsg,
} from "./commands";

function result(seq: bigint, ok: boolean, error = "", createdId?: bigint) {
  return create(CommandResultSchema, { seq, ok, error, createdId });
}

describe("CommandTracker", () => {
  it("never allocates the fire-and-forget seq", () => {
    const tracker = new CommandTracker();
    for (let i = 0; i < 5; i++) {
      expect(tracker.nextSeq()).not.toBe(FIRE_AND_FORGET_SEQ);
    }
  });

  it("allocates unique seqs", () => {
    const tracker = new CommandTracker();
    const seqs = new Set([tracker.nextSeq(), tracker.nextSeq(), tracker.nextSeq()]);
    expect(seqs.size).toBe(3);
  });

  it("resolves a pending command with its createdId", async () => {
    const tracker = new CommandTracker();
    const seq = tracker.nextSeq();
    const promise = tracker.register(seq);
    tracker.settle(result(seq, true, "", 42n));
    await expect(promise).resolves.toEqual({ createdId: 42n });
    expect(tracker.pendingCount).toBe(0);
  });

  it("rejects with the server error string verbatim", async () => {
    const tracker = new CommandTracker();
    const seq = tracker.nextSeq();
    const promise = tracker.register(seq);
    tracker.settle(result(seq, false, "project path is not absolute"));
    await expect(promise).rejects.toThrow("project path is not absolute");
  });

  it("only settles the matching seq", async () => {
    const tracker = new CommandTracker();
    const seqA = tracker.nextSeq();
    const seqB = tracker.nextSeq();
    const a = tracker.register(seqA);
    const b = tracker.register(seqB);
    tracker.settle(result(seqB, true));
    await expect(b).resolves.toEqual({ createdId: undefined });
    expect(tracker.pendingCount).toBe(1);
    tracker.settle(result(seqA, true));
    await expect(a).resolves.toEqual({ createdId: undefined });
  });

  it("ignores results for unknown seqs", async () => {
    const tracker = new CommandTracker();
    const seq = tracker.nextSeq();
    const promise = tracker.register(seq);
    tracker.settle(result(999n, false, "stale"));
    expect(tracker.pendingCount).toBe(1);
    tracker.settle(result(seq, true));
    await expect(promise).resolves.toEqual({ createdId: undefined });
  });

  it("rejects everything pending on failAll", async () => {
    const tracker = new CommandTracker();
    const a = tracker.register(tracker.nextSeq());
    const b = tracker.register(tracker.nextSeq());
    tracker.failAll("connection lost");
    await expect(a).rejects.toThrow("connection lost");
    await expect(b).rejects.toThrow("connection lost");
    expect(tracker.pendingCount).toBe(0);
  });

  it("ignores late results after failAll", async () => {
    const tracker = new CommandTracker();
    const seq = tracker.nextSeq();
    const promise = tracker.register(seq);
    tracker.failAll("connection lost");
    await expect(promise).rejects.toThrow("connection lost");
    expect(() => tracker.settle(result(seq, true))).not.toThrow();
  });
});

describe("permission-mode message builders", () => {
  it("builds a SetBucketPermissionMode payload", () => {
    const msg = setBucketPermissionModeMsg(7n, PermissionMode.BYPASS);
    expect(msg.case).toBe("setBucketPermissionMode");
    if (msg.case !== "setBucketPermissionMode") throw new Error("wrong case");
    expect(msg.value.bucketId).toBe(7n);
    expect(msg.value.mode).toBe(PermissionMode.BYPASS);
  });

  it("builds a SetProjectPermissionMode payload, carrying UNSPECIFIED to clear", () => {
    const msg = setProjectPermissionModeMsg(9n, PermissionMode.UNSPECIFIED);
    expect(msg.case).toBe("setProjectPermissionMode");
    if (msg.case !== "setProjectPermissionMode") throw new Error("wrong case");
    expect(msg.value.projectId).toBe(9n);
    expect(msg.value.mode).toBe(PermissionMode.UNSPECIFIED);
  });
});

describe("project creation message builder", () => {
  it("carries an explicit worker override", () => {
    const msg = createProjectMsg(7n, "api", "/srv/api", 4n);
    expect(msg.case).toBe("createProject");
    if (msg.case !== "createProject") throw new Error("wrong case");
    expect(msg.value).toMatchObject({
      bucketId: 7n,
      name: "api",
      path: "/srv/api",
      workerId: 4n,
    });
  });

  it("leaves the worker absent when the project inherits", () => {
    const msg = createProjectMsg(7n, "api", "/srv/api");
    expect(msg.case).toBe("createProject");
    if (msg.case !== "createProject") throw new Error("wrong case");
    expect(msg.value.workerId).toBeUndefined();
  });
});
