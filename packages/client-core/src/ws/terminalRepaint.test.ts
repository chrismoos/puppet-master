import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { VIEWER_SIZE_ASSERT_DEBOUNCE_MS, wireViewerSize } from "./terminalRepaint";
import type { TerminalSize } from "./pty";
import type { TerminalStreamStatus } from "./terminalSocket";

describe("wireViewerSize", () => {
  it("does not reclaim on focus, visibility or reconnect while a size choice is pending", () => {
    vi.useFakeTimers();
    let blocked = true;
    let online: ((status: TerminalStreamStatus) => void) | undefined;
    const resize = vi.fn();
    const viewer = wireViewerSize({
      resize,
      ptySize: () => ({ cols: 50, rows: 24 }),
      onStatus: (listener) => { online = listener; return () => {}; },
    }, { cols: 120, rows: 40 }, { canAssert: () => !blocked });
    viewer.opened();
    viewer.refocused();
    viewer.setVisible(false);
    viewer.setVisible(true);
    online?.({ phase: "online" });
    vi.advanceTimersByTime(VIEWER_SIZE_ASSERT_DEBOUNCE_MS);
    expect(resize).not.toHaveBeenCalled();
    blocked = false;
    viewer.refocused();
    vi.advanceTimersByTime(VIEWER_SIZE_ASSERT_DEBOUNCE_MS);
    expect(resize).toHaveBeenCalledExactlyOnceWith(120, 40);
    viewer.dispose();
  });
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  function harness(cols: number, rows: number) {
    const resizes: Array<{ cols: number; rows: number }> = [];
    let listener: ((status: TerminalStreamStatus) => void) | null = null;
    let unsubscribed = false;
    const handle = {
      resize: (sentCols: number, sentRows: number) => resizes.push({ cols: sentCols, rows: sentRows }),
      onStatus: (candidate: (status: TerminalStreamStatus) => void) => {
        listener = candidate;
        return () => {
          unsubscribed = true;
        };
      },
      ptySize: (): TerminalSize | null => null,
    };
    const term = { cols, rows };
    const viewer = wireViewerSize(handle, term);
    return {
      resizes,
      term,
      viewer,
      online: () => listener?.({ phase: "online" }),
      reconnecting: () => listener?.({ phase: "reconnecting", canRetry: false }),
      settle: () => vi.advanceTimersByTime(VIEWER_SIZE_ASSERT_DEBOUNCE_MS),
      wasUnsubscribed: () => unsubscribed,
    };
  }

  it("starts hidden and never asserts on replay while hidden", () => {
    const sized = harness(168, 48);
    expect(sized.viewer.visible()).toBe(false);
    sized.online();
    sized.online();
    sized.settle();
    expect(sized.resizes).toEqual([]);
  });

  it("asserts the viewer size once on open, even when nothing changed locally", () => {
    // The attach gap: the viewer fitted before the socket existed, so no
    // local size change ever calls resize on its own.
    const sized = harness(56, 30);
    sized.viewer.opened();
    expect(sized.resizes).toEqual([]);
    sized.settle();
    expect(sized.resizes).toEqual([{ cols: 56, rows: 30 }]);
    sized.online();
    sized.settle();
    expect(sized.resizes).toEqual([
      { cols: 56, rows: 30 },
      { cols: 56, rows: 30 },
    ]);
  });

  it("asserts again each time the viewer switches back to the terminal", () => {
    const sized = harness(132, 48);
    sized.viewer.opened();
    sized.settle();
    sized.viewer.setVisible(false);
    sized.viewer.opened();
    sized.settle();
    expect(sized.resizes).toEqual([
      { cols: 132, rows: 48 },
      { cols: 132, rows: 48 },
    ]);
  });

  it("asserts when a hidden viewer regains visibility and stays quiet when it hides", () => {
    const sized = harness(56, 30);
    sized.viewer.opened();
    sized.settle();
    sized.viewer.setVisible(false);
    sized.settle();
    expect(sized.resizes).toHaveLength(1);
    sized.viewer.setVisible(true);
    sized.settle();
    expect(sized.resizes).toEqual([
      { cols: 56, rows: 30 },
      { cols: 56, rows: 30 },
    ]);
  });

  it("ignores repeated visible edges in the same state", () => {
    const sized = harness(56, 30);
    sized.viewer.opened();
    sized.settle();
    sized.viewer.setVisible(true);
    sized.settle();
    expect(sized.resizes).toHaveLength(1);
  });

  it("coalesces rapid transitions into one assert", () => {
    const sized = harness(56, 30);
    sized.viewer.opened();
    sized.viewer.setVisible(false);
    sized.viewer.setVisible(true);
    sized.viewer.setVisible(false);
    sized.viewer.setVisible(true);
    vi.advanceTimersByTime(VIEWER_SIZE_ASSERT_DEBOUNCE_MS - 1);
    expect(sized.resizes).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(sized.resizes).toEqual([{ cols: 56, rows: 30 }]);
  });

  it("drops a scheduled assert when the viewer hides before it fires", () => {
    const sized = harness(168, 48);
    sized.viewer.opened();
    sized.viewer.setVisible(false);
    sized.settle();
    expect(sized.resizes).toEqual([]);
  });

  it("asserts the plain size after a replay, never a row jiggle", () => {
    // Every replay is a state snapshot laid out for the PTY's size, so a
    // reconnect has nothing to repaint.
    const sized = harness(132, 48);
    sized.online();
    sized.settle();
    sized.viewer.opened();
    sized.settle();
    sized.online();
    sized.settle();
    expect(sized.resizes).toEqual([
      { cols: 132, rows: 48 },
      { cols: 132, rows: 48 },
    ]);
  });

  it("folds an open and a replay inside the debounce window into one assert", () => {
    const sized = harness(132, 48);
    sized.viewer.opened();
    sized.online();
    sized.settle();
    expect(sized.resizes).toEqual([{ cols: 132, rows: 48 }]);
  });

  it("ignores reconnecting states", () => {
    const sized = harness(132, 48);
    sized.viewer.opened();
    sized.settle();
    sized.reconnecting();
    sized.settle();
    expect(sized.resizes).toEqual([{ cols: 132, rows: 48 }]);
  });

  it("uses the terminal size at assert time, not wiring time", () => {
    const sized = harness(80, 24);
    sized.viewer.opened();
    sized.term.cols = 190;
    sized.term.rows = 52;
    sized.settle();
    expect(sized.resizes).toEqual([{ cols: 190, rows: 52 }]);
  });

  it("skips degenerate sizes", () => {
    const sized = harness(1, 0);
    sized.viewer.opened();
    sized.settle();
    expect(sized.resizes).toEqual([]);
  });

  it("stops asserting after dispose", () => {
    const sized = harness(132, 48);
    sized.viewer.opened();
    sized.viewer.dispose();
    sized.settle();
    expect(sized.resizes).toEqual([]);
    expect(sized.wasUnsubscribed()).toBe(true);
  });
});

