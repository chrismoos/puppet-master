import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { SilenceTimer } from "./silence";

const WAITING = 10_000;
const ENDED = 1_500;

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("giving up on silence", () => {
  it("stops listening once nothing has been said for long enough", () => {
    const quiet = vi.fn();
    new SilenceTimer(quiet).restart(WAITING);

    vi.advanceTimersByTime(WAITING - 1);
    expect(quiet).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(quiet).toHaveBeenCalledTimes(1);
  });

  /// A pause mid-sentence is thinking, not finishing, so every word
  /// heard buys another full stretch of quiet.
  it("keeps listening while words keep arriving", () => {
    const quiet = vi.fn();
    const timer = new SilenceTimer(quiet);
    timer.restart(WAITING);

    for (let spoken = 0; spoken < 5; spoken += 1) {
      vi.advanceTimersByTime(ENDED - 1);
      timer.restart(ENDED);
    }
    expect(quiet).not.toHaveBeenCalled();

    vi.advanceTimersByTime(ENDED);
    expect(quiet).toHaveBeenCalledTimes(1);
  });

  /// Once something has been heard the wait shortens, so a finished
  /// sentence is sent rather than sitting there for ten seconds.
  it("takes a new wait on every restart", () => {
    const quiet = vi.fn();
    const timer = new SilenceTimer(quiet);
    timer.restart(WAITING);
    timer.restart(ENDED);

    vi.advanceTimersByTime(ENDED);
    expect(quiet).toHaveBeenCalledTimes(1);
  });

  it("says nothing once it has been cancelled", () => {
    const quiet = vi.fn();
    const timer = new SilenceTimer(quiet);
    timer.restart(WAITING);
    expect(timer.waiting).toBe(true);

    timer.cancel();
    expect(timer.waiting).toBe(false);
    vi.advanceTimersByTime(60_000);
    expect(quiet).not.toHaveBeenCalled();
  });

  /// Two countdowns running at once would end a sentence the user is
  /// still saying.
  it("runs one countdown however often it is restarted", () => {
    const quiet = vi.fn();
    const timer = new SilenceTimer(quiet);
    timer.restart(ENDED);
    timer.restart(ENDED);
    timer.restart(ENDED);

    vi.advanceTimersByTime(ENDED);
    expect(quiet).toHaveBeenCalledTimes(1);
  });
});
