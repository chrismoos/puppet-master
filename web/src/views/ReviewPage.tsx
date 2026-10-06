import {
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { highlightFileRows } from "./diffHighlight";
import { ReviewChoiceSelect, ReviewSide } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { sessionDisplayName } from "@puppet-master/client-core/format";
import { reviewRoutePath, sessionRoutePath } from "@puppet-master/client-core/router";
import { authedFetch } from "../api/token";
import { navigate, replaceRoute } from "../router";
import { startColumnResize } from "../columnResize";
import {
  groupThreadsByFile,
  nextThread,
  threadTurn,
  walkThreads,
  type ThreadGroup,
  type WalkThread,
  type SeenMarks,
} from "./reviewAttention";
import {
  readReviewRailCollapsed,
  readReviewRailWidth,
  readString,
  REVIEW_RAIL_COLLAPSED_KEY,
  REVIEW_RAIL_COLLAPSED_WIDTH,
  REVIEW_RAIL_MAX_WIDTH,
  REVIEW_RAIL_MIN_WIDTH,
  REVIEW_RAIL_WIDTH_KEY,
  writeBool,
  writeString,
} from "../storage";
import { useAppState, useClient } from "../state/hooks";
import { Markdown, markdownAnchors, type BlockRange } from "../components/Markdown";
import type { ChoiceList } from "../components/markdownChoice";
import { ChoiceCard } from "./ChoiceCard";
import { choiceAnswerBody, type ChoiceAnswer } from "./reviewChoice";
import { StateBadge } from "../components/StateBadge";
import {
  anchorKey,
  emptyFileThreads,
  newThreadSliceCache,
  sliceThreadsByFile,
  strayThreads,
  type FileThreads,
} from "./reviewThreads";
import {
  anchorNote,
  changesView,
  choiceBearingFiles,
  fileDefaultPreview,
  isImage,
  draftPaths,
  isMarkdown,
  liveSnapshot,
  nextUnviewedFile,
  parseDiff,
  previewExcerpt,
  renderSnapshot,
  scrollKey,
  staleAction,
  viewOptions,
  type AnchorStatus,
  type DiffFile,
  type DiffRow,
  withMissingThreadFiles,
  reviewReadError,
} from "./reviewDiff";
import { ReviewImage } from "./ReviewImage";

interface ApiMessage {
  id: number;
  author: "user" | "session";
  session_id: number;
  body: string;
  addressed: boolean;
  changes_rev: number;
  changed_files: string[];
  created_at_unix_ms: number;
  choice: ChoiceAnswer | null;
}

interface ApiThread {
  id: number;
  path: string;
  line: number;
  side: "left" | "right";
  excerpt?: string;
  current_line: number;
  anchor_status: AnchorStatus;
  current_excerpt: string;
  state: "draft" | "sent" | "answered" | "resolved";
  changed_ahead: boolean;
  messages: ApiMessage[];
}

interface ApiReview {
  id: number;
  session_id: number;
  label: string;
  mode: "range" | "file";
  worktree: string;
  source_file: string;
  state: "open" | "finished";
  revision: number;
  draft_count: number;
  open_count: number;
  answered_count: number;
  resolved_count: number;
}

interface ApiDetail {
  review: ApiReview;
  threads: ApiThread[];
  revisions: { rev: number; kind: "sent" | "received"; files: string[] }[];
  viewer: {
    pinned_rev: number;
    view: string;
    layout: string;
    context: number;
    viewed_files: string[];
    preview_off_files: string[];
    last_thread_id: number;
    scroll: Record<string, number>;
    file_list_collapsed: boolean;
    drafts: Record<string, string>;
  };
  files: string[];
  skipped: { path: string; reason: string }[];
  latest_rev: number;
  pending_files: string[];
  detached?: boolean;
}

/** What the reader was looking at when they opened the composer: the text
 * at that line and the snapshot it was rendered from, both of which travel
 * with the comment. */
interface Composing {
  path: string;
  line: number;
  side: ReviewSide;
  excerpt: string;
  snapshot: bigint | null;
}

const CONTEXT_CHOICES = [3, 5, 10, 20, 50, 100000];

/** Typing should not be a write per keystroke, and a draft only has to
 * survive leaving the page, not every character. */
const DRAFT_SAVE_DELAY_MS = 400;

/** Scrolling past a run of threads should cost one write, not one per
 * thread, so marks are batched. */
const SEEN_SAVE_DELAY_MS = 600;

/** How often an open page asks whether the tree it is reading has moved.
 * Nothing else tells it: the daemon renders the working tree per read,
 * and a plain edit raises no event. The check costs one repository read
 * that sends back only the paths and SHAs it found, so the interval
 * buys promptness against a cost that does not grow with the review. */
const TREE_CHECK_INTERVAL_MS = 3_000;

/** How many changed files the banner names before it counts the rest.
 * A branch-wide edit can move every file in the review, and the point
 * of naming them is to recognize one, not to read a list. */
const STALE_FILES_NAMED = 5;

/** The newest answer to this question on this file, and the thread it
 *  lives on. Newest wins: an answer that went stale leaves its thread
 *  behind as the record of what was decided at the time, and the reader
 *  answers again on a new one. */
function latestChoiceThread(
  threads: readonly ApiThread[],
  choiceId: string,
): { thread: ApiThread; message: ApiMessage; answer: ChoiceAnswer } | null {
  for (const thread of [...threads].sort((a, b) => b.id - a.id)) {
    const message = thread.messages.find((m) => m.choice?.choice_id === choiceId);
    if (message?.choice) return { thread, message, answer: message.choice };
  }
  return null;
}

type PmClientLike = {
  setReviewViewerState: (input: {
    reviewId: bigint;
    draftKey?: string;
    draftBody?: string;
    seenThread?: bigint;
    seenMessage?: bigint;
  }) => Promise<unknown>;
};

/** A handler whose identity never changes while it still calls the latest
 * closure, so a memoised file section is not re-rendered by its neighbours. */
function useStableCallback<A extends unknown[], R>(fn: (...args: A) => R): (...args: A) => R {
  const held = useRef(fn);
  held.current = fn;
  return useCallback((...args: A) => held.current(...args), []);
}

/** Where in a review the URL says the reader is. */
export interface ReviewAt {
  view?: string;
  file?: string;
  thread?: number;
}

function contextLabel(n: number): string {
  return n >= 100000 ? "all" : String(n);
}

function readMarkdownPreviewOn(reviewId: number | string): Set<string> {
  const raw = readString(`pm.review.${reviewId}.markdownPreviewOn`);
  if (!raw) return new Set();
  try {
    const list = JSON.parse(raw);
    return Array.isArray(list) ? new Set(list) : new Set();
  } catch {
    return new Set();
  }
}

function writeMarkdownPreviewOn(reviewId: number | string, set: Set<string>): void {
  writeString(`pm.review.${reviewId}.markdownPreviewOn`, JSON.stringify([...set].sort()));
}

export function ReviewPage({
  id,
  at,
  onFinished,
  onExit,
}: {
  id: number;
  at?: ReviewAt;
  onFinished?: () => boolean | void;
  onExit?: (sessionId: string) => void;
}) {
  const client = useClient();
  const state = useAppState();
  const [detail, setDetail] = useState<ApiDetail | null>(null);
  const [finishing, setFinishing] = useState(false);
  const exitRequested = useRef(false);
  useEffect(() => {
    if (!onExit || finishing) return;
    if (exitRequested.current && detail) {
      exitRequested.current = false;
      onExit(String(detail.review.session_id));
      return;
    }
    const exit = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      if (detail) onExit(String(detail.review.session_id));
      else exitRequested.current = true;
    };
    window.addEventListener("keydown", exit, true);
    return () => window.removeEventListener("keydown", exit, true);
  }, [onExit, detail, finishing]);
  const [diff, setDiff] = useState<DiffFile[]>([]);
  const [diffLoaded, setDiffLoaded] = useState(false);
  const [diffEpoch, setDiffEpoch] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [composing, setComposing] = useState<Composing | null>(null);
  const [replyOn, setReplyOn] = useState<number | null>(null);
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const draftsRef = useRef<Record<string, string>>({});
  // The server copy is the source of truth on arrival; local edits take
  // over from there so typing never fights the round trip.
  const seededDrafts = useRef(false);
  const [preview, setPreview] = useState<Record<string, string>>({});
  const [diffSnapshot, setDiffSnapshot] = useState<bigint | null>(null);
  // The tree the render was measured against, and the files that have
  // moved since. These are not the rendered snapshot: a reader on a
  // stored revision is told about the working tree, not about the
  // revision they chose.
  const [renderedTree, setRenderedTree] = useState<bigint | null>(null);
  const [changedFiles, setChangedFiles] = useState<string[]>([]);
  const [previewSnapshots, setPreviewSnapshots] = useState<Record<string, bigint>>({});
  const scrollRef = useRef<HTMLDivElement>(null);
  const railRef = useRef<HTMLDivElement>(null);
  // Where the walk through open feedback last landed, so the next step
  // carries on instead of returning to the top of the file.
  const [landed, setLanded] = useState<WalkThread | null>(null);
  // Which circle the last landing came from, so clicking the other one
  // starts its own group rather than resuming the first's position.
  const [landedGroup, setLandedGroup] = useState<ThreadGroup>("open");
  const [pendingJump, setPendingJump] = useState<number | null>(null);
  // The thread a resolve just closed, so the walk can carry on from
  // where it stood rather than from the top of the review.
  const [bounceFrom, setBounceFrom] = useState<WalkThread | null>(null);
  const [justViewed, setJustViewed] = useState<string | null>(null);
  const [railWidth, setRailWidth] = useState(() => readReviewRailWidth());
  const [railCollapsed, setRailCollapsed] = useState(() => readReviewRailCollapsed());
  const [railResizing, setRailResizing] = useState(false);
  const restored = useRef(false);
  // The last position read out of the URL, so arriving at one is acted
  // on once rather than on every render that follows.
  const arrived = useRef<string | null>(null);

  // Embedded in a session pane there is no review URL to keep in step,
  // so the position lives only in the stored viewer state.
  const linked = at !== undefined;

  const viewer = detail?.viewer;
  const view = viewer?.view ?? "";
  const layout = viewer?.layout ?? "side-by-side";
  const context = viewer?.context ?? 10;
  const viewed = useMemo(
    () => new Set(viewer?.viewed_files ?? []),
    [viewer?.viewed_files],
  );
  // Seen marks are per reader, so they arrive with the user's own
  // snapshot rather than in the review payload every reader shares.
  const seen: SeenMarks = useMemo(() => {
    const marks: SeenMarks = {};
    for (const [thread, message] of Object.entries(
      state.reviewViewerStates.get(String(id))?.seen ?? {},
    )) {
      marks[thread] = Number(message);
    }
    return marks;
  }, [state.reviewViewerStates, id]);
  const previewOff = useMemo(
    () => new Set(viewer?.preview_off_files ?? []),
    [viewer?.preview_off_files],
  );
  const [markdownPreviewOn, setMarkdownPreviewOn] = useState<Set<string>>(() =>
    readMarkdownPreviewOn(id),
  );
  useEffect(() => {
    setMarkdownPreviewOn(readMarkdownPreviewOn(id));
  }, [id]);

  // A file with an active thread that the diff does not carry is retained
  // so comments and replies are never orphaned.
  const displayFiles = useMemo(
    () => withMissingThreadFiles(diff, (detail?.threads ?? []).map((t) => t.path), diffLoaded),
    [diff, detail?.threads, diffLoaded],
  );

  const deletedFiles = useMemo(() => {
    const set = new Set<string>();
    for (const file of displayFiles) {
      if (file.deleted) set.add(file.path);
    }
    return set;
  }, [displayFiles]);

  // Highlighted once per diff rather than per render, and per file rather
  // than per line, because a line on its own cannot tell that it sits
  // inside a block comment or a template literal.
  const highlighted = useMemo(() => {
    const byPath = new Map<string, (string | null)[]>();
    for (const file of displayFiles) {
      if (!file.binary) byPath.set(file.path, highlightFileRows(file.path, file.rows));
    }
    return byPath;
  }, [displayFiles]);

  const composerKey = composing
    ? `line:${composing.path}:${composing.line}:${composing.side}`
    : null;
  const initialDraftBody = composerKey
    ? draftsRef.current[composerKey] ?? drafts[composerKey] ?? ""
    : "";
  const replyKey = replyOn !== null ? replyDraftKey(replyOn) : null;
  const initialReplyBody = replyKey
    ? draftsRef.current[replyKey] ?? drafts[replyKey] ?? ""
    : "";

  const detailRef = useRef<ApiDetail | null>(null);
  detailRef.current = detail;

  const load = useCallback(async () => {
    try {
      const res = await authedFetch(`/api/reviews/${id}`, { cache: "no-store" });
      // The daemon says why in the body. Reporting only that it failed is
      // what made a missing worktree indistinguishable from a bad id.
      if (!res.ok) throw new Error(await reviewReadError(res, id));
      setDetail((await res.json()) as ApiDetail);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [id]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (seededDrafts.current || !detail) return;
    const initial = detail.viewer.drafts ?? {};
    draftsRef.current = { ...initial };
    setDrafts(initial);
    seededDrafts.current = true;
  }, [detail]);

  // The daemon publishes counts on the event bus, so a reply landing
  // refreshes the threads without the reader doing anything. The diff
  // is deliberately not refetched: the reader is pinned.
  const reviewRow = state.reviews.get(String(id));
  const counts = reviewRow
    ? `${reviewRow.draftCount}:${reviewRow.openCount}:${reviewRow.answeredCount}:${reviewRow.resolvedCount}`
    : "";
  useEffect(() => {
    if (counts) void load();
  }, [counts, load]);

  // The daemon renders the diff from the view selector, the context width
  // and the revision being read, so only those refetch it. Depending on the
  // whole detail object refetched, reparsed and re-highlighted every file in
  // the review whenever the reader's own state was saved, which marking a
  // file viewed and settling a scroll both do.
  const loaded = detail !== null;
  const pinnedRev = viewer?.pinned_rev ?? 0;
  useEffect(() => {
    if (!loaded) return;
    const params = new URLSearchParams({ view, context: String(context) });
    setDiffLoaded(false);
    authedFetch(`/api/reviews/${id}/diff?${params}`, { cache: "no-store" })
      .then((r) => {
        if (!r.ok) throw new Error("diff failed");
        setDiffSnapshot(renderSnapshot(r));
        setRenderedTree(liveSnapshot(r));
        setChangedFiles([]);
        return r.text();
      })
      .then((text) => {
        setDiff(parseDiff(text));
        setDiffLoaded(true);
      })
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
  }, [id, view, context, pinnedRev, loaded, diffEpoch]);

  // The daemon renders the working tree per read and raises nothing when
  // that tree changes, so a page that is open asks. Only the paths come
  // back, and only while there is a render to be stale against.
  useEffect(() => {
    if (renderedTree === null) return;
    let cancelled = false;
    const check = async () => {
      try {
        const res = await authedFetch(
          `/api/reviews/${id}/tree?since=${renderedTree}`,
          { cache: "no-store" },
        );
        if (!res.ok || cancelled) return;
        const body = (await res.json()) as { changed?: string[] };
        if (!cancelled) setChangedFiles(body.changed ?? []);
      } catch {
        // A check that did not land says nothing about the tree, and the
        // next one is a few seconds away.
      }
    };
    const timer = setInterval(() => void check(), TREE_CHECK_INTERVAL_MS);
    void check();
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [id, renderedTree]);

  const threadPaths = useMemo(() => {
    const byId = new Map<number, string>();
    for (const thread of detail?.threads ?? []) byId.set(thread.id, thread.path);
    return byId;
  }, [detail?.threads]);

  const stale = staleAction(view, changedFiles, draftPaths(drafts, threadPaths));

  // The diff is not the whole page: a file added to the tree after the
  // page loaded is in neither the rendered diff nor the file list, so
  // both are read again.
  const refetch = useCallback(() => {
    setDiffEpoch((n) => n + 1);
    void load();
  }, [load]);

  // Nothing is pinned and nothing is half-written, so there is no reason
  // to make the reader ask for what they would have seen on a reload.
  useEffect(() => {
    if (stale === "refresh") refetch();
  }, [stale, refetch]);

  // Restoring the reader's place is the point of persisting it, so it
  // happens once per page rather than on every diff refresh.
  //
  // "Once" means once it actually takes. A markdown file's rendered text
  // arrives after the diff does, so the document can still be far shorter
  // than it will end up, and a scrollTop past its current end is clamped
  // to it. Marking the restore done at that moment is what left the reader
  // at the top after switching to the agent tab and back. Retrying while
  // the position has not landed lets the previews arrive and the document
  // grow underneath it.
  useEffect(() => {
    if (restored.current || !viewer || displayFiles.length === 0) return;
    const top = viewer.scroll[scrollKey(view, layout, context)];
    const el = scrollRef.current;
    if (typeof top !== "number" || !el) {
      restored.current = true;
      return;
    }
    if (applyScrollRestore(el, top)) restored.current = true;
  }, [viewer, displayFiles, preview, view, layout, context]);

  const draftDebouncer = useRef<ReturnType<typeof createDraftDebouncer> | null>(null);
  if (!draftDebouncer.current) {
    draftDebouncer.current = createDraftDebouncer(DRAFT_SAVE_DELAY_MS, (key, body) => {
      void client.setReviewViewerState({
        reviewId: BigInt(id),
        draftKey: key,
        draftBody: body,
      });
      setDrafts((prev) => (prev[key] === body ? prev : { ...prev, [key]: body }));
    });
  }

  useEffect(() => {
    return () => {
      draftDebouncer.current?.cancelAll();
    };
  }, []);

  const setDraft = useCallback((key: string | null, body: string) => {
    if (!key) return;
    draftsRef.current[key] = body;
    draftDebouncer.current?.save(key, body);
  }, []);

  const clearDraft = useCallback(
    (key: string | null) => {
      if (!key) return;
      draftDebouncer.current?.clear(key);
      delete draftsRef.current[key];
      setDrafts((prev) => {
        if (!(key in prev)) return prev;
        const next = { ...prev };
        delete next[key];
        return next;
      });
      void client.setReviewViewerState({
        reviewId: BigInt(id),
        draftKey: key,
        draftBody: "",
      });
    },
    [client, id],
  );

  // A reply is seen when the reader actually reaches it, not when the
  // page loads: one at the bottom of a long diff stays unseen until
  // they scroll to it.
  const markSeen = useMemo(() => {
    const pending = new Map<number, number>();
    let timer: ReturnType<typeof setTimeout> | null = null;
    return (threadId: number, messageId: number, client: PmClientLike, reviewId: number) => {
      if ((pending.get(threadId) ?? 0) >= messageId) return;
      pending.set(threadId, messageId);
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => {
        for (const [thread, message] of pending) {
          void client.setReviewViewerState({
            reviewId: BigInt(reviewId),
            seenThread: BigInt(thread),
            seenMessage: BigInt(message),
          });
        }
        pending.clear();
      }, SEEN_SAVE_DELAY_MS);
    };
  }, []);

  const saveViewer = useCallback(
    (patch: Parameters<typeof client.setReviewViewerState>[0]) => {
      void client.setReviewViewerState({ ...patch, reviewId: BigInt(id) }).then(load);
    },
    [client, id, load],
  );

  const rememberScroll = useCallback(() => {
    const top = scrollRef.current?.scrollTop;
    if (typeof top !== "number") return;
    saveViewer({
      reviewId: BigInt(id),
      scrollKey: scrollKey(view, layout, context),
      scrollTop: Math.round(top),
    });
  }, [saveViewer, id, view, layout, context]);

  // `debounced` builds a fresh timer each time it is called, so building it
  // during render gave every render its own and none of them coalesced.
  const rememberRef = useRef(rememberScroll);
  rememberRef.current = rememberScroll;
  const onScrollSave = useMemo(() => debounced(() => rememberRef.current()), []);

  // Which file the reader is on. Observed rather than measured on scroll:
  // a review runs to dozens of sections, and reading every one's box on
  // every scroll event is the cost this page has twice been fixed for.
  const [readingFile, setReadingFile] = useState("");
  useEffect(() => {
    const container = scrollRef.current;
    if (!container || displayFiles.length === 0) return;
    const tops = new Map<string, number>();
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          const path = (entry.target as HTMLElement).dataset.file;
          if (!path) continue;
          if (entry.isIntersecting) tops.set(path, entry.boundingClientRect.top);
          else tops.delete(path);
        }
        // The topmost section still on screen is the one being read: a file
        // taller than the viewport is the only one intersecting, and at a
        // boundary the one above is what the reader is finishing.
        let reading = "";
        let highest = Number.POSITIVE_INFINITY;
        for (const [path, top] of tops) {
          if (top < highest) {
            highest = top;
            reading = path;
          }
        }
        if (reading) setReadingFile(reading);
      },
      { root: container, threshold: 0 },
    );
    for (const file of displayFiles) {
      const node = document.getElementById(`review-file-${cssId(file.path)}`);
      if (node) observer.observe(node);
    }
    return () => observer.disconnect();
  }, [displayFiles]);

  // Keep that file's row on screen in the rail. Moved by the least that
  // makes it visible, and never when it already is, so the rail does not
  // fight a reader who scrolled it somewhere deliberately.
  useEffect(() => {
    const rail = railRef.current;
    if (!rail || !readingFile || railCollapsed) return;
    const row = rail.querySelector<HTMLElement>(
      `[data-rail-file="${CSS.escape(readingFile)}"]`,
    );
    if (!row) return;
    const top = row.offsetTop;
    const bottom = top + row.offsetHeight;
    if (top < rail.scrollTop) rail.scrollTop = top;
    else if (bottom > rail.scrollTop + rail.clientHeight) {
      rail.scrollTop = bottom - rail.clientHeight;
    }
  }, [readingFile, railCollapsed]);

  const setFileViewed = useStableCallback((path: string, next: boolean) => {
    const files = new Set(viewed);
    if (next) files.add(path);
    else files.delete(path);
    saveViewer({
      reviewId: BigInt(id),
      viewedFiles: [...files].sort(),
      setViewedFiles: true,
    });
    // Where to land is decided once the file has actually collapsed,
    // not here. Reopening a file never moves the reader.
    if (next) setJustViewed(path);
  });

  // Marking a file viewed takes its body out of the document, so a scroll
  // measured before that lands as far down as the body was tall. The move
  // waits for the collapsed state to come back and be laid out, and puts
  // the next file still needing attention at the top. With nothing left
  // below, it holds the reader on the file they just cleared rather than
  // throwing them back up the review.
  useEffect(() => {
    if (!justViewed || !detail || !viewed.has(justViewed)) return;
    const target = nextUnviewedFile(detail.files, justViewed, viewed) ?? justViewed;
    const node = document.getElementById(`review-file-${cssId(target)}`);
    if (node) scrollToSettled(node, scrollRef.current, "start");
    setJustViewed(null);
  }, [justViewed, detail, viewed]);

  const togglePreview = useStableCallback((path: string, on: boolean) => {
    if (isMarkdown(path)) {
      setMarkdownPreviewOn((prev) => {
        const next = new Set(prev);
        if (on) next.add(path);
        else next.delete(path);
        writeMarkdownPreviewOn(id, next);
        return next;
      });
    }
    // Markdown records both sides, because a file that would default to
    // the preview needs somewhere to hold the reader's "off".
    const off = new Set(previewOff);
    if (on) off.delete(path);
    else off.add(path);
    saveViewer({
      reviewId: BigInt(id),
      previewOffFiles: [...off].sort(),
      setPreviewOffFiles: true,
    });
  });

  // A viewed file is collapsed to its header, so scrolling to it alone
  // leaves the reader looking at the thing they just asked to see.
  const openFile = useCallback(
    (path: string) => {
      if (viewed.has(path)) setFileViewed(path, false);
      const node = document.getElementById(`review-file-${cssId(path)}`);
      if (node) scrollToSettled(node, scrollRef.current, "start");
    },
    [viewed, setFileViewed],
  );

  const attention = useMemo(
    () => groupThreadsByFile(detail?.threads ?? []),
    [detail, seen],
  );
  const walk = useMemo(
    () => walkThreads(detail?.files ?? [], detail?.threads ?? [], "open"),
    [detail, seen],
  );

  // The file has to be open before its threads exist to scroll to, and
  // clearing the viewed mark is a round trip, so the jump waits for the
  // thread to be rendered rather than assuming it already is.
  const jumpTo = useCallback(
    (target: WalkThread) => {
      setLanded(target);
      openFile(target.path);
      setPendingJump(target.id);
      // Walking is continuous, so the URL follows without adding a
      // history entry for every step. A reload or a copied link is
      // still right; the back button still means the last real move.
      if (linked) {
        replaceRoute(reviewRoutePath(id, { view, file: target.path, thread: target.id }));
      }
    },
    [openFile, id, view, linked],
  );

  // The URL and the stored position are one position in two forms. A URL
  // that names one wins and is written through; a bare one is filled in
  // from what was stored, so the reader's first move already has
  // somewhere to come back to.
  useEffect(() => {
    if (!at || !detail) return;
    const key = `${at.view ?? ""}\u0000${at.file ?? ""}\u0000${at.thread ?? ""}`;
    if (arrived.current === key) return;
    const arriving = arrived.current === null;
    if (at.file && !document.getElementById(`review-file-${cssId(at.file)}`)) return;
    arrived.current = key;
    if (at.view === undefined && at.file === undefined && at.thread === undefined) {
      // A URL naming nothing means two different things. On arrival it
      // means "wherever I left off", so the stored position fills it
      // in. Afterwards the reader asked for the live tree, by the way
      // back or by the back button, and the position follows them.
      if (arriving) {
        if (view) replaceRoute(reviewRoutePath(id, { view }));
      } else if (view) {
        saveViewer({ reviewId: BigInt(id), view: "" });
      }
      return;
    }
    if (at.view !== undefined && at.view !== view) {
      saveViewer({ reviewId: BigInt(id), view: at.view });
    }
    if (at.file) openFile(at.file);
    if (at.thread) setPendingJump(at.thread);
  }, [at, detail, displayFiles, view, id, saveViewer, openFile]);

  useEffect(() => {
    if (!bounceFrom) return;
    const target = nextThread(walk, bounceFrom);
    setBounceFrom(null);
    // Nothing open left, or the walk has not caught up and offers back
    // the thread just settled: either way there is nowhere to go.
    if (target && target.id !== bounceFrom.id) jumpTo(target);
  }, [bounceFrom, walk, jumpTo]);

  const jumpInFile = useCallback(
    (path: string, group: ThreadGroup) => {
      const inFile = walkThreads([path], detail?.threads ?? [], group);
      // Landing carries within one circle: clicking the same circle again
      // steps on, while switching circles starts that group from its top.
      const from = landed?.path === path && landedGroup === group ? landed : null;
      const target = nextThread(inFile, from);
      if (target) {
        setLandedGroup(group);
        jumpTo(target);
      }
    },
    [landed, landedGroup, detail?.threads, jumpTo],
  );

  const stepWalk = useCallback(
    (direction: 1 | -1) => {
      const target = nextThread(walk, landed, direction);
      if (target) jumpTo(target);
    },
    [walk, landed, jumpTo],
  );

  useEffect(() => {
    if (pendingJump === null) return;
    const node = document.getElementById(`review-thread-${pendingJump}`);
    if (!node) return;
    scrollToSettled(node, scrollRef.current, "center");
    setPendingJump(null);
  }, [pendingJump, detail, displayFiles, preview]);

  // n and N walk open feedback across the whole review. A reader typing
  // a comment is typing an n, not asking to move.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      const target = event.target as HTMLElement | null;
      if (target?.closest("input, textarea, select, [contenteditable='true']")) return;
      if (event.key === "n") stepWalk(1);
      else if (event.key === "N") stepWalk(-1);
      else return;
      event.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [stepWalk]);

  const startRailResize = useCallback((e: ReactPointerEvent) => {
    e.preventDefault();
    startColumnResize({
      originX: railRef.current?.getBoundingClientRect().left ?? 0,
      min: REVIEW_RAIL_MIN_WIDTH,
      max: REVIEW_RAIL_MAX_WIDTH,
      start: readReviewRailWidth(),
      onWidth: setRailWidth,
      onCommit: (width) => writeString(REVIEW_RAIL_WIDTH_KEY, String(width)),
      onDragging: setRailResizing,
    });
  }, []);

  const toggleRail = useCallback(() => {
    setRailCollapsed((collapsed) => {
      writeBool(REVIEW_RAIL_COLLAPSED_KEY, !collapsed);
      return !collapsed;
    });
  }, []);

  useEffect(() => {
    setPreview({});
    setPreviewSnapshots({});
  }, [view]);

  useEffect(() => {
    if (!detail) return;
    for (const path of detail.files) {
      if (!isMarkdown(path) || previewOff.has(path) || preview[path] !== undefined) continue;
      const params = new URLSearchParams({ file: path, view });
      authedFetch(`/api/reviews/${id}/file?${params}`, { cache: "no-store" })
        .then((r) => {
          if (!r.ok) return "";
          const snapshot = renderSnapshot(r);
          if (snapshot !== null) {
            setPreviewSnapshots((prev) => ({ ...prev, [path]: snapshot }));
          }
          return r.text();
        })
        .then((text) => setPreview((prev) => ({ ...prev, [path]: text })))
        .catch(() => undefined);
    }
  }, [detail, previewOff, preview, id, view]);

  // Whether a document asks the reader to choose is only knowable from
  // its rendered form, so it is read as the texts arrive.
  const choiceFiles = useMemo(() => choiceBearingFiles(preview), [preview]);
  const previewDefaults = useMemo(
    () => ({
      previewOff,
      markdownPreviewOn,
      choiceFiles,
      documentReview: detail?.review.mode === "file",
    }),
    [previewOff, markdownPreviewOn, choiceFiles, detail?.review.mode],
  );

  const onSeen = useCallback(
    (threadId: number, messageId: number) => markSeen(threadId, messageId, client, id),
    [markSeen, client, id],
  );

  // Saving the reader's own state replaces the review payload, so threads
  // that did not move still arrive as new objects. The slices are held by
  // value across that so a file whose feedback is unchanged keeps the props
  // it had and stays out of the render.
  const threadSlices = useRef(newThreadSliceCache<ApiThread>());
  const threadsByFile = useMemo(
    () => sliceThreadsByFile(detail?.threads ?? [], threadSlices.current),
    [detail?.threads],
  );

  // A comment placed on a rendered block is written against that
  // document, so it carries the block's own text and the snapshot the
  // document was fetched from.
  const previewComposer = useStableCallback((path: string, range: BlockRange) => ({
    path,
    line: range.start,
    side: ReviewSide.RIGHT,
    excerpt: previewExcerpt(preview[path] ?? "", range),
    snapshot: previewSnapshots[path] ?? null,
  }));

  // Answering a question writes a draft thread, so the answer batches
  // with the reader's comments and survives leaving the page. Writes are
  // queued per question: clicking through options must not race two
  // threads into existence for one of them.
  const choiceTimers = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  const choiceWrites = useRef(new Map<string, Promise<unknown>>());
  const choicePending = useRef(new Map<string, () => Promise<void>>());

  const writeChoice = useStableCallback(
    (path: string, range: BlockRange, choice: ChoiceList, answer: ChoiceAnswer) =>
      async () => {
        const held = latestChoiceThread(
          (detailRef.current?.threads ?? []).filter((t) => t.path === path),
          choice.id,
        );
        const body = choiceAnswerBody(answer);
        const wire = {
          choiceId: answer.choice_id,
          select: answer.select === "many" ? ReviewChoiceSelect.MANY : ReviewChoiceSelect.ONE,
          optionIds: answer.option_ids,
          optionLabels: answer.option_labels,
          otherText: answer.other_text,
          notes: answer.notes,
        };
        // Only an unsent answer is edited in place. One the agent
        // already has stays as it was said, and a fresh answer to a
        // rewritten question starts its own thread.
        if (held && held.thread.state === "draft") {
          await client.editReviewComment(BigInt(held.message.id), body, wire);
        } else {
          await client.addReviewComment({
            reviewId: BigInt(id),
            path,
            line: range.start,
            side: ReviewSide.RIGHT,
            excerpt: previewExcerpt(preview[path] ?? "", range),
            body,
            send: false,
            anchorSnapshotId: previewSnapshots[path] ?? undefined,
            choice: wire,
          });
        }
        await load();
      },
  );

  const queueChoice = useCallback((key: string, write: () => Promise<void>) => {
    const queued = (choiceWrites.current.get(key) ?? Promise.resolve())
      .then(write)
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
    choiceWrites.current.set(key, queued);
    return queued;
  }, []);

  const answerChoice = useStableCallback(
    (path: string, range: BlockRange, choice: ChoiceList, answer: ChoiceAnswer) => {
      const key = `${path}:${choice.id}`;
      const write = writeChoice(path, range, choice, answer);
      choicePending.current.set(key, write);
      clearTimeout(choiceTimers.current.get(key));
      choiceTimers.current.set(
        key,
        setTimeout(() => {
          choicePending.current.delete(key);
          void queueChoice(key, write);
        }, DRAFT_SAVE_DELAY_MS),
      );
    },
  );

  // Sending one answer on its own still has to send the answer the
  // reader is looking at, so anything still waiting on the debounce is
  // written first rather than sent as it stood a moment ago.
  const sendChoice = useCallback(
    async (path: string, choiceId: string) => {
      const key = `${path}:${choiceId}`;
      clearTimeout(choiceTimers.current.get(key));
      const pending = choicePending.current.get(key);
      if (pending) {
        choicePending.current.delete(key);
        await queueChoice(key, pending);
      } else {
        await choiceWrites.current.get(key);
      }
      const held = latestChoiceThread(
        (detailRef.current?.threads ?? []).filter((t) => t.path === path),
        choiceId,
      );
      if (!held || held.thread.state !== "draft") return;
      await client.sendReviewThreads(BigInt(id), [BigInt(held.thread.id)]);
      await load();
    },
    [client, id, load, queueChoice],
  );

  const submitComment = useStableCallback(
    async (bodyOrSend: string | boolean, maybeSend?: boolean) => {
      const send = typeof bodyOrSend === "boolean" ? bodyOrSend : (maybeSend ?? false);
      const body =
        typeof bodyOrSend === "string"
          ? bodyOrSend
          : (composerKey ? draftsRef.current[composerKey] ?? drafts[composerKey] ?? "" : "");
      if (!composing || !body.trim()) return;
      await client.addReviewComment({
        reviewId: BigInt(id),
        path: composing.path,
        line: composing.line,
        side: composing.side,
        excerpt: composing.excerpt,
        body,
        send,
        anchorSnapshotId: composing.snapshot ?? undefined,
      });
      clearDraft(composerKey);
      setComposing(null);
      await load();
    },
  );

  // A reply belongs inside the thread it answers. Opening a second
  // thread on the same line instead is what left a conversation stacked
  // up the page in pieces, each resolvable on its own.
  const replyToThread = useStableCallback(async (threadId: number, body: string) => {
    if (!body.trim()) return;
    await client.replyReviewThread(BigInt(threadId), body);
    clearDraft(replyDraftKey(threadId));
    setReplyOn(null);
    await load();
  });
  // Resolving is the reader saying they are done with a thread, so the
  // next open one is where they want to be. Recorded as a position
  // rather than acted on here, because the thread only leaves the open
  // list once the reload lands.
  const resolveThread = useStableCallback((threadId: number, resolved: boolean) => {
    const from = resolved ? (walk.find((o) => o.id === threadId) ?? null) : null;
    void client
      .resolveReviewThread(BigInt(threadId), resolved)
      .then(load)
      .then(() => {
        if (from) setBounceFrom(from);
      });
  });
  const deleteThread = useStableCallback((threadId: number) => {
    void client.deleteReviewThread(BigInt(threadId)).then(load);
  });
  const sendThread = useStableCallback((threadId: number) => {
    void client.sendReviewThreads(BigInt(id), [BigInt(threadId)]).then(load);
  });
  // One position in two forms. A deliberate move writes both and adds a
  // history entry, so the back button returns to where the reader was
  // rather than to whatever the server last stored.
  const goTo = useStableCallback((next: ReviewAt) => {
    if (next.view !== undefined && next.view !== view) {
      saveViewer({ reviewId: BigInt(id), view: next.view });
    }
    if (!linked) {
      if (next.file) openFile(next.file);
      if (next.thread) setPendingJump(next.thread);
      return;
    }
    navigate(reviewRoutePath(id, { view, ...next }));
  });
  const openChanges = useStableCallback((rev: number, threadId: number) => {
    goTo({ view: changesView(rev), thread: threadId });
  });
  const advance = useStableCallback(async () => {
    try {
      await client.advanceReview(BigInt(id));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      return;
    }
    setPreview({});
    setPreviewSnapshots({});
    setDiffEpoch((e) => e + 1);
    await load();
  });
  const sendChoiceAnswer = useStableCallback((path: string, choiceId: string) => {
    void sendChoice(path, choiceId);
  });
  const cancelCompose = useStableCallback(() => setComposing(null));

  // Every handler a file section is given is fixed for the life of the page,
  // so the only prop that moves when a file is marked viewed is that file's
  // own flag.
  const actions = useMemo<ReviewFileActions>(
    () => ({
      setComposing,
      cancelCompose,
      setDraft,
      submitComment,
      setReplyOn,
      reply: replyToThread,
      resolve: resolveThread,
      remove: deleteThread,
      sendThread,
      openChanges,
      advance,
      markSeen: onSeen,
      setViewed: setFileViewed,
      togglePreview,
      answerChoice,
      sendChoice: sendChoiceAnswer,
      previewComposer,
    }),
    [
      cancelCompose,
      setDraft,
      submitComment,
      replyToThread,
      resolveThread,
      deleteThread,
      sendThread,
      openChanges,
      advance,
      onSeen,
      setFileViewed,
      togglePreview,
      answerChoice,
      sendChoiceAnswer,
      previewComposer,
    ],
  );

  if (error && !detail) {
    return <div className="pane-empty"><p className="muted-line">{error}</p></div>;
  }
  if (!detail) {
    return <div className="pane-empty"><p className="muted-line">loading review…</p></div>;
  }

  const r = detail.review;
  const session = state.sessions.get(String(r.session_id));
  const options = viewOptions(detail.revisions, view);
  // The picker's own words for where the reader is, so the notice and
  // the picker never describe the same view differently.
  const viewLabel = options.find((o) => o.value === view)?.label ?? view;
  const pending = detail.pending_files.length;
  const pinned = viewer?.pinned_rev ?? 0;
  const behind = readerIsBehind(detail.latest_rev, pinned);

  return (
    <div className="review">
      {error && <div className="flash-error" role="alert">{error}</div>}
      <div className="review-head">
        <div className="review-head-top">
          <span className="review-title">Review</span>
          <span className="review-range">{r.label}</span>
          {r.draft_count > 0 && <span className="badge st-starting">{r.draft_count} draft</span>}
          {r.answered_count > 0 && (
            <span className="badge st-needs-input">{r.answered_count} your turn</span>
          )}
          {r.open_count > 0 && <span className="badge st-working">{r.open_count} with agent</span>}
          {r.resolved_count > 0 && <span className="badge st-idle">{r.resolved_count} resolved</span>}
          {detail.detached && (
            <span
              className="badge st-idle review-detached"
              title="The worktree this review was opened against is gone, so this is the last captured revision rather than the live tree."
            >
              worktree gone
            </span>
          )}
          <div className="review-actions">
            <label className="review-select">
              view
              <select
                value={view}
                onChange={(e) => goTo({ view: e.target.value })}
              >
                {options.map((o) => (
                  <option key={o.value} value={o.value}>{o.label}</option>
                ))}
              </select>
            </label>
            <label className="review-select">
              layout
              <select
                value={layout}
                onChange={(e) => saveViewer({ reviewId: BigInt(id), layout: e.target.value })}
              >
                <option value="line-by-line">unified</option>
                <option value="side-by-side">side by side</option>
              </select>
            </label>
            <label className="review-select">
              context
              <select
                value={context}
                onChange={(e) => saveViewer({ reviewId: BigInt(id), context: Number(e.target.value) })}
              >
                {CONTEXT_CHOICES.map((n) => (
                  <option key={n} value={n}>{contextLabel(n)}</option>
                ))}
              </select>
            </label>
            {r.draft_count > 0 && (
              <button
                type="button"
                className="btn btn-primary"
                onClick={() => void client.sendReviewThreads(BigInt(id)).then(load)}
              >
                Send {r.draft_count} draft{r.draft_count === 1 ? "" : "s"}
              </button>
            )}
            <button
              type="button"
              className="btn btn-loud"
              disabled={finishing}
              onClick={() => {
                setError(null);
                setFinishing(true);
                void client.finishReview(BigInt(id)).then(() => {
                  if (onFinished?.()) return;
                  navigate(sessionRoutePath(String(r.session_id), "agent"));
                }).catch((reason) => {
                  setFinishing(false);
                  setError(String(reason));
                });
              }}
            >
              {finishing ? "Finishing…" : "Finish review"}
            </button>
            {onExit && (
              <button type="button" className="btn" aria-label="exit focus mode" disabled={finishing} onClick={() => onExit(String(r.session_id))}>
                Back to session
              </button>
            )}
          </div>
        </div>
        <div className="review-head-bot">
          <span>{detail.files.length} files</span>
          <span>·</span>
          <span>{r.mode === "file" ? "single file" : r.worktree}</span>
          {session && (
            <>
              <span>·</span>
              <span className="review-session">
                <StateBadge state={session.state} dot />
                {sessionDisplayName(session)}
              </span>
            </>
          )}
          <span>·</span>
          <span className="review-reading">{readingLabel(pinned)}</span>
        </div>
        {behind && (
          <div className="review-pending">
            <span className="review-pending-dot" />
            <span>
              <strong>Rev {detail.latest_rev} ready</strong>
              {pending > 0 && ` — ${pending} file${pending === 1 ? "" : "s"} changed ${pendingSince(pinned)}`}
            </span>
            <button
              type="button"
              className="btn"
              onClick={() => void advance()}
            >
              Advance
            </button>
          </div>
        )}
        {stale !== "none" && (
          <div className="review-stale">
            <span>
              The worktree has changed since this diff was rendered:{" "}
              <strong>{changedFiles.slice(0, STALE_FILES_NAMED).join(", ")}</strong>
              {changedFiles.length > STALE_FILES_NAMED
                && ` and ${changedFiles.length - STALE_FILES_NAMED} more`}
            </span>
            <button
              type="button"
              className="btn"
              onClick={() => (view === "" ? refetch() : goTo({ view: "" }))}
            >
              {view === "" ? "Refresh" : "Refresh to the working tree"}
            </button>
          </div>
        )}
        {view && (
          <div className="review-viewing">
            <span>
              You are viewing <strong>{viewLabel}</strong>
            </span>
            <button type="button" className="btn" onClick={() => goTo({ view: "" })}>
              Back to the working tree
            </button>
          </div>
        )}
        {detail.skipped.length > 0 && (
          <div className="review-skipped">
            {detail.skipped.length} untracked file{detail.skipped.length === 1 ? "" : "s"} not shown:{" "}
            {detail.skipped.map((s) => `${s.path} (${s.reason})`).join(", ")}
          </div>
        )}
      </div>

      <div
        className="review-body"
        style={{
          "--review-rail-w": `${railCollapsed ? REVIEW_RAIL_COLLAPSED_WIDTH : railWidth}px`,
        } as CSSProperties}
      >
        <div
          className={`review-rail ${railCollapsed ? "is-collapsed" : ""}`}
          ref={railRef}
        >
          <div className="review-rail-head">
            {!railCollapsed && (
              <>
                Files <span>{detail.files.length}</span>
              </>
            )}
            <button
              type="button"
              className="review-rail-fold"
              title={railCollapsed ? "show the file list" : "hide the file list"}
              aria-label={railCollapsed ? "show the file list" : "hide the file list"}
              aria-expanded={!railCollapsed}
              onClick={toggleRail}
            >
              {railCollapsed ? "\u00bb" : "\u00ab"}
            </button>
          </div>
          {!railCollapsed && detail.files.map((path) => {
            const marks = attention.get(path);
            return (
              <div
                key={path}
                data-rail-file={path}
                className={`review-rail-file ${viewed.has(path) ? "is-viewed" : ""}`}
              >
                <button
                  type="button"
                  className="review-rail-open"
                  onClick={() => goTo({ file: path })}
                >
                  <span className="review-rail-name">{path}</span>
                  {deletedFiles.has(path) && (
                    <span className="badge review-rail-deleted">deleted</span>
                  )}
                </button>
                {RAIL_GROUPS.map((group) => {
                  const count = marks?.[group].length ?? 0;
                  if (count === 0) return null;
                  return (
                    <button
                      key={group}
                      type="button"
                      className={`review-rail-count is-${group}`}
                      title={jumpLabel(count, group, path)}
                      aria-label={jumpLabel(count, group, path)}
                      onClick={() => jumpInFile(path, group)}
                    >
                      {count}
                    </button>
                  );
                })}
              </div>
            );
          })}
        </div>

        {!railCollapsed && (
          <div
            className={`review-rail-resizer ${railResizing ? "is-dragging" : ""}`}
            role="separator"
            aria-orientation="vertical"
            aria-label="resize file list"
            onPointerDown={startRailResize}
          />
        )}

        <div className="review-diff" ref={scrollRef} onScroll={onScrollSave}>
          {displayFiles.length === 0 && (
            <p className="muted-line review-empty">nothing to show in this view</p>
          )}
          {displayFiles.map((file) => {
            const threads = threadsByFile.get(file.path) ?? emptyFileThreads<ApiThread>();
            // The composer and the open reply belong to one file at a time,
            // so every other file is handed nothing rather than a value that
            // moves as the reader types.
            const ownsComposer = composing?.path === file.path;
            const ownsReply = replyOn !== null && threads.ids.has(replyOn);
            return (
              <ReviewFile
                key={file.path}
                reviewId={id}
                view={view}
                file={file}
                highlighted={highlighted.get(file.path)}
                isViewed={viewed.has(file.path)}
                showPreview={fileDefaultPreview(file.path, previewDefaults)}
                previewText={preview[file.path] ?? ""}
                threads={threads}
                diffSnapshot={diffSnapshot}
                latestRev={detail.latest_rev}
                behind={behind}
                composing={ownsComposer ? composing : null}
                composerKey={ownsComposer ? composerKey : null}
                initialDraftBody={ownsComposer ? initialDraftBody : ""}
                replyOn={ownsReply ? replyOn : null}
                replyKey={ownsReply ? replyKey : null}
                initialReplyBody={ownsReply ? initialReplyBody : ""}
                actions={actions}
              />
            );
          })}        </div>
      </div>
    </div>
  );
}

