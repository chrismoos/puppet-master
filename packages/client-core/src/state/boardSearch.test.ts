import { describe, expect, it } from "vitest";
import { boardHash, EMPTY_ITEM_FILTERS, hasItemFilters, itemFiltersEqual, parseBoardSearch } from "./boardSearch";

describe("board search URL state", () => {
  it("round-trips every server filter and safely decodes search text", () => {
    const filters = { ...EMPTY_ITEM_FILTERS, query: "auth & url", project: "9", status: "blocked", priority: "high", source: "github", includeDone: true, includeSnoozed: true, summary: "needs_you" };
    const hash = boardHash("2", filters);
    expect(parseBoardSearch(hash)).toEqual(filters);
    expect(hash).toContain("q=auth+%26+url");
    expect(hash).toContain("summary=needs_you");
  });

  it("uses a clean route for the default view", () => {
    expect(boardHash("2", EMPTY_ITEM_FILTERS)).toBe("#/bucket/2/board");
    expect(hasItemFilters(EMPTY_ITEM_FILTERS)).toBe(false);
  });

  it("compares filters by value so re-parsing the same route is not a change", () => {
    const hash = boardHash("2", { ...EMPTY_ITEM_FILTERS, query: "auth", includeSnoozed: true });
    expect(itemFiltersEqual(parseBoardSearch(hash), parseBoardSearch(hash))).toBe(true);
    expect(itemFiltersEqual(EMPTY_ITEM_FILTERS, { ...EMPTY_ITEM_FILTERS })).toBe(true);
    expect(itemFiltersEqual(EMPTY_ITEM_FILTERS, { ...EMPTY_ITEM_FILTERS, includeDone: false })).toBe(false);
    expect(itemFiltersEqual(EMPTY_ITEM_FILTERS, { ...EMPTY_ITEM_FILTERS, project: "3" })).toBe(false);
  });

  it("distinguishes the default completed history from an explicit exclusion", () => {
    expect(parseBoardSearch("#/bucket/2/board").includeDone).toBe(true);
    const excluded = { ...EMPTY_ITEM_FILTERS, includeDone: false };
    expect(boardHash("2", excluded)).toBe("#/bucket/2/board?done=0");
    expect(parseBoardSearch(boardHash("2", excluded))).toEqual(excluded);
    expect(hasItemFilters(excluded)).toBe(true);
  });
});
