import { describe, expect, it } from "vitest";
import { PaneGestureRecognizer, paneAfter, type TouchPoint } from "./paneGesture";

const one = (x: number, y: number): TouchPoint[] => [{ id: 1, x, y }];
const two = (x: number, y: number): TouchPoint[] => [{ id: 1, x, y }, { id: 2, x: x + 40, y: y + 10 }];

function swipe(recognizer: PaneGestureRecognizer, from: TouchPoint[], to: TouchPoint[], ms: number, at = 0) {
  recognizer.start(from, at);
  recognizer.move(to);
  return recognizer.end([], at + ms);
}

describe("pane gestures", () => {
  it("reads a quick one-finger swipe left as next and right as previous", () => {
    const r = new PaneGestureRecognizer();
    expect(swipe(r, one(250, 300), one(150, 310), 200)).toBe("next");
    expect(swipe(r, one(100, 300), one(200, 290), 200)).toBe("previous");
  });

  it("leaves scrolls, slow drags, short moves and edge swipes alone", () => {
    const r = new PaneGestureRecognizer();
    expect(swipe(r, one(200, 100), one(160, 300), 200)).toBeNull();
    expect(swipe(r, one(250, 300), one(150, 300), 1200)).toBeNull();
    expect(swipe(r, one(250, 300), one(220, 300), 100)).toBeNull();
    expect(swipe(r, one(10, 300), one(200, 300), 200)).toBeNull();
  });

  it("ignores one-finger swipes where the pane scrolls sideways itself", () => {
    const r = new PaneGestureRecognizer({ swipes: false });
    expect(swipe(r, one(250, 300), one(150, 300), 200)).toBeNull();
    expect(swipe(r, two(80, 300), two(200, 300), 200)).toBe("agent");
  });

  it("goes back to the agent on a two-finger swipe right, not left", () => {
    const r = new PaneGestureRecognizer();
    expect(swipe(r, two(80, 300), two(200, 305), 250)).toBe("agent");
    expect(swipe(r, two(200, 300), two(80, 300), 250)).toBeNull();
  });

  it("goes back to the agent on a two-finger double-tap, but not a single tap or slow taps", () => {
    const r = new PaneGestureRecognizer();
    expect(swipe(r, two(100, 200), two(102, 201), 120, 0)).toBeNull();
    expect(swipe(r, two(100, 200), two(101, 200), 120, 300)).toBe("agent");
    expect(swipe(r, two(100, 200), two(101, 200), 120, 2000)).toBeNull();
    expect(swipe(r, two(100, 200), two(101, 200), 120, 3000)).toBeNull();
  });

  it("waits until every finger lifts, and counts a second finger that lands later", () => {
    const r = new PaneGestureRecognizer();
    r.start(one(80, 300), 0);
    r.start(two(80, 300), 20);
    r.move(two(200, 300));
    expect(r.end([{ id: 2, x: 240, y: 310 }], 200)).toBeNull();
    expect(r.end([], 220)).toBe("agent");
  });

  it("steps through panes without wrapping, and jumps to the agent", () => {
    expect(paneAfter("next", 0, 3)).toBe(1);
    expect(paneAfter("next", 2, 3)).toBeNull();
    expect(paneAfter("previous", 0, 3)).toBeNull();
    expect(paneAfter("agent", 2, 3)).toBe(0);
    expect(paneAfter("agent", 0, 3)).toBeNull();
  });
});
