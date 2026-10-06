import { describe, expect, it, vi } from "vitest";

import { TerminalGestureInput } from "./gestureInput";
import type { LongPressScheduler } from "./mouseReports";
import { TerminalPanBridge, type PanSample } from "./panBridge";
import { encodeHostMessage, parseHostMessage, type HostMessage } from "./protocol";
import type { ScrollModeSnapshot } from "./scrollRouting";
import { DEFAULT_TOUCH_SCROLL_CONFIG, type PanPhase } from "./touchScroll";

const LINE_HEIGHT_PX = 20;
const CELL_W = 10;
const COLS = 80;
const ROWS = 24;
const VIEWPORT_START = 50;

const NORMAL_TRACKING: ScrollModeSnapshot = {
  bufferType: "normal",
  applicationCursorKeys: false,
  mouseTracking: "any",
  mouseEncoding: "sgr",
};
const ALT_TRACKING: ScrollModeSnapshot = {
  bufferType: "alternate",
  applicationCursorKeys: false,
  mouseTracking: "any",
  mouseEncoding: "sgr",
};
const ALT_NO_TRACKING: ScrollModeSnapshot = {
  bufferType: "alternate",
  applicationCursorKeys: false,
  mouseTracking: "none",
  mouseEncoding: "sgr",
};

function clamp(value: number, max: number): number {
  return Math.min(Math.max(value, 1), max);
}

function fakeScheduler() {
  const pending: Array<{ cb: () => void; cancelled: boolean }> = [];
  const schedule: LongPressScheduler = (cb) => {
    const entry = { cb, cancelled: false };
    pending.push(entry);
    return () => {
      entry.cancelled = true;
    };
  };
  const fire = () => {
    for (const entry of pending.splice(0)) {
      if (!entry.cancelled) entry.cb();
    }
  };
  return { schedule, fire };
}

function makeInput(snapshot: ScrollModeSnapshot) {
  const writes: string[] = [];
  let viewportY = VIEWPORT_START;
  let focusCount = 0;
  let scrollEvents = 0;
  let flingStarts = 0;
  const scheduler = fakeScheduler();
  const input = new TerminalGestureInput(
    {
      scrollLines: (lines) => {
        viewportY = Math.max(0, viewportY + lines);
      },
      input: (data) => writes.push(data),
      lineHeightPx: () => LINE_HEIGHT_PX,
      cellFromPoint: (xPx, yPx) => ({
        col: clamp(Math.floor(xPx / CELL_W) + 1, COLS),
        row: clamp(Math.floor(yPx / LINE_HEIGHT_PX) + 1, ROWS),
      }),
      modes: () => snapshot,
      focus: () => {
        focusCount += 1;
      },
    },
    {
      onViewportScroll: () => {
        scrollEvents += 1;
      },
      startFling: () => {
        flingStarts += 1;
      },
    },
    DEFAULT_TOUCH_SCROLL_CONFIG,
    scheduler.schedule,
  );
  return {
    input,
    writes,
    viewportY: () => viewportY,
    focused: () => focusCount,
    scrollEvents: () => scrollEvents,
    flingStarts: () => flingStarts,
    fireLongPress: scheduler.fire,
  };
}

/**
 * Drives the native side the way TerminalScreen does: every raw touch is
 * forwarded as a touch host message, the pan responder claims through the
 * real TerminalPanBridge, and everything crosses the real protocol
 * encode/parse boundary before reaching the WebView pipeline.
 */
function nativeDriver(h: ReturnType<typeof makeInput>) {
  const deliver = (msg: HostMessage) => {
    const parsed = parseHostMessage(encodeHostMessage(msg));
    if (!parsed) throw new Error("host message failed to parse");
    if (parsed.type === "pan") {
      h.input.pan(parsed.phase, { x: parsed.x, y: parsed.y, timeMs: parsed.timeMs });
    } else if (parsed.type === "touch") {
      h.input.touch(parsed.phase, { x: parsed.x, y: parsed.y, timeMs: parsed.timeMs }, parsed.touchCount);
    }
  };
  const bridge = new TerminalPanBridge(deliver);
  let claimed = false;
  const forward = (phase: PanPhase, s: PanSample) =>
    deliver({ type: "touch", phase, x: s.x, y: s.y, timeMs: s.timeMs, touchCount: s.touchCount });
  return {
    start(s: PanSample) {
      forward("start", s);
      bridge.touchStart(s);
      claimed = false;
    },
    move(s: PanSample) {
      forward("move", s);
      if (!claimed && bridge.shouldClaim(s)) {
        claimed = true;
        bridge.grant(s);
      } else if (claimed) {
        bridge.move(s);
      }
    },
    end(s: PanSample) {
      forward("end", s);
      bridge.release(s);
    },
  };
}

