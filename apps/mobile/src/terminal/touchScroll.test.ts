import { describe, expect, it } from "vitest";

import {
  arrowKeySequence,
  BridgedPanSink,
  DEFAULT_TOUCH_SCROLL_CONFIG,
  TouchScrollGesture,
  type GestureAction,
  type GestureContext,
} from "./touchScroll";

const LINE_HEIGHT_PX = 20;

const SCROLL: GestureContext = { lineHeightPx: LINE_HEIGHT_PX, mode: "viewport" };
const ARROWS: GestureContext = { lineHeightPx: LINE_HEIGHT_PX, mode: "arrows" };
const WHEEL: GestureContext = { lineHeightPx: LINE_HEIGHT_PX, mode: "wheel" };

function point(y: number, timeMs: number, x = 50) {
  return { x, y, timeMs };
}

function drag(
  gesture: TouchScrollGesture,
  ctx: GestureContext,
  points: Array<{ x: number; y: number; timeMs: number }>,
): GestureAction[] {
  gesture.touchStart(points[0], 1);
  return points.slice(1).map((p) => gesture.touchMove(p, ctx));
}

describe("TouchScrollGesture", () => {
  it("ignores movement below the drag slop", () => {
    const gesture = new TouchScrollGesture();
    const actions = drag(gesture, SCROLL, [point(100, 0), point(105, 30)]);
    expect(actions).toEqual([{ kind: "none" }]);
    expect(gesture.scrolling()).toBe(false);
    expect(gesture.touchEnd(40, SCROLL)).toEqual({ claimed: false });
  });

  it("scrolls the viewport up when the finger drags down", () => {
    const gesture = new TouchScrollGesture();
    const actions = drag(gesture, SCROLL, [point(100, 0), point(150, 50)]);
    expect(actions).toEqual([{ kind: "scroll", lines: -2 }]);
    expect(gesture.scrolling()).toBe(true);
  });

  it("scrolls the viewport down when the finger drags up", () => {
    const gesture = new TouchScrollGesture();
    const actions = drag(gesture, SCROLL, [point(200, 0), point(155, 50)]);
    expect(actions).toEqual([{ kind: "scroll", lines: 2 }]);
  });

  it("accumulates partial-line remainders so slow drags still move", () => {
    const gesture = new TouchScrollGesture();
    const actions = drag(gesture, SCROLL, [
      point(100, 0),
      point(108, 40),
      point(116, 80),
      point(124, 120),
      point(132, 160),
    ]);
    expect(actions.flatMap((a) => (a.kind === "scroll" ? [a.lines] : []))).toEqual([-1]);
    const more = gesture.touchMove(point(140, 200), SCROLL);
    expect(more).toEqual({ kind: "scroll", lines: -1 });
  });

  it("claims a mostly-horizontal drag and scrolls only its vertical travel", () => {
    const gesture = new TouchScrollGesture();
    const actions = drag(gesture, SCROLL, [point(100, 0), { x: 90, y: 105, timeMs: 40 }]);
    expect(actions).toEqual([{ kind: "none" }]);
    expect(gesture.scrolling()).toBe(true);
    expect(gesture.touchMove(point(160, 80, 90), SCROLL)).toEqual({ kind: "scroll", lines: -3 });
  });

  it("releases long-press drags to selection", () => {
    const gesture = new TouchScrollGesture();
    const actions = drag(gesture, SCROLL, [point(100, 0), point(150, 600)]);
    expect(actions).toEqual([{ kind: "none" }]);
    expect(gesture.scrolling()).toBe(false);
  });

  it("releases multi-touch gestures", () => {
    const gesture = new TouchScrollGesture();
    gesture.touchStart(point(100, 0), 2);
    expect(gesture.touchMove(point(150, 50), SCROLL)).toEqual({ kind: "none" });
    expect(gesture.scrolling()).toBe(false);
  });

  it("emulates Up arrows for downward drags on the alternate screen", () => {
    const gesture = new TouchScrollGesture();
    const actions = drag(gesture, ARROWS, [point(100, 0), point(163, 60)]);
    expect(actions).toEqual([{ kind: "arrows", arrow: "up", count: 3 }]);
  });

  it("emulates Down arrows for upward drags on the alternate screen", () => {
    const gesture = new TouchScrollGesture();
    const actions = drag(gesture, ARROWS, [point(200, 0), point(158, 60)]);
    expect(actions).toEqual([{ kind: "arrows", arrow: "down", count: 2 }]);
  });

  it("produces wheel-up steps at the touch point in wheel mode", () => {
    const gesture = new TouchScrollGesture();
    const actions = drag(gesture, WHEEL, [point(100, 0), point(163, 60)]);
    expect(actions).toEqual([{ kind: "wheel", direction: "up", count: 3, xPx: 50, yPx: 163 }]);
  });

  it("produces wheel-down steps for upward drags in wheel mode", () => {
    const gesture = new TouchScrollGesture();
    const actions = drag(gesture, WHEEL, [point(200, 0), point(158, 60)]);
    expect(actions).toEqual([{ kind: "wheel", direction: "down", count: 2, xPx: 50, yPx: 158 }]);
  });

  it("keeps the remainder across an alt-screen mode switch mid-drag", () => {
    const gesture = new TouchScrollGesture();
    const first = drag(gesture, SCROLL, [point(100, 0), point(130, 30)]);
    expect(first).toEqual([{ kind: "scroll", lines: -1 }]);
    const second = gesture.touchMove(point(160, 60), ARROWS);
    expect(second).toEqual({ kind: "arrows", arrow: "up", count: 2 });
  });

  it("starts a fling after a fast lift and decays it to a stop", () => {
    const gesture = new TouchScrollGesture();
    drag(gesture, SCROLL, [point(100, 0), point(140, 20), point(180, 40)]);
    expect(gesture.touchEnd(48, SCROLL)).toEqual({ claimed: true });
    expect(gesture.flingActive()).toBe(true);
    let scrolled = 0;
    let now = 48;
    for (let i = 0; i < 200 && gesture.flingActive(); i += 1) {
      now += 16;
      const action = gesture.flingStep(now, SCROLL);
      if (action.kind === "scroll") scrolled += action.lines;
    }
    expect(gesture.flingActive()).toBe(false);
    expect(scrolled).toBeLessThan(0);
    expect(gesture.flingStep(now + 16, SCROLL)).toEqual({ kind: "none" });
  });

  it("does not fling after a pause before the lift", () => {
    const gesture = new TouchScrollGesture();
    drag(gesture, SCROLL, [point(100, 0), point(140, 20), point(180, 40)]);
    gesture.touchEnd(400, SCROLL);
    expect(gesture.flingActive()).toBe(false);
  });

  it("does not fling below the start velocity", () => {
    const gesture = new TouchScrollGesture();
    drag(gesture, SCROLL, [point(100, 0), point(110, 100), point(120, 200)]);
    gesture.touchEnd(210, SCROLL);
    expect(gesture.flingActive()).toBe(false);
  });

  it("does not fling in PTY delivery modes and stops if a fling crosses into one", () => {
    const gesture = new TouchScrollGesture();
    drag(gesture, ARROWS, [point(100, 0), point(140, 20), point(180, 40)]);
    gesture.touchEnd(48, ARROWS);
    expect(gesture.flingActive()).toBe(false);

    drag(gesture, WHEEL, [point(100, 50), point(140, 70), point(180, 90)]);
    gesture.touchEnd(98, WHEEL);
    expect(gesture.flingActive()).toBe(false);

    drag(gesture, SCROLL, [point(100, 100), point(140, 120), point(180, 140)]);
    gesture.touchEnd(148, SCROLL);
    expect(gesture.flingActive()).toBe(true);
    expect(gesture.flingStep(164, WHEEL)).toEqual({ kind: "none" });
    expect(gesture.flingActive()).toBe(false);
  });

  it("stops an active fling when a new touch lands", () => {
    const gesture = new TouchScrollGesture();
    drag(gesture, SCROLL, [point(100, 0), point(140, 20), point(180, 40)]);
    gesture.touchEnd(48, SCROLL);
    expect(gesture.flingActive()).toBe(true);
    gesture.touchStart(point(200, 100), 1);
    expect(gesture.flingActive()).toBe(false);
  });

  it("resets on touch cancel", () => {
    const gesture = new TouchScrollGesture();
    drag(gesture, SCROLL, [point(100, 0), point(150, 50)]);
    gesture.touchCancel();
    expect(gesture.scrolling()).toBe(false);
    expect(gesture.touchMove(point(300, 100), SCROLL)).toEqual({ kind: "none" });
  });

  it("uses the configured slop and long-press thresholds", () => {
    const gesture = new TouchScrollGesture({ ...DEFAULT_TOUCH_SCROLL_CONFIG, dragSlopPx: 2, longPressMs: 10_000 });
    const actions = drag(gesture, SCROLL, [point(100, 0), point(140, 600)]);
    expect(actions).toEqual([{ kind: "scroll", lines: -2 }]);
  });

  it("handles a clock-domain mismatch between sample and rAF clocks", () => {
    const gesture = new TouchScrollGesture();
    // Sample clock uses evt.nativeEvent.timestamp — could be epoch ms.
    const sampleBase = 1_700_000_000_000;
    drag(gesture, SCROLL, [
      point(100, sampleBase),
      point(140, sampleBase + 20),
      point(180, sampleBase + 40),
    ]);
    gesture.touchEnd(sampleBase + 48, SCROLL);
    expect(gesture.flingActive()).toBe(true);

    // rAF clock is ms since page load — a completely different epoch.
    const rafBase = 5_000;
    let scrolled = 0;
    let now = rafBase;
    for (let i = 0; i < 300 && gesture.flingActive(); i += 1) {
      now += 16;
      const action = gesture.flingStep(now, SCROLL);
      if (action.kind === "scroll") scrolled += action.lines;
    }
    // The fling must produce bounded travel despite the clock mismatch.
    expect(gesture.flingActive()).toBe(false);
    expect(scrolled).toBeLessThan(0);
    // A 5000-line scrollback at 20px lines is 250 lines. A normal swipe
    // should scroll far less than the entire buffer.
    expect(Math.abs(scrolled)).toBeLessThan(200);
  });

  it("ends the fling when a single step has a nonsensical elapsed time", () => {
    const gesture = new TouchScrollGesture();
    drag(gesture, SCROLL, [point(100, 0), point(140, 20), point(180, 40)]);
    gesture.touchEnd(48, SCROLL);
    expect(gesture.flingActive()).toBe(true);

    // First step: baseline adoption, returns none.
    const first = gesture.flingStep(100, SCROLL);
    expect(first).toEqual({ kind: "none" });
    expect(gesture.flingActive()).toBe(true);

    // A stall of >100ms between frames ends the fling rather than applying
    // a huge displacement.
    const second = gesture.flingStep(300, SCROLL);
    expect(second).toEqual({ kind: "none" });
    expect(gesture.flingActive()).toBe(false);
  });

  it("ends the fling when a negative elapsed time is detected", () => {
    const gesture = new TouchScrollGesture();
    drag(gesture, SCROLL, [point(100, 0), point(140, 20), point(180, 40)]);
    gesture.touchEnd(48, SCROLL);
    expect(gesture.flingActive()).toBe(true);

    // Adopt baseline at 200.
    gesture.flingStep(200, SCROLL);
    expect(gesture.flingActive()).toBe(true);

    // A negative elapsed (clock went backwards) ends the fling.
    gesture.flingStep(100, SCROLL);
    expect(gesture.flingActive()).toBe(false);
  });

  it("caps fling velocity from a tiny-span high-displacement swipe", () => {
    const gesture = new TouchScrollGesture();
    // Two samples 1ms apart with 80px delta: raw velocity = 80 px/ms,
    // which would scroll the entire buffer in a few frames uncapped.
    drag(gesture, SCROLL, [point(100, 0), point(110, 8), point(190, 9)]);
    gesture.touchEnd(10, SCROLL);
    expect(gesture.flingActive()).toBe(true);

    let scrolled = 0;
    let now = 100;
    // First frame adopts baseline.
    gesture.flingStep(now, SCROLL);
    for (let i = 0; i < 300 && gesture.flingActive(); i += 1) {
      now += 16;
      const action = gesture.flingStep(now, SCROLL);
      if (action.kind === "scroll") scrolled += action.lines;
    }
    // With the velocity cap, total travel is bounded.
    expect(Math.abs(scrolled)).toBeLessThan(200);
  });

  it("first fling step adopts the rAF baseline and produces no scroll", () => {
    const gesture = new TouchScrollGesture();
    drag(gesture, SCROLL, [point(100, 0), point(140, 20), point(180, 40)]);
    gesture.touchEnd(48, SCROLL);
    expect(gesture.flingActive()).toBe(true);
    // First step sets baseline — no movement.
    expect(gesture.flingStep(100, SCROLL)).toEqual({ kind: "none" });
    // Second step produces actual scroll.
    const second = gesture.flingStep(116, SCROLL);
    expect(second.kind).toBe("scroll");
  });
});

