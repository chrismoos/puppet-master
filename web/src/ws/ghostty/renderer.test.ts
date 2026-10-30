import { describe, expect, it, vi } from "vitest";
import { renderGhosttySnapshot } from "./renderer";
import type { GhosttySnapshot } from "./core";

function paint(text: string, cursorOn = false, invisible = false) {
  const fills: { bounds: number[]; alpha: number; color: unknown }[] = [];
  const alphas: number[] = [];
  const context = {
    canvas: { width: 100, height: 18 }, globalAlpha: 1, fillStyle: "",
    getTransform: () => ({ a: 2, d: 2, e: 0, f: 0 }),
    save() { alphas.push(this.globalAlpha); },
    restore() { this.globalAlpha = alphas.pop()!; },
    resetTransform() {}, beginPath() {}, rect() {}, clip() {},
    fillRect(...bounds: number[]) { fills.push({ bounds, alpha: this.globalAlpha, color: this.fillStyle }); },
    fillText: vi.fn(),
  };
  const black = { r: 0, g: 0, b: 0 };
  const white = { r: 240, g: 240, b: 240 };
  const cells = [...text].map((text) => ({ text, foreground: white, background: black, invisible, wide: 0 }));
  const snapshot = {
    cols: cells.length, rows: 1, rowData: [{ cells }], dirtyRows: [0],
    background: black, cursor: white, cursorVisible: cursorOn, cursorX: 0, cursorY: 0,
  } as unknown as GhosttySnapshot;
  renderGhosttySnapshot({ context: context as unknown as CanvasRenderingContext2D,
    snapshot, metrics: { width: 7.8, height: 18, baseline: 14 }, fontSize: 13,
    fontFamily: "monospace", padding: 0, forceFull: true, cursorOn });
  return { fills, context };
}

describe("terminal block graphics", () => {
  it("uses uniform coverage for each shade and restores alpha", () => {
    const { fills, context } = paint("░▒▓");
    expect(fills.slice(-3).map(({ alpha }) => alpha)).toEqual([0.25, 0.5, 0.75]);
    expect(context.globalAlpha).toBe(1);
    expect(context.fillText).not.toHaveBeenCalled();
  });
  it("joins full blocks at a shared device pixel boundary", () => {
    const { fills } = paint("██");
    expect(fills.slice(-2).map(({ bounds }) => bounds)).toEqual([[0, 0, 8, 18], [8, 0, 7.5, 18]]);
  });
  it("draws upper, lower, and quadrant blocks in their cell regions", () => {
    expect(paint("▀▄▖").fills.slice(-3).map(({ bounds }) => bounds)).toEqual([
      [0, 0, 8, 9], [8, 9, 7.5, 9], [15.5, 9, 4, 9],
    ]);
  });
  it("separates graphics from neighboring text runs", () => {
    const { context } = paint("A░B");
    expect(context.fillText.mock.calls.map((call) => call[0])).toEqual(["A", "B"]);
  });
  it("redraws a shade using cursor text color", () => {
    const { fills } = paint("░", true);
    expect(fills.at(-1)).toMatchObject({ alpha: 0.25, color: "rgb(0, 0, 0)" });
  });
  it("keeps invisible graphics hidden", () => {
    expect(paint("░█", false, true).fills).toHaveLength(2);
  });
});