function s(x: number, y: number, timeMs: number, touchCount = 1): PanSample {
  return { x, y, timeMs, touchCount };
}

describe("TerminalGestureInput native round trip", () => {
  it("scrolls a plain drag locally and writes zero bytes even with mouse tracking on", () => {
    const h = makeInput(NORMAL_TRACKING);
    const native = nativeDriver(h);
    native.start(s(50, 300, 0));
    native.move(s(50, 340, 30));
    native.move(s(50, 360, 60));
    native.end(s(50, 360, 200));
    h.fireLongPress();
    expect(h.writes).toEqual([]);
    expect(h.viewportY()).toBe(VIEWPORT_START - 3);
    expect(h.scrollEvents()).toBeGreaterThan(0);
    expect(h.focused()).toBe(0);
    expect(h.flingStarts()).toBe(0);
  });

  it("keeps wheel reports for a plain drag on the alternate screen", () => {
    const h = makeInput(ALT_TRACKING);
    const native = nativeDriver(h);
    native.start(s(50, 300, 0));
    native.move(s(50, 340, 30));
    native.move(s(50, 360, 60));
    native.end(s(50, 360, 200));
    expect(h.writes).toEqual(["\x1b[<64;6;18M\x1b[<64;6;18M", "\x1b[<64;6;19M"]);
    expect(h.viewportY()).toBe(VIEWPORT_START);
    expect(h.scrollEvents()).toBe(0);
  });

  it("keeps arrow emulation for a plain drag on the alternate screen without tracking", () => {
    const h = makeInput(ALT_NO_TRACKING);
    const native = nativeDriver(h);
    native.start(s(50, 300, 0));
    native.move(s(50, 340, 30));
    native.move(s(50, 360, 60));
    native.end(s(50, 360, 200));
    expect(h.writes).toEqual(["\x1b[A\x1b[A", "\x1b[A"]);
    expect(h.viewportY()).toBe(VIEWPORT_START);
    expect(h.scrollEvents()).toBe(0);
  });

  it("delivers press, motion, and release for a long-press drag and never scrolls", () => {
    const h = makeInput(NORMAL_TRACKING);
    const native = nativeDriver(h);
    native.start(s(55, 90, 0));
    h.fireLongPress();
    native.move(s(55, 130, 600));
    native.move(s(55, 170, 650));
    native.end(s(55, 170, 700));
    expect(h.writes).toEqual(["\x1b[<0;6;5M", "\x1b[<32;6;7M", "\x1b[<32;6;9M", "\x1b[<0;6;9m"]);
    expect(h.viewportY()).toBe(VIEWPORT_START);
    expect(h.scrollEvents()).toBe(0);
  });

  it("delivers a press/release pair for a tap and focuses", () => {
    const h = makeInput(NORMAL_TRACKING);
    const native = nativeDriver(h);
    native.start(s(55, 90, 0));
    native.end(s(55, 90, 80));
    expect(h.writes).toEqual(["\x1b[<0;6;5M", "\x1b[<0;6;5m"]);
    expect(h.viewportY()).toBe(VIEWPORT_START);
    expect(h.focused()).toBe(1);
  });

  it("keeps momentum scrolling local after a fast lift", () => {
    const h = makeInput(NORMAL_TRACKING);
    const native = nativeDriver(h);
    native.start(s(50, 300, 0));
    native.move(s(50, 340, 20));
    native.move(s(50, 380, 40));
    native.end(s(50, 380, 48));
    expect(h.flingStarts()).toBe(1);
    const afterDrag = h.viewportY();
    let now = 48;
    for (let i = 0; i < 400 && h.input.flingActive(); i += 1) {
      now += 16;
      h.input.flingStep(now);
    }
    expect(h.input.flingActive()).toBe(false);
    expect(h.viewportY()).toBeLessThan(afterDrag);
    expect(h.writes).toEqual([]);
  });

  it("stops momentum when a new finger lands", () => {
    const h = makeInput(NORMAL_TRACKING);
    const native = nativeDriver(h);
    native.start(s(50, 300, 0));
    native.move(s(50, 340, 20));
    native.move(s(50, 380, 40));
    native.end(s(50, 380, 48));
    expect(h.input.flingActive()).toBe(true);
    native.start(s(50, 200, 100));
    expect(h.input.flingActive()).toBe(false);
    const frozen = h.viewportY();
    h.input.flingStep(200);
    expect(h.viewportY()).toBe(frozen);
    expect(h.writes).toEqual([]);
  });
});

