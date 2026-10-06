import { describe, expect, it, vi } from "vitest";
import { applyScrollRestore } from "./ReviewPage";

/** A scroll container that clamps like a real one. */
function container(height: number) {
  let top = 0;
  return {
    get scrollTop() {
      return top;
    },
    set scrollTop(next: number) {
      top = Math.max(0, Math.min(next, height));
    },
  };
}

describe("restoring the reader's place", () => {
  it("reports success once the document is tall enough to hold the position", () => {
    const el = container(4000);
    expect(applyScrollRestore(el, 1800)).toBe(true);
    expect(el.scrollTop).toBe(1800);
  });

  // Markdown previews arrive after the diff, so the document is short at
  // first and the position is clamped. Reporting success there is what left
  // the reader at the top when they came back from the agent tab.
  it("reports failure while the document is still too short", () => {
    const el = container(200);
    expect(applyScrollRestore(el, 1800)).toBe(false);
    expect(el.scrollTop).toBe(200);
  });

  it("succeeds on a later attempt once the document has grown", () => {
    const short = container(200);
    expect(applyScrollRestore(short, 1800)).toBe(false);
    const grown = container(4000);
    expect(applyScrollRestore(grown, 1800)).toBe(true);
  });

  it("treats the top of a document as restored", () => {
    const el = container(0);
    expect(applyScrollRestore(el, 0)).toBe(true);
  });
});

import {
  aheadMarker,
  createDraftDebouncer,
  pendingSince,
  readerIsBehind,
  readingLabel,
  replyDraftKey,
} from "./ReviewPage";

describe("where the reader sits in a review's rounds", () => {
  // The first round is the one the old defaulting got wrong: it read a
  // reader who had never advanced as already standing on Rev 1.
  it("counts a reader who has never advanced as behind the first round", () => {
    expect(readerIsBehind(1, 0)).toBe(true);
  });

  it("settles once they have advanced onto it", () => {
    expect(readerIsBehind(1, 1)).toBe(false);
  });

  it("puts them behind again on the next round", () => {
    expect(readerIsBehind(2, 1)).toBe(true);
  });

  it("says nothing is waiting before the agent has produced a round", () => {
    expect(readerIsBehind(0, 0)).toBe(false);
  });

  it("names the tree handed over rather than a revision never seen", () => {
    expect(readingLabel(0)).toBe("reading Rev 1 as sent");
    expect(readingLabel(1)).toBe("reading Rev 1");
    expect(readingLabel(3)).toBe("reading Rev 3");
  });

  it("dates waiting files from what the reader actually did", () => {
    expect(pendingSince(0)).toBe("since the review opened");
    expect(pendingSince(2)).toBe("since you pinned");
  });
});

describe("what a thread says about work touching its file", () => {
  it("offers the advance to a reader who is behind", () => {
    expect(aheadMarker(true, 3, true)).toBe("advance");
  });

  // Advancing at the head pins the revision already pinned, so offering it
  // is a control that cannot do anything.
  it("states the fact instead of offering a no-op at the head", () => {
    expect(aheadMarker(true, 3, false)).toBe("current");
  });

  it("says nothing when the file did not change", () => {
    expect(aheadMarker(false, 3, true)).toBe("none");
    expect(aheadMarker(false, 3, false)).toBe("none");
  });

  it("says nothing before there is a revision to speak of", () => {
    expect(aheadMarker(true, 0, true)).toBe("none");
  });
});

describe("review thread reply drafts", () => {
  it("formats reply draft keys by thread id", () => {
    expect(replyDraftKey(1)).toBe("reply:1");
    expect(replyDraftKey(42)).toBe("reply:42");
  });

  it("debounces draft saves and cancels pending saves on clear", () => {
    vi.useFakeTimers();
    try {
      const saved: Array<[string, string]> = [];
      const debouncer = createDraftDebouncer(100, (key, body) => {
        saved.push([key, body]);
      });

      debouncer.save("reply:1", "first draft");
      expect(debouncer.hasPending("reply:1")).toBe(true);

      debouncer.clear("reply:1");
      expect(debouncer.hasPending("reply:1")).toBe(false);

      vi.advanceTimersByTime(200);
      expect(saved).toEqual([]);

      debouncer.save("reply:2", "second draft");
      expect(debouncer.hasPending("reply:2")).toBe(true);
      vi.advanceTimersByTime(200);
      expect(saved).toEqual([["reply:2", "second draft"]]);
      expect(debouncer.hasPending("reply:2")).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it("cancels all pending saves on cleanup", () => {
    vi.useFakeTimers();
    try {
      const saved: Array<[string, string]> = [];
      const debouncer = createDraftDebouncer(100, (key, body) => {
        saved.push([key, body]);
      });

      debouncer.save("reply:1", "one");
      debouncer.save("reply:2", "two");
      debouncer.cancelAll();

      vi.advanceTimersByTime(200);
      expect(saved).toEqual([]);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("review advance and viewer state transitions", () => {
  it("transitions a behind reader to current when advancing to latest rev", () => {
    const latestRev = 3;
    const initialPinned = 1;
    expect(readerIsBehind(latestRev, initialPinned)).toBe(true);
    expect(aheadMarker(true, latestRev, readerIsBehind(latestRev, initialPinned))).toBe("advance");

    const advancedPinned = latestRev;
    expect(readerIsBehind(latestRev, advancedPinned)).toBe(false);
    expect(aheadMarker(true, latestRev, readerIsBehind(latestRev, advancedPinned))).toBe("current");
  });
});
