import { ItemStatus, type Item } from "../gen/pm/v1/pm_pb";
import type { ItemSearchCounts, ItemSearchFilters } from "../api/items";
import type { KeyValueStorage } from "../platform";

const STORAGE_PREFIX = "pm.board.completedCollapsed.";

export function isCompletedItem(item: Item): boolean {
  return item.status === ItemStatus.DONE || item.status === ItemStatus.DROPPED;
}

/** Keep query membership authoritative unless the live snapshot is strictly newer. */
export function preferNewerLiveItem(queryItem: Item, liveItem: Item | undefined): Item {
  return liveItem && liveItem.updatedAtUnixMs > queryItem.updatedAtUnixMs ? liveItem : queryItem;
}

export function completedMatchingCount(counts?: ItemSearchCounts): number {
  return (counts?.byStatus.done ?? 0) + (counts?.byStatus.dropped ?? 0);
}

/** Search and explicit completed-only views reveal results without overwriting preference. */
export function completedGroupCollapsed(
  preference: boolean,
  filters: ItemSearchFilters,
  directItem: Item | undefined,
): boolean {
  if (!preference) return false;
  if (filters.query.trim()) return false;
  if (filters.status === "done" || filters.status === "dropped") return false;
  if (filters.summary === "done_recently") return false;
  if (directItem && isCompletedItem(directItem)) return false;
  return true;
}

/** The preference lasts for the platform storage's lifetime, per bucket. */
export function readCompletedCollapse(bucketId: string, storage: KeyValueStorage): boolean {
  try {
    return storage.getItem(`${STORAGE_PREFIX}${bucketId}`) !== "0";
  } catch {
    return true;
  }
}

export function writeCompletedCollapse(bucketId: string, collapsed: boolean, storage: KeyValueStorage): void {
  try {
    storage.setItem(`${STORAGE_PREFIX}${bucketId}`, collapsed ? "1" : "0");
  } catch {
    // A blocked storage API degrades to the default collapsed behavior.
  }
}