/** True when the pointer released without selecting anything, which is
 * what separates a click meaning "comment here" from a drag meaning
 * "let me copy this". */
function selectionIsCollapsed(): boolean {
  const selection = window.getSelection();
  return !selection || selection.isCollapsed || selection.toString().length === 0;
}

/** A dot per state a file actually holds, in the order a reader meets
 * them: what they owe, what the agent owes, then what is settled. */
const RAIL_GROUPS = ["yours", "theirs", "resolved"] as const;

/** The head pills' words, so a dot and a pill name a state alike. */
const GROUP_LABEL: Record<Exclude<ThreadGroup, "open">, string> = {
  yours: "your turn",
  theirs: "with agent",
  resolved: "resolved",
};

function jumpLabel(count: number, group: Exclude<ThreadGroup, "open">, path: string): string {
  const noun = count === 1 ? "thread" : "threads";
  return `${GROUP_LABEL[group]} — go to ${count} ${noun} in ${path}`;
}

/**
 * Puts an element where the reader was aiming and keeps it there. A file
 * below the fold is laid out at an estimated height until it comes close,
 * so a single scroll aims at geometry that changes underneath it: the
 * correction is re-applied until the target sits where it was asked to,
 * or until the scroll can go no further.
 */
function scrollToSettled(
  el: Element,
  container: HTMLElement | null,
  block: "start" | "center",
  tries = 20,
): void {
  if (!container) {
    el.scrollIntoView({ block });
    return;
  }
  const offBy = () => {
    const box = el.getBoundingClientRect();
    const view = container.getBoundingClientRect();
    const goal =
      block === "center"
        ? view.top + Math.max(0, (container.clientHeight - box.height) / 2)
        : view.top;
    return box.top - goal;
  };
  const step = (left: number) => {
    const delta = offBy();
    if (Math.abs(delta) <= 1 || left <= 0) return;
    container.scrollTop += delta;
    // A correction that cannot move yet is not a correction that failed:
    // the scroll runs out of document while the files below are still
    // laid out at an estimated height, and reaching them is what gives
    // them their real one.
    requestAnimationFrame(() => step(left - 1));
  };
  step(tries);
}