describe("BridgedPanSink", () => {
  function sink(ctx: GestureContext = SCROLL) {
    const gesture = new TouchScrollGesture();
    const applied: GestureAction[] = [];
    let flings = 0;
    const s = new BridgedPanSink(gesture, {
      context: () => ctx,
      apply: (action) => applied.push(action),
      startFling: () => {
        flings += 1;
      },
    });
    return { s, gesture, applied, flingCount: () => flings };
  }

  it("scrolls from forwarded pan events like a DOM drag", () => {
    const { s, applied } = sink();
    s.handle("start", point(100, 0));
    s.handle("move", point(150, 50));
    expect(applied).toEqual([{ kind: "scroll", lines: -2 }]);
  });

  it("emulates arrows on the alternate screen", () => {
    const { s, applied } = sink(ARROWS);
    s.handle("start", point(100, 0));
    s.handle("move", point(163, 60));
    expect(applied).toEqual([{ kind: "arrows", arrow: "up", count: 3 }]);
  });

  it("starts the fling loop after a fast lift", () => {
    const { s, gesture, flingCount } = sink();
    s.handle("start", point(100, 0));
    s.handle("move", point(140, 20));
    s.handle("move", point(180, 40));
    s.handle("end", point(180, 48));
    expect(gesture.flingActive()).toBe(true);
    expect(flingCount()).toBe(1);
  });

  it("does not fling after a slow lift", () => {
    const { s, flingCount } = sink();
    s.handle("start", point(100, 0));
    s.handle("move", point(140, 20));
    s.handle("end", point(140, 400));
    expect(flingCount()).toBe(0);
  });

  it("resets on a forwarded cancel", () => {
    const { s, applied } = sink();
    s.handle("start", point(100, 0));
    s.handle("move", point(150, 50));
    s.handle("cancel", point(150, 60));
    s.handle("move", point(300, 100));
    expect(applied).toEqual([{ kind: "scroll", lines: -2 }, { kind: "none" }]);
  });
});

