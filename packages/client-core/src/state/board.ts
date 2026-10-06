import { ItemPriority, ItemStatus, SessionState, type Item, type Project, type Session } from "../gen/pm/v1/pm_pb";
import { itemKey } from "./reducer";

export const DONE_RECENTLY_MS = 7 * 24 * 60 * 60 * 1000;

export interface BoardMetricCounts {
  needsYou: number;
  inProgress: number;
  planned: number;
  blockedExternal: number;
  doneRecently: number;
  liveLinked: number;
}

// Board grouping: which section of a bucket's board each item renders
// in, and the order within a section.

export type BoardGroup = "needsYou" | "inProgress" | "blockedExternal" | "planned" | "closed";

export interface BoardModel {
  needsYou: Item[];
  inProgress: Item[];
  blockedExternal: Item[];
  planned: Item[];
  /** Done or dropped, newest first. */
  closed: Item[];
  /** Snoozed items, parked out of every other group. */
  snoozed: Item[];
}

export interface ResolvedBlocker {
  id: bigint;
  title: string;
}

export interface BlockerIndicator {
  blockers: ResolvedBlocker[];
  label: string;
  title: string;
}

const PRIORITY_RANK: Record<number, number> = {
  [ItemPriority.URGENT]: 0,
  [ItemPriority.HIGH]: 1,
  [ItemPriority.NORMAL]: 2,
  [ItemPriority.LOW]: 3,
};

export function priorityRank(priority: ItemPriority): number {
  return PRIORITY_RANK[priority] ?? 4;
}

export function isSnoozed(item: Item, nowMs: number): boolean {
  return item.snoozedUntilUnixMs !== undefined && Number(item.snoozedUntilUnixMs) > nowMs;
}

function group(item: Item): BoardGroup {
  if (item.question.length > 0) return "needsYou";
  switch (item.status) {
    case ItemStatus.INBOX:
    case ItemStatus.BLOCKED:
      return "needsYou";
    case ItemStatus.IN_PROGRESS:
      return "inProgress";
    case ItemStatus.BLOCKED_EXTERNAL:
      return "blockedExternal";
    case ItemStatus.DONE:
    case ItemStatus.DROPPED:
      return "closed";
    default:
      return "planned";
  }
}

function byUrgency(a: Item, b: Item): number {
  const rank = priorityRank(a.priority) - priorityRank(b.priority);
  if (rank !== 0) return rank;
  const dueA = a.dueAtUnixMs !== undefined ? Number(a.dueAtUnixMs) : Number.MAX_SAFE_INTEGER;
  const dueB = b.dueAtUnixMs !== undefined ? Number(b.dueAtUnixMs) : Number.MAX_SAFE_INTEGER;
  if (dueA !== dueB) return dueA - dueB;
  return Number(a.id - b.id);
}

function byRecency(a: Item, b: Item): number {
  return Number(b.updatedAtUnixMs - a.updatedAtUnixMs) || Number(b.id - a.id);
}

export function buildBoard(
  items: Iterable<Item>,
  bucketId: string,
  nowMs: number,
  projectId?: string,
  preserveQueryOrder = false,
): BoardModel {
  const model: BoardModel = {
    needsYou: [],
    inProgress: [],
    blockedExternal: [],
    planned: [],
    closed: [],
    snoozed: [],
  };
  for (const item of items) {
    if (item.bucketId.toString() !== bucketId) continue;
    if (projectId !== undefined && item.projectId?.toString() !== projectId) continue;
    if (isSnoozed(item, nowMs) && group(item) !== "closed") {
      model.snoozed.push(item);
      continue;
    }
    model[group(item)].push(item);
  }
  if (!preserveQueryOrder) {
    model.needsYou.sort(byUrgency);
    model.inProgress.sort(byUrgency);
    model.blockedExternal.sort(byUrgency);
    model.planned.sort(byUrgency);
    model.snoozed.sort(byUrgency);
    model.closed.sort(byRecency);
  }
  return model;
}

/** Items needing the user for a bucket, used by the sidebar's board badge. */
export function needsYouCount(items: Iterable<Item>, bucketId: string, nowMs: number): number {
  let count = 0;
  for (const item of items) {
    if (item.bucketId.toString() !== bucketId) continue;
    if (isSnoozed(item, nowMs)) continue;
    if (group(item) === "needsYou") count += 1;
  }
  return count;
}

export function boardMetricCounts(
  items: Iterable<Item>,
  sessions: Iterable<Session>,
  bucketId: string,
  nowMs: number,
): BoardMetricCounts {
  const liveSessionIds = new Set(
    [...sessions]
      .filter((session) => session.state !== SessionState.EXITED && session.state !== SessionState.FAILED)
      .map((session) => session.id.toString()),
  );
  const counts: BoardMetricCounts = {
    needsYou: 0,
    inProgress: 0,
    planned: 0,
    blockedExternal: 0,
    doneRecently: 0,
    liveLinked: 0,
  };
  for (const item of items) {
    if (item.bucketId.toString() !== bucketId) continue;
    if (!isSnoozed(item, nowMs) && group(item) === "needsYou") counts.needsYou += 1;
    if (item.status === ItemStatus.IN_PROGRESS) counts.inProgress += 1;
    if (item.status === ItemStatus.PLANNED) counts.planned += 1;
    if (item.status === ItemStatus.BLOCKED_EXTERNAL) counts.blockedExternal += 1;
    if (item.status === ItemStatus.DONE && Number(item.updatedAtUnixMs) >= nowMs - DONE_RECENTLY_MS) counts.doneRecently += 1;
    if (item.sessionIds.some((id) => liveSessionIds.has(id.toString()))) counts.liveLinked += 1;
  }
  return counts;
}

export function resolveProjectName(
  item: Item,
  projects: ReadonlyMap<string, Project>,
): string | null {
  if (item.projectId === undefined) return null;
  return projects.get(item.projectId.toString())?.name ?? null;
}

export function resolveBlockerIndicator(
  item: Item,
  items: ReadonlyMap<string, Item>,
): BlockerIndicator | null {
  const blockers = item.blockedBy.flatMap((id): ResolvedBlocker[] => {
    const blocker = items.get(itemKey(item.bucketId, id));
    if (blocker?.status === ItemStatus.DONE || blocker?.status === ItemStatus.DROPPED) return [];
    return [{ id, title: blocker?.title || id.toString() }];
  });
  if (blockers.length === 0) return null;

  return {
    blockers,
    label:
      blockers.length === 1
        ? `blocked by: ${blockers[0].title}`
        : `blocked by ${blockers.length} items`,
    title: blockers.map((blocker) => `${blocker.title} — pm:item/${item.bucketId}/${blocker.id}`).join("\n"),
  };
}

export const STATUS_LABELS: ReadonlyMap<ItemStatus, string> = new Map([
  [ItemStatus.INBOX, "inbox"],
  [ItemStatus.PLANNED, "planned"],
  [ItemStatus.IN_PROGRESS, "in progress"],
  [ItemStatus.BLOCKED, "blocked"],
  [ItemStatus.BLOCKED_EXTERNAL, "blocked external"],
  [ItemStatus.DONE, "done"],
  [ItemStatus.DROPPED, "dropped"],
]);

export const PRIORITY_LABELS: ReadonlyMap<ItemPriority, string> = new Map([
  [ItemPriority.URGENT, "urgent"],
  [ItemPriority.HIGH, "high"],
  [ItemPriority.NORMAL, "normal"],
  [ItemPriority.LOW, "low"],
]);
