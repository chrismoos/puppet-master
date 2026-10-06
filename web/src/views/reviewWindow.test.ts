import { describe, expect, it } from "vitest";
import { parseRoute } from "@puppet-master/client-core/router";
import { isReviewEntryRoute, opensOwnWindow, ownsItsWindow, reviewEntry } from "./reviewWindow";

const click = (over: Partial<Parameters<typeof opensOwnWindow>[0]> = {}) => ({
  button: 0,
  metaKey: false,
  ctrlKey: false,
  shiftKey: false,
  altKey: false,
  defaultPrevented: false,
  ...over,
});

describe("the window a review owns", () => {
  it("recognises an address that shows a review and nothing else", () => {
    expect(isReviewEntryRoute(parseRoute("#/session/5?tab=review:12"))).toBe(true);
    expect(isReviewEntryRoute(parseRoute("#/review/12"))).toBe(true);
  });

  it("leaves the dashboard alone, so finishing never closes the window a reader still needs", () => {
    for (const hash of ["#/", "#/session/5", "#/session/5?tab=agent", "#/session/5?tab=plan:3", "#/board/1"]) {
      expect(isReviewEntryRoute(parseRoute(hash)), hash).toBe(false);
    }
  });
});

describe("whether a review owns its window", () => {
  const opener = {};

  it("owns a window another window opened on it", () => {
    expect(ownsItsWindow(parseRoute("#/session/5?tab=review:12"), opener)).toBe(true);
    expect(ownsItsWindow(parseRoute("#/review/12"), opener)).toBe(true);
  });

  it("leaves a window the reader opened themselves, which finishing must not close", () => {
    expect(ownsItsWindow(parseRoute("#/review/12"), null)).toBe(false);
    expect(ownsItsWindow(parseRoute("#/session/5?tab=review:12"), undefined)).toBe(false);
  });

  it("leaves a window that opened on anything else, opener or not", () => {
    expect(ownsItsWindow(parseRoute("#/session/5"), opener)).toBe(false);
    expect(ownsItsWindow(parseRoute("#/session/5?tab=plan:3"), opener)).toBe(false);
    expect(ownsItsWindow(parseRoute("#/"), opener)).toBe(false);
  });
});

describe("taking over a review tab's click", () => {
  it("takes a plain primary click, which is the one that needs a closable window", () => {
    expect(opensOwnWindow(click())).toBe(true);
  });

  it("leaves every click the browser already opens a window for", () => {
    expect(opensOwnWindow(click({ button: 1 })), "middle").toBe(false);
    expect(opensOwnWindow(click({ metaKey: true })), "meta").toBe(false);
    expect(opensOwnWindow(click({ ctrlKey: true })), "ctrl").toBe(false);
    expect(opensOwnWindow(click({ shiftKey: true })), "shift").toBe(false);
    expect(opensOwnWindow(click({ altKey: true })), "alt").toBe(false);
  });

  it("stands aside once something else has handled the click", () => {
    expect(opensOwnWindow(click({ defaultPrevented: true }))).toBe(false);
  });
});


describe("fullscreen review routes", () => {
  it("preserves a direct review's reading location", () => {
    const route = parseRoute("#/review/12?view=changes&file=a.txt&thread=4");
    expect(reviewEntry(route)).toEqual(route);
  });

  it("selects the review component from a session review tab", () => {
    expect(reviewEntry(parseRoute("#/session/5?tab=review%3A12&focus=1")))
      .toEqual({ name: "review", id: 12 });
  });

  it("does not treat invalid review ids or ordinary tabs as reviews", () => {
    for (const tab of ["review:", "review:0", "review:abc", "review:9007199254740992", "agent", "plan:4"]) {
      expect(reviewEntry(parseRoute(`#/session/5?tab=${encodeURIComponent(tab)}`))).toBeNull();
    }
  });
});
