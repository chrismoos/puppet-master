import { sessionLastActiveAt } from "../format";
import { SessionRole, SessionState, type Project, type Session } from "../gen/pm/v1/pm_pb";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/** A top-level entry in the mobile session list: either a standalone session or a supervisor group. */
export type MobileListEntry =
  | { kind: "standalone"; session: Session }
  | { kind: "group"; supervisor: Session; workers: Session[]; needsAttention: boolean };

/** Optional filter narrowing the session list to specific buckets/projects. */
export interface MobileSessionFilter {
  bucketId?: string;
  projectId?: string;
}

// ---------------------------------------------------------------------------
// Filtering
// ---------------------------------------------------------------------------

/**
 * Return the set of project ids that belong to a given bucket.
 * Callers should build this once and pass to `filterSessions`.
 */
export function projectIdsForBucket(
  projects: Iterable<Project>,
  bucketId: string,
): ReadonlySet<string> {
  const ids = new Set<string>();
  for (const p of projects) {
    if (p.bucketId.toString() === bucketId) ids.add(p.id.toString());
  }
  return ids;
}

/**
 * Narrow a session iterable by bucket and/or project. Returns all sessions
 * when no filter is active. When filtering by bucket, sessions whose project
 * belongs to that bucket pass. When filtering by project, only that project's
 * sessions pass. Both may be set; project wins (it's more specific).
 */
export function filterSessions(
  sessions: Iterable<Session>,
  filter: MobileSessionFilter,
  projectsInBucket: ReadonlySet<string> | null,
): Session[] {
  if (!filter.bucketId && !filter.projectId) return [...sessions];
  const result: Session[] = [];
  for (const s of sessions) {
    const pid = s.projectId.toString();
    if (filter.projectId) {
      if (pid === filter.projectId) result.push(s);
    } else if (projectsInBucket) {
      if (projectsInBucket.has(pid)) result.push(s);
    }
  }
  return result;
}

// ---------------------------------------------------------------------------
// Sorting – reuses the minute-bucket debounce from sidebar.ts
// ---------------------------------------------------------------------------

const RECENCY_BUCKET_MS = 60_000;

function recencyBucket(session: Session): number {
  const at = sessionLastActiveAt(session) || Number(session.createdAtUnixMs);
  return Math.floor(at / RECENCY_BUCKET_MS);
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

/** Compare two sessions by salience: state rank, then minute-bucketed activity, then creation time. */
export function bySalience(a: Session, b: Session): number {
  const rank = sortRank(a.state) - sortRank(b.state);
  if (rank !== 0) return rank;
  const recency = recencyBucket(b) - recencyBucket(a);
  if (recency !== 0) return recency;
  return Number(b.createdAtUnixMs - a.createdAtUnixMs);
}

// ---------------------------------------------------------------------------
// Attention propagation
// ---------------------------------------------------------------------------

/** Whether a session is in a state that demands the user's attention. */
export function needsAttention(session: Session): boolean {
  return session.state === SessionState.NEEDS_INPUT || session.state === SessionState.FAILED;
}

/** Whether any worker in the list needs attention. */
export function groupNeedsAttention(workers: Session[]): boolean {
  return workers.some(needsAttention);
}

// ---------------------------------------------------------------------------
// Grouping
// ---------------------------------------------------------------------------

/** Most recent activity across a group (supervisor + its workers). */
export function groupLastActiveAt(supervisor: Session, workers: Session[]): number {
  let best = sessionLastActiveAt(supervisor) || Number(supervisor.createdAtUnixMs);
  for (const w of workers) {
    const at = sessionLastActiveAt(w) || Number(w.createdAtUnixMs);
    if (at > best) best = at;
  }
  return best;
}

/**
 * Build a flat list of entries for the mobile session list.
 *
 * - Supervisor sessions become group headers; their spawned workers nest beneath them.
 * - Unsupervised workers and sessions with no role appear as standalone entries.
 * - Groups order by the most recent activity within the group (minute-bucketed).
 * - Children within a group order by their own activity (bySalience).
 * - Standalone sessions intermix with groups by the same ordering.
 */
export function buildMobileSessionList(sessions: Iterable<Session>): MobileListEntry[] {
  const supervisors = new Map<string, Session>();
  const workersBySuper = new Map<string, Session[]>();
  const standalones: Session[] = [];

  for (const s of sessions) {
    if (s.role === SessionRole.SUPERVISOR) {
      supervisors.set(s.id.toString(), s);
    } else if (s.spawnedBySessionId !== undefined) {
      const key = s.spawnedBySessionId.toString();
      const list = workersBySuper.get(key) ?? [];
      list.push(s);
      workersBySuper.set(key, list);
    } else {
      standalones.push(s);
    }
  }

  // Sort workers within each group
  for (const workers of workersBySuper.values()) {
    workers.sort(bySalience);
  }

  // Build entries: groups for supervisors, standalone for the rest
  const entries: MobileListEntry[] = [];

  for (const [id, supervisor] of supervisors) {
    const workers = workersBySuper.get(id) ?? [];
    // Remove these workers from the workersBySuper so we can detect orphans
    workersBySuper.delete(id);
    entries.push({
      kind: "group",
      supervisor,
      workers,
      needsAttention: needsAttention(supervisor) || groupNeedsAttention(workers),
    });
  }

  // Workers whose supervisor is not in the current set appear as standalone
  for (const orphanWorkers of workersBySuper.values()) {
    for (const w of orphanWorkers) {
      standalones.push(w);
    }
  }

  for (const s of standalones) {
    entries.push({ kind: "standalone", session: s });
  }

  // Sort entries: groups by their group-level activity, standalones by their own
  entries.sort((a, b) => {
    const aRank = entryStateRank(a);
    const bRank = entryStateRank(b);
    if (aRank !== bRank) return aRank - bRank;

    const aRecency = Math.floor(entryLastActiveAt(a) / RECENCY_BUCKET_MS);
    const bRecency = Math.floor(entryLastActiveAt(b) / RECENCY_BUCKET_MS);
    if (aRecency !== bRecency) return bRecency - aRecency;

    const aCreated = entryCreatedAt(a);
    const bCreated = entryCreatedAt(b);
    return bCreated - aCreated;
  });

  return entries;
}

// ---------------------------------------------------------------------------
// Entry-level helpers
// ---------------------------------------------------------------------------

function entryStateRank(entry: MobileListEntry): number {
  if (entry.kind === "standalone") return sortRank(entry.session.state);
  // For a group, the best (lowest) rank among supervisor and its workers wins
  let best = sortRank(entry.supervisor.state);
  for (const w of entry.workers) {
    const r = sortRank(w.state);
    if (r < best) best = r;
  }
  return best;
}

function entryLastActiveAt(entry: MobileListEntry): number {
  if (entry.kind === "standalone") {
    return sessionLastActiveAt(entry.session) || Number(entry.session.createdAtUnixMs);
  }
  return groupLastActiveAt(entry.supervisor, entry.workers);
}

function entryCreatedAt(entry: MobileListEntry): number {
  if (entry.kind === "standalone") return Number(entry.session.createdAtUnixMs);
  return Number(entry.supervisor.createdAtUnixMs);
}
