import { sessionEnded, sessionLastActiveAt } from "@puppet-master/client-core/format";
import { SessionState, type Session } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { awaitsUser } from "@puppet-master/client-core/state/displayState";
import type { AppState } from "@puppet-master/client-core/state/reducer";
import type { SidebarModel } from "@puppet-master/client-core/state/sidebar";

/** Which part of the fleet the sidebar lists. */
export type SidebarScope =
  | { kind: "all" }
  | { kind: "needs" }
  | { kind: "bucket"; bucketId: string };

export const ALL_SCOPE: SidebarScope = { kind: "all" };

const BUCKET_PREFIX = "bucket:";

export function scopeKey(scope: SidebarScope): string {
  return scope.kind === "bucket" ? `${BUCKET_PREFIX}${scope.bucketId}` : scope.kind;
}

export function parseScope(raw: string | null): SidebarScope {
  if (raw === "needs") return { kind: "needs" };
  if (raw?.startsWith(BUCKET_PREFIX) && raw.length > BUCKET_PREFIX.length) {
    return { kind: "bucket", bucketId: raw.slice(BUCKET_PREFIX.length) };
  }
  return ALL_SCOPE;
}

/** A stored bucket scope whose bucket is gone falls back to every bucket. */
export function effectiveScope(scope: SidebarScope, bucketIds: ReadonlySet<string>): SidebarScope {
  return scope.kind === "bucket" && !bucketIds.has(scope.bucketId) ? ALL_SCOPE : scope;
}

/** Keeps only the scoped bucket, or only sessions needing the user. */
export function scopeSidebar(
  model: SidebarModel,
  scope: SidebarScope,
  sessions: ReadonlyMap<string, Session>,
): SidebarModel {
  if (scope.kind === "all") return model;
  if (scope.kind === "bucket") {
    return {
      buckets: model.buckets.filter(({ bucket }) => bucket.id.toString() === scope.bucketId),
      orphans: [],
      order: model.order,
    };
  }
  const needsInput = (session: Session) => awaitsUser(session, (id) => sessions.get(id));
  const buckets = model.buckets
    .map(({ bucket, supervisors, sessions }) => {
      const kept = sessions.filter(needsInput);
      const parents = new Set(kept.map((session) => session.spawnedBySessionId?.toString()));
      return {
        bucket,
        sessions: kept,
        supervisors: supervisors.filter(
          (supervisor) => needsInput(supervisor) || parents.has(supervisor.id.toString()),
        ),
      };
    })
    .filter(({ supervisors, sessions }) => supervisors.length > 0 || sessions.length > 0);
  return { buckets, orphans: model.orphans.filter(needsInput), order: model.order };
}

/** How long an idle session goes untouched before a bucket may fold it under Older. */
export const STALE_IDLE_MS = 24 * 60 * 60 * 1000;
/** A bucket keeps at least this many rows showing, topping up with its most recent older ones. */
export const RECENT_ROW_MINIMUM = 5;

function lastActiveAt(session: Session): number {
  const last = sessionEnded(session) ? Number(session.endedAtUnixMs) : sessionLastActiveAt(session);
  return last || Number(session.createdAtUnixMs);
}

/**
 * True when every session in the row is idle or ended and none has been active
 * for a day. A row holding the selected session is never stale, so the sidebar
 * keeps showing what the main pane does.
 */
export function isStaleRow(members: readonly Session[], now: number, selectedId: string | null): boolean {
  return members.every((session) => {
    if (session.id.toString() === selectedId) return false;
    if (!sessionEnded(session) && session.state !== SessionState.IDLE) return false;
    return now - lastActiveAt(session) >= STALE_IDLE_MS;
  });
}

/**
 * The rows a bucket folds under Older, in their listed order: stale rows,
 * less the most recent of them needed to keep RECENT_ROW_MINIMUM rows showing.
 */
export function olderRows<T>(
  rows: readonly T[],
  members: (row: T) => readonly Session[],
  now: number,
  selectedId: string | null,
): T[] {
  const stale = rows.filter((row) => isStaleRow(members(row), now, selectedId));
  const topUp = Math.max(0, RECENT_ROW_MINIMUM - (rows.length - stale.length));
  const rowAt = (row: T) => Math.max(...members(row).map(lastActiveAt));
  const kept = new Set([...stale].sort((a, b) => rowAt(b) - rowAt(a)).slice(0, topUp));
  return stale.filter((row) => !kept.has(row));
}

export { bucketTallies, type BucketTally } from "@puppet-master/client-core/state/buckets";

export function bucketOfSession(
  state: Pick<AppState, "projects" | "sessions">,
  sessionId: string,
): string | null {
  const session = state.sessions.get(sessionId);
  if (!session) return null;
  return state.projects.get(session.projectId.toString())?.bucketId.toString() ?? null;
}
