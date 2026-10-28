import { describe, expect, it, vi } from "vitest";
import { isRealPaneFit, measureFit, spawnGeometry, DEFAULT_TERMINAL_COLS } from "./terminalFit";

describe("measureFit", () => {
  it("measures the current host before returning its desired grid", () => {
    let grid = { cols: 1, rows: 1 };
    const resize = vi.fn();
    const term = {
      cols: 80,
      rows: 24,
      get cellSize() { return grid; },
      fit: () => { grid = { cols: 120, rows: 40 }; return true; },
      resize,
    };

    expect(measureFit(term)).toEqual({ cols: 120, rows: 40 });
    expect(resize).not.toHaveBeenCalled();
    expect({ cols: term.cols, rows: term.rows }).toEqual({ cols: 80, rows: 24 });
  });

  it("reports an adapter that has no measured geometry", () => {
    expect(measureFit({
      cols: 80,
      rows: 24,
      cellSize: null,
      fit: () => false,
      resize: vi.fn(),
    })).toBeNull();
  });

  it("retains measured geometry while a hidden renderer defers its local fit", () => {
    expect(measureFit({
      cols: 80,
      rows: 24,
      cellSize: { cols: 120, rows: 40 },
      fit: () => false,
      resize: vi.fn(),
    })).toEqual({ cols: 120, rows: 40 });
  });
});

describe("isRealPaneFit", () => {
  it("rejects an unmeasured default grid", () => {
    expect(isRealPaneFit(0, DEFAULT_TERMINAL_COLS)).toBe(false);
    expect(isRealPaneFit(0, null)).toBe(false);
    expect(isRealPaneFit(960, null)).toBe(false);
  });

  it("accepts 80 only when the host actually measures 80", () => {
    expect(isRealPaneFit(640, DEFAULT_TERMINAL_COLS)).toBe(true);
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