function cssId(path: string): string {
  return path.replace(/[^a-zA-Z0-9_-]/g, "_");
}

/** Scroll fires continuously; persisting on every frame would be a
 * write per pixel. */
/**
 * Puts the reader back where they were, reporting whether it took. A
 * scrollTop past the element's current end is clamped to it, so a caller
 * that treats the attempt as final strands the reader at the top while
 * the rest of the document is still arriving.
 */
/**
 * What a thread says about work that touched its file.
 *
 * `changed_ahead` covers the round being read as well as later ones, so the
 * reader's position decides which of the two it means. Advancing at the head
 * pins the revision already pinned and moves nothing, so it is never offered
 * there — but the fact still is, because it is what stops someone commenting
 * on a line that has moved underneath them.
 */
/**
 * Whether the reader has yet to be shown the newest round.
 *
 * A reader who has never advanced carries a pinned revision of zero. The
 * daemon reads that as the tree it handed the agent at Rev 1, which is
 * not Rev 1 as the agent gave it back: the first round an agent produces
 * is one the reader is behind. Defaulting the zero to one instead said
 * they were already at the head of a review whose head they had not
 * seen, and withheld the only control that would take them there.
 */
export function readerIsBehind(latestRev: number, pinnedRev: number): boolean {
  return latestRev > pinnedRev;
}

