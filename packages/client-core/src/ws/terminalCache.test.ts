import { describe, expect, it } from "vitest";
import { BackgroundPtyBuffer, terminalEvictionPriority } from "./terminalCache";

describe("terminal cache eviction", () => {
  it("evicts shell layers before agent layers", () => {
    expect(terminalEvictionPriority("t:9")).toBeLessThan(terminalEvictionPriority("s:1"));
  });

  it("keeps bounded background output in order", () => {
    const buffer = new BackgroundPtyBuffer(3);
    expect(buffer.push({ data: new Uint8Array([1, 2]), replay: false })).toBe(true);
    expect(buffer.push({ data: new Uint8Array([3]), replay: false })).toBe(true);
    expect(buffer.drain().flatMap((frame) => [...frame.data])).toEqual([1, 2, 3]);
    expect(buffer.drain()).toEqual([]);
  });

  it("drops the warm layer when background output exceeds its bound", () => {
    const buffer = new BackgroundPtyBuffer(2);
    expect(buffer.push({ data: new Uint8Array([1, 2]), replay: false })).toBe(true);
    expect(buffer.push({ data: new Uint8Array([3]), replay: false })).toBe(false);
    expect(buffer.drain()).toEqual([]);
  });

  it("replaces stale background output when a replay starts", () => {
    const buffer = new BackgroundPtyBuffer(3);
    buffer.push({ data: new Uint8Array([1, 2]), replay: false });
    expect(buffer.push({ data: new Uint8Array([3]), replay: true })).toBe(true);
    expect(buffer.drain()).toEqual([{ data: new Uint8Array([3]), replay: true }]);
  });

  it("holds a replay larger than the byte cap instead of rejecting it", () => {
    const buffer = new BackgroundPtyBuffer(2);
    buffer.push({ data: new Uint8Array([1]), replay: false });
    expect(buffer.push({ data: new Uint8Array([2, 3, 4, 5]), replay: true })).toBe(true);
    expect(buffer.bytesHeld()).toBe(4);
    expect(buffer.drain().map((frame) => frame.replay)).toEqual([true]);
  });

  // An over-cap replay that still counted against the cap overflowed on the
  // next live frame, and the resync answering that overflow fetched another
  // replay just as large, so a hidden busy terminal reconnected forever.
  it("keeps accepting live output after holding an over-cap replay", () => {
    const buffer = new BackgroundPtyBuffer(2);
    expect(buffer.push({ data: new Uint8Array([1, 2, 3, 4, 5]), replay: true })).toBe(true);
    expect(buffer.push({ data: new Uint8Array([6]), replay: false })).toBe(true);
    expect(buffer.push({ data: new Uint8Array([7]), replay: false })).toBe(true);
    expect(buffer.bytesHeld()).toBe(7);
    expect(buffer.drain()).toHaveLength(3);
  });

  it("still overflows once live output alone passes the cap", () => {
    const buffer = new BackgroundPtyBuffer(2);
    buffer.push({ data: new Uint8Array([1, 2, 3, 4, 5]), replay: true });
    buffer.push({ data: new Uint8Array([6, 7]), replay: false });
    expect(buffer.push({ data: new Uint8Array([8]), replay: false })).toBe(false);
    expect(buffer.bytesHeld()).toBe(0);
    expect(buffer.drain()).toEqual([]);
  });
});
