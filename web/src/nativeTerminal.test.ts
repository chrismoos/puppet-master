import { afterEach, describe, expect, it, vi } from "vitest";
import { placementOf, samePlacement } from "./nativeTerminal";

const LAYOUT = { cellWidth: 8, cellHeight: 17, fontSize: 13, padding: 4 };

function host(rect: { left: number; top: number; width: number; height: number }, visibility = "visible") {
  return { getBoundingClientRect: () => rect, visibility } as unknown as HTMLElement & { visibility: string };
}

describe("native terminal placement", () => {
  afterEach(() => vi.unstubAllGlobals());

  function stubDocument(visibilityState: string) {
    vi.stubGlobal("document", { visibilityState });
    vi.stubGlobal("getComputedStyle", (element: { visibility: string }) => ({ visibility: element.visibility }));
  }

  it("follows the page's terminal area and grid", () => {
    stubDocument("visible");
    expect(placementOf(host({ left: 300, top: 120, width: 800, height: 500 }), { visibleCellLayout: () => LAYOUT })).toEqual({
      x: 300, y: 120, width: 800, height: 500, ...LAYOUT, visible: true,
    });
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
    const a = { x: 1, y: 2, width: 3, height: 4, ...LAYOUT, visible: true };
    expect(samePlacement(a, { ...a })).toBe(true);
    expect(samePlacement(a, { ...a, y: 3 })).toBe(false);
    expect(samePlacement(a, { ...a, visible: false })).toBe(false);
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
