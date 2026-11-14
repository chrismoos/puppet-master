import { afterEach, describe, expect, it, vi } from "vitest";
import { placementOf, samePlacement } from "./nativeTerminal";

const LAYOUT = { cellWidth: 8, cellHeight: 17, fontSize: 13, padding: 4 };

function host(rect: { left: number; top: number; width: number; height: number }, visibility = "visible") {
  return { getBoundingClientRect: () => rect, visibility } as unknown as HTMLElement & { visibility: string };
}

describe("native terminal placement", () => {
  afterEach(() => vi.unstubAllGlobals());

  function stubDocument(visibilityState: string, devicePixelRatio = 2) {
    vi.stubGlobal("document", { visibilityState });
    vi.stubGlobal("window", { devicePixelRatio, innerWidth: 1440 });
    vi.stubGlobal("getComputedStyle", (element: { visibility: string }) => ({ visibility: element.visibility }));
  }

  it("follows the page's terminal area and grid", () => {
    stubDocument("visible");
    expect(placementOf(host({ left: 300, top: 120, width: 800, height: 500 }), { visibleCellLayout: () => LAYOUT })).toEqual({
      x: 300, y: 120, width: 800, height: 500, ...LAYOUT, visible: true, scale: 2, viewportWidth: 1440,
    });
  });

  it("follows the display's scale as the window moves to another", () => {
    stubDocument("visible", 1);
    expect(placementOf(host({ left: 0, top: 0, width: 800, height: 500 }), { visibleCellLayout: () => LAYOUT })?.scale).toBe(1);
  });

  it("waits for the page's terminal to measure its cells", () => {
    stubDocument("visible");
    expect(placementOf(host({ left: 0, top: 0, width: 800, height: 500 }), { visibleCellLayout: () => null })).toBeNull();
  });

  it("is hidden while the area is hidden, empty or the tab is in the background", () => {
    stubDocument("visible");
    const stage = { visibleCellLayout: () => LAYOUT };
    expect(placementOf(host({ left: 0, top: 0, width: 800, height: 500 }, "hidden"), stage)?.visible).toBe(false);
    expect(placementOf(host({ left: 0, top: 0, width: 0, height: 500 }), stage)?.visible).toBe(false);
    stubDocument("hidden");
    expect(placementOf(host({ left: 0, top: 0, width: 800, height: 500 }), stage)?.visible).toBe(false);
  });

  it("compares every field", () => {
    const a = { x: 1, y: 2, width: 3, height: 4, ...LAYOUT, visible: true, scale: 2, viewportWidth: 1440 };
    expect(samePlacement(a, { ...a })).toBe(true);
    expect(samePlacement(a, { ...a, y: 3 })).toBe(false);
    expect(samePlacement(a, { ...a, visible: false })).toBe(false);
    expect(samePlacement(a, { ...a, scale: 1 })).toBe(false);
    expect(samePlacement(a, { ...a, viewportWidth: 1200 })).toBe(false);
  });
});

describe("warm native terminals", () => {
  it("closes the longest hidden first, keeping the limit", async () => {
    const { warmEvictions } = await import("./nativeTerminal");
    const hidden = new Map([["a", 30], ["b", 10], ["c", 20], ["d", 40]]);
    expect(warmEvictions(hidden, 2)).toEqual(["b", "c"]);
    expect(warmEvictions(hidden, 8)).toEqual([]);
  });
});

describe("the native terminal theme", () => {
  it("carries the page theme's colors and its translucent selection", async () => {
    const { nativeThemeOf, parseThemeColor } = await import("./nativeTerminal");
    expect(parseThemeColor("#44475a")).toEqual({ r: 0x44, g: 0x47, b: 0x5a });
    expect(parseThemeColor("#ff000080")).toEqual({ r: 255, g: 0, b: 0, a: 128 / 255 });
    expect(parseThemeColor("rgb(1, 2, 3)")).toBeUndefined();
    const ink = { r: 1, g: 2, b: 3 };
    const paper = { r: 250, g: 250, b: 250 };
    expect(nativeThemeOf({ foreground: ink, background: paper, cursor: ink, palette: [ink], selectionBackground: "#eee8d5" })).toEqual({
      foreground: ink,
      background: paper,
      cursor: ink,
      palette: [ink],
      selection: { r: 0xee, g: 0xe8, b: 0xd5 },
    });
    expect(nativeThemeOf({ foreground: ink, background: paper, cursor: ink })).not.toHaveProperty("selection");
  });
});
