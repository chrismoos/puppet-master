import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { SessionSchema } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { parseRoute } from "@puppet-master/client-core/router";
import {
  canResumeSession,
  focusModeAfterLeaving,
  planTabId,
  removedTerminalIds,
  reviewSelectionWasRemoved,
  reviewHasUnseen,
  reviewTabId,
  reviewTabLabel,
  reviewTabTitle,
  selectedPlanId,
  selectedReviewId,
  sessionPaneTitle,
  selectionNeedsReset,
  sessionTabHref,
  shellTabLabel,
  terminalSelectionWasRemoved,
} from "./SessionPane";

describe("session pane title", () => {
  it("shows the summary in place of the headline", () => {
    expect(sessionPaneTitle(create(SessionSchema, {
      id: 4n,
      goal: "Moving auth to JWTs",
      headline: "Swapping cookie checks",
      summary: "Three of five handlers now read JWTs.",
    }))).toBe("Three of five handlers now read JWTs.");
  });

  it("falls back to the headline, then the display name", () => {
    expect(sessionPaneTitle(create(SessionSchema, {
      id: 4n,
      goal: "Moving auth to JWTs",
      headline: "Swapping cookie checks",
      summary: "  ",
    }))).toBe("Swapping cookie checks");
    expect(sessionPaneTitle(create(SessionSchema, { id: 4n, goal: "Moving auth to JWTs" }))).toBe("Moving auth to JWTs");
  });
});

describe("session resume availability", () => {
  it("offers resume for saved conversations and restart for empty sessions", () => {
    expect(canResumeSession(true, true, "task")).toBe(true);
    expect(canResumeSession(true, false, "")).toBe(true);
  });

  it("does not offer resume for live or missing non-empty conversations", () => {
    expect(canResumeSession(false, true, "task")).toBe(false);
    expect(canResumeSession(true, false, "task")).toBe(false);
  });
});

describe("terminal selection", () => {
  it("keeps a newly created terminal selected while its event is pending", () => {
    expect(terminalSelectionWasRemoved("9", new Set(), new Set())).toBe(false);
  });

  it("returns to the agent after an existing shell is removed", () => {
    expect(terminalSelectionWasRemoved("9", new Set(["9"]), new Set())).toBe(true);
    expect(terminalSelectionWasRemoved("agent", new Set(["9"]), new Set())).toBe(false);
  });

  it("resets an active pane when its session changes or its shell is removed", () => {
    expect(selectionNeedsReset(true, true, "agent", new Set(), new Set())).toBe(true);
    expect(selectionNeedsReset(true, false, "9", new Set(["9"]), new Set())).toBe(true);
    expect(selectionNeedsReset(true, false, "9", new Set(["9"]), new Set(["9"]))).toBe(false);
  });

  it("never resets a pane retained behind another page", () => {
    expect(selectionNeedsReset(false, true, "agent", new Set(), new Set())).toBe(false);
    expect(selectionNeedsReset(false, false, "9", new Set(["9"]), new Set())).toBe(false);
  });

  it("identifies removed shell layers for immediate disposal", () => {
    expect(removedTerminalIds(new Set(["7", "9"]), new Set(["9", "11"]))).toEqual(["7"]);
  });
});

describe("shell tab labels", () => {
  it("uses a live terminal title instead of the generic shell title", () => {
    expect(shellTabLabel(9n, "Shell", "cargo nextest run")).toBe("cargo nextest run");
  });

  it("truncates long titles and gives untitled shells an identity", () => {
    expect(shellTabLabel(9n, "Shell")).toBe("shell 9");
    const label = shellTabLabel(9n, "Shell", "a".repeat(80));
    expect([...label]).toHaveLength(32);
    expect(label.endsWith("…")).toBe(true);
  });
});

describe("review tabs", () => {
  it("keeps a review selection distinguishable from a terminal id", () => {
    expect(selectedReviewId(reviewTabId(3))).toBe("3");
    expect(selectedReviewId("agent")).toBeNull();
    // A terminal whose id happens to be numeric is not a review.
    expect(selectedReviewId("3")).toBeNull();
  });

  it("does not treat a review selection as a removed terminal", () => {
    expect(
      terminalSelectionWasRemoved(reviewTabId(3), new Set(["9"]), new Set()),
    ).toBe(false);
  });

  it("falls back when the selected review is gone", () => {
    expect(reviewSelectionWasRemoved(reviewTabId(3), new Set(["3"]))).toBe(false);
    expect(reviewSelectionWasRemoved(reviewTabId(3), new Set())).toBe(true);
    // A terminal selection is never a missing review.
    expect(reviewSelectionWasRemoved("agent", new Set())).toBe(false);
  });

});

