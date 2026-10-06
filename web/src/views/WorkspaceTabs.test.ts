import { describe, expect, it, vi } from "vitest";
import { adjacentWorkspaceTab, moveTab, moveTabToIndex, removeWorkspaceFromTabs, requestWorkspaceDelete, requestWorkspaceRename, tabInsertionIndex } from "./WorkspaceTabs";
import type { SavedWorkspace } from "../api/workspaces";

describe("workspace tab ordering", () => {
  it("moves workspace tabs before a drop target", () => {
    const tabs = [7, 1, 2];
    expect(moveTab(tabs, 2, 7)).toEqual([2, 7, 1]);
    expect(moveTab(tabs, 7, 2)).toEqual([1, 7, 2]);
    expect(moveTab(tabs, 7, null)).toEqual([1, 2, 7]);
  });

  it("ignores missing and self targets", () => {
    const tabs = [7, 1];
    expect(moveTab(tabs, 1, 1)).toBe(tabs);
    expect(moveTab(tabs, 9, 1)).toBe(tabs);
  });

  it("inserts before or after a tab and after the final tab", () => {
    const tabs = [1, 2, 3];
    expect(tabInsertionIndex(24, 0, 100, 1)).toBe(1);
    expect(tabInsertionIndex(76, 0, 100, 1)).toBe(2);
    expect(moveTabToIndex(tabs, 1, tabs.length)).toEqual([2, 3, 1]);
  });

  it("opens rename from a workspace tab title", () => {
    const event = { stopPropagation: vi.fn() };
    const onRename = vi.fn();
    const workspace = { id: 4, name: "release", position: 0 } as SavedWorkspace;

    requestWorkspaceRename(event, workspace, onRename);

    expect(event.stopPropagation).toHaveBeenCalledOnce();
    expect(onRename).toHaveBeenCalledWith(workspace);
  });

  it("opens delete without activating the workspace tab", () => {
    const event = { preventDefault: vi.fn(), stopPropagation: vi.fn() };
    const onDelete = vi.fn();
    const workspace = { id: 4, name: "release", position: 0 } as SavedWorkspace;

    requestWorkspaceDelete(event, workspace, onDelete);

    expect(event.preventDefault).toHaveBeenCalledOnce();
    expect(event.stopPropagation).toHaveBeenCalledOnce();
    expect(onDelete).toHaveBeenCalledWith(workspace);
  });

  it("compacts workspace positions after deletion", () => {
    const workspaces = [
      { id: 2, position: 0 },
      { id: 4, position: 1 },
      { id: 6, position: 2 },
    ] as SavedWorkspace[];

    const next = removeWorkspaceFromTabs(workspaces, 4);

    expect(next.map((workspace) => [workspace.id, workspace.position])).toEqual([[2, 0], [6, 1]]);
  });

  it("supports arrow, Home, and End keyboard navigation", () => {
    expect(adjacentWorkspaceTab(0, 3, "ArrowLeft")).toBe(2);
    expect(adjacentWorkspaceTab(2, 3, "ArrowRight")).toBe(0);
    expect(adjacentWorkspaceTab(1, 3, "Home")).toBe(0);
    expect(adjacentWorkspaceTab(1, 3, "End")).toBe(2);
    expect(adjacentWorkspaceTab(1, 3, "Enter")).toBeNull();
  });
});
