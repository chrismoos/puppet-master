import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { ItemSchema, ItemStatus } from "../gen/pm/v1/pm_pb";
import { EMPTY_ITEM_FILTERS } from "./boardSearch";
import {
  completedGroupCollapsed,
  completedMatchingCount,
  preferNewerLiveItem,
  readCompletedCollapse,
  writeCompletedCollapse,
} from "./boardCompleted";

describe("completed Board history", () => {
  it("defaults collapsed and stores a per-bucket preference for this tab", () => {
    const values = new Map<string, string>();
    const storage = {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => { values.set(key, value); },
    };
    expect(readCompletedCollapse("1", storage)).toBe(true);
    writeCompletedCollapse("1", false, storage);
    expect(readCompletedCollapse("1", storage)).toBe(false);
    expect(readCompletedCollapse("2", storage)).toBe(true);
  });

  it("auto-reveals search, completed filters, metrics, and direct completed routes", () => {
    const done = create(ItemSchema, { status: ItemStatus.DONE });
    expect(completedGroupCollapsed(true, EMPTY_ITEM_FILTERS, undefined)).toBe(true);
    expect(completedGroupCollapsed(true, { ...EMPTY_ITEM_FILTERS, query: "history" }, undefined)).toBe(false);
    expect(completedGroupCollapsed(true, { ...EMPTY_ITEM_FILTERS, status: "dropped" }, undefined)).toBe(false);
    expect(completedGroupCollapsed(true, { ...EMPTY_ITEM_FILTERS, summary: "done_recently" }, undefined)).toBe(false);
    expect(completedGroupCollapsed(true, EMPTY_ITEM_FILTERS, done)).toBe(false);
  });

  it("derives collapsed counts from bounded status facets", () => {
    expect(completedMatchingCount({ bucketTotal: 1000, matchingTotal: 900, byStatus: { done: 850, dropped: 25, planned: 25 } })).toBe(875);
  });

  it("does not let a stale live snapshot leak a freshly completed query row", () => {
    const queryDone = create(ItemSchema, { status: ItemStatus.DONE, updatedAtUnixMs: 20n });
    const staleLive = create(ItemSchema, { status: ItemStatus.IN_PROGRESS, updatedAtUnixMs: 10n });
    const newerLive = create(ItemSchema, { status: ItemStatus.PLANNED, updatedAtUnixMs: 30n });
    expect(preferNewerLiveItem(queryDone, staleLive).status).toBe(ItemStatus.DONE);
    expect(preferNewerLiveItem(queryDone, newerLive).status).toBe(ItemStatus.PLANNED);
  });
});
