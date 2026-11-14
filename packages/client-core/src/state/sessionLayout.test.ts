import { describe, expect, it } from "vitest";
import { pagePanePath, parseSessionLayout, swipePanes, type PaneMeta, type SessionPaneLayout } from "./sessionLayout";

function pane(id: string, extra: Partial<PaneMeta>): PaneMeta {
  return { id, kind: "shell", title: "", version: 1, user_input: true, ...extra };
}

describe("swiping through a session's panes", () => {
  it("starts at the agent and follows the layout's reading order", () => {
    const layout: SessionPaneLayout = {
      rev: 3,
      tree: { row: [{ pane: "agent" }, { column: [{ pane: "shell-1" }, { pane: "preview" }] }] },
      panes: {
        "shell-1": pane("shell-1", { terminal_id: 41, command: "vim" }),
        preview: pane("preview", { kind: "page", title: "desktop-preview", page_token: "tok", version: 2 }),
      },
      unplaced: [],
    };
    expect(swipePanes(layout)).toEqual([
      { kind: "agent" },
      { kind: "terminal", paneId: "shell-1", terminalId: "41", title: "vim" },
      { kind: "page", paneId: "preview", pageToken: "tok", version: 2, title: "desktop-preview" },
    ]);
  });

  it("leaves out ended programs and unplaced panes, and is just the agent without a layout", () => {
    const layout: SessionPaneLayout = {
      rev: 1,
      tree: { row: [{ pane: "agent" }, { pane: "build" }] },
      panes: { build: pane("build", { kind: "program", terminal_id: 7, exit_code: 0 }), later: pane("later", { terminal_id: 8 }) },
      unplaced: ["later"],
    };
    expect(swipePanes(layout)).toEqual([{ kind: "agent" }]);
    expect(swipePanes(null)).toEqual([{ kind: "agent" }]);
  });

  it("parses only a layout with a tree, and builds a page pane's path", () => {
    expect(parseSessionLayout("{\"rev\":1}")).toBeNull();
    expect(parseSessionLayout("not json")).toBeNull();
    expect(pagePanePath("abc", 4)).toBe("pane-pages/abc/?v=4");
  });
});
