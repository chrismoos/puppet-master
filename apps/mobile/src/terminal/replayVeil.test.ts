import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { createReplayVeil, VEIL_TIMEOUT_MS, KB_VEIL_TIMEOUT_MS } from "./replayVeil";

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

  it("unveils on online when no replay was received", () => {
    const setter = vi.fn();
    const veil = createReplayVeil(setter);
    veil.event("init");
    veil.event("online");
    expect(veil.veiled()).toBe(false);
    veil.dispose();
  });

  it("clears timeout on online without replay", () => {
    const setter = vi.fn();
    const veil = createReplayVeil(setter);
    veil.event("init");
    veil.event("online");
    setter.mockClear();
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

  it("kbShow veils immediately", () => {
    const setter = vi.fn();
    const veil = createReplayVeil(setter);
    veil.event("init");
    veil.event("painted");
    expect(veil.veiled()).toBe(false);
    setter.mockClear();
    veil.event("kbShow");
    expect(veil.veiled()).toBe(true);
    expect(setter).toHaveBeenCalledWith(true);
    veil.dispose();
  });

  it("kbSettle unveils after kbShow", () => {
    const setter = vi.fn();
    const veil = createReplayVeil(setter);
    veil.event("kbShow");
    expect(veil.veiled()).toBe(true);
    veil.event("kbSettle");
    expect(veil.veiled()).toBe(false);
    veil.dispose();
  });

  it("kbShow timeout falls back after KB_VEIL_TIMEOUT_MS", () => {
    const setter = vi.fn();
    const veil = createReplayVeil(setter);
    veil.event("kbShow");
    expect(veil.veiled()).toBe(true);
    vi.advanceTimersByTime(KB_VEIL_TIMEOUT_MS - 1);
    expect(veil.veiled()).toBe(true);
    vi.advanceTimersByTime(1);
    expect(veil.veiled()).toBe(false);
    veil.dispose();
  });

  it("kbSettle clears the keyboard timeout", () => {
    const setter = vi.fn();
    const veil = createReplayVeil(setter);
    veil.event("kbShow");
    veil.event("kbSettle");
    setter.mockClear();
    vi.advanceTimersByTime(KB_VEIL_TIMEOUT_MS + 100);
    expect(setter).not.toHaveBeenCalled();
    veil.dispose();
  });
});
