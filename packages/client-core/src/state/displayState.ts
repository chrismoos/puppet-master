import { sessionEnded } from "../format";
import { SessionRole, SessionState, type Session } from "../gen/pm/v1/pm_pb";

/** The child states that mean a supervisor is waiting on work rather than out of it. */
function childIsRunning(state: SessionState): boolean {
  return (
    state === SessionState.WORKING ||
    state === SessionState.STARTING ||
    state === SessionState.AWAITING_WORKER
  );
}

/**
 * The state a session's row renders as, resolved in priority order: the
 * session's own needs-input, then a supervisor parked between turns while a
 * session it spawned is still running, then its real state. A supervisor
 * whose running children are all stranded on an offline host reads as
 * awaiting worker rather than working, because none of that work can move
 * until the host returns.
 *
 * Display only. The daemon dedupes supervisor wake notices on the real state,
 * so this value must never be written back onto the session.
 */
export function displaySessionState(
  session: Session,
  children: Iterable<Session>,
): SessionState {
  if (session.state === SessionState.NEEDS_INPUT) return SessionState.NEEDS_INPUT;
  if (session.role !== SessionRole.SUPERVISOR || session.state !== SessionState.IDLE) {
    return session.state;
  }
  let running = 0;
  let awaiting = 0;
  for (const child of children) {
    if (!childIsRunning(child.state)) continue;
    running += 1;
    if (child.state === SessionState.AWAITING_WORKER) awaiting += 1;
  }
  if (running === 0) return session.state;
  return running === awaiting ? SessionState.AWAITING_WORKER : SessionState.WORKING;
}

export type SessionLookup = (id: string) => Session | undefined;

function supervisorIsLive(session: Session, lookup: SessionLookup): boolean {
  if (session.spawnedBySessionId === undefined) return false;
  const supervisor = lookup(session.spawnedBySessionId.toString());
  return supervisor !== undefined && supervisor.role === SessionRole.SUPERVISOR && !sessionEnded(supervisor);
}

/**
 * Whether a session needs the user. A worker's question goes to its
 * supervisor while that supervisor runs, which escalates with its own
 * flag_blocked when it cannot answer, and comes back to the user once the
 * supervisor has ended.
 */
export function awaitsUser(session: Session, lookup: SessionLookup): boolean {
  return session.state === SessionState.NEEDS_INPUT && !supervisorIsLive(session, lookup);
}

/** Whether a session's question is waiting on its running supervisor rather than the user. */
export function awaitsSupervisor(session: Session, lookup: SessionLookup): boolean {
  return session.state === SessionState.NEEDS_INPUT && supervisorIsLive(session, lookup);
}
