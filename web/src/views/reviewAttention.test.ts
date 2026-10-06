import { describe, expect, it } from "vitest";
import {
  groupThreadsByFile,
  isOpen,
  nextThread,
  threadTurn,
  walkThreads,
  type AttentionThread,
  type WalkThread,
} from "./reviewAttention";

/** A thread the reader opened and the agent has not answered, which is
 * the shape every thread starts in. */
function thread(
  id: number,
  path: string,
  line: number,
  state: AttentionThread["state"],
  authors: AttentionThread["messages"][number]["author"][] = ["user"],
): AttentionThread {
  return {
    id,
    path,
    current_line: line,
    state,
    messages: authors.map((author) => ({ author })),
  };
}

/** The same thread after the agent has replied to it. */
function answered(id: number, path: string, line: number): AttentionThread {
  return thread(id, path, line, "answered", ["user", "session"]);
}

describe("isOpen", () => {
  it("counts everything nobody has closed", () => {
    // Waiting on the agent, answered and awaiting a close, and not yet
    // sent are all still open conversations from the reader's side.
    expect(isOpen(thread(1, "a.ts", 1, "sent"))).toBe(true);
    expect(isOpen(thread(2, "a.ts", 2, "answered"))).toBe(true);
    expect(isOpen(thread(3, "a.ts", 3, "draft"))).toBe(true);
    expect(isOpen(thread(4, "a.ts", 4, "resolved"))).toBe(false);
  });
});

describe("threadTurn", () => {
  it("waits on the reader once the agent has replied", () => {
    expect(threadTurn(answered(1, "a.ts", 1))).toBe("yours");
  });

  // One rule, and the reader's own opening is not an exception to it:
  // they spoke, so the agent owes the next word.
  it("waits on the agent while the reader's own sent words are the last ones", () => {
    expect(threadTurn(thread(2, "a.ts", 2, "sent"))).toBe("theirs");
    expect(threadTurn(thread(4, "a.ts", 4, "sent", ["user", "session", "user"]))).toBe("theirs");
  });

  it("waits on the reader while a draft has not been sent", () => {
    // The agent has never been handed it, so nothing is pending on the
    // agent: the move left is the reader's own, to send it.
    const draft = thread(3, "a.ts", 3, "draft");
    expect(threadTurn(draft)).toBe("yours");
    expect(threadTurn(draft)).not.toBe("theirs");
  });

  it("takes a closed thread out of the question", () => {
    expect(threadTurn(thread(5, "a.ts", 5, "resolved", ["user", "session"]))).toBe("resolved");
  });
});

describe("groupThreadsByFile", () => {
  const threads = [
    thread(3, "b.ts", 5, "resolved"),
    thread(1, "a.ts", 9, "sent"),
    thread(2, "a.ts", 2, "resolved"),
    answered(4, "a.ts", 4),
  ];

  it("splits each file by whose turn it is, in line order", () => {
    const byFile = groupThreadsByFile(threads);
    expect(byFile.get("a.ts")).toEqual({
      open: [4, 1],
      yours: [4],
      theirs: [1],
      resolved: [2],
    });
    expect(byFile.get("b.ts")).toEqual({
      open: [],
      yours: [],
      theirs: [],
      resolved: [3],
    });
  });

  // The whole-review walk and the resolve-and-carry-on step move through
  // open threads whoever they wait on, so the union stays exactly what it
  // was before the split.
  it("keeps open as everything nobody has closed", () => {
    const file = groupThreadsByFile(threads).get("a.ts")!;
    expect([...file.open].sort()).toEqual([...file.yours, ...file.theirs].sort());
  });

  it("leaves out a file with no threads", () => {
    expect(groupThreadsByFile(threads).has("c.ts")).toBe(false);
  });

  it("keeps an unsent draft out of the threads waiting on the agent", () => {
    const file = groupThreadsByFile([...threads, thread(5, "a.ts", 1, "draft")]).get("a.ts")!;
    expect(file.yours).toContain(5);
    expect(file.theirs).not.toContain(5);
  });
});

/**
 * The rail's circles and the head's pills count the same threads, and a
 * reader comparing them will notice the moment they disagree. The head
 * counts by state, one pill per state, exactly as the daemon does. The
 * rail counts by turn, folding the drafts in with what the reader owes,
 * so every thread lands in exactly one circle and the totals reconcile.
 */
