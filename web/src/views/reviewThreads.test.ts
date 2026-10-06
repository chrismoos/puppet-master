import { describe, expect, it } from "vitest";
import {
  anchorKey,
  emptyFileThreads,
  newThreadSliceCache,
  sliceThreadsByFile,
  strayThreads,
  type AnchoredThread,
} from "./reviewThreads";

interface Thread extends AnchoredThread {
  body: string;
}

const thread = (
  id: number,
  path: string,
  line: number,
  body = "said",
  side: "left" | "right" = "right",
): Thread => ({ id, path, current_line: line, side, body });

/** The review payload is re-fetched and re-parsed, so a reload hands back
 * threads that are equal but never the same objects. */
const reparsed = (threads: Thread[]): Thread[] =>
  JSON.parse(JSON.stringify(threads)) as Thread[];

describe("sliceThreadsByFile", () => {
  it("puts each thread under its own file", () => {
    const sliced = sliceThreadsByFile(
      [thread(1, "a.ts", 4), thread(2, "b.ts", 9), thread(3, "a.ts", 1)],
      newThreadSliceCache<Thread>(),
    );
    expect([...sliced.keys()].sort()).toEqual(["a.ts", "b.ts"]);
    expect(sliced.get("a.ts")!.threads.map((t) => t.id)).toEqual([3, 1]);
    expect(sliced.get("b.ts")!.threads.map((t) => t.id)).toEqual([2]);
  });

  it("keeps a file's slice identical when the payload is re-parsed unchanged", () => {
    const cache = newThreadSliceCache<Thread>();
    const threads = [thread(1, "a.ts", 4), thread(2, "b.ts", 9)];
    const first = sliceThreadsByFile(threads, cache);
    const second = sliceThreadsByFile(reparsed(threads), cache);

    expect(second.get("a.ts")).toBe(first.get("a.ts"));
    expect(second.get("a.ts")!.threads).toBe(first.get("a.ts")!.threads);
    expect(second.get("a.ts")!.byAnchor).toBe(first.get("a.ts")!.byAnchor);
    expect(second.get("b.ts")).toBe(first.get("b.ts"));
  });

  it("gives only the changed file a new slice", () => {
    const cache = newThreadSliceCache<Thread>();
    const threads = [thread(1, "a.ts", 4), thread(2, "b.ts", 9)];
    const first = sliceThreadsByFile(threads, cache);
    const second = sliceThreadsByFile(
      [thread(1, "a.ts", 4), thread(2, "b.ts", 9, "answered")],
      cache,
    );

    expect(second.get("a.ts")).toBe(first.get("a.ts"));
    expect(second.get("b.ts")).not.toBe(first.get("b.ts"));
    expect(second.get("b.ts")!.threads[0].body).toBe("answered");
  });

  it("notices a thread that moved to a different line", () => {
    const cache = newThreadSliceCache<Thread>();
    const first = sliceThreadsByFile([thread(1, "a.ts", 4)], cache);
    const second = sliceThreadsByFile([thread(1, "a.ts", 7)], cache);
    expect(second.get("a.ts")).not.toBe(first.get("a.ts"));
    expect(second.get("a.ts")!.byAnchor.has(anchorKey("a.ts", 7, "right"))).toBe(true);
  });

  it("groups threads on one anchor and separates the two sides", () => {
    const sliced = sliceThreadsByFile(
      [
        thread(1, "a.ts", 4),
        thread(2, "a.ts", 4),
        thread(3, "a.ts", 4, "said", "left"),
      ],
      newThreadSliceCache<Thread>(),
    );
    const anchors = sliced.get("a.ts")!.byAnchor;
    expect(anchors.get(anchorKey("a.ts", 4, "right"))!.map((t) => t.id)).toEqual([1, 2]);
    expect(anchors.get(anchorKey("a.ts", 4, "left"))!.map((t) => t.id)).toEqual([3]);
  });

  it("reports the ids a file owns, so a reply can be scoped to it", () => {
    const sliced = sliceThreadsByFile(
      [thread(1, "a.ts", 4), thread(2, "b.ts", 9)],
      newThreadSliceCache<Thread>(),
    );
    expect(sliced.get("a.ts")!.ids.has(1)).toBe(true);
    expect(sliced.get("a.ts")!.ids.has(2)).toBe(false);
  });

  it("forgets a file whose last thread was deleted", () => {
    const cache = newThreadSliceCache<Thread>();
    sliceThreadsByFile([thread(1, "a.ts", 4), thread(2, "b.ts", 9)], cache);
    const second = sliceThreadsByFile([thread(2, "b.ts", 9)], cache);
    expect(second.has("a.ts")).toBe(false);
    expect(cache.has("a.ts")).toBe(false);
  });

  it("hands every file without threads the same empty slice", () => {
    expect(emptyFileThreads()).toBe(emptyFileThreads());
    expect(emptyFileThreads().threads).toHaveLength(0);
  });
});

describe("strayThreads", () => {
  const rows = [
    { oldLine: 1, newLine: 1 },
    { oldLine: null, newLine: 2 },
    { oldLine: 3, newLine: null },
  ];

  it("finds nothing while every thread sits on a rendered line", () => {
    const shown = [thread(1, "a.ts", 1), thread(2, "a.ts", 2)];
    expect(strayThreads(rows, "a.ts", shown)).toEqual([]);
  });

  it("returns the thread whose line no row carries", () => {
    const lost = thread(3, "a.ts", 9);
    expect(strayThreads(rows, "a.ts", [thread(1, "a.ts", 1), lost])).toEqual([lost]);
  });

  it("keeps the two sides apart, since a line number means a different line on each", () => {
    // Line 2 is rendered on the right only, line 3 on the left only.
    const onLeft = thread(4, "a.ts", 2, "said", "left");
    const onRight = thread(5, "a.ts", 3);
    expect(strayThreads(rows, "a.ts", [onLeft, onRight])).toEqual([onLeft, onRight]);
    expect(strayThreads(rows, "a.ts", [thread(6, "a.ts", 3, "said", "left")])).toEqual([]);
  });

  it("treats all threads as strays on a deleted file with no rows", () => {
    const threadOnDeleted = thread(7, "deleted.ts", 5);
    expect(strayThreads([], "deleted.ts", [threadOnDeleted])).toEqual([threadOnDeleted]);
  });
});
