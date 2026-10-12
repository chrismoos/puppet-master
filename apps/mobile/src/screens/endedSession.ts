import { formatAgo, sessionEnded, sessionLastActive } from "@puppet-master/client-core/format";
import { SessionState, type Session } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { sessionHasSource, type AppState } from "@puppet-master/client-core/state/reducer";

/**
 * Whether an ended session offers resume (or restart), matching the web
 * session pane: a saved conversation resumes, and a session spawned without
 * a task prompt can always restart fresh.
 */
export function canResumeSession(session: Session): boolean {
  return sessionEnded(session) && (session.resumable || session.taskPrompt.length === 0);
}

/** The resume action's label, as the web pane words it. */
export function resumeLabel(session: Session): string {
  return session.resumable ? "Resume conversation" : "Restart session";
}

/**
 * Whether the list shows a session. Like the web sidebar, a session that
 * just ended stays listed through the reducer's recent-ended grace window
 * (the subscription drops it afterwards) so it can still be resumed, even
 * while ended sessions are otherwise hidden.
 */
export function sessionListed(state: AppState, session: Session, showEnded: boolean): boolean {
  return showEnded || !sessionEnded(session) || sessionHasSource(state, session.id.toString(), "subscription");
}

/** A row's trailing time: when it ended for an ended session, else its last activity. */
export function sessionRowTime(session: Session, now: number): string {
  if (!sessionEnded(session)) return sessionLastActive(session, now);
  const verb = session.state === SessionState.FAILED ? "failed" : "ended";
  if (session.endedAtUnixMs === undefined) return verb;
  return `${verb} ${formatAgo(now - Number(session.endedAtUnixMs))} ago`;
}

/** The ended banner's line, e.g. "Session ended with exit code 1". */
export function endedSummary(session: Session): string {
  const verb = session.state === SessionState.FAILED ? "failed" : "ended";
  return `Session ${verb}${session.exitCode !== undefined ? ` with exit code ${session.exitCode}` : ""}`;
}
