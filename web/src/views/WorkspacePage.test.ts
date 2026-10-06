import { describe, expect, it, vi } from "vitest";
import { paneDragGhostLabel, shouldAdoptIncomingLayout, togglePaneZoomFromHeader } from "./WorkspacePage";

describe("workspace pane zoom", () => {
  it("toggles from header whitespace", () => {
    const onToggle = vi.fn();
    const target = { closest: vi.fn().mockReturnValue(null) };

    togglePaneZoomFromHeader(target, "pane-one", onToggle);

    expect(target.closest).toHaveBeenCalledWith("button, select, .workspace-renderer-warning");
    expect(onToggle).toHaveBeenCalledWith("pane-one");
  });

  it("ignores pane controls", () => {
    const onToggle = vi.fn();
    togglePaneZoomFromHeader({ closest: vi.fn().mockReturnValue({}) }, "pane-one", onToggle);
    expect(onToggle).not.toHaveBeenCalled();
  });
});

describe("incoming layout adoption", () => {
  it("always adopts when the workspace changes", () => {
    expect(shouldAdoptIncomingLayout(false, "a", "a", ["a"])).toBe(true);
  });

  it("skips layouts already matching local state", () => {
    expect(shouldAdoptIncomingLayout(true, "a", "a", [])).toBe(false);
  });

  it("skips echoes of recent saves that local edits have outpaced", () => {
    expect(shouldAdoptIncomingLayout(true, "saved", "newer", ["saved"])).toBe(false);
  });

  it("adopts genuine remote changes", () => {
    expect(shouldAdoptIncomingLayout(true, "remote", "local", ["saved"])).toBe(true);
  });
});

describe("pane drag ghost label", () => {
  it("keeps short labels intact", () => {
    expect(paneDragGhostLabel("build · agent")).toBe("build · agent");
  });

  it("ellipsizes long labels", () => {
    const label = paneDragGhostLabel("very long session name · agent");
    expect(label).toBe("very long sessi…");
    expect(label.length).toBeLessThanOrEqual(16);
  });

  it("trims surrounding whitespace before truncating", () => {
    expect(paneDragGhostLabel("  build · agent  ")).toBe("build · agent");
  });
});
