import type { ItemSearchCounts } from "../api/items";

export interface BoardIndexStatusInput {
  shown: number;
  counts?: ItemSearchCounts;
  hasMore: boolean;
  filtered: boolean;
  loading: boolean;
  error: boolean;
  /** Matching rows intentionally represented only by a collapsed heading. */
  collapsed?: number;
}

/**
 * Formats the index's three deliberately separate quantities:
 * - shown: matching rows represented in the current expanded view;
 * - hidden: bucket rows excluded by the query or an explicit collapse policy;
 * - not loaded: matching, visible-policy rows beyond the fetched page.
 */
export function boardIndexStatus(input: BoardIndexStatusInput): string {
  if (input.loading) return "Finding issues…";
  if (input.error) return "Issue count unavailable";

  const noun = input.filtered ? "matches" : "issues";
  if (!input.counts) {
    return input.hasMore
      ? `${input.shown} shown · more available`
      : `${input.shown} ${noun}`;
  }

  const collapsed = Math.max(0, Math.min(input.collapsed ?? 0, input.counts.matchingTotal));
  const hidden = Math.max(0, input.counts.bucketTotal - input.counts.matchingTotal) + collapsed;
  const visibleMatches = Math.max(0, input.counts.matchingTotal - collapsed);
  const notLoaded = Math.max(0, visibleMatches - input.shown);

  if (notLoaded > 0) {
    const parts = [`${input.shown} shown`];
    if (hidden > 0) parts.push(`${hidden} hidden`);
    parts.push(`${notLoaded} not loaded`);
    return parts.join(" · ");
  }
  return hidden > 0
    ? `${input.shown} ${noun} · ${hidden} hidden`
    : `${input.shown} ${noun}`;
}
