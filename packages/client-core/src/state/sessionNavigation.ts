import { sessionEnded } from "../format";
import type { Session } from "../gen/pm/v1/pm_pb";

export function selectedSessionBecameUnavailable(
  previous: ReadonlyMap<string, Session>,
  current: ReadonlyMap<string, Session>,
  selectedId: string,
): boolean {
  const before = previous.get(selectedId);
  if (!before) return false;
  const after = current.get(selectedId);
  return !after;
}

export function sessionFallbackPath(
  sessions: Iterable<Session>,
  workspaceIds: readonly number[],
  excludedSessionId: string,
): string {
  const nextSession = [...sessions]
    .filter((session) => session.id.toString() !== excludedSessionId && !sessionEnded(session))
    .sort((a, b) => Number(b.createdAtUnixMs - a.createdAtUnixMs) || Number(b.id - a.id))[0];
  if (nextSession) return `/session/${nextSession.id.toString()}`;
  return workspaceIds.length > 0 ? `/workspace/${workspaceIds[0]}` : "/";
}
