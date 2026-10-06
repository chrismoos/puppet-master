import { describe, expect, it } from "vitest";

import { computeFitDimensions } from "./terminalFit";

// Menlo 13 px on an iPhone 15 Pro (393 × 852 logical points, 3× scale).
// Cell width measured from xterm's _renderService.dimensions.css.cell.
const CELL_W = 7.80078125;
const CELL_H = 17;
const IPHONE_W = 393;
const IPHONE_H = 680; // after header + keyboard-inset

describe("computeFitDimensions", () => {
  it("returns null when cell dimensions are zero", () => {
    expect(computeFitDimensions(IPHONE_W, IPHONE_H, 0, 0, 0, CELL_H)).toBeNull();
    expect(computeFitDimensions(IPHONE_W, IPHONE_H, 0, 0, CELL_W, 0)).toBeNull();
  });

  it("returns null for zero-dimension containers", () => {
    expect(computeFitDimensions(0, IPHONE_H, 0, 0, CELL_W, CELL_H)).toBeNull();
    expect(computeFitDimensions(IPHONE_W, 0, 0, 0, CELL_W, CELL_H)).toBeNull();
  });

  it("returns null for non-finite dimensions", () => {
    expect(computeFitDimensions(NaN, IPHONE_H, 0, 0, CELL_W, CELL_H)).toBeNull();
    expect(computeFitDimensions(IPHONE_W, Infinity, 0, 0, CELL_W, CELL_H)).toBeNull();
  });

  it("computes cols and rows without scrollbar gutter", () => {
    const result = computeFitDimensions(IPHONE_W, IPHONE_H, 0, 0, CELL_W, CELL_H);
    expect(result).not.toBeNull();
    // 393 / 7.80078125 = 50.38… → 50 cols
    expect(result!.cols).toBe(50);
    // 680 / 17 = 40
    expect(result!.rows).toBe(40);
  });

  it("does not subtract the 14 px scrollbar gutter that FitAddon uses", () => {
    // FitAddon would compute floor((393 - 14) / 7.80078125) = floor(48.59) = 48.
    // The correct answer without the gutter is 50.
    const withGutter = Math.floor((IPHONE_W - 14) / CELL_W);
    const result = computeFitDimensions(IPHONE_W, IPHONE_H, 0, 0, CELL_W, CELL_H);
    expect(result!.cols).toBeGreaterThan(withGutter);
  });

  it("subtracts element padding from available space", () => {
    const pad = 8;
    const result = computeFitDimensions(IPHONE_W, IPHONE_H, pad * 2, pad * 2, CELL_W, CELL_H);
    const noPad = computeFitDimensions(IPHONE_W, IPHONE_H, 0, 0, CELL_W, CELL_H);
    expect(result!.cols).toBeLessThan(noPad!.cols);
    // (393 - 16) / 7.80078125 = 48.33… → 48
    expect(result!.cols).toBe(48);
  });

  it("clamps cols to minimum of 2", () => {
    // Tiny container that can only hold 1 cell
    const result = computeFitDimensions(5, IPHONE_H, 0, 0, CELL_W, CELL_H);
    expect(result!.cols).toBe(2);
  });

  it("clamps rows to minimum of 1", () => {
    const result = computeFitDimensions(IPHONE_W, 5, 0, 0, CELL_W, CELL_H);
    expect(result!.rows).toBe(1);
  });

  it("uses full width for landscape orientation", () => {
    // iPhone landscape: roughly 852 × 393
    const result = computeFitDimensions(852, 393, 0, 0, CELL_W, CELL_H);
    // 852 / 7.80078125 = 109.2… → 109
    expect(result!.cols).toBe(109);
  });

  it("rendered width never exceeds container", () => {
    // The critical invariant: cols * cellWidth must fit inside the container.
    // This is what FitAddon's gutter subtraction was trying to ensure, but
    // without the gutter we rely on floor() alone.
    for (const w of [320, 375, 390, 393, 414, 428, 744, 768, 852, 1024]) {
      const result = computeFitDimensions(w, IPHONE_H, 0, 0, CELL_W, CELL_H);
      expect(result).not.toBeNull();
      const renderedWidth = result!.cols * CELL_W;
      expect(renderedWidth).toBeLessThanOrEqual(w);
    }
  });
});