/** Where the reader sits, named the way the revision picker names it. */
export function readingLabel(pinnedRev: number): string {
  return pinnedRev === 0 ? "reading Rev 1 as sent" : `reading Rev ${pinnedRev}`;
}

/** What the files waiting for the reader have changed since. */
export function pendingSince(pinnedRev: number): string {
  return pinnedRev === 0 ? "since the review opened" : "since you pinned";
}

export type AheadMarker = "none" | "advance" | "current";

export function aheadMarker(
  changedAhead: boolean,
  latestRev: number,
  behind: boolean,
): AheadMarker {
  if (!changedAhead || latestRev <= 0) return "none";
  return behind ? "advance" : "current";
}

export function replyDraftKey(threadId: number): string {
  return `reply:${threadId}`;
}

export function createDraftDebouncer(
  delayMs: number,
  saveFn: (key: string, body: string) => void,
) {
  const timers = new Map<string, ReturnType<typeof setTimeout>>();
  return {
    save(key: string, body: string) {
      const existing = timers.get(key);
      if (existing) clearTimeout(existing);
      timers.set(
        key,
        setTimeout(() => {
          timers.delete(key);
          saveFn(key, body);
        }, delayMs),
      );
    },
    clear(key: string) {
      const timer = timers.get(key);
      if (timer) {
        clearTimeout(timer);
        timers.delete(key);
      }
    },
    hasPending(key: string) {
      return timers.has(key);
    },
    cancelAll() {
      for (const timer of timers.values()) {
        clearTimeout(timer);
      }
      timers.clear();
    },
  };
}