describe("velocity and fling from sample density", () => {
  it("computes velocity from a dense sample sequence", () => {
    const gesture = new TouchScrollGesture();
    // Simulate a fast flick: 10 samples over 80ms, moving 200px
    const startY = 400;
    const endY = 200; // 200px upward in 80ms = 2.5 px/ms
    const startMs = 1000;
    const durationMs = 80;
    const samples = 10;

    gesture.touchStart(point(startY, startMs), 1);
    for (let i = 1; i <= samples; i++) {
      const t = startMs + (i * durationMs) / samples;
      const y = startY + (i * (endY - startY)) / samples;
      gesture.touchMove(point(y, t), SCROLL);
    }

    // End immediately after last move
    const result = gesture.touchEnd(startMs + durationMs, SCROLL);
    expect(result.claimed).toBe(true);
    expect(gesture.flingActive()).toBe(true);
  });

  it("computes velocity from the samples retained in the window", () => {
    const gesture = new TouchScrollGesture();
    // recordSample always keeps at least 2 samples, so even with a wide gap
    // the engine still has oldest and newest to differentiate.
    gesture.touchStart(point(400, 0), 1);
    gesture.touchMove(point(380, 10), SCROLL); // claims at 20px > 8px slop
    gesture.touchMove(point(200, 200), SCROLL); // 190ms later

    const result = gesture.touchEnd(200, SCROLL);
    expect(result.claimed).toBe(true);
    // 2 samples retained: the velocity is computed from whatever span they cover
    // (180px / 190ms = ~0.95 px/ms, above the 0.25 threshold)
    expect(gesture.flingActive()).toBe(true);
  });

  it("starts fling from a 3-sample sequence within the velocity window", () => {
    const gesture = new TouchScrollGesture();
    gesture.touchStart(point(400, 1000), 1);
    // 3 moves within 100ms window, fast enough
    gesture.touchMove(point(370, 1020), SCROLL); // claims (30px > 8px slop)
    gesture.touchMove(point(320, 1050), SCROLL);
    gesture.touchMove(point(260, 1080), SCROLL);

    const result = gesture.touchEnd(1080, SCROLL);
    expect(result.claimed).toBe(true);
    expect(gesture.flingActive()).toBe(true);
  });

  it("rejects fling when pause between last move and end exceeds threshold", () => {
    const gesture = new TouchScrollGesture({
      ...DEFAULT_TOUCH_SCROLL_CONFIG,
      flingMaxPauseMs: 80,
    });
    gesture.touchStart(point(400, 1000), 1);
    gesture.touchMove(point(370, 1020), SCROLL);
    gesture.touchMove(point(300, 1050), SCROLL);

    // End 100ms after last move — exceeds 80ms pause threshold
    const result = gesture.touchEnd(1150, SCROLL);
    expect(result.claimed).toBe(true);
    expect(gesture.flingActive()).toBe(false);
  });

  it("reports fling diagnostics through onFlingDiag callback", () => {
    const gesture = new TouchScrollGesture();
    const diags: string[] = [];
    gesture.onFlingDiag = (msg) => diags.push(msg);

    gesture.touchStart(point(400, 1000), 1);
    gesture.touchMove(point(370, 1020), SCROLL);
    gesture.touchMove(point(300, 1050), SCROLL);
    gesture.touchEnd(1050, SCROLL);

    expect(diags.length).toBe(1);
    expect(diags[0]).toMatch(/START.*vel=/);
  });

  it("reports rejection reason through onFlingDiag", () => {
    const gesture = new TouchScrollGesture();
    const diags: string[] = [];
    gesture.onFlingDiag = (msg) => diags.push(msg);

    gesture.touchStart(point(400, 1000), 1);
    gesture.touchMove(point(370, 1020), SCROLL);
    // Long pause then end
    gesture.touchEnd(1200, SCROLL);

    expect(diags.length).toBe(1);
    expect(diags[0]).toMatch(/reject.*pause/);
  });
});

