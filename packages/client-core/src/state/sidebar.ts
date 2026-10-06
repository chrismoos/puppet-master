import { sessionEnded } from "../format";
import { SessionRole, SessionState, type Bucket, type Session } from "../gen/pm/v1/pm_pb";
import { sessionHasSource, type AppState } from "./reducer";

export interface SidebarBucket {
  bucket: Bucket;
  supervisors: Session[];
  /** Every Worker in the bucket, whichever of its projects each one runs in. */
  sessions: Session[];
}

export interface SidebarModel {
  buckets: SidebarBucket[];
  orphans: Session[];
  /** The comparator applied to every list here, so callers that regroup sessions keep the same order. */
  order: (a: Session, b: Session) => number;
}

/** Hides exited/failed sessions unless the user asked to see them. */
export function visibleSessions(
  sessions: Iterable<Session>,
  showEnded: boolean,
  selectedId?: string | null,
): Session[] {
  const out: Session[] = [];
  for (const s of sessions) {
    // The selected session is always shown so the sidebar never
    // omits what the main pane is displaying.
    if (showEnded || !sessionEnded(s) || s.id.toString() === selectedId) out.push(s);
  }
  return out;
}

function sortRank(state: SessionState): number {
  switch (state) {
    case SessionState.NEEDS_INPUT:
      return 0;
    case SessionState.STARTING:
    case SessionState.WORKING:
    case SessionState.IDLE:
    case SessionState.UNSPECIFIED:
    case SessionState.AWAITING_WORKER:
      return 1;
    case SessionState.EXITED:
    case SessionState.FAILED:
      return 2;
  }
}

const RECENCY_BUCKET_MS = 60_000;

/**
 * Activity coarsened to a minute, read from the same clock the row shows.
 *
 * The row reports the session's last activity, or when it ended. Ordering
 * by anything else lets the list disagree with its own labels: a session
 * whose raw interaction clock is newer sorted above one reading "now", so
 * the newest-looking row sat in the middle of the list.
 *
 * Falls back to creation time so a session that has not reported yet sorts
 * by when it started rather than sinking below every session that has. Raw
 * timestamps would re-rank the list on every chunk of output and move rows
 * out from under the pointer, so only crossing a minute can reorder
 * anything.
 */
function recencyBucket(session: Session): number {
  const shown = sessionEnded(session)
    ? Number(session.endedAtUnixMs)
    : Number(session.lastActivityAtUnixMs);
  const at = shown || Number(session.createdAtUnixMs);
  return Math.floor(at / RECENCY_BUCKET_MS);
}

function bySalience(a: Session, b: Session): number {
  const rank = sortRank(a.state) - sortRank(b.state);
  if (rank !== 0) return rank;
  const recency = recencyBucket(b) - recencyBucket(a);
  if (recency !== 0) return recency;
  return Number(b.createdAtUnixMs - a.createdAtUnixMs);
}

export function buildSidebar(
  state: AppState,
  showEnded: boolean,
  selectedId?: string | null,
  includedIds?: ReadonlySet<string> | null,
  includedOrder?: ReadonlyMap<string, number>,
): SidebarModel {
  const buckets = [...state.buckets.values()].sort(
    (a, b) => a.position - b.position || a.name.localeCompare(b.name),
  );

  const bucketOfProject = new Map<string, string>();
  for (const project of state.projects.values()) {
    bucketOfProject.set(project.id.toString(), project.bucketId.toString());
  }

  const sessionsByBucket = new Map<string, Session[]>();
  const orphans: Session[] = [];
  const visible = visibleSessions(state.sessions.values(), true, selectedId).filter((session) => {
    const id = session.id.toString();
    if (includedIds) return includedIds.has(id);
    return showEnded || !sessionEnded(session) || sessionHasSource(state, id, "subscription") || sessionHasSource(state, id, "selected");
  });
  const sessionOrder = includedIds
    ? (a: Session, b: Session) => (includedOrder?.get(a.id.toString()) ?? Number.MAX_SAFE_INTEGER)
      - (includedOrder?.get(b.id.toString()) ?? Number.MAX_SAFE_INTEGER)
    : bySalience;
  for (const session of visible) {
    if (session.role === SessionRole.SUPERVISOR) continue;
    const bucketId = bucketOfProject.get(session.projectId.toString());
    if (bucketId === undefined) {
      orphans.push(session);
      continue;
    }
    const list = sessionsByBucket.get(bucketId) ?? [];
    list.push(session);
    sessionsByBucket.set(bucketId, list);
  }

  const model: SidebarBucket[] = buckets.map((bucket) => {
    const key = bucket.id.toString();
    const sessions = (sessionsByBucket.get(key) ?? []).sort(sessionOrder);
    const supervisors = visible
      .filter(
        (s) =>
          s.role === SessionRole.SUPERVISOR &&
          bucketOfProject.get(s.projectId.toString()) === key,
      )
      .sort(sessionOrder);
    return { bucket, supervisors, sessions };
  });

  return { buckets: model, orphans: orphans.sort(sessionOrder), order: sessionOrder };
}
