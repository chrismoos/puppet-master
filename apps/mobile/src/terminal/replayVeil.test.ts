import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { createReplayVeil, ONLINE_GRACE_MS, VEIL_TIMEOUT_MS } from "./replayVeil";

describe("createReplayVeil", () => {
  beforeEach(() => { vi.useFakeTimers(); });
  afterEach(() => { vi.useRealTimers(); });

  it("starts unveiled", () => {
    const setter = vi.fn();
    const veil = createReplayVeil(setter);
    expect(veil.veiled()).toBe(false);
    expect(setter).not.toHaveBeenCalled();
    veil.dispose();
  });

  it("veils on init", () => {
    const setter = vi.fn();
    const veil = createReplayVeil(setter);
    veil.event("init");
    expect(veil.veiled()).toBe(true);
    expect(setter).toHaveBeenCalledWith(true);
    veil.dispose();
  });

  it("unveils on painted after replay", () => {
    const setter = vi.fn();
    const veil = createReplayVeil(setter);
    veil.event("init");
    veil.event("replay");
    veil.event("painted");
    expect(veil.veiled()).toBe(false);
    expect(setter).toHaveBeenLastCalledWith(false);
    veil.dispose();
  });

  it("clears timeout on painted", () => {
    const setter = vi.fn();
    const veil = createReplayVeil(setter);
    veil.event("init");
    veil.event("replay");
    veil.event("painted");
    setter.mockClear();
    vi.advanceTimersByTime(VEIL_TIMEOUT_MS);
    expect(setter).not.toHaveBeenCalled();
    veil.dispose();
  });

  it("stays veiled on online, waiting briefly for the replay that follows", () => {
    const setter = vi.fn();
    const veil = createReplayVeil(setter);
    veil.event("init");
    veil.event("online");
    expect(veil.veiled()).toBe(true);
    vi.advanceTimersByTime(ONLINE_GRACE_MS - 1);
    expect(veil.veiled()).toBe(true);
    veil.event("replay");
    veil.event("painted");
    expect(veil.veiled()).toBe(false);
    veil.dispose();
  });

  it("unveils after the online grace when no replay comes", () => {
    const onTimeout = vi.fn();
    const veil = createReplayVeil(vi.fn(), onTimeout);
    veil.event("init");
    veil.event("online");
    vi.advanceTimersByTime(ONLINE_GRACE_MS);
    expect(onTimeout).toHaveBeenCalledTimes(1);
    veil.dispose();
  });

  it("online does not veil a terminal already showing", () => {
    const setter = vi.fn();
    const veil = createReplayVeil(setter);
    veil.event("online");
    expect(veil.veiled()).toBe(false);
    vi.advanceTimersByTime(VEIL_TIMEOUT_MS);
    expect(setter).not.toHaveBeenCalled();
    veil.dispose();
  });

  it("stays veiled on online when replay was received (waits for painted)", () => {
    const setter = vi.fn();
    const veil = createReplayVeil(setter);
    veil.event("init");
    veil.event("replay");
    setter.mockClear();
    veil.event("online");
    expect(veil.veiled()).toBe(true);
    expect(setter).not.toHaveBeenCalled();
    veil.dispose();
  });

  it("unveils on error", () => {
    const setter = vi.fn();
    const veil = createReplayVeil(setter);
    veil.event("init");
    veil.event("error");
    expect(veil.veiled()).toBe(false);
    veil.dispose();
  });

  it("clears timeout on error", () => {
    const setter = vi.fn();
    const veil = createReplayVeil(setter);
    veil.event("init");
    veil.event("error");
    setter.mockClear();
    vi.advanceTimersByTime(VEIL_TIMEOUT_MS);
    expect(setter).not.toHaveBeenCalled();
    veil.dispose();
  });

  it("unveils on timeout fallback without onTimeout", () => {
    const setter = vi.fn();
    const veil = createReplayVeil(setter);
    veil.event("init");
    expect(veil.veiled()).toBe(true);
    vi.advanceTimersByTime(VEIL_TIMEOUT_MS);
    expect(veil.veiled()).toBe(false);
    expect(setter).toHaveBeenLastCalledWith(false);
    veil.dispose();
  });

  it("calls onTimeout instead of setter(false) when provided", () => {
    const setter = vi.fn();
    const onTimeout = vi.fn();
    const veil = createReplayVeil(setter, onTimeout);
    veil.event("init");
    expect(veil.veiled()).toBe(true);
    vi.advanceTimersByTime(VEIL_TIMEOUT_MS);
    expect(onTimeout).toHaveBeenCalledTimes(1);
    // State machine has not advanced yet — caller drives it
    expect(veil.veiled()).toBe(true);
    // Caller advances after revealAtBottom resolves
    veil.event("timeout");
    expect(veil.veiled()).toBe(false);
    veil.dispose();
  });

  it("replay resets the timeout", () => {
    const setter = vi.fn();
    const veil = createReplayVeil(setter);
    veil.event("init");
    vi.advanceTimersByTime(VEIL_TIMEOUT_MS - 500);
    veil.event("replay");
    vi.advanceTimersByTime(VEIL_TIMEOUT_MS - 500);
    expect(veil.veiled()).toBe(true);
    vi.advanceTimersByTime(500);
    expect(veil.veiled()).toBe(false);
    veil.dispose();
  });

  it("does not call setter when state does not change", () => {
    const setter = vi.fn();
    const veil = createReplayVeil(setter);
    veil.event("init");
    expect(setter).toHaveBeenCalledTimes(1);
    veil.event("replay");
    // Already veiled, should not call setter again
    expect(setter).toHaveBeenCalledTimes(1);
    veil.dispose();
  });

  it("dispose clears the timeout", () => {
    const setter = vi.fn();
    const veil = createReplayVeil(setter);
    veil.event("init");
    veil.dispose();
    vi.advanceTimersByTime(VEIL_TIMEOUT_MS + 1000);
    // Still veiled because timer was cleared, setter not called with false
    expect(setter).toHaveBeenCalledTimes(1);
    expect(setter).toHaveBeenCalledWith(true);
  });

  it("re-veils on a second init after painted", () => {
    const setter = vi.fn();
    const veil = createReplayVeil(setter);
    veil.event("init");
    veil.event("replay");
    veil.event("painted");
    expect(veil.veiled()).toBe(false);
    veil.event("init");
    expect(veil.veiled()).toBe(true);
    veil.dispose();
  });

});
