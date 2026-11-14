import { describe, expect, it } from "vitest";
import { DOUBLE_TAP_MS, DoubleTap, PaneGestureRecognizer, paneAfter, type TouchPoint } from "./paneGesture";

const one = (x: number, y: number): TouchPoint[] => [{ id: 1, x, y }];
const two = (x: number, y: number): TouchPoint[] => [{ id: 1, x, y }, { id: 2, x: x + 40, y: y + 10 }];

function swipe(recognizer: PaneGestureRecognizer, from: TouchPoint[], to: TouchPoint[], ms: number) {
  recognizer.start(from, 0);
  recognizer.move(to);
  return recognizer.end([], ms);
}

describe("pane gestures", () => {
  it("reads a two-finger swipe left as next and right as previous", () => {
    const r = new PaneGestureRecognizer();
    expect(swipe(r, two(250, 300), two(130, 305), 250)).toBe("next");
    expect(swipe(r, two(80, 300), two(200, 295), 250)).toBe("previous");
  });

  it("leaves one-finger swipes to the terminal or page underneath", () => {
    const r = new PaneGestureRecognizer();
    expect(swipe(r, one(250, 300), one(100, 300), 200)).toBeNull();
    expect(swipe(r, one(100, 300), one(250, 300), 200)).toBeNull();
  });

  it("ignores two-finger scrolls, short moves and slow drags", () => {
    const r = new PaneGestureRecognizer();
    expect(swipe(r, two(200, 100), two(170, 300), 250)).toBeNull();
    expect(swipe(r, two(200, 300), two(170, 300), 250)).toBeNull();
    expect(swipe(r, two(250, 300), two(100, 300), 1500)).toBeNull();
  });

  it("waits until every finger lifts, and counts a second finger that lands later", () => {
    const r = new PaneGestureRecognizer();
    r.start(one(250, 300), 0);
    r.start(two(250, 300), 20);
    r.move(two(120, 300));
    expect(r.end([{ id: 2, x: 160, y: 310 }], 200)).toBeNull();
    expect(r.end([], 220)).toBe("next");
  });

  it("counts two quick taps as a double tap, but not slow ones", () => {
    const taps = new DoubleTap();
    expect(taps.tap(0)).toBe(false);
    expect(taps.tap(DOUBLE_TAP_MS - 50)).toBe(true);
    expect(taps.tap(1000)).toBe(false);
    expect(taps.tap(1000 + DOUBLE_TAP_MS + 50)).toBe(false);
  });

  it("steps through panes without wrapping, and jumps to the agent", () => {
    expect(paneAfter("next", 0, 3)).toBe(1);
    expect(paneAfter("next", 2, 3)).toBeNull();
    expect(paneAfter("previous", 0, 3)).toBeNull();
    expect(paneAfter("agent", 2, 3)).toBe(0);
    expect(paneAfter("agent", 0, 3)).toBeNull();
  });
});
