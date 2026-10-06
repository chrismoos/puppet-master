import { describe, expect, it, vi } from "vitest";
import {
  applyEchoedPtySize,
  applyEchoedPtySizeThenWrite,
} from "./terminalResize";

describe("applyEchoedPtySizeThenWrite", () => {
  it("writes when the emulator already matches the echo", () => {
    const order: string[] = [];
    const term = {
      cols: 100,
      rows: 30,
      resize(cols: number, rows: number) {
        this.cols = cols;
        this.rows = rows;
        order.push(`resize:${cols}x${rows}`);
      },
    };
    expect(
      applyEchoedPtySizeThenWrite(term, { cols: 100, rows: 30 }, () => {
        order.push(`write@${term.cols}x${term.rows}`);
      }),
    ).toBe(true);
    expect(order).toEqual(["write@100x30"]);
  });

  it("does not paint at 80 cols then resize the emulator to 120", () => {
    const order: string[] = [];
    const term = {
      cols: 80,
      rows: 24,
      resize(cols: number, rows: number) {
        this.cols = cols;
        this.rows = rows;
        order.push(`resize:${cols}x${rows}`);
      },
    };
    expect(
      applyEchoedPtySizeThenWrite(term, { cols: 120, rows: 40 }, () => {
        order.push(`write@${term.cols}x${term.rows}`);
      }),
    ).toBe(false);
    expect(order).toEqual([]);
    expect(term.cols).toBe(80);
    expect(term.rows).toBe(24);
  });

  it("does not resize when the emulator already matches the echo", () => {
    const resize = vi.fn();
    const term = { cols: 100, rows: 30, resize };
    const write = vi.fn();
    expect(applyEchoedPtySizeThenWrite(term, { cols: 100, rows: 30 }, write)).toBe(true);
    expect(resize).not.toHaveBeenCalled();
    expect(write).toHaveBeenCalledOnce();
  });

  it("writes even when no echo has arrived yet", () => {
    const resize = vi.fn();
    const write = vi.fn();
    expect(applyEchoedPtySizeThenWrite({ cols: 80, rows: 24, resize }, null, write)).toBe(true);
    expect(resize).not.toHaveBeenCalled();
    expect(write).toHaveBeenCalledOnce();
    expect(applyEchoedPtySize({ cols: 80, rows: 24, resize }, { cols: 1, rows: 1 })).toBe(false);
  });
});

describe("applyEchoedPtySize", () => {
  it("does not shrink or grow a frozen visible emulator", () => {
    const resize = vi.fn();
    const term = { cols: 142, rows: 48, resize };
    expect(applyEchoedPtySize(term, { cols: 120, rows: 32 }, { freeze: true })).toBe(false);
    expect(resize).not.toHaveBeenCalled();
  });

  it("still tracks the echo when the layer is not frozen", () => {
    const term = {
      cols: 80,
      rows: 24,
      resize(cols: number, rows: number) {
        this.cols = cols;
        this.rows = rows;
      },
    };
    expect(applyEchoedPtySize(term, { cols: 100, rows: 30 })).toBe(true);
    expect(term.cols).toBe(100);
    expect(term.rows).toBe(30);
  });
});
