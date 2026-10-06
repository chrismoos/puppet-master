import { describe, expect, it } from "vitest";

import {
  PanScrollRouter,
  scrollDeliveryMode,
  wheelReport,
  type ScrollModeSnapshot,
  type ScrollTerminal,
} from "./scrollRouting";
import { BridgedPanSink, TouchScrollGesture } from "./touchScroll";

const LINE_HEIGHT_PX = 20;
const COLS = 80;
const ROWS = 24;

function point(y: number, timeMs: number, x = 50) {
  return { x, y, timeMs };
}

function harness(snapshot: ScrollModeSnapshot) {
  const gesture = new TouchScrollGesture();
  const scrolled: number[] = [];
  const ptyWrites: string[] = [];
  const term: ScrollTerminal = {
    scrollLines: (lines) => scrolled.push(lines),
    input: (data) => ptyWrites.push(data),
    lineHeightPx: () => LINE_HEIGHT_PX,
    cellFromPoint: (xPx, yPx) => ({
      col: Math.min(Math.max(Math.floor(xPx / 10) + 1, 1), COLS),
      row: Math.min(Math.max(Math.floor(yPx / LINE_HEIGHT_PX) + 1, 1), ROWS),
    }),
    modes: () => snapshot,
  };
  const router = new PanScrollRouter(term);
  let flings = 0;
  const sink = new BridgedPanSink(gesture, {
    context: () => router.context(),
    apply: (action) => router.apply(action),
    startFling: () => {
      flings += 1;
    },
  });
  return { sink, gesture, router, scrolled, ptyWrites, flingCount: () => flings };
}

const NORMAL: ScrollModeSnapshot = {
  bufferType: "normal",
  applicationCursorKeys: false,
  mouseTracking: "none",
  mouseEncoding: "sgr",
};
const ALT_TRACKING: ScrollModeSnapshot = {
  bufferType: "alternate",
  applicationCursorKeys: true,
  mouseTracking: "drag",
  mouseEncoding: "sgr",
};
const ALT_TRACKING_X10: ScrollModeSnapshot = { ...ALT_TRACKING, mouseEncoding: "default" };
const ALT_NO_TRACKING: ScrollModeSnapshot = {
  bufferType: "alternate",
  applicationCursorKeys: false,
  mouseTracking: "none",
  mouseEncoding: "sgr",
};
const ALT_NO_TRACKING_APP_CURSOR: ScrollModeSnapshot = { ...ALT_NO_TRACKING, applicationCursorKeys: true };
const NORMAL_TRACKING: ScrollModeSnapshot = { ...NORMAL, mouseTracking: "vt200" };

