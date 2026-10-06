import { describe, expect, it } from "vitest";
import { boardIndexStatus, type BoardIndexStatusInput } from "./boardIndexStatus";

const base: BoardIndexStatusInput = {
  shown: 9,
  counts: { bucketTotal: 9, matchingTotal: 9, byStatus: { planned: 9 } },
  hasMore: false,
  filtered: false,
  loading: false,
  error: false,
};

describe("boardIndexStatus", () => {
  it("labels empty and one-page views without implying pagination", () => {
    expect(boardIndexStatus({ ...base, shown: 0, counts: { bucketTotal: 0, matchingTotal: 0, byStatus: {} } })).toBe("0 issues");
    expect(boardIndexStatus(base)).toBe("9 issues");
  });

  it("keeps query-hidden and not-yet-fetched counts separate", () => {
    expect(boardIndexStatus({ ...base, shown: 50, counts: { bucketTotal: 194, matchingTotal: 59, byStatus: { planned: 59 } }, hasMore: true }))
      .toBe("50 shown · 135 hidden · 9 not loaded");
  });

  it("uses matches for active filters and search", () => {
    expect(boardIndexStatus({ ...base, filtered: true })).toBe("9 matches");
    expect(boardIndexStatus({ ...base, filtered: true, shown: 0, counts: { bucketTotal: 144, matchingTotal: 0, byStatus: {} } }))
      .toBe("0 matches · 144 hidden");
  });

  it("counts collapsed completed matches as hidden for item 63", () => {
    expect(boardIndexStatus({
      ...base,
      shown: 7,
      collapsed: 2,
      counts: { bucketTotal: 12, matchingTotal: 9, byStatus: { planned: 7, done: 2 } },
    })).toBe("7 issues · 5 hidden");
    expect(boardIndexStatus({
      ...base,
      shown: 7,
      collapsed: 2,
      hasMore: true,
      counts: { bucketTotal: 9, matchingTotal: 9, byStatus: { planned: 7, done: 2 } },
    })).toBe("7 issues · 2 hidden");
  });

  it("falls back honestly when aggregate totals are unavailable", () => {
    expect(boardIndexStatus({ ...base, counts: undefined, shown: 50, hasMore: true })).toBe("50 shown · more available");
    expect(boardIndexStatus({ ...base, counts: undefined, filtered: true })).toBe("9 matches");
  });

  it("uses consistent loading and error language", () => {
    expect(boardIndexStatus({ ...base, loading: true })).toBe("Finding issues…");
    expect(boardIndexStatus({ ...base, error: true })).toBe("Issue count unavailable");
  });
});
