import { sessionEnded } from "../format";
import { TerminalKind, type Session, type Terminal } from "../gen/pm/v1/pm_pb";

export interface TerminalResourceReleaser {
  disposeSession(sessionId: bigint): void;
  disposeTerminal(terminalId: bigint): void;
}

/**
 * Releases cached agent layers whenever their session is ended or disappears.
 * Disposal is intentionally idempotent: session-page merges can batch ahead of
 * React effects, so transition-only detection can miss the live -> ended edge.
 */
export function releaseEndedOrRemovedSessions(
  previous: ReadonlyMap<string, Session>,
  current: ReadonlyMap<string, Session>,
  stage: TerminalResourceReleaser,
): void {
  const candidates = new Set(previous.keys());
  for (const [id, session] of current) {
    if (sessionEnded(session)) candidates.add(id);
  }
  for (const id of candidates) {
    const after = current.get(id);
    if (!after || sessionEnded(after)) {
      stage.disposeSession(BigInt(id));
    }
  }
}

/** Releases the cache address that owns a removed terminal's live stream. */
export function releaseRemovedTerminals(
  previous: ReadonlyMap<string, Terminal>,
  current: ReadonlyMap<string, Terminal>,
  stage: TerminalResourceReleaser,
): void {
  for (const [id, terminal] of previous) {
    if (current.has(id)) continue;
    if (terminal.kind === TerminalKind.AGENT) {
      stage.disposeSession(terminal.sessionId);
    } else {
      stage.disposeTerminal(terminal.id);
    }
  }
}
