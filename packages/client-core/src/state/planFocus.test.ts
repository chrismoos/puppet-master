import { describe, expect, it } from "vitest";

import type { KeyValueStorage } from "../platform";
import {
  PLAN_FOCUS_KEY,
  PLAN_FOCUS_LIMIT,
  readFocusedDecision,
  resolveFocusedDecision,
  writeFocusedDecision,
} from "./planFocus";

function memoryStorage(initial: Record<string, string> = {}): KeyValueStorage & { values: Map<string, string> } {
  const values = new Map(Object.entries(initial));
  return {
    values,
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => { values.set(key, value); },
  };
}

describe("remembered plan focus", () => {
  it("reads back the decision written for a plan and nothing for others", () => {
    const storage = memoryStorage();
    writeFocusedDecision(storage, "plan-a", 7);
    writeFocusedDecision(storage, "plan-b", 3);
    expect(readFocusedDecision(storage, "plan-a")).toBe(7);
    expect(readFocusedDecision(storage, "plan-b")).toBe(3);
    expect(readFocusedDecision(storage, "plan-c")).toBeNull();
  });

  it("overwrites the entry for a plan instead of keeping both", () => {
    const storage = memoryStorage();
    writeFocusedDecision(storage, "plan-a", 7);
    writeFocusedDecision(storage, "plan-a", 8);
    expect(readFocusedDecision(storage, "plan-a")).toBe(8);
    expect(Object.keys(JSON.parse(storage.values.get(PLAN_FOCUS_KEY)!))).toEqual(["plan-a"]);
  });

  it("drops the least recently written plans past the limit", () => {
    const storage = memoryStorage();
    for (let i = 0; i <= PLAN_FOCUS_LIMIT; i++) writeFocusedDecision(storage, `plan-${i}`, i);
    expect(readFocusedDecision(storage, "plan-0")).toBeNull();
    expect(readFocusedDecision(storage, "plan-1")).toBe(1);
    expect(readFocusedDecision(storage, `plan-${PLAN_FOCUS_LIMIT}`)).toBe(PLAN_FOCUS_LIMIT);
  });

  it("re-writing a plan makes it the most recent again", () => {
    const storage = memoryStorage();
    for (let i = 0; i < PLAN_FOCUS_LIMIT; i++) writeFocusedDecision(storage, `plan-${i}`, i);
    writeFocusedDecision(storage, "plan-0", 99);
    writeFocusedDecision(storage, "plan-new", 1);
    expect(readFocusedDecision(storage, "plan-0")).toBe(99);
    expect(readFocusedDecision(storage, "plan-1")).toBeNull();
  });

  it("treats unreadable or malformed storage as empty", () => {
    expect(readFocusedDecision(memoryStorage({ [PLAN_FOCUS_KEY]: "not json" }), "plan-a")).toBeNull();
    expect(readFocusedDecision(memoryStorage({ [PLAN_FOCUS_KEY]: "[1,2]" }), "plan-a")).toBeNull();
    expect(readFocusedDecision(memoryStorage({ [PLAN_FOCUS_KEY]: '{"plan-a":"7"}' }), "plan-a")).toBeNull();
    const storage = memoryStorage({ [PLAN_FOCUS_KEY]: "not json" });
    writeFocusedDecision(storage, "plan-a", 2);
    expect(readFocusedDecision(storage, "plan-a")).toBe(2);
  });
});

describe("resolveFocusedDecision", () => {
  const ids = [10, 11, 12];

  it("keeps the current decision while it is still active", () => {
    expect(resolveFocusedDecision(ids, 11, 12)).toBe(11);
  });

  it("falls back to the remembered decision when nothing is focused yet", () => {
    expect(resolveFocusedDecision(ids, null, 12)).toBe(12);
  });

  it("starts at the first decision when neither is part of the batch", () => {
    expect(resolveFocusedDecision(ids, 4, 5)).toBe(10);
    expect(resolveFocusedDecision(ids, null, null)).toBe(10);
  });

  it("returns null for an empty batch", () => {
    expect(resolveFocusedDecision([], 4, 5)).toBeNull();
  });
});
