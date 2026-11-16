import { describe, expect, it, vi } from "vitest";
import { TerminalWarmCache, TerminalHistoryError, type WarmSnapshot, type WarmTerminal } from "./terminalWarmCache";

vi.mock("../api/token", () => ({ authedFetch: vi.fn() }));
const terminal = (id: string): WarmTerminal => ({ id, sessionId: id, generation: "1" });
const snapshot = (id: string, bytes = 4): WarmSnapshot => ({ data: new Uint8Array(bytes), capturedAt: 0, history: { snapshotId: id, cursor: 0, steps: 0, hasMore: false } });
const flush = async () => { for (let i = 0; i < 10; i += 1) await Promise.resolve(); };

describe("terminal background warming", () => {
  it("warms every active session with bounded concurrency and promotes a clicked session", async () => {
    const requests: string[] = [];
    const pending = new Map<string, (value: WarmSnapshot) => void>();
    let active = 0;
    let peak = 0;
    const cache = new TerminalWarmCache(async (item) => {
      requests.push(item.id);
      active += 1;
      peak = Math.max(peak, active);
      const result = await new Promise<WarmSnapshot>((resolve) => pending.set(item.id, resolve));
      active -= 1;
      return result;
    });
    cache.select("3");
    cache.update(["1", "2", "3", "4", "5"].map(terminal));
    expect(requests).toEqual(["3", "1"]);
    cache.select("5");
    pending.get("1")!(snapshot("1"));
    await flush();
    expect(requests).toEqual(["3", "1", "5"]);
    for (const id of ["3", "5", "2", "4"]) { pending.get(id)!(snapshot(id)); await flush(); }
    expect(cache.stats().warmed).toBe(5);
    expect(peak).toBe(2);
  });

  it("uses activity to order sessions without displacing recently viewed sessions", () => {
    const cache = new TerminalWarmCache(() => new Promise(() => {}));
    cache.select("1");
    cache.select("2");
    cache.update([{ ...terminal("1"), activity: 1 }, { ...terminal("2"), activity: 2 },
      { ...terminal("3"), activity: 100 }, { ...terminal("4"), activity: 200 }]);
    expect(cache.stats().order).toEqual(["2", "1", "4", "3"]);
  });

  it("caps serialized memory and does not immediately refetch evicted entries", async () => {
    const fetch = vi.fn(async (item: WarmTerminal) => snapshot(item.id));
    const cache = new TerminalWarmCache(fetch, 8);
    cache.update(["1", "2", "3", "4"].map(terminal));
    await flush();
    expect(cache.stats().bytes).toBeLessThanOrEqual(8);
    expect(fetch).toHaveBeenCalledTimes(4);
  });

  it("does not fetch extra pages when the current-screen prefix already fills a preview", async () => {
    const fetch = vi.fn(async (item: WarmTerminal) => ({ ...snapshot(item.id, 64 * 1024), history: { snapshotId: item.id, cursor: 0, steps: 0, hasMore: true } }));
    const cache = new TerminalWarmCache(fetch);
    cache.update([terminal("1"), terminal("2")]);
    await flush();
    expect(cache.stats().warmed).toBe(2);
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it("keeps a fresh live replay when an older preload finishes afterward", async () => {
    let resolve!: (value: WarmSnapshot) => void;
    const cache = new TerminalWarmCache(() => new Promise((done) => { resolve = done; }));
    const item = terminal("1");
    cache.update([item]);
    const live = snapshot("newer", 6);
    cache.remember(item, live);
    resolve(snapshot("older"));
    await flush();
    expect(cache.get(item)?.history.snapshotId).toBe("newer");
    expect(cache.stats().bytes).toBe(6);
    cache.reset();
    expect(cache.stats().bytes).toBe(0);
    expect(cache.stats().active).toBe(0);
  });

  it("retries unavailable sessions without a click and cancels retries on reset", async () => {
    vi.useFakeTimers();
    try {
      const fetch = vi.fn().mockRejectedValueOnce(new Error("worker offline")).mockResolvedValue(snapshot("1"));
      const cache = new TerminalWarmCache(fetch);
      cache.update([terminal("1")]);
      await flush();
      expect(fetch).toHaveBeenCalledTimes(1);
      await vi.advanceTimersByTimeAsync(30_000);
      expect(cache.stats().warmed).toBe(1);
      expect(fetch).toHaveBeenCalledTimes(2);
      cache.reset();
      fetch.mockRejectedValue(new Error("worker offline"));
      cache.update([terminal("2")]);
      await flush();
      cache.reset();
      await vi.advanceTimersByTimeAsync(60_000);
      expect(fetch).toHaveBeenCalledTimes(3);
    } finally { vi.useRealTimers(); }
  });

  it("refreshes an expired preview instead of retrying its obsolete history cursor", async () => {
    vi.useFakeTimers();
    try {
      const prefix = { ...snapshot("expired"), history: { snapshotId: "expired", cursor: 0, steps: 0, hasMore: true } };
      const fetch = vi.fn().mockResolvedValueOnce(prefix).mockRejectedValueOnce(new TerminalHistoryError(410)).mockResolvedValue(snapshot("fresh"));
      const cache = new TerminalWarmCache(fetch);
      cache.update([terminal("1")]);
      await flush();
      await vi.advanceTimersByTimeAsync(30_000);
      expect(cache.get(terminal("1"))?.history.snapshotId).toBe("fresh");
      expect(fetch.mock.calls[2]?.[1]).toBeUndefined();
      cache.reset();
    } finally { vi.useRealTimers(); }
  });

  it("discards a stale generation fetched before inventory changed", async () => {
    let resolve!: (value: WarmSnapshot) => void;
    const cache = new TerminalWarmCache(() => new Promise((done) => { resolve = done; }));
    cache.update([terminal("1")]);
    cache.update([]);
    resolve(snapshot("1"));
    await flush();
    expect(cache.stats().warmed).toBe(0);
  });
});
