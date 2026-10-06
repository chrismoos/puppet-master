import { afterEach, describe, expect, it, vi } from "vitest";
import {
  CONNECTION_PANEL_DEFAULT_WIDTH,
  CONNECTION_PANEL_MAX_WIDTH,
  CONNECTION_PANEL_MIN_WIDTH,
  readConnectionPanelWidth,
  readReviewRailCollapsed,
  readReviewRailWidth,
  readSidebarWidth,
  REVIEW_RAIL_DEFAULT_WIDTH,
  REVIEW_RAIL_MAX_WIDTH,
  REVIEW_RAIL_MIN_WIDTH,
  removeLegacyNavigationPreferences,
  SIDEBAR_DEFAULT_WIDTH,
} from "./storage";

function stubStorage(values: Map<string, string>) {
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  });
}

afterEach(() => vi.unstubAllGlobals());

describe("legacy navigation storage", () => {
  it("removes obsolete view tabs, Board pin mode, and project collapse state", () => {
    const values = new Map<string, string>();
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
      removeItem: (key: string) => values.delete(key),
    });

    values.set("pm.viewTabs", JSON.stringify(["session:1", "workspace:4"]));
    values.set("pm.boardSidebarPinned", "1");
    values.set("pm.collapsed.projects", JSON.stringify(["10"]));
    values.set("unrelated", "preserved");

    removeLegacyNavigationPreferences();

    expect(values.has("pm.viewTabs")).toBe(false);
    expect(values.has("pm.boardSidebarPinned")).toBe(false);
    expect(values.has("pm.collapsed.projects")).toBe(false);
    expect(values.get("unrelated")).toBe("preserved");
  });
});

describe("stored column widths", () => {
  it("restores the width each column was left at", () => {
    stubStorage(new Map([["pm.sidebarWidth", "410"], ["pm.reviewRailWidth", "380"]]));
    expect(readSidebarWidth()).toBe(410);
    expect(readReviewRailWidth()).toBe(380);
  });

  it("falls back to the default when nothing is stored or the value is not a width", () => {
    stubStorage(new Map([["pm.reviewRailWidth", "wide"]]));
    expect(readReviewRailWidth()).toBe(REVIEW_RAIL_DEFAULT_WIDTH);
    expect(readSidebarWidth()).toBe(SIDEBAR_DEFAULT_WIDTH);
  });

  it("pulls a width stored outside the limits back inside them", () => {
    stubStorage(new Map([["pm.reviewRailWidth", "20"]]));
    expect(readReviewRailWidth()).toBe(REVIEW_RAIL_MIN_WIDTH);
    stubStorage(new Map([["pm.reviewRailWidth", "4000"]]));
    expect(readReviewRailWidth()).toBe(REVIEW_RAIL_MAX_WIDTH);
  });
});

describe("stored connection panel width", () => {
  it("restores the width the panel was dragged to, inside its limits", () => {
    stubStorage(new Map());
    expect(readConnectionPanelWidth()).toBe(CONNECTION_PANEL_DEFAULT_WIDTH);
    stubStorage(new Map([["pm.connectionPanelWidth", "612"]]));
    expect(readConnectionPanelWidth()).toBe(612);
    stubStorage(new Map([["pm.connectionPanelWidth", "20"]]));
    expect(readConnectionPanelWidth()).toBe(CONNECTION_PANEL_MIN_WIDTH);
    stubStorage(new Map([["pm.connectionPanelWidth", "4000"]]));
    expect(readConnectionPanelWidth()).toBe(CONNECTION_PANEL_MAX_WIDTH);
  });
});

describe("stored file list collapse", () => {
  it("starts expanded and reads back what was stored", () => {
    const values = new Map<string, string>();
    stubStorage(values);
    expect(readReviewRailCollapsed()).toBe(false);
    values.set("pm.reviewRailCollapsed", "1");
    expect(readReviewRailCollapsed()).toBe(true);
    values.set("pm.reviewRailCollapsed", "0");
    expect(readReviewRailCollapsed()).toBe(false);
  });
});
