export type ThreadState = "draft" | "sent" | "answered" | "resolved";

/** Who wrote a message. The daemon authors every agent message as
 * `session` and everything the reader writes as `user`. */
export type ThreadAuthor = "user" | "session";

export interface AttentionThread {
  id: number;
  path: string;
  current_line: number;
  state: ThreadState;
  messages: readonly { author: ThreadAuthor }[];
}

/** Newest message this reader has had in front of them, per thread.
 * The rail no longer distinguishes read from unread, but the page still
 * records what has been reached so a reply at the bottom of a long diff
 * is not marked read on load. */
export type SeenMarks = Record<string, number>;

/**
 * Which of a file's groups a marker or a walk is about.
 *
 * `open` is `yours` and `theirs` together, which is what the whole-review
 * walk and the resolve-and-carry-on step move through; the rail splits
 * that same set by whose turn it is.
 */
export type ThreadGroup = "open" | "yours" | "theirs" | "resolved";

export interface FileThreads {
  /** Thread ids in line order, still open. */
  open: number[];
  /** Open, and the reader owes the next move: the agent answered, or
   * the thread is a draft they have not sent. */
  yours: number[];
  /** Open, sent, and the reader spoke last, so the agent owes it. */
  theirs: number[];
  /** Thread ids in line order, closed. */
  resolved: number[];
}

export interface WalkThread {
  id: number;
  path: string;
  line: number;
  /** Where the file sits in the rail, which is the outer sort key. */
  fileIndex: number;
}

/**
 * A thread is open until somebody resolves it.
 *
 * Whether its newest message has been read was once shown here too, and
 * is not: a marker for something the reader can settle by scrolling past
 * it answered a question nobody was asking.
 */
export function isOpen(thread: AttentionThread): boolean {
  return thread.state !== "resolved";
}

/**
 * Whose turn a thread is on: whoever spoke last owns it, and a thread
 * the agent has never been handed is the reader's.
 *
 * A draft waits on the reader to send it, so it belongs with the threads
 * they owe a move on rather than with the ones the agent owes. Past that
 * the last author is the whole answer: a comment the agent has not
 * answered is the reader's own last word, so it waits on the agent
 * exactly like a reply does, and only the daemon writes a `session`
 * message, and only when an agent replies.
 */
export function threadTurn(thread: AttentionThread): Exclude<ThreadGroup, "open"> {
  if (!isOpen(thread)) return "resolved";
  if (thread.state === "draft") return "yours";
  const last = thread.messages[thread.messages.length - 1];
  return last?.author === "session" ? "yours" : "theirs";
}

export function groupThreadsByFile(
  threads: readonly AttentionThread[],
): Map<string, FileThreads> {
  const byFile = new Map<string, FileThreads>();
  const ordered = [...threads].sort(
    (a, b) => a.current_line - b.current_line || a.id - b.id,
  );
  for (const thread of ordered) {
    const file = byFile.get(thread.path)
      ?? { open: [], yours: [], theirs: [], resolved: [] };
    const turn = threadTurn(thread);
    file[turn].push(thread.id);
    if (turn !== "resolved") file.open.push(thread.id);
    byFile.set(thread.path, file);
  }
  return byFile;
}

/**
 * Every thread of one group, in the order the reader meets them: down
 * the file list, then down each file. A thread on a path the review does
 * not list is left out rather than appended, so a walk and the rail
 * cannot disagree about what exists.
 */
export function walkThreads(
  files: readonly string[],
  threads: readonly AttentionThread[],
  group: ThreadGroup,
): WalkThread[] {
  const byFile = groupThreadsByFile(threads);
  const lines = new Map(threads.map((t) => [t.id, t.current_line]));
  const walk: WalkThread[] = [];
  files.forEach((path, fileIndex) => {
    for (const id of byFile.get(path)?.[group] ?? []) {
      walk.push({ id, path, line: lines.get(id) ?? 0, fileIndex });
    }
  });
  return walk;
}

/** Later in the walk: further down the file list, or further down a file. */
function isAfter(a: WalkThread, b: WalkThread): boolean {
  if (a.fileIndex !== b.fileIndex) return a.fileIndex > b.fileIndex;
  if (a.line !== b.line) return a.line > b.line;
  return a.id > b.id;
}

/**
 * The next thread to land on, walking on from where the reader last
 * landed and wrapping at the end. Where they were is carried as a
 * position rather than a list index, so a thread leaving the walk —
 * resolved while they were reading it — does not send the next step
 * back to the top.
 */
export function nextThread(
  order: readonly WalkThread[],
  from: WalkThread | null,
  direction: 1 | -1 = 1,
): WalkThread | null {
  if (order.length === 0) return null;
  const last = order[order.length - 1];
  if (!from) return direction === 1 ? order[0] : last;
  const rest = order.filter((o) => (direction === 1 ? isAfter(o, from) : isAfter(from, o)));
  if (rest.length === 0) return direction === 1 ? order[0] : last;
  return direction === 1 ? rest[0] : rest[rest.length - 1];
}
