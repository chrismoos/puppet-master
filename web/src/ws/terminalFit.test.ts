import { describe, expect, it } from "vitest";
import { isRealPaneFit, spawnGeometry, XTERM_DEFAULT_COLS } from "./terminalFit";

describe("isRealPaneFit", () => {
  it("rejects the unmeasured xterm default of 80", () => {
    expect(isRealPaneFit(0, XTERM_DEFAULT_COLS)).toBe(false);
    expect(isRealPaneFit(0, null)).toBe(false);
    expect(isRealPaneFit(960, null)).toBe(false);
  });

  it("accepts 80 only when the host actually measures 80", () => {
    expect(isRealPaneFit(640, XTERM_DEFAULT_COLS)).toBe(true);
  });

  it("accepts a measured pane that is not 80", () => {
    expect(isRealPaneFit(1120, 140)).toBe(true);
  });
});

describe("spawnGeometry", () => {
  const probe = (result: { cols: number; rows: number } | null) => {
    let calls = 0;
    return { fn: () => (calls += 1, result), calls: () => calls };
  };

  it("takes the first pane that has been measured", () => {
    const { fn, calls } = probe({ cols: 80, rows: 24 });
    expect(spawnGeometry([{ cols: 140, rows: 40 }, { cols: 96, rows: 30 }], fn))
      .toEqual({ cols: 140, rows: 40 });
    expect(calls()).toBe(0);
  });

  it("skips panes that have not been measured yet", () => {
    const { fn, calls } = probe({ cols: 80, rows: 24 });
    expect(spawnGeometry([{ cols: 0, rows: 0 }, { cols: 96, rows: 30 }], fn))
      .toEqual({ cols: 96, rows: 30 });
    expect(calls()).toBe(0);
  });

  it("probes only when no pane is mounted", () => {
    const { fn, calls } = probe({ cols: 112, rows: 36 });
    expect(spawnGeometry([], fn)).toEqual({ cols: 112, rows: 36 });
    expect(calls()).toBe(1);
  });

  it("probes when every pane is still unmeasured", () => {
    const { fn, calls } = probe({ cols: 112, rows: 36 });
    expect(spawnGeometry([{ cols: 0, rows: 24 }, { cols: 80, rows: 0 }], fn))
      .toEqual({ cols: 112, rows: 36 });
    expect(calls()).toBe(1);
  });

  it("reports no geometry when the probe cannot measure either", () => {
    const { fn } = probe(null);
    expect(spawnGeometry([], fn)).toBeNull();
  });
});
