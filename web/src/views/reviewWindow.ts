import type { Route } from "@puppet-master/client-core/router";

export const REVIEW_TAB_PREFIX = "review:";

export function reviewEntry(route: Route): Extract<Route, { name: "review" }> | null {
  if (route.name === "review") return route;
  if (route.name !== "session") return null;
  const match = route.tab?.match(/^review:(\d+)$/);
  const id = Number(match?.[1]);
  return Number.isSafeInteger(id) && id > 0 ? { name: "review", id } : null;
}

export function isReviewEntryRoute(route: Route): boolean {
  return reviewEntry(route) !== null;
}

export function ownsItsWindow(route: Route, opener: unknown): boolean {
  return opener !== null && opener !== undefined && isReviewEntryRoute(route);
}

export function opensOwnWindow(event: {
  button: number;
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  defaultPrevented: boolean;
}): boolean {
  return (
    event.button === 0 &&
    !event.defaultPrevented &&
    !event.metaKey &&
    !event.ctrlKey &&
    !event.shiftKey &&
    !event.altKey
  );
}
