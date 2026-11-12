import { sessionEnded, sessionLastActiveAt } from "@puppet-master/client-core/format";
import { SessionState, type Session } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
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

/** Keeps only the scoped bucket, or only sessions needing input. */
export function scopeSidebar(model: SidebarModel, scope: SidebarScope): SidebarModel {
  if (scope.kind === "all") return model;
  if (scope.kind === "bucket") {
    return {
      buckets: model.buckets.filter(({ bucket }) => bucket.id.toString() === scope.bucketId),
      orphans: [],
      order: model.order,
    };
  }
  const needsInput = (session: Session) => session.state === SessionState.NEEDS_INPUT;
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

/** How long an idle session goes untouched before a bucket folds it away. */
export const STALE_IDLE_MS = 24 * 60 * 60 * 1000;

/**
 * True when every session in the row is idle or ended and none has been active
 * for a day. A row holding the selected session is never stale, so the sidebar
 * keeps showing what the main pane does.
 */
export function isStaleRow(members: readonly Session[], now: number, selectedId: string | null): boolean {
  return members.every((session) => {
    if (session.id.toString() === selectedId) return false;
    if (!sessionEnded(session) && session.state !== SessionState.IDLE) return false;
    const last = sessionEnded(session) ? Number(session.endedAtUnixMs) : sessionLastActiveAt(session);
    const at = last || Number(session.createdAtUnixMs);
    return now - at >= STALE_IDLE_MS;
  });
}

export interface BucketTally {
  live: number;
  needsInput: number;
}

/** Live and needs-input counts per bucket id, over every session that has not ended. */
export function bucketTallies(state: Pick<AppState, "projects" | "sessions">): Map<string, BucketTally> {
  const bucketOfProject = new Map<string, string>();
  for (const project of state.projects.values()) {
    bucketOfProject.set(project.id.toString(), project.bucketId.toString());
  }
  const tallies = new Map<string, BucketTally>();
  for (const session of state.sessions.values()) {
    if (sessionEnded(session)) continue;
    const bucketId = bucketOfProject.get(session.projectId.toString());
    if (bucketId === undefined) continue;
    const tally = tallies.get(bucketId) ?? { live: 0, needsInput: 0 };
    tally.live += 1;
    if (session.state === SessionState.NEEDS_INPUT) tally.needsInput += 1;
    tallies.set(bucketId, tally);
  }
  return tallies;
}

export function bucketOfSession(
  state: Pick<AppState, "projects" | "sessions">,
  sessionId: string,
): string | null {
  const session = state.sessions.get(sessionId);
  if (!session) return null;
  return state.projects.get(session.projectId.toString())?.bucketId.toString() ?? null;
}