describe("the rail's circles and the head's pills", () => {
  function pillCounts(threads: readonly AttentionThread[]) {
    const pills = { draft: 0, sent: 0, answered: 0, resolved: 0 };
    for (const t of threads) pills[t.state] += 1;
    return pills;
  }

  function dotCounts(threads: readonly AttentionThread[]) {
    const dots = { yours: 0, theirs: 0, resolved: 0 };
    for (const file of groupThreadsByFile(threads).values()) {
      dots.yours += file.yours.length;
      dots.theirs += file.theirs.length;
      dots.resolved += file.resolved.length;
    }
    return dots;
  }

  const sets: Record<string, AttentionThread[]> = {
    "one of each": [
      thread(1, "a.ts", 1, "draft"),
      thread(2, "a.ts", 2, "sent"),
      answered(3, "a.ts", 3),
      thread(4, "a.ts", 4, "resolved"),
    ],
    "spread across files": [
      thread(1, "a.ts", 9, "sent"),
      answered(2, "a.ts", 4),
      answered(3, "a.ts", 1),
      thread(4, "b.ts", 3, "draft"),
      thread(5, "c.ts", 2, "resolved"),
      thread(6, "c.ts", 8, "resolved"),
    ],
    "nothing but drafts": [
      thread(1, "a.ts", 1, "draft"),
      thread(2, "b.ts", 1, "draft"),
    ],
    "nothing at all": [],
  };

  for (const [name, threads] of Object.entries(sets)) {
    it(`reconciles with ${name}`, () => {
      const pills = pillCounts(threads);
      const dots = dotCounts(threads);
      // A draft is the reader's move to make, so it is counted amber
      // beside the answers waiting on them rather than blue.
      expect(dots.yours).toBe(pills.answered + pills.draft);
      expect(dots.theirs).toBe(pills.sent);
      expect(dots.resolved).toBe(pills.resolved);
      expect(dots.yours + dots.theirs + dots.resolved).toBe(threads.length);
    });
  }
});

describe("walkThreads", () => {
  const files = ["a.ts", "b.ts"];
  const threads = [
    thread(1, "b.ts", 1, "sent"),
    thread(2, "a.ts", 7, "sent"),
    thread(3, "a.ts", 3, "resolved"),
  ];

  it("walks down the file list, then down each file", () => {
    expect(walkThreads(files, threads, "open").map((t) => t.id)).toEqual([2, 1]);
  });

  it("walks each group separately", () => {
    expect(walkThreads(files, threads, "resolved").map((t) => t.id)).toEqual([3]);
  });

  it("walks one turn's threads without the other's", () => {
    const mixed = [...threads, answered(4, "a.ts", 1)];
    expect(walkThreads(files, mixed, "yours").map((t) => t.id)).toEqual([4]);
    expect(walkThreads(files, mixed, "theirs").map((t) => t.id)).toEqual([2, 1]);
    expect(walkThreads(files, mixed, "open").map((t) => t.id)).toEqual([4, 2, 1]);
  });

  it("leaves out a thread on a path the review does not list", () => {
    // Otherwise a circle and its walk disagree about what exists.
    const stray = [...threads, thread(9, "gone.ts", 1, "sent")];
    expect(walkThreads(files, stray, "open").map((t) => t.id)).toEqual([2, 1]);
  });
});

describe("nextThread", () => {
  const order: WalkThread[] = [
    { id: 1, path: "a.ts", line: 2, fileIndex: 0 },
    { id: 2, path: "a.ts", line: 8, fileIndex: 0 },
    { id: 3, path: "b.ts", line: 1, fileIndex: 1 },
  ];

  it("starts at the top and advances on each further click", () => {
    const first = nextThread(order, null);
    expect(first?.id).toBe(1);
    expect(nextThread(order, first!)?.id).toBe(2);
  });

  it("wraps rather than stopping at the end", () => {
    // A circle that goes dead on the last thread reads as broken.
    expect(nextThread(order, order[2])?.id).toBe(1);
  });

  it("steps backwards and wraps the other way", () => {
    expect(nextThread(order, order[1], -1)?.id).toBe(1);
    expect(nextThread(order, order[0], -1)?.id).toBe(3);
  });

  it("carries position, not an index, so a thread leaving does not reset the walk", () => {
    // The reader was on thread 2; it is resolved and gone from the walk.
    const without = order.filter((t) => t.id !== 2);
    expect(nextThread(without, order[1])?.id).toBe(3);
  });

  it("has nowhere to go when the group is empty", () => {
    expect(nextThread([], null)).toBeNull();
  });
});