export function applyScrollRestore(el: { scrollTop: number }, top: number): boolean {
  el.scrollTop = top;
  return Math.abs(el.scrollTop - top) <= 1;
}

function debounced(fn: () => void): () => void {
  let timer: ReturnType<typeof setTimeout> | null = null;
  return () => {
    if (timer) clearTimeout(timer);
    timer = setTimeout(fn, 400);
  };
}

export function ReviewComposer({
  initialBody,
  placeholder,
  className = "",
  saveDraft,
  onSubmit,
  onCancel,
}: {
  initialBody: string;
  placeholder: string;
  className?: string;
  saveDraft: (body: string) => void;
  onSubmit: (body: string, send: boolean) => void;
  onCancel: () => void;
}) {
  const [body, setBody] = useState(initialBody);

  const onChange = (next: string) => {
    setBody(next);
    saveDraft(next);
  };

  return (
    <div className={`review-compose ${className}`.trim()}>
      <textarea
        autoFocus
        value={body}
        placeholder={placeholder}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if ((e.metaKey || e.ctrlKey) && e.key === "Enter") onSubmit(body, true);
          if (e.key === "Escape") onCancel();
        }}
      />
      <div className="review-compose-actions">
        <button type="button" className="btn" onClick={onCancel}>
          Cancel
        </button>
        <button type="button" className="btn" onClick={() => onSubmit(body, false)}>
          Save as draft
        </button>
        <button type="button" className="btn btn-primary" onClick={() => onSubmit(body, true)}>
          Send
        </button>
      </div>
    </div>
  );
}

