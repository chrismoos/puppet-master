import { describe, expect, it } from "vitest";

import {
  TouchMouseReporter,
  buttonReport,
  trackingDelivers,
  type LongPressScheduler,
} from "./mouseReports";
import type { MouseTrackingMode, ScrollModeSnapshot } from "./scrollRouting";
import { DEFAULT_TOUCH_SCROLL_CONFIG } from "./touchScroll";

const CELL_W = 10;
const CELL_H = 20;
const COLS = 80;
const ROWS = 24;

const SLOP = DEFAULT_TOUCH_SCROLL_CONFIG.dragSlopPx;
const LONG_PRESS_MS = DEFAULT_TOUCH_SCROLL_CONFIG.longPressMs;

function snapshot(tracking: MouseTrackingMode, encoding: "sgr" | "default" = "sgr"): ScrollModeSnapshot {
  return {
    bufferType: "alternate",
    applicationCursorKeys: true,
    mouseTracking: tracking,
    mouseEncoding: encoding,
  };
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

function harness(modes: ScrollModeSnapshot, scrollActive = () => false) {
  const ptyWrites: string[] = [];
  let focusCount = 0;
  const scheduler = fakeScheduler();
  const reporter = new TouchMouseReporter(
    {
      cellFromPoint: (xPx, yPx) => ({
        col: Math.min(Math.max(Math.floor(xPx / CELL_W) + 1, 1), COLS),
        row: Math.min(Math.max(Math.floor(yPx / CELL_H) + 1, 1), ROWS),
      }),
      modes: () => modes,
      input: (data) => ptyWrites.push(data),
      focus: () => {
        focusCount += 1;
      },
      scrollActive,
    },
    DEFAULT_TOUCH_SCROLL_CONFIG,
    scheduler.schedule,
  );
  return { reporter, ptyWrites, focused: () => focusCount, fireLongPress: scheduler.fire };
}

function pt(x: number, y: number, timeMs: number) {
  return { x, y, timeMs };
}

describe("TouchMouseReporter", () => {
  it("reports a tap as a press/release pair at the tapped cell and focuses", () => {
    const { reporter, ptyWrites, focused } = harness(snapshot("drag"));
    reporter.handle("start", pt(55, 90, 0), 1);
    reporter.handle("end", pt(55, 90, 80), 1);
    // x=55 y=90 with 10x20 cells is column 6, row 5.
    expect(ptyWrites).toEqual(["\x1b[<0;6;5M", "\x1b[<0;6;5m"]);
    expect(focused()).toBe(1);
  });

  it("focuses but writes nothing for a tap with mouse tracking off", () => {
    const { reporter, ptyWrites, focused } = harness(snapshot("none"));
    reporter.handle("start", pt(55, 90, 0), 1);
    reporter.handle("end", pt(55, 90, 80), 1);
    expect(ptyWrites).toEqual([]);
    expect(focused()).toBe(1);
  });

  it("omits the release for x10 tracking, which is press-only", () => {
    const { reporter, ptyWrites } = harness(snapshot("x10"));
    reporter.handle("start", pt(55, 90, 0), 1);
    reporter.handle("end", pt(55, 90, 80), 1);
    expect(ptyWrites).toEqual(["\x1b[<0;6;5M"]);
  });

  it("emits the press the moment the long-press timer fires, before any movement", () => {
    const { reporter, ptyWrites, fireLongPress } = harness(snapshot("drag"));
    reporter.handle("start", pt(55, 90, 0), 1);
    fireLongPress();
    expect(ptyWrites).toEqual(["\x1b[<0;6;5M"]);
    reporter.handle("move", pt(56, 132, LONG_PRESS_MS + 40), 1);
    reporter.handle("end", pt(56, 132, LONG_PRESS_MS + 80), 1);
    expect(ptyWrites).toEqual(["\x1b[<0;6;5M", "\x1b[<32;6;7M", "\x1b[<0;6;7m"]);
  });

  it("cancels the long-press timer on a quick tap", () => {
    const { reporter, ptyWrites, fireLongPress } = harness(snapshot("drag"));
    reporter.handle("start", pt(55, 90, 0), 1);
    reporter.handle("end", pt(55, 90, 80), 1);
    fireLongPress();
    expect(ptyWrites).toEqual(["\x1b[<0;6;5M", "\x1b[<0;6;5m"]);
  });

  it("emits no press when the long-press fires with mouse tracking off", () => {
    const { reporter, ptyWrites } = harness(snapshot("none"));
    reporter.handle("start", pt(55, 90, 0), 1);
    reporter.handle("move", pt(56, 132, LONG_PRESS_MS + 40), 1);
    reporter.handle("end", pt(56, 132, LONG_PRESS_MS + 80), 1);
    expect(ptyWrites).toEqual([]);
  });

  it("begins the selection from a late move when the timer never fired", () => {
    const { reporter, ptyWrites } = harness(snapshot("drag"));
    reporter.handle("start", pt(55, 90, 0), 1);
    reporter.handle("move", pt(56, 92, LONG_PRESS_MS + 10), 1);
    reporter.handle("move", pt(56, 132, LONG_PRESS_MS + 40), 1);
    reporter.handle("end", pt(56, 132, LONG_PRESS_MS + 80), 1);
    expect(ptyWrites).toEqual(["\x1b[<0;6;5M", "\x1b[<32;6;7M", "\x1b[<0;6;7m"]);
  });

  it("skips motion reports for vt200 tracking but keeps press and release", () => {
    const { reporter, ptyWrites } = harness(snapshot("vt200"));
    reporter.handle("start", pt(55, 90, 0), 1);
    reporter.handle("move", pt(95, 91, LONG_PRESS_MS + 10), 1);
    reporter.handle("end", pt(95, 91, LONG_PRESS_MS + 40), 1);
    expect(ptyWrites).toEqual(["\x1b[<0;6;5M", "\x1b[<0;10;5m"]);
  });

  it("coalesces motion inside one cell to a single report", () => {
    const { reporter, ptyWrites, fireLongPress } = harness(snapshot("any"));
    reporter.handle("start", pt(55, 90, 0), 1);
    fireLongPress();
    reporter.handle("move", pt(75, 91, LONG_PRESS_MS + 30), 1);
    reporter.handle("move", pt(76, 92, LONG_PRESS_MS + 40), 1);
    reporter.handle("move", pt(77, 93, LONG_PRESS_MS + 50), 1);
    reporter.handle("end", pt(77, 93, LONG_PRESS_MS + 80), 1);
    expect(ptyWrites).toEqual(["\x1b[<0;6;5M", "\x1b[<32;8;5M", "\x1b[<0;8;5m"]);
  });

  it("leaves plain vertical drags to the scroll path", () => {
    const { reporter, ptyWrites, focused, fireLongPress } = harness(snapshot("drag"));
    reporter.handle("start", pt(55, 90, 0), 1);
    reporter.handle("move", pt(56, 90 + SLOP + 20, 30), 1);
    reporter.handle("move", pt(56, 200, 60), 1);
    reporter.handle("end", pt(56, 200, 90), 1);
    fireLongPress();
    expect(ptyWrites).toEqual([]);
    expect(focused()).toBe(0);
  });

  it("leaves plain horizontal drags to the scroll path", () => {
    const { reporter, ptyWrites, focused } = harness(snapshot("drag"));
    reporter.handle("start", pt(55, 90, 0), 1);
    reporter.handle("move", pt(55 + SLOP + 4, 91, 30), 1);
    reporter.handle("move", pt(105, 91, 60), 1);
    reporter.handle("end", pt(105, 91, 90), 1);
    expect(ptyWrites).toEqual([]);
    expect(focused()).toBe(0);
  });

  it("stays silent while the scroll gesture owns the touch", () => {
    const { reporter, ptyWrites, fireLongPress } = harness(snapshot("drag"), () => true);
    reporter.handle("start", pt(55, 90, 0), 1);
    fireLongPress();
    reporter.handle("move", pt(95, 91, 30), 1);
    reporter.handle("end", pt(95, 91, 60), 1);
    expect(ptyWrites).toEqual([]);
  });

  it("releases the held button when a drag is cancelled", () => {
    const { reporter, ptyWrites } = harness(snapshot("drag"));
    reporter.handle("start", pt(55, 90, 0), 1);
    reporter.handle("move", pt(95, 91, LONG_PRESS_MS + 10), 1);
    reporter.handle("cancel", pt(95, 91, LONG_PRESS_MS + 40), 1);
    expect(ptyWrites).toEqual(["\x1b[<0;6;5M", "\x1b[<32;10;5M", "\x1b[<0;10;5m"]);
  });

  it("releases and goes silent when a second finger lands mid-drag", () => {
    const { reporter, ptyWrites } = harness(snapshot("drag"));
    reporter.handle("start", pt(55, 90, 0), 1);
    reporter.handle("move", pt(95, 91, LONG_PRESS_MS + 10), 1);
    reporter.handle("move", pt(96, 91, LONG_PRESS_MS + 20), 2);
    reporter.handle("move", pt(120, 91, LONG_PRESS_MS + 30), 2);
    reporter.handle("end", pt(120, 91, LONG_PRESS_MS + 60), 1);
    expect(ptyWrites).toEqual(["\x1b[<0;6;5M", "\x1b[<32;10;5M", "\x1b[<0;10;5m"]);
  });

  it("never reports a multi-touch gesture", () => {
    const { reporter, ptyWrites, focused, fireLongPress } = harness(snapshot("drag"));
    reporter.handle("start", pt(55, 90, 0), 2);
    fireLongPress();
    reporter.handle("move", pt(95, 91, 30), 2);
    reporter.handle("end", pt(95, 91, 60), 1);
    expect(ptyWrites).toEqual([]);
    expect(focused()).toBe(0);
  });

  it("encodes drags X10-style when SGR was never negotiated", () => {
    const { reporter, ptyWrites } = harness(snapshot("drag", "default"));
    reporter.handle("start", pt(55, 90, 0), 1);
    reporter.handle("move", pt(95, 91, LONG_PRESS_MS + 10), 1);
    reporter.handle("end", pt(95, 91, LONG_PRESS_MS + 40), 1);
    const byte = (v: number) => String.fromCharCode(32 + v);
    expect(ptyWrites).toEqual([
      `\x1b[M${byte(0)}${byte(6)}${byte(5)}`,
      `\x1b[M${byte(32)}${byte(10)}${byte(5)}`,
      `\x1b[M${byte(3)}${byte(10)}${byte(5)}`,
    ]);
  });
});

describe("buttonReport", () => {
  it("encodes SGR press, release, and motion for the left button", () => {
    expect(buttonReport("press", { col: 3, row: 7 }, "sgr")).toBe("\x1b[<0;3;7M");
    expect(buttonReport("release", { col: 3, row: 7 }, "sgr")).toBe("\x1b[<0;3;7m");
    expect(buttonReport("motion", { col: 3, row: 7 }, "sgr")).toBe("\x1b[<32;3;7M");
  });

  it("caps X10-style coordinates at one byte", () => {
    expect(buttonReport("press", { col: 500, row: 500 }, "default")).toBe("\x1b[M\x20\xff\xff");
  });
});

describe("trackingDelivers", () => {
  it("matches each protocol's report set", () => {
    expect(trackingDelivers("none", "press")).toBe(false);
    expect(trackingDelivers("x10", "press")).toBe(true);
    expect(trackingDelivers("x10", "release")).toBe(false);
    expect(trackingDelivers("vt200", "release")).toBe(true);
    expect(trackingDelivers("vt200", "motion")).toBe(false);
    expect(trackingDelivers("drag", "motion")).toBe(true);
    expect(trackingDelivers("any", "motion")).toBe(true);
  });
});