describe("review tab naming", () => {
  it("shows the name an agent gave it", () => {
    expect(reviewTabLabel("session git field")).toBe("session git field");
  });

  it("shortens a name too long for a tab, keeping the front", () => {
    const label = reviewTabLabel("a very long review name that will not fit in a tab");
    expect(label.length).toBeLessThanOrEqual(22);
    expect(label.startsWith("a very long")).toBe(true);
    expect(label.endsWith("\u2026")).toBe(true);
  });

  it("falls back rather than showing an empty tab", () => {
    expect(reviewTabLabel("")).toBe("review");
    expect(reviewTabLabel("   ")).toBe("review");
  });
});

describe("review tab titles", () => {
  const counts = { draftCount: 1, openCount: 2, answeredCount: 3, resolvedCount: 4 };

  it("says whose turn the threads are on rather than what they are called", () => {
    expect(reviewTabTitle("api rewrite", counts, false)).toBe(
      "api rewrite — 1 unsent, 3 your turn, 2 with agent, 4 resolved",
    );
  });

  it("leads with unseen replies and drops the states a review has none of", () => {
    expect(
      reviewTabTitle("api rewrite", { ...counts, openCount: 0, resolvedCount: 0 }, true),
    ).toBe("api rewrite — unseen replies, 1 unsent, 3 your turn");
  });

  it("is the bare name while nothing is waiting on anyone", () => {
    expect(
      reviewTabTitle("api rewrite", { draftCount: 0, openCount: 0, answeredCount: 0, resolvedCount: 0 }, false),
    ).toBe("api rewrite");
  });
});

describe("the unseen mark", () => {
  it("marks a review holding a reply this reader has not reached", () => {
    expect(reviewHasUnseen({ threadLatestMessage: { "1": 9n } }, { "1": 8n })).toBe(true);
  });

  it("clears once every thread has been seen through its newest message", () => {
    expect(reviewHasUnseen({ threadLatestMessage: { "1": 9n, "2": 4n } }, { "1": 9n, "2": 4n })).toBe(false);
  });

  it("marks a thread never seen at all", () => {
    expect(reviewHasUnseen({ threadLatestMessage: { "1": 1n } }, {})).toBe(true);
    expect(reviewHasUnseen({ threadLatestMessage: { "1": 1n } }, undefined)).toBe(true);
  });

  it("stays clear on a review with no messages yet", () => {
    expect(reviewHasUnseen({ threadLatestMessage: {} }, {})).toBe(false);
  });

  it("does not un-mark when one thread is read and another is not", () => {
    expect(reviewHasUnseen({ threadLatestMessage: { "1": 9n, "2": 4n } }, { "1": 9n })).toBe(true);
  });
});

describe("tab focus mode transitions", () => {
  it("exits focus mode when leaving a review or plan tab", () => {
    expect(focusModeAfterLeaving("review:3", true)).toBe(false);
    expect(focusModeAfterLeaving("plan:5", true)).toBe(false);
  });

  it("preserves focus mode when switching between standard terminals", () => {
    expect(focusModeAfterLeaving("shell", true)).toBe(true);
    expect(focusModeAfterLeaving("agent", false)).toBe(false);
  });
});

describe("opening a review or plan in its own window", () => {
  const base = { pathname: "/", search: "" };

  it("addresses the tab as a document URL rather than a bare fragment", () => {
    expect(sessionTabHref(base, "7", reviewTabId(3)))
      .toBe("/#/session/7?tab=review%3A3&focus=1");
    expect(sessionTabHref(base, "7", planTabId(5)))
      .toBe("/#/session/7?tab=plan%3A5&focus=1");
  });

  it("keeps the address the app is served from, so a forward prefix survives", () => {
    expect(sessionTabHref({ pathname: "/forwards/12/", search: "" }, "7", reviewTabId(3)))
      .toBe("/forwards/12/#/session/7?tab=review%3A3&focus=1");
    expect(sessionTabHref({ pathname: "/", search: "?token=abc" }, "7", planTabId(5)))
      .toBe("/?token=abc#/session/7?tab=plan%3A5&focus=1");
  });

  it("round-trips through the router, so the window opens on the tab", () => {
    const href = sessionTabHref(base, "7", reviewTabId(3));
    const route = parseRoute(href.slice(href.indexOf("#")));
    expect(route).toEqual({ name: "session", id: "7", tab: "review:3", focus: true });
    expect(selectedReviewId(route.name === "session" ? route.tab! : "")).toBe("3");

    const planRoute = parseRoute(sessionTabHref(base, "7", planTabId(5)).slice(1));
    expect(planRoute.name === "session" && selectedPlanId(planRoute.tab!)).toBe("5");
  });
});

