import { describe, expect, it, vi } from "vitest";
import { POINTER_MOVE_MIN_PX, PresenceTracker, RETURN_IDLE_MS } from "./userPresence";

function harness() {
  let now = 0;
  const returned = vi.fn();
  const tracker = new PresenceTracker(() => now, returned);
  return { tracker, returned, advance: (ms: number) => { now += ms; } };
}

describe("PresenceTracker", () => {
  it("stays quiet while the user keeps interacting", () => {
    const h = harness();
    h.tracker.input();
    h.advance(RETURN_IDLE_MS - 1);
    h.tracker.input();
    expect(h.returned).not.toHaveBeenCalled();
  });

  it("returns once on the first input after leaving", () => {
    const h = harness();
    h.tracker.leave();
    h.tracker.input();
    h.tracker.input();
    expect(h.returned).toHaveBeenCalledTimes(1);
  });

  it("treats input after a long idle as a return", () => {
    const h = harness();
    h.advance(RETURN_IDLE_MS);
    h.tracker.input();
    expect(h.returned).toHaveBeenCalledTimes(1);
  });

  it("counts real pointer travel and ignores moves repeated in place", () => {
    const h = harness();
    h.tracker.pointerMoved(100, 100);
    h.tracker.leave();
    h.tracker.pointerMoved(100, 100);
    h.tracker.pointerMoved(100 + POINTER_MOVE_MIN_PX - 1, 100);
    expect(h.returned).not.toHaveBeenCalled();
    h.tracker.pointerMoved(100 + POINTER_MOVE_MIN_PX * 2, 100);
    expect(h.returned).toHaveBeenCalledTimes(1);
  });

  it("does not take the first pointer position it sees as movement", () => {
    const h = harness();
    h.tracker.leave();
    h.tracker.pointerMoved(500, 500);
    expect(h.returned).not.toHaveBeenCalled();
  });
});