describe("regaining window focus", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  function focusHarness(cols: number, rows: number) {
    const sent: Array<[number, number]> = [];
    const ptyState: { size: TerminalSize | null } = { size: null };
    const handle = {
      resize: (sentCols: number, sentRows: number) => sent.push([sentCols, sentRows]),
      onStatus: () => () => undefined,
      ptySize: () => ptyState.size,
    };
    const term = { cols, rows };
    return {
      sent,
      ptyState,
      term,
      viewer: wireViewerSize(handle, term),
      settle: () => vi.advanceTimersByTime(VIEWER_SIZE_ASSERT_DEBOUNCE_MS),
    };
  }

  it("sends nothing when the PTY already holds this viewer's size", () => {
    // Focus is not a visibility transition. A viewer that already owns the
    // size has nothing to claim, and asserting anyway would SIGWINCH every
    // full-screen program each time the reader clicks back into the window.
    const focus = focusHarness(100, 30);
    focus.viewer.opened();
    focus.settle();
    expect(focus.sent).toEqual([[100, 30]]);

    focus.ptyState.size = { cols: 100, rows: 30 };
    focus.viewer.refocused();
    focus.settle();
    expect(focus.sent).toEqual([[100, 30]]);
  });

  it("re-asserts when another viewer has taken the PTY size", () => {
    // The case the focus listener exists for: a window that never went
    // hidden gets no visibilitychange, so focus is the only signal that
    // someone else resized the PTY while this window was away.
    const focus = focusHarness(100, 30);
    focus.viewer.opened();
    focus.settle();
    focus.ptyState.size = { cols: 56, rows: 20 };

    focus.viewer.refocused();
    focus.settle();
    expect(focus.sent).toEqual([[100, 30], [100, 30]]);
  });

  it("re-asserts while the PTY size is still unknown", () => {
    // No echo has arrived, so the viewer cannot tell whether it owns the
    // size and claims it rather than leaving a wrong one in place.
    const focus = focusHarness(100, 30);
    focus.viewer.opened();
    focus.settle();

    focus.viewer.refocused();
    focus.settle();
    expect(focus.sent).toEqual([[100, 30], [100, 30]]);
  });

  it("stays silent for a viewer that is not visible", () => {
    // Warm hidden layers must never assert, or they fight the viewer the
    // reader is actually looking at.
    const focus = focusHarness(80, 24);
    focus.ptyState.size = { cols: 168, rows: 48 };
    focus.viewer.refocused();
    focus.settle();
    expect(focus.sent).toEqual([]);
  });
});

describe("opening or switching to a terminal", () => {
  it("asserts again on a second opened() even when nothing changed", () => {
    // Opening or switching is a deliberate edge: that viewer claims the size
    // even if it happens to match what the PTY already holds.
    const term = { cols: 100, rows: 30 };
    const sent: Array<[number, number]> = [];
    const handle = {
      resize: (cols: number, rows: number) => sent.push([cols, rows]),
      onStatus: () => () => undefined,
      ptySize: () => ({ cols: 100, rows: 30 }),
    };
    vi.useFakeTimers();
    const viewer = wireViewerSize(handle, term);

    viewer.opened();
    vi.advanceTimersByTime(VIEWER_SIZE_ASSERT_DEBOUNCE_MS);
    expect(sent).toEqual([[100, 30]]);

    viewer.opened();
    vi.advanceTimersByTime(VIEWER_SIZE_ASSERT_DEBOUNCE_MS);
    expect(sent).toEqual([[100, 30], [100, 30]]);
    vi.useRealTimers();
  });
});
