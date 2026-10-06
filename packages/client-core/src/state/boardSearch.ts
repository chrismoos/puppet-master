import type { ItemSearchFilters } from "../api/items";

export const EMPTY_ITEM_FILTERS: ItemSearchFilters = {
  query: "", project: "", status: "", priority: "", source: "",
  includeDone: true, includeSnoozed: false,
  summary: "",
};

export function parseBoardSearch(hash: string): ItemSearchFilters {
  const query = new URLSearchParams(hash.split("?", 2)[1] ?? "");
  return {
    query: query.get("q") ?? "",
    project: query.get("project") ?? "",
    status: query.get("status") ?? "",
    priority: query.get("priority") ?? "",
    source: query.get("source") ?? "",
    // Completed history is part of the default dataset. `done=0` is retained
    // as an explicit opt-out so old clean/default links gain the new behavior.
    includeDone: query.get("done") !== "0",
    includeSnoozed: query.get("snoozed") === "1",
    summary: query.get("summary") ?? "",
  };
}

export function boardHash(bucketId: string, filters: ItemSearchFilters): string {
  const query = new URLSearchParams();
  if (filters.query) query.set("q", filters.query);
  if (filters.project) query.set("project", filters.project);
  if (filters.status) query.set("status", filters.status);
  if (filters.priority) query.set("priority", filters.priority);
  if (filters.source) query.set("source", filters.source);
  if (!filters.includeDone) query.set("done", "0");
  if (filters.includeSnoozed) query.set("snoozed", "1");
  if (filters.summary) query.set("summary", filters.summary);
  const suffix = query.toString();
  return `#/bucket/${bucketId}/board${suffix ? `?${suffix}` : ""}`;
}

export function itemFiltersEqual(a: ItemSearchFilters, b: ItemSearchFilters): boolean {
  return (Object.keys(EMPTY_ITEM_FILTERS) as Array<keyof ItemSearchFilters>)
    .every((key) => a[key] === b[key]);
}

export function hasItemFilters(filters: ItemSearchFilters): boolean {
  return Object.entries(filters).some(([key, value]) =>
    key === "includeDone" ? value !== true : value !== "" && value !== false,
  );
}