describe("PanScrollRouter", () => {
  it("scrolls the viewport on the normal screen and writes nothing to the PTY", () => {
    const { sink, scrolled, ptyWrites } = harness(NORMAL);
    sink.handle("start", point(100, 0));
    sink.handle("move", point(150, 50));
    sink.handle("end", point(150, 400));
    expect(scrolled).toEqual([-2]);
    expect(ptyWrites).toEqual([]);
  });

  it("keeps viewport scrolling on the normal screen even with mouse tracking on", () => {
    const { sink, scrolled, ptyWrites } = harness(NORMAL_TRACKING);
    sink.handle("start", point(100, 0));
    sink.handle("move", point(150, 50));
    expect(scrolled).toEqual([-2]);
    expect(ptyWrites).toEqual([]);
  });

  it("emits SGR wheel-up reports at the touch cell when mouse tracking is on", () => {
    const { sink, scrolled, ptyWrites } = harness(ALT_TRACKING);
    sink.handle("start", point(100, 0));
    sink.handle("move", point(163, 60));
    sink.handle("end", point(163, 400));
    expect(scrolled).toEqual([]);
    // Finger at x=50 y=163 with 10x20 cells is column 6, row 9.
    expect(ptyWrites).toEqual(["\x1b[<64;6;9M\x1b[<64;6;9M\x1b[<64;6;9M"]);
  });

  it("emits SGR wheel-down reports for upward drags when mouse tracking is on", () => {
    const { sink, ptyWrites } = harness(ALT_TRACKING);
    sink.handle("start", point(200, 0));
    sink.handle("move", point(158, 60));
    expect(ptyWrites).toEqual(["\x1b[<65;6;8M\x1b[<65;6;8M"]);
  });

  it("never emits arrow sequences while mouse tracking is on", () => {
    const { sink, ptyWrites } = harness(ALT_TRACKING);
    sink.handle("start", point(100, 0));
    sink.handle("move", point(163, 60));
    sink.handle("end", point(163, 400));
    for (const chunk of ptyWrites) {
      expect(chunk).not.toContain("\x1b[A");
      expect(chunk).not.toContain("\x1b[B");
      expect(chunk).not.toContain("\x1bOA");
      expect(chunk).not.toContain("\x1bOB");
    }
  });

  it("encodes wheel reports X10-style when the application never negotiated SGR", () => {
    const { sink, ptyWrites } = harness(ALT_TRACKING_X10);
    sink.handle("start", point(100, 0));
    sink.handle("move", point(121, 30));
    // Button 64 + offset 32 is byte 96, column 6 is byte 38, row 7 is byte 39.
    expect(ptyWrites).toEqual([`\x1b[M${String.fromCharCode(96)}${String.fromCharCode(38)}${String.fromCharCode(39)}`]);
  });

  it("emits Up arrows on the alternate screen with mouse tracking off", () => {
    const { sink, scrolled, ptyWrites } = harness(ALT_NO_TRACKING);
    sink.handle("start", point(100, 0));
    sink.handle("move", point(163, 60));
    expect(scrolled).toEqual([]);
    expect(ptyWrites).toEqual(["\x1b[A\x1b[A\x1b[A"]);
  });

  it("emits Down arrows for upward drags with mouse tracking off", () => {
    const { sink, ptyWrites } = harness(ALT_NO_TRACKING);
    sink.handle("start", point(200, 0));
    sink.handle("move", point(158, 60));
    expect(ptyWrites).toEqual(["\x1b[B\x1b[B"]);
  });

  it("selects application cursor sequences when that mode is set", () => {
    const { sink, ptyWrites } = harness(ALT_NO_TRACKING_APP_CURSOR);
    sink.handle("start", point(100, 0));
    sink.handle("move", point(142, 60));
    expect(ptyWrites).toEqual(["\x1bOA\x1bOA"]);
  });

  it("clamps the wheel report cell to the terminal grid", () => {
    const { sink, ptyWrites } = harness(ALT_TRACKING);
    sink.handle("start", point(-500, 0, 2000));
    sink.handle("move", point(-470, 30, 2000));
    expect(ptyWrites).toEqual([`\x1b[<64;${COLS};1M`]);
  });
});

describe("scrollDeliveryMode", () => {
  it("scrolls the viewport on the normal screen regardless of mouse tracking", () => {
    expect(scrollDeliveryMode(NORMAL)).toBe("viewport");
    expect(scrollDeliveryMode(NORMAL_TRACKING)).toBe("viewport");
  });

  it("delivers wheel reports on the alternate screen with mouse tracking on", () => {
    expect(scrollDeliveryMode(ALT_TRACKING)).toBe("wheel");
    expect(scrollDeliveryMode(ALT_TRACKING_X10)).toBe("wheel");
    expect(scrollDeliveryMode({ ...ALT_TRACKING, mouseTracking: "x10" })).toBe("wheel");
    expect(scrollDeliveryMode({ ...ALT_TRACKING, mouseTracking: "any" })).toBe("wheel");
  });

  it("emulates arrows on the alternate screen only with mouse tracking off", () => {
    expect(scrollDeliveryMode(ALT_NO_TRACKING)).toBe("arrows");
    expect(scrollDeliveryMode(ALT_NO_TRACKING_APP_CURSOR)).toBe("arrows");
  });
});

describe("wheelReport", () => {
  it("encodes SGR wheel buttons 64 and 65 as press-only reports", () => {
    expect(wheelReport("up", { col: 10, row: 5 }, "sgr")).toBe("\x1b[<64;10;5M");
    expect(wheelReport("down", { col: 10, row: 5 }, "sgr")).toBe("\x1b[<65;10;5M");
  });

  it("encodes X10-style reports with offset bytes", () => {
    expect(wheelReport("up", { col: 1, row: 1 }, "default")).toBe("\x1b[M\x60\x21\x21");
    expect(wheelReport("down", { col: 1, row: 1 }, "default")).toBe("\x1b[M\x61\x21\x21");
  });

  it("caps X10-style coordinates at one byte", () => {
    expect(wheelReport("up", { col: 500, row: 500 }, "default")).toBe("\x1b[M\x60\xff\xff");
  });
});