describe("TerminalGestureInput event ordering", () => {
  it("does not focus after a native scroll claim cancels a pending tap", () => {
    const h = makeInput(NORMAL_TRACKING);
    h.input.touch("start", { x: 50, y: 300, timeMs: 0 }, 1);
    h.input.cancelTouch();
    h.input.touch("end", { x: 50, y: 360, timeMs: 800 }, 1);
    expect(h.focused()).toBe(0);
    expect(h.writes).toEqual([]);
  });

  it("stays silent when pan claims land before the touch stream", () => {
    const h = makeInput(NORMAL_TRACKING);
    h.input.touch("start", { x: 50, y: 300, timeMs: 0 }, 1);
    h.input.pan("start", { x: 50, y: 300, timeMs: 0 });
    h.input.pan("move", { x: 50, y: 340, timeMs: 30 });
    h.input.touch("move", { x: 50, y: 340, timeMs: 30 }, 1);
    h.input.pan("move", { x: 50, y: 360, timeMs: 60 });
    h.input.touch("move", { x: 50, y: 360, timeMs: 60 }, 1);
    h.input.pan("end", { x: 50, y: 360, timeMs: 200 });
    h.input.touch("end", { x: 50, y: 360, timeMs: 200 }, 1);
    expect(h.writes).toEqual([]);
    expect(h.viewportY()).toBe(VIEWPORT_START - 3);
  });

  it("stays silent when the touch stream lands before pan claims", () => {
    const h = makeInput(NORMAL_TRACKING);
    h.input.touch("start", { x: 50, y: 300, timeMs: 0 }, 1);
    h.input.touch("move", { x: 50, y: 340, timeMs: 30 }, 1);
    h.input.pan("start", { x: 50, y: 300, timeMs: 0 });
    h.input.pan("move", { x: 50, y: 340, timeMs: 30 });
    h.input.touch("move", { x: 50, y: 360, timeMs: 60 }, 1);
    h.input.pan("move", { x: 50, y: 360, timeMs: 60 });
    h.input.touch("end", { x: 50, y: 360, timeMs: 200 }, 1);
    h.input.pan("end", { x: 50, y: 360, timeMs: 200 });
    expect(h.writes).toEqual([]);
    expect(h.viewportY()).toBe(VIEWPORT_START - 3);
  });
});

describe("TerminalGestureInput diagnostics", () => {
  for (const development of [undefined, false, true]) {
    it(`logs fling diagnostics only for an enabled development flag (${development})`, () => {
      vi.stubGlobal("__DEV__", development);
      const log = vi.spyOn(console, "log").mockImplementation(() => {});
      try {
        const h = makeInput(NORMAL_TRACKING);
        h.input.pan("start", { x: 50, y: 100, timeMs: 0 });
        h.input.pan("move", { x: 50, y: 200, timeMs: 50 });
        h.input.pan("end", { x: 50, y: 220, timeMs: 60 });
        if (development === true) {
          expect(log).toHaveBeenCalledWith(expect.stringContaining("[fling]"));
        } else {
          expect(log).not.toHaveBeenCalled();
        }
      } finally {
        log.mockRestore();
        vi.unstubAllGlobals();
      }
    });
  }
});