/** What a file section can do. None of it depends on which file it is, so
 * it is built once and shared, which is what keeps a file out of the render
 * when its neighbour is marked viewed. */
export interface ReviewFileActions {
  setComposing: (composing: Composing | null) => void;
  cancelCompose: () => void;
  setDraft: (key: string | null, body: string) => void;
  submitComment: (bodyOrSend: string | boolean, send?: boolean) => void;
  setReplyOn: (v: number | null) => void;
  reply: (threadId: number, body: string) => void;
  resolve: (threadId: number, resolved: boolean) => void;
  remove: (threadId: number) => void;
  sendThread: (threadId: number) => void;
  openChanges: (rev: number, threadId: number) => void;
  advance: () => void;
  markSeen: (threadId: number, messageId: number) => void;
  setViewed: (path: string, next: boolean) => void;
  togglePreview: (path: string, on: boolean) => void;
  answerChoice: (
    path: string,
    range: BlockRange,
    choice: ChoiceList,
    answer: ChoiceAnswer,
  ) => void;
  sendChoice: (path: string, choiceId: string) => void;
  previewComposer: (path: string, range: BlockRange) => Composing;
}

interface ReviewFileProps {
  reviewId: number | string;
  view: string;
  file: DiffFile;
  /// Highlighted HTML per row, or undefined for a file rendered plainly.
  highlighted: (string | null)[] | undefined;
  isViewed: boolean;
  showPreview: boolean;
  previewText: string;
  threads: FileThreads<ApiThread>;
  diffSnapshot: bigint | null;
  latestRev: number;
  behind: boolean;
  composing: Composing | null;
  composerKey: string | null;
  initialDraftBody: string;
  replyOn: number | null;
  replyKey: string | null;
  initialReplyBody: string;
  actions: ReviewFileActions;
}

const ROW_CHUNK_SIZE = 50;

/**
 * One file of the review.
 *
 * Memoised because marking any file viewed replaces the review payload and
 * re-renders the page. Without this, React rebuilt and re-committed every
 * line of every open file to change one checkbox, which is most of what
 * marking a file viewed used to cost.
 */
