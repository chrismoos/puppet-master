import { SessionAlertKind, SessionRole, type Session } from "../gen/pm/v1/pm_pb";

export interface CatchUpAlert {
  session: Session;
  kind: SessionAlertKind;
}

const UNSEEN_FLAGS: ReadonlyArray<readonly [SessionAlertKind, (session: Session) => boolean]> = [
  [SessionAlertKind.NEEDS_INPUT, (session) => session.needsInputUnseen],
  [SessionAlertKind.COMPLETED, (session) => session.idleUnseen],
];

/** The daemon's default alert scope: a supervised worker's supervisor is its audience. */
function inDefaultScope(session: Session): boolean {
  return session.role === SessionRole.SUPERVISOR || session.spawnedBySessionId === undefined;
}

function episodeKey(sessionId: bigint, kind: SessionAlertKind): string {
  return `${sessionId.toString()}:${kind}`;
}

/**
 * Raises the alerts a client missed while it was disconnected, judged from
 * the unseen flags a reconnect snapshot carries. An unseen episode lasts
 * while its flag stays set, and is alerted at most once whether the live
 * alert or the snapshot reported it first.
 */
export class AlertCatchUp {
  private alerted = new Set<string>();

  noteAlert(sessionId: bigint, kind: SessionAlertKind): void {
    this.alerted.add(episodeKey(sessionId, kind));
  }

  observe(session: Session): void {
    for (const [kind, unseen] of UNSEEN_FLAGS) {
      if (!unseen(session)) this.alerted.delete(episodeKey(session.id, kind));
    }
  }

  /**
   * `previous` is the session map from before the snapshot, or null on the
   * first hydration, where every unseen flag predates this client.
   */
  snapshot(previous: ReadonlyMap<string, Session> | null, sessions: Iterable<Session>): CatchUpAlert[] {
    const raised: CatchUpAlert[] = [];
    const present = new Set<string>();
    for (const session of sessions) {
      present.add(session.id.toString());
      const before = previous?.get(session.id.toString());
      for (const [kind, unseen] of UNSEEN_FLAGS) {
        const key = episodeKey(session.id, kind);
        if (!unseen(session)) continue;
        if (previous && inDefaultScope(session) && !(before && unseen(before)) && !this.alerted.has(key)) {
          raised.push({ session, kind });
        }
        this.alerted.add(key);
      }
      this.observe(session);
    }
    for (const key of this.alerted) {
      if (!present.has(key.slice(0, key.indexOf(":")))) this.alerted.delete(key);
    }
    return raised;
  }
}
