import { describe, expect, it, vi } from "vitest";
import { applyMeasuredFit, deliverLayerFrame, isInitialSnapshotFrame, showLayer } from "./terminalLayer";

describe("hidden layers", () => {
  it("keep parsing every frame into the emulator when sizes already match", () => {
    const write = vi.fn();
    const term = { cols: 80, rows: 24, resize: vi.fn() };
    expect(deliverLayerFrame(term, { cols: 80, rows: 24 }, write)).toBe(true);
    expect(write).toHaveBeenCalledOnce();
    expect(term.resize).not.toHaveBeenCalled();
  });

  it("does not paint a snapshot into a differently sized emulator", () => {
    const order: string[] = [];
    const term = {
      cols: 142,
      rows: 48,
      resize(cols: number, rows: number) {
        this.cols = cols;
        this.rows = rows;
        order.push(`resize:${cols}x${rows}`);
      },
    };
    expect(deliverLayerFrame(term, { cols: 120, rows: 32 }, () => order.push("write"))).toBe(false);
    expect(order).toEqual([]);
    expect(term.cols).toBe(142);
    expect(term.rows).toBe(48);
  });
});

describe("isInitialSnapshotFrame", () => {
  it("is only the first snapshot replay", () => {
    expect(isInitialSnapshotFrame(true, { replay: true, snapshot: true })).toBe(true);
    expect(isInitialSnapshotFrame(true, { replay: true, snapshot: false })).toBe(false);
    expect(isInitialSnapshotFrame(true, { replay: false, snapshot: true })).toBe(false);
    expect(isInitialSnapshotFrame(false, { replay: true, snapshot: true })).toBe(false);
  });
});

describe("showLayer", () => {
  it("fits before any write when the DOM size differs from the PTY", () => {
    const order: string[] = [];
    const term = {
      cols: 80,
      rows: 24,
      resize(cols: number, rows: number) {
        this.cols = cols;
        this.rows = rows;
        order.push(`term.resize:${cols}x${rows}`);
      },
      refresh: (start: number, end: number) => order.push(`refresh:${start}-${end}`),
    };
    showLayer(
      term,
      () => {
        order.push("fit");
        term.cols = 100;
        term.rows = 30;
      },
      { cols: 80, rows: 24 },
      (cols, rows) => order.push(`pty:${cols}x${rows}`),
    );
    expect(order[0]).toBe("fit");
    expect(order).toEqual(["fit", "pty:100x30", "refresh:0-29"]);
    expect(order.some((step) => step.startsWith("write"))).toBe(false);
  });

  it("does not jiggle rows on a same-size show", () => {
    const order: string[] = [];
    const term = {
      cols: 120,
      rows: 40,
      resize: (cols: number, rows: number) => order.push(`resize:${cols}x${rows}`),
      refresh: (start: number, end: number) => order.push(`refresh:${start}-${end}`),
    };
    showLayer(term, () => order.push("fit"), { cols: 120, rows: 40 }, (cols, rows) => {
      order.push(`pty:${cols}x${rows}`);
    });
    expect(order).toEqual(["fit", "refresh:0-39"]);
  });
});

describe("applyMeasuredFit", () => {
  it("resizes width before any cells are painted", () => {
    const order: string[] = [];
    const term = {
      cols: 80,
      rows: 24,
      resize(cols: number, rows: number) {
        this.cols = cols;
        this.rows = rows;
        order.push(`resize:${cols}x${rows}`);
      },
      reset: () => order.push("reset"),
    };
    expect(applyMeasuredFit(term, { cols: 140, rows: 40 }, false)).toBe("resized");
    expect(order).toEqual(["resize:140x40"]);
    expect(term.cols).toBe(140);
  });

  it("does not grow cols after paint unless reset runs first", () => {
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
    expect(applyMeasuredFit(term, { cols: 140, rows: 40 }, true)).toBe("unchanged");
    expect(order).toEqual([]);
    expect(term.cols).toBe(80);
    expect(term.rows).toBe(24);
  });

  it("resets before a post-paint width change", () => {
    const order: string[] = [];
    const term = {
      cols: 80,
      rows: 24,
      resize(cols: number, rows: number) {
        this.cols = cols;
        this.rows = rows;
        order.push(`resize:${cols}x${rows}`);
      },
      reset: () => order.push("reset"),
    };
    expect(applyMeasuredFit(term, { cols: 140, rows: 40 }, true)).toBe("reset");
    expect(order).toEqual(["reset", "resize:140x40"]);
  });

  it("allows a height-only resize after paint", () => {
    const order: string[] = [];
    const term = {
      cols: 140,
      rows: 40,
      resize(cols: number, rows: number) {
        this.cols = cols;
        this.rows = rows;
        order.push(`resize:${cols}x${rows}`);
      },
      reset: () => order.push("reset"),
    };
    expect(applyMeasuredFit(term, { cols: 140, rows: 48 }, true)).toBe("height-only");
    expect(order).toEqual(["resize:140x48"]);
    expect(order.some((step) => step === "reset")).toBe(false);
  });

  it("is a no-op when the measured size already matches", () => {
    const resize = vi.fn();
    const reset = vi.fn();
    const term = { cols: 140, rows: 40, resize, reset };
    expect(applyMeasuredFit(term, { cols: 140, rows: 40 }, true)).toBe("unchanged");
    expect(resize).not.toHaveBeenCalled();
    expect(reset).not.toHaveBeenCalled();
  });
});