const ReviewFile = memo(function ReviewFile(p: ReviewFileProps) {
  const { file, threads, actions, isViewed, showPreview } = p;

  const rowChunks = useMemo(() => {
    const list: DiffRow[][] = [];
    for (let i = 0; i < file.rows.length; i += ROW_CHUNK_SIZE) {
      list.push(file.rows.slice(i, i + ROW_CHUNK_SIZE));
    }
    return list;
  }, [file.rows]);

  // A file with nothing left to diff is in the view only because a
  // comment sits on it: the edit that comment asked for took the last
  // difference with it.
  const unchanged =
    !file.binary && !file.unreadable && !file.deleted && file.added === 0 && file.removed === 0;
  // Once those comments are settled there is nothing to read, so it
  // starts folded rather than holding the space. Deliberately not
  // recomputed as threads resolve, because folding a file away under
  // someone who is still reading it is worse than the space it costs.
  const [folded, setFolded] = useState(
    () =>
      (unchanged || file.deleted) &&
      threads.threads.length > 0 &&
      threads.threads.every((t) => t.state === "resolved"),
  );

  // Every thread reaches the reader. One whose line is not rendered —
  // a comment on the far side of a view, or on a line no hunk covers —
  // is shown under the file rather than dropped with its anchor.
  const strays = useMemo(
    () =>
      showPreview || isViewed || folded
        ? []
        : strayThreads(file.rows, file.path, threads.threads),
    [file.rows, file.path, threads, showPreview, isViewed, folded],
  );

  // Every thread lands on exactly one rendered block: the one holding its
  // line, else the last block starting before it, else the first. A comment
  // anchored to a blank line between blocks is still shown rather than lost.
  const previewBlocks = useMemo(() => {
    const byBlock = new Map<number, ApiThread[]>();
    if (!showPreview) return byBlock;
    // Anchors, not blocks: a list anchors per item, so keying by block
    // would file a comment made on one bullet under the first one.
    const anchors = markdownAnchors(p.previewText);
    for (const t of threads.threads) {
      const hit =
        anchors.find((a) => t.current_line >= a.start && t.current_line <= a.end) ??
        [...anchors].reverse().find((a) => a.start <= t.current_line) ??
        anchors[0];
      if (!hit) continue;
      byBlock.set(hit.start, [...(byBlock.get(hit.start) ?? []), t]);
    }
    return byBlock;
  }, [showPreview, p.previewText, threads]);

  const threadCard = (t: ApiThread, showExcerpt = false) => (
    <ThreadCard
      key={t.id}
      thread={t}
      replyOn={p.replyOn}
      initialReplyBody={p.initialReplyBody}
      replyKey={p.replyKey}
      setReplyOn={actions.setReplyOn}
      setDraft={actions.setDraft}
      onReply={actions.reply}
      onResolve={actions.resolve}
      onDelete={actions.remove}
      onSendThread={actions.sendThread}
      onOpenChanges={actions.openChanges}
      latestRev={p.latestRev}
      behind={p.behind}
      onAdvance={actions.advance}
      onSeen={actions.markSeen}
      showExcerpt={showExcerpt}
    />
  );

  return (
    <section
      id={`review-file-${cssId(file.path)}`}
      data-file={file.path}
      className={`review-file ${isViewed ? "is-viewed" : ""}`}
    >
      <header className="review-file-head">
        <span className="review-file-path">{file.path}</span>
        {file.deleted ? (
          <button
            type="button"
            className="review-file-stat review-file-fold is-deleted"
            title={folded ? "show this file" : "hide this file"}
            aria-expanded={!folded}
            onClick={() => setFolded(!folded)}
          >
            deleted file
          </button>
        ) : unchanged ? (
          <button
            type="button"
            className="review-file-stat review-file-fold"
            title={folded ? "show this file" : "hide this file"}
            aria-expanded={!folded}
            onClick={() => setFolded(!folded)}
          >
            no changes at this revision
          </button>
        ) : (
          <span className="review-file-stat">
            <span className="plus">+{file.added}</span> <span className="minus">−{file.removed}</span>
          </span>
        )}
        {(isMarkdown(file.path) || isImage(file.path)) && (
          <label className="review-toggle">
            <input
              type="checkbox"
              checked={showPreview}
              onChange={(e) => actions.togglePreview(file.path, e.target.checked)}
            />
            {isImage(file.path) ? "Image preview" : "Render preview"}
          </label>
        )}
        <label className="review-toggle">
          <input
            type="checkbox"
            checked={isViewed}
            onChange={(e) => actions.setViewed(file.path, e.target.checked)}
          />
          Viewed
        </label>
      </header>
      {!isViewed && !folded && file.binary && !isImage(file.path) && (
        <p className="muted-line review-empty">binary file</p>
      )}
      {!isViewed && !folded && file.binary && isImage(file.path) && !showPreview && (
        <p className="muted-line review-empty">binary image (preview toggled off)</p>
      )}
      {!isViewed && !folded && file.unreadable && (
        <p className="muted-line review-empty">
          the {file.unreadable} of this file could not be read, so no diff is
          shown
        </p>
      )}
      {!isViewed && !folded && showPreview && isImage(file.path) && (
        <ReviewImage
          reviewId={p.reviewId}
          file={file}
          view={p.view}
          diffSnapshot={p.diffSnapshot}
          threads={threads.threads}
          composing={p.composing}
          composerKey={p.composerKey}
          initialDraftBody={p.initialDraftBody}
          actions={actions}
          threadCard={threadCard}
        />
      )}
      {!isViewed && !folded && showPreview && isMarkdown(file.path) && (
        <div className="review-preview">
          <Markdown
            text={p.previewText}
            onPmLink={() => undefined}
            renderBlock={(block, range, choice) => {
              // A rendered block is a comment anchor the way a diff
              // line is: it carries the source lines it came from, and
              // a comment placed on it lands on the first of them, so
              // preview and source comments share one anchor space.
              const blockThreads = previewBlocks.get(range.start) ?? [];
              if (choice) {
                const held = latestChoiceThread(threads.threads, choice.id);
                // The card is the answer's own display, so its
                // threads are not repeated underneath. Anything
                // else said about this block still shows.
                const said = blockThreads.filter(
                  (t) => !t.messages.some((m) => m.choice?.choice_id === choice.id),
                );
                return (
                  <div className="review-preview-block">
                    <ChoiceCard
                      choice={choice}
                      answer={held?.answer ?? null}
                      threadState={held?.thread.state ?? null}
                      anchor={held?.thread.anchor_status ?? "same"}
                      onAnswer={(answer) =>
                        actions.answerChoice(file.path, range, choice, answer)
                      }
                      onSend={() => actions.sendChoice(file.path, choice.id)}
                    />
                    {said.map((t) => threadCard(t))}
                  </div>
                );
              }
              const composingHere =
                p.composing !== null &&
                p.composing.line >= range.start &&
                p.composing.line <= range.end;
              return (
                <div className="review-preview-block">
                  <div
                    className="review-preview-body"
                    role="button"
                    tabIndex={0}
                    title="Comment on this block"
                    onMouseUp={(e) => {
                      // A link, an open thread or the composer inside
                      // the block is not an anchor for a new comment.
                      if ((e.target as HTMLElement).closest("a, .review-thread, .review-compose")) return;
                      if (!selectionIsCollapsed()) return;
                      actions.setComposing(actions.previewComposer(file.path, range));
                    }}
                    onKeyDown={(e) => {
                      if (e.target !== e.currentTarget) return;
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        actions.setComposing(actions.previewComposer(file.path, range));
                      }
                    }}
                  >
                    {block}
                    {blockThreads.length > 0 && (
                      <span className="review-preview-marker" aria-hidden="true">
                        {blockThreads.length > 1 ? `${blockThreads.length}` : ""}
                      </span>
                    )}
                  </div>
                  {composingHere && (
                    <ReviewComposer
                      initialBody={p.initialDraftBody}
                      placeholder="Comment on this block…"
                      saveDraft={(body) => actions.setDraft(p.composerKey, body)}
                      onSubmit={(body, send) => actions.submitComment(body, send)}
                      onCancel={actions.cancelCompose}
                    />
                  )}
                  {blockThreads.map((t) => threadCard(t))}
                </div>
              );
            }}
          />
        </div>
      )}
      {!isViewed && !folded && !showPreview && !file.binary && (
        <div className="review-rows">
          {rowChunks.map((chunk, chunkIdx) => {
            const ownsComposing = p.composing !== null && p.composing.path === file.path;
            const chunkHasComposer =
              ownsComposing &&
              chunk.some((r) => {
                const line = r.newLine ?? r.oldLine ?? 0;
                const side = r.newLine !== null ? ReviewSide.RIGHT : ReviewSide.LEFT;
                return p.composing!.line === line && p.composing!.side === side;
              });

            const ownsReply = p.replyOn !== null && threads.ids.has(p.replyOn);
            const chunkHasReply =
              ownsReply &&
              threads.threads.some((t) => {
                if (t.id !== p.replyOn) return false;
                return chunk.some((r) => (r.newLine ?? r.oldLine ?? 0) === t.current_line);
              });

            return (
              <DiffChunk
                key={chunkIdx}
                startIndex={chunkIdx * ROW_CHUNK_SIZE}
                rows={chunk}
                path={file.path}
                highlighted={p.highlighted}
                threads={threads.byAnchor}
                diffSnapshot={p.diffSnapshot}
                composing={chunkHasComposer ? p.composing : null}
                composerKey={chunkHasComposer ? p.composerKey : null}
                initialDraftBody={chunkHasComposer ? p.initialDraftBody : ""}
                replyOn={chunkHasReply ? p.replyOn : null}
                replyKey={chunkHasReply ? p.replyKey : null}
                initialReplyBody={chunkHasReply ? p.initialReplyBody : ""}
                actions={actions}
                latestRev={p.latestRev}
                behind={p.behind}
              />
            );
          })}
        </div>
      )}
      {strays.length > 0 && (
        <div className="review-strays">
          <p className="muted-line">
            {file.deleted
              ? strays.length === 1
                ? "thread on deleted file"
                : `${strays.length} threads on deleted file`
              : strays.length === 1
                ? "a comment on code this view does not show"
                : `${strays.length} comments on code this view does not show`}
          </p>
          {strays.map((t) => threadCard(t, true))}
        </div>
      )}
    </section>
  );
});

interface DiffChunkProps {
  startIndex: number;
  rows: DiffRow[];
  path: string;
  highlighted?: (string | null)[];
  threads: Map<string, ApiThread[]>;
  diffSnapshot: bigint | null;
  composing: Composing | null;
  composerKey: string | null;
  initialDraftBody: string;
  replyOn: number | null;
  replyKey: string | null;
  initialReplyBody: string;
  actions: ReviewFileActions;
  latestRev: number;
  behind: boolean;
}

