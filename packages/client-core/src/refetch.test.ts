import { describe, expect, it } from "vitest";
import { coalescedRefetch } from "./refetch";

function deferredFetch() {
  const pending: Array<() => void> = [];
  let calls = 0;
  const fetch = () => {
    calls += 1;
    return new Promise<void>((resolve) => pending.push(resolve));
  };
  return { fetch, pending, calls: () => calls };
}

describe("coalescedRefetch", () => {
  it("runs once more when called during a fetch", async () => {
    const source = deferredFetch();
    const refetch = coalescedRefetch(source.fetch);
    const first = refetch();
    void refetch();
    void refetch();
    expect(source.calls()).toBe(1);
    source.pending.shift()!();
    await Promise.resolve();
    await Promise.resolve();
    expect(source.calls()).toBe(2);
    source.pending.shift()!();
    await first;
    expect(source.calls()).toBe(2);
  });

  it("starts a fresh fetch once the last one finished", async () => {
    const source = deferredFetch();
    const refetch = coalescedRefetch(source.fetch);
    const first = refetch();
    source.pending.shift()!();
    await first;
    void refetch();
    expect(source.calls()).toBe(2);
  });

  it("keeps working after a fetch fails", async () => {
    let calls = 0;
    const refetch = coalescedRefetch(async () => {
      calls += 1;
      if (calls === 1) throw new Error("offline");
    });
    await expect(refetch()).rejects.toThrow("offline");
    await refetch();
    expect(calls).toBe(2);
  });
});
