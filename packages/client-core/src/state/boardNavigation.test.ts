import { describe, expect, it } from "vitest";
import { boardReturnPath, rememberBoardView, validBoardView, type BoardViewInventory } from "./boardNavigation";

const inventory = (sessions: string[] = [], workspaces: number[] = []): BoardViewInventory => ({
  liveSessionIds: new Set(sessions),
  workspaceIds: new Set(workspaces),
});

describe("per-bucket Board previous-view memory", () => {
  it("remembers exact session and workspace routes independently by bucket", () => {
    let memory = new Map();
    memory = new Map(rememberBoardView(memory, "1", { name: "session", id: "17" }));
    memory = new Map(rememberBoardView(memory, "2", { name: "workspace", id: 8 }));

    expect(validBoardView(memory, "1", inventory(["17"], [8]))).toEqual({ name: "session", id: "17" });
    expect(validBoardView(memory, "2", inventory(["17"], [8]))).toEqual({ name: "workspace", id: 8 });
  });

  it("does not overwrite memory while traversing Board and item routes", () => {
    const original = rememberBoardView(new Map(), "1", { name: "session", id: "17" });
    expect(rememberBoardView(original, "1", { name: "board", bucketId: "1" })).toBe(original);
    expect(rememberBoardView(original, "1", { name: "item", bucketId: "1", id: "59" })).toBe(original);
  });

  it("falls back safely to home for missing, ended, or deleted targets", () => {
    const session = rememberBoardView(new Map(), "1", { name: "session", id: "17" });
    const workspace = rememberBoardView(session, "2", { name: "workspace", id: 8 });

    expect(boardReturnPath(session, "1", inventory([], []))).toBe("/");
    expect(boardReturnPath(workspace, "2", inventory(["17"], []))).toBe("/");
    expect(boardReturnPath(workspace, "3", inventory(["17"], [8]))).toBe("/");
  });

  it("returns deterministic application routes without consulting browser history", () => {
    const session = rememberBoardView(new Map(), "1", { name: "session", id: "9007199254740993" });
    expect(boardReturnPath(session, "1", inventory(["9007199254740993"]))).toBe("/session/9007199254740993");
  });
});