describe("BridgedPanSink preserves all samples for the engine", () => {
  it("feeds every move to the gesture engine without coalescing", () => {
    const gesture = new TouchScrollGesture();
    const actions: GestureAction[] = [];
    let flingStarted = false;
    const sink = new BridgedPanSink(gesture, {
      context: () => SCROLL,
      apply: (a) => actions.push(a),
      startFling: () => { flingStarted = true; },
    });

    // 8 moves in 80ms, 200px displacement
    sink.handle("start", point(400, 1000));
    for (let i = 1; i <= 8; i++) {
      sink.handle("move", point(400 - i * 25, 1000 + i * 10));
    }
    sink.handle("end", point(200, 1080));

    // The engine should have seen enough samples for a fling
    expect(flingStarted).toBe(true);
    expect(gesture.flingActive()).toBe(true);
  });
});

describe("arrowKeySequence", () => {
  it("selects normal cursor sequences by default", () => {
    expect(arrowKeySequence("up", false)).toBe("\x1b[A");
    expect(arrowKeySequence("down", false)).toBe("\x1b[B");
  });

  it("selects application cursor sequences when the mode is set", () => {
    expect(arrowKeySequence("up", true)).toBe("\x1bOA");
    expect(arrowKeySequence("down", true)).toBe("\x1bOB");
  });
});
