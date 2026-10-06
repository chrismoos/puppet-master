import { describe, expect, it } from "vitest";
import {
  settingsEntryReturnTarget,
  settingsReturnPath,
  settingsReturnView,
  parseSettingsReturnTarget,
  serializeSettingsReturnTarget,
  type SettingsReturnInventory,
} from "./settingsNavigation";

const inventory = (
  sessions: string[] = [],
  workspaces: number[] = [],
  retainedViews: SettingsReturnInventory["retainedViews"] = [],
  recentViews: SettingsReturnInventory["recentViews"] = [],
): SettingsReturnInventory => ({
  liveSessionIds: new Set(sessions),
  workspaceIds: new Set(workspaces),
  retainedViews,
  recentViews,
});

describe("Settings return navigation", () => {
  it("carries a live previously open session through Settings entered from Home", () => {
    expect(settingsEntryReturnTarget(
      { name: "home" },
      "17",
      new Set(["17"]),
    )).toEqual({ name: "session", id: "17" });
    expect(settingsEntryReturnTarget(
      { name: "home" },
      "17",
      new Set(),
    )).toEqual({ name: "home" });
  });

  it("round-trips exact application routes for refresh-safe restoration", () => {
    const routes = [
      { name: "session", id: "9007199254740993" } as const,
      { name: "workspace", id: 8 } as const,
      { name: "board", bucketId: "3" } as const,
      { name: "item", bucketId: "3", id: "65" } as const,
    ];
    for (const route of routes) {
      expect(parseSettingsReturnTarget(serializeSettingsReturnTarget(route))).toEqual(route);
    }
  });

  it("keeps the exact live session or saved workspace mounted", () => {
    expect(settingsReturnView(
      { name: "session", id: "17" },
      inventory(["17"], [8]),
    )).toEqual({ name: "session", id: "17" });
    expect(settingsReturnPath(
      { name: "workspace", id: 8 },
      inventory(["17"], [8]),
    )).toBe("/workspace/8");
  });

  it("falls back through retained tabs and then recent live views", () => {
    const retained = inventory(
      ["19"],
      [9],
      [{ name: "session", id: "ended" }, { name: "workspace", id: 9 }],
      [{ name: "session", id: "19" }],
    );
    expect(settingsReturnPath({ name: "session", id: "17" }, retained)).toBe("/workspace/9");

    const recent = inventory(
      ["19"],
      [],
      [{ name: "workspace", id: 9 }],
      [{ name: "session", id: "19" }],
    );
    expect(settingsReturnPath({ name: "workspace", id: 8 }, recent)).toBe("/session/19");
    expect(settingsReturnPath({ name: "session", id: "17" }, inventory())).toBe("/");
  });

  it("restores non-terminal routes without browser history", () => {
    expect(settingsReturnPath({ name: "item", bucketId: "3", id: "65" }, inventory())).toBe("/bucket/3/item/65");
    expect(settingsReturnPath({ name: "home" }, inventory())).toBe("/");
  });
});
