import { describe, expect, it } from "vitest";
import { insertPaneInTree, movePaneInTree, movePositionAt, type LayoutNode } from "./paneMoves";

const leaf = (pane: string, size?: number): LayoutNode => (size === undefined ? { pane } : { pane, size });

describe("movePositionAt", () => {
  it("picks the nearest edge inside the edge band and swaps in the middle", () => {
    expect(movePositionAt(0.1, 0.5)).toBe("left");
    expect(movePositionAt(0.9, 0.5)).toBe("right");
    expect(movePositionAt(0.5, 0.05)).toBe("above");
    expect(movePositionAt(0.5, 0.95)).toBe("below");
    expect(movePositionAt(0.5, 0.5)).toBe("swap");
    expect(movePositionAt(0.3, 0.5)).toBe("swap");
  });
});

describe("movePaneInTree", () => {
  const sideBySide: LayoutNode = { row: [leaf("agent", 60), leaf("log", 40)] };

  it("leaves the tree alone when a pane is dropped on itself", () => {
    expect(movePaneInTree(sideBySide, "log", "log", "left")).toBe(sideBySide);
  });

  it("swaps two leaves, keeping each position's size", () => {
    expect(movePaneInTree(sideBySide, "log", "agent", "swap")).toEqual({ row: [leaf("log", 60), leaf("agent", 40)] });
  });

  it("moves a pane to the other side of its sibling", () => {
    expect(movePaneInTree(sideBySide, "log", "agent", "left")).toEqual({ row: [leaf("log", 50), leaf("agent", 50)] });
  });

  it("stacks a pane under another by wrapping the target in a column", () => {
    expect(movePaneInTree(sideBySide, "log", "agent", "below")).toEqual({ column: [leaf("agent", 50), leaf("log", 50)] });
  });

  it("halves the target's share when inserting into a split along the same axis", () => {
    const three: LayoutNode = { row: [leaf("agent", 50), leaf("a", 25), leaf("b", 25)] };
    expect(movePaneInTree(three, "b", "agent", "right")).toEqual({ row: [leaf("agent", 33.4), leaf("b", 33.4), leaf("a", 33.3)] });
  });

  it("merges a split left with one child and flattens one nested along the same axis", () => {
    const tree: LayoutNode = {
      column: [{ row: [leaf("agent", 50), leaf("sh", 50)], size: 50 }, leaf("sig", 50)],
    };
    expect(movePaneInTree(tree, "sh", "sig", "below")).toEqual({
      column: [leaf("agent", 50), leaf("sig", 25), leaf("sh", 25)],
    });
  });

  it("returns the tree unchanged when the source or the target is not in it", () => {
    expect(movePaneInTree(sideBySide, "ghost", "agent", "left")).toBe(sideBySide);
    expect(movePaneInTree(sideBySide, "log", "ghost", "left")).toBe(sideBySide);
  });
});

describe("insertPaneInTree", () => {
  it("puts a new pane beside the agent alone", () => {
    expect(insertPaneInTree(leaf("agent"), "shell-1", "agent", "right")).toEqual({ row: [leaf("agent", 50), leaf("shell-1", 50)] });
  });

  it("puts a new pane above a pane inside a row", () => {
    expect(insertPaneInTree({ row: [leaf("agent", 60), leaf("log", 40)] }, "shell-1", "log", "above")).toEqual({
      row: [leaf("agent", 60), { column: [leaf("shell-1", 50), leaf("log", 50)], size: 40 }],
    });
  });
});