const DiffChunk = memo(function DiffChunk(p: DiffChunkProps) {
  const {
    startIndex,
    rows,
    path,
    highlighted,
    threads,
    diffSnapshot,
    composing,
    composerKey,
    initialDraftBody,
    replyOn,
    replyKey,
    initialReplyBody,
    actions,
    latestRev,
    behind,
  } = p;

  return (
    <div className="review-chunk">
      {rows.map((row, i) => {
        const rowIdx = startIndex + i;
        return (
          <DiffLine
            key={rowIdx}
            row={row}
            path={path}
            html={highlighted?.[rowIdx] ?? null}
            threads={threads}
            diffSnapshot={diffSnapshot}
            composing={composing}
            composerKey={composerKey}
            initialDraftBody={initialDraftBody}
            replyOn={replyOn}
            replyKey={replyKey}
            initialReplyBody={initialReplyBody}
            actions={actions}
            latestRev={latestRev}
            behind={behind}
          />
        );
      })}
    </div>
  );
});

interface DiffLineProps {
  row: DiffRow;
  path: string;
  /// Highlighted HTML for this row, or null to render its text plainly.
  html: string | null;
  threads: Map<string, ApiThread[]>;
  diffSnapshot: bigint | null;
  composing: Composing | null;
  composerKey: string | null;
  initialDraftBody: string;
  replyOn: number | null;
  replyKey: string | null;
  initialReplyBody: string;
  actions: ReviewFileActions;
  latestRev: number;
  behind: boolean;
}

const DiffLine = memo(function DiffLine(p: DiffLineProps) {
  const { row, path, actions, diffSnapshot } = p;
  if (row.kind === "hunk") {
    return <div className="review-hunk">{row.text}</div>;
  }
  const side = row.newLine !== null ? ReviewSide.RIGHT : ReviewSide.LEFT;
  const line = row.newLine ?? row.oldLine ?? 0;
  const threads =
    p.threads.get(anchorKey(path, line, side === ReviewSide.RIGHT ? "right" : "left")) ?? [];
  const isComposing =
    p.composing?.path === path && p.composing.line === line && p.composing.side === side;

  return (
    <>
      <div
        className={`review-row is-${row.kind}`}
        role="button"
        tabIndex={0}
        title="Comment on this line"
        onMouseUp={() => {
          // Selecting text across a line is not a request to comment on
          // it, so a drag that leaves a selection is left alone.
          if (!selectionIsCollapsed()) return;
          actions.setComposing({
            path,
            line,
            side,
            excerpt: row.text,
            snapshot: diffSnapshot,
          });
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            actions.setComposing({
              path,
              line,
              side,
              excerpt: row.text,
              snapshot: diffSnapshot,
            });
          }
        }}
      >
        <span className="review-gutter">{row.oldLine ?? ""}</span>
        <span className="review-gutter is-new">{row.newLine ?? ""}</span>
        <span className="review-sign">
          {row.kind === "add" ? "+" : row.kind === "del" ? "−" : " "}
        </span>
        {p.html === null ? (
          <span className="review-code">{row.text || " "}</span>
        ) : (
          // highlight.js escapes the source it is given, so the only
          // markup here is the spans it added.
          <span className="review-code" dangerouslySetInnerHTML={{ __html: p.html || " " }} />
        )}
      </div>
      {isComposing && (
        <ReviewComposer
          initialBody={p.initialDraftBody}
          placeholder="Comment on this line…"
          saveDraft={(body) => actions.setDraft(p.composerKey, body)}
          onSubmit={(body, send) => actions.submitComment(body, send)}
          onCancel={actions.cancelCompose}
        />
      )}
      {threads.map((t) => (
        <ThreadCard
          key={t.id}
          thread={t}
          replyOn={p.replyOn}
          initialReplyBody={p.initialReplyBody}
          replyKey={p.replyKey}
          setReplyOn={actions.setReplyOn}
          setDraft={actions.setDraft}
          onReply={actions.reply}
          onResolve={actions.resolve}
          onDelete={actions.remove}
          onSendThread={actions.sendThread}
          onOpenChanges={actions.openChanges}
          latestRev={p.latestRev}
          behind={p.behind}
          onAdvance={actions.advance}
          onSeen={actions.markSeen}
        />
      ))}
    </>
  );
});

function ThreadCard({
  thread,
  behind,
  replyOn,
  initialReplyBody,
  replyKey,
  setReplyOn,
  setDraft,
  onReply,
  onResolve,
  onDelete,
  onSendThread,
  onOpenChanges,
  latestRev,
  onAdvance,
  onSeen,
  showExcerpt = false,
}: {
  thread: ApiThread;
  replyOn: number | null;
  initialReplyBody: string;
  replyKey: string | null;
  setReplyOn: (v: number | null) => void;
  setDraft: (key: string | null, body: string) => void;
  onReply: (threadId: number, body: string) => void;
  onResolve: (threadId: number, resolved: boolean) => void;
  onDelete: (threadId: number) => void;
  onSendThread: (threadId: number) => void;
  onOpenChanges: (rev: number, threadId: number) => void;
  latestRev: number;
  behind: boolean;
  onAdvance: () => void;
  onSeen: (threadId: number, messageId: number) => void;
  showExcerpt?: boolean;
}) {
  const note = anchorNote(thread.anchor_status, thread.line, thread.current_line);
  const resolved = thread.state === "resolved";
  // A resolved thread is settled business, so it stops taking up the
  // diff. It stays reachable, because "resolved" is not "gone".
  const [expanded, setExpanded] = useState(false);
  const cardRef = useRef<HTMLDivElement>(null);
  const newest = thread.messages.length
    ? thread.messages[thread.messages.length - 1].id
    : 0;
  useEffect(() => {
    const node = cardRef.current;
    if (!node || !newest) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) onSeen(thread.id, newest);
      },
      { threshold: 0.4 },
    );
    observer.observe(node);
    return () => observer.disconnect();
  }, [newest, thread.id, onSeen]);

  const isReplying = replyOn === thread.id;
  const [replyText, setReplyText] = useState(initialReplyBody);
  useEffect(() => {
    if (isReplying) {
      setReplyText(initialReplyBody);
    }
  }, [isReplying, initialReplyBody]);

  if (resolved && !expanded) {
    const first = thread.messages[0];
    return (
      <div
        className="review-thread is-resolved is-collapsed"
        id={`review-thread-${thread.id}`}
        ref={cardRef}
      >
        <button
          type="button"
          className="review-thread-reopen"
          onClick={() => setExpanded(true)}
        >
          <span className="review-thread-check">✓</span>
          <span className="review-thread-gist">{first?.body ?? "resolved"}</span>
          <span className="review-thread-show">show</span>
        </button>
      </div>
    );
  }
  const marker = aheadMarker(thread.changed_ahead, latestRev, behind);

  return (
    <div
      className={`review-thread is-${thread.state} is-${threadTurn(thread)}`}
      id={`review-thread-${thread.id}`}
      ref={cardRef}
    >
      {resolved && (
        <button
          type="button"
          className="review-thread-reopen is-open"
          onClick={() => setExpanded(false)}
        >
          <span className="review-thread-check">✓</span>
          <span className="review-thread-gist">resolved</span>
          <span className="review-thread-show">hide</span>
        </button>
      )}
      {showExcerpt && thread.excerpt && (
        <div className="review-thread-excerpt">
          <span className="review-thread-excerpt-line">Line {thread.line}</span>
          <code>{thread.excerpt}</code>
        </div>
      )}
      {thread.messages.map((m) => (
        <div key={m.id} className={`review-msg ${m.author === "session" ? "is-agent" : ""}`}>
          <div className="review-msg-head">
            <strong>{m.author === "session" ? "agent" : "you"}</strong>
            {!m.addressed && m.author === "session" && (
              <span className="badge st-failed">not addressed</span>
            )}
            {m.author === "session" && m.changes_rev > 0 && m.changed_files.length > 0 && (
              <button
                type="button"
                className="review-changes-link"
                onClick={() => onOpenChanges(m.changes_rev, thread.id)}
              >
                ↗ changes in Rev {m.changes_rev} · {m.changed_files.length} file
                {m.changed_files.length === 1 ? "" : "s"}
              </button>
            )}
          </div>
          <div className="review-msg-body">{m.body}</div>
        </div>
      ))}
      {note && <div className="review-thread-note">{note}</div>}
      {marker === "advance" && (
        <button type="button" className="review-thread-ahead" onClick={onAdvance}>
          Updates in Rev {latestRev} · click to update diff
        </button>
      )}
      {marker === "current" && (
        <p className="review-thread-ahead is-current">changed in this round</p>
      )}
      {isReplying && (
        <textarea
          autoFocus
          className="review-reply"
          value={replyText}
          placeholder="Reply…"
          onChange={(e) => {
            setReplyText(e.target.value);
            setDraft(replyKey, e.target.value);
          }}
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === "Enter") onReply(thread.id, replyText);
            if (e.key === "Escape") setReplyOn(null);
          }}
        />
      )}
      <div className="review-thread-actions">
        {thread.state === "draft" && (
          <button type="button" className="btn btn-primary" onClick={() => onSendThread(thread.id)}>
            Send
          </button>
        )}
        {isReplying ? (
          <>
            <button
              type="button"
              className="btn btn-primary"
              onClick={() => onReply(thread.id, replyText)}
            >
              Send reply
            </button>
            <button type="button" className="btn" onClick={() => setReplyOn(null)}>
              Cancel
            </button>
          </>
        ) : (
          <button type="button" className="btn" onClick={() => setReplyOn(thread.id)}>
            Reply
          </button>
        )}
        <div className="review-thread-spacer" />
        <button type="button" className="btn" onClick={() => onResolve(thread.id, !resolved)}>
          {resolved ? "Reopen" : "Resolve"}
        </button>
        <button type="button" className="btn btn-danger" onClick={() => onDelete(thread.id)}>
          Delete
        </button>
      </div>
    </div>
  );
}
