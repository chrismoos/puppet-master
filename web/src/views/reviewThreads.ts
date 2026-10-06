/** The shape a file section needs from a thread to place and address it. */
export interface AnchoredThread {
  id: number;
  path: string;
  current_line: number;
  side: "left" | "right";
}

/** One file's threads, the anchors they sit on, and the ids they own. */
export interface FileThreads<T extends AnchoredThread> {
  threads: T[];
  byAnchor: Map<string, T[]>;
  ids: Set<number>;
}

export type ThreadSliceCache<T extends AnchoredThread> = Map<
  string,
  { key: string; value: FileThreads<T> }
>;

export function anchorKey(path: string, line: number, side: "left" | "right"): string {
  return `${path}:${line}:${side}`;
}

/** The line numbers a rendered row carries, which is all `strayThreads`
 * needs to know about a diff row. */
export interface AnchorRow {
  oldLine: number | null;
  newLine: number | null;
}

/**
 * Threads no rendered row can host.
 *
 * A comment outlives the change it asked for: once the edit lands, the
 * line it was written against may not be in the view at all. Its thread
 * is still the reviewer's own words, so a file section shows these
 * rather than dropping them along with their anchor.
 */
export function strayThreads<T extends AnchoredThread>(
  rows: readonly AnchorRow[],
  path: string,
  threads: readonly T[],
): T[] {
  const shown = new Set<string>();
  for (const row of rows) {
    if (row.newLine !== null) shown.add(anchorKey(path, row.newLine, "right"));
    if (row.oldLine !== null) shown.add(anchorKey(path, row.oldLine, "left"));
  }
  return threads.filter((t) => !shown.has(anchorKey(t.path, t.current_line, t.side)));
}

export function newThreadSliceCache<T extends AnchoredThread>(): ThreadSliceCache<T> {
  return new Map();
}

/**
 * Splits threads per file, reusing the previous slice for any file whose
 * threads are unchanged.
 *
 * The review payload is replaced whenever the reader's own state is saved,
 * so every thread arrives as a new object even when nothing about it moved.
 * Handing a file section a slice with a new identity on every save is what
 * made marking one file viewed re-render the lines of all the others, so
 * identity here is decided by value rather than by the payload it came from.
 */
export function sliceThreadsByFile<T extends AnchoredThread>(
  threads: readonly T[],
  cache: ThreadSliceCache<T>,
): Map<string, FileThreads<T>> {
  const grouped = new Map<string, T[]>();
  for (const thread of threads) {
    const held = grouped.get(thread.path);
    if (held) held.push(thread);
    else grouped.set(thread.path, [thread]);
  }

  const sliced = new Map<string, FileThreads<T>>();
  for (const [path, list] of grouped) {
    list.sort((a, b) => a.current_line - b.current_line);
    const key = JSON.stringify(list);
    const held = cache.get(path);
    if (held?.key === key) {
      sliced.set(path, held.value);
      continue;
    }
    const byAnchor = new Map<string, T[]>();
    for (const thread of list) {
      const anchor = anchorKey(thread.path, thread.current_line, thread.side);
      byAnchor.set(anchor, [...(byAnchor.get(anchor) ?? []), thread]);
    }
    const value: FileThreads<T> = {
      threads: list,
      byAnchor,
      ids: new Set(list.map((thread) => thread.id)),
    };
    cache.set(path, { key, value });
    sliced.set(path, value);
  }

  for (const path of [...cache.keys()]) if (!sliced.has(path)) cache.delete(path);
  return sliced;
}

const EMPTY: FileThreads<AnchoredThread> = {
  threads: [],
  byAnchor: new Map(),
  ids: new Set(),
};

/** The slice for a file with no threads, which is one shared value so that
 * a file without feedback is never handed a new one. */
export function emptyFileThreads<T extends AnchoredThread>(): FileThreads<T> {
  return EMPTY as FileThreads<T>;
}
