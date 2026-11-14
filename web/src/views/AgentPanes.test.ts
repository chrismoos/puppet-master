import { describe, expect, it } from "vitest";
import { isPaneForward, layoutHasPanes, measure, paneTerminalIds, parseSessionLayout, type Gutter, type Placed, type SessionPaneLayout } from "./AgentPanes";

function layout(tree: SessionPaneLayout["tree"], collapsed: string[] = []): SessionPaneLayout {
  const ids = ["a", "b", "c"];
  return {
    rev: 1,
    tree,
    unplaced: [],
    panes: Object.fromEntries(ids.map((id, index) => [id, {
      id,
      kind: "program" as const,
      title: id,
      terminal_id: 10 + index,
      version: 0,
      user_input: false,
      collapsed: collapsed.includes(id),
    }])),
  };
}

function place(value: SessionPaneLayout, w = 1001, h = 600) {
  const panes = new Map<string, Placed>();
  const gutters: Gutter[] = [];
  measure(value.tree, { x: 0, y: 0, w, h }, null, [], value, panes, gutters);
  return { panes, gutters };
}

describe("pane geometry", () => {
  it("splits a row by size with a 1px divider between", () => {
    const { panes, gutters } = place(layout({ row: [{ pane: "agent", size: 60 }, { pane: "a", size: 40 }] }));
    expect(panes.get("agent")!.rect).toEqual({ x: 0, y: 0, w: 600, h: 600 });
    expect(panes.get("a")!.rect).toEqual({ x: 601, y: 0, w: 400, h: 600 });
    expect(gutters).toHaveLength(1);
    expect(gutters[0].rect).toEqual({ x: 600, y: 0, w: 1, h: 600 });
  });

  it("nests columns inside rows and records each leaf's parent axis", () => {
    const { panes } = place(layout({ row: [{ pane: "agent" }, { column: [{ pane: "a" }, { pane: "b" }] }] }), 1001, 601);
    expect(panes.get("a")!.axis).toBe("column");
    expect(panes.get("a")!.rect.h + panes.get("b")!.rect.h + 1).toBe(601);
    expect(panes.get("b")!.rect.y).toBe(panes.get("a")!.rect.h + 1);
  });

  it("shrinks a collapsed pane to its header and gives the rest to its siblings", () => {
    const { panes, gutters } = place(layout({ column: [{ pane: "agent" }, { pane: "a", size: 50 }, { pane: "b", size: 50 }] }, ["a"]), 800, 602);
    expect(panes.get("a")!.rect.h).toBe(24);
    expect(panes.get("agent")!.rect.h + panes.get("b")!.rect.h).toBeCloseTo(602 - 2 - 24);
    expect(gutters.map((gutter) => gutter.index)).toEqual([]);
  });

  it("keeps dividers only between panes that are both expanded", () => {
    const { gutters } = place(layout({ row: [{ pane: "agent" }, { pane: "a" }, { pane: "b" }] }, ["b"]));
    expect(gutters).toHaveLength(1);
    expect(gutters[0].index).toBe(0);
  });
});

describe("pane layout helpers", () => {
  it("reads a published layout and ignores anything unparsable", () => {
    expect(parseSessionLayout(undefined)).toBeNull();
    expect(parseSessionLayout("{not json")).toBeNull();
    const parsed = parseSessionLayout(JSON.stringify(layout({ pane: "agent" })));
    expect(parsed?.rev).toBe(1);
    expect(layoutHasPanes(parsed)).toBe(false);
    expect(layoutHasPanes(layout({ row: [{ pane: "agent" }, { pane: "a" }] }))).toBe(true);
  });

  it("names the terminals panes hold and the forwards they own", () => {
    expect([...paneTerminalIds(layout({ pane: "agent" }))].sort()).toEqual(["10", "11", "12"]);
    expect(isPaneForward("pane-7-live", "7")).toBe(true);
    expect(isPaneForward("pane-70-live", "7")).toBe(false);
    expect(isPaneForward("docs", "7")).toBe(false);
  });
});
