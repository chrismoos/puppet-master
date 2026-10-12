import type { Session, SessionForward } from "../gen/pm/v1/pm_pb";
import type { AppState } from "./reducer";

/** One session's published forwards, under the session that published them. */
export interface ForwardGroup {
  session: Session;
  forwards: readonly SessionForward[];
}

/** One forward with the session that published it. */
export interface OwnedForward {
  session: Session;
  forward: SessionForward;
}

function compareBigintAsc(a: bigint, b: bigint): number {
  return a === b ? 0 : a > b ? 1 : -1;
}

/**
 * The forwards a session's views should list: its own, then those of every
 * session it spawned, transitively, in spawn order and depth first. A group
 * appears only while it has forwards, so an ended descendant keeps its URLs
 * listed until the daemon drops their rows.
 */
export function forwardsForSession(
  state: Pick<AppState, "sessions" | "forwards">,
  sessionId: string,
): ForwardGroup[] {
  const root = state.sessions.get(sessionId);
  if (!root) return [];

  const children = new Map<string, Session[]>();
  for (const session of state.sessions.values()) {
    const parent = session.spawnedBySessionId?.toString();
    if (parent === undefined) continue;
    const siblings = children.get(parent);
    if (siblings) siblings.push(session);
    else children.set(parent, [session]);
  }
  for (const siblings of children.values()) {
    siblings.sort((a, b) => compareBigintAsc(a.id, b.id));
  }

  const published = new Map<string, SessionForward[]>();
  for (const forward of state.forwards.values()) {
    const key = forward.sessionId.toString();
    const existing = published.get(key);
    if (existing) existing.push(forward);
    else published.set(key, [forward]);
  }
  for (const forwards of published.values()) {
    forwards.sort((a, b) => compareBigintAsc(a.id, b.id));
  }

  const groups: ForwardGroup[] = [];
  // A session that reports itself as its own ancestor must not loop.
  const walked = new Set<string>();
  const walk = (session: Session): void => {
    const key = session.id.toString();
    if (walked.has(key)) return;
    walked.add(key);
    const forwards = published.get(key);
    if (forwards) groups.push({ session, forwards });
    for (const child of children.get(key) ?? []) walk(child);
  };
  walk(root);
  return groups;
}

function compareBigintDesc(a: bigint, b: bigint): number {
  return compareBigintAsc(b, a);
}

/**
 * Every forward in the groups as one list, newest first. Two forwards
 * published in the same millisecond keep their creation order by id.
 */
export function forwardsNewestFirst(groups: readonly ForwardGroup[]): OwnedForward[] {
  const owned: OwnedForward[] = [];
  for (const group of groups) {
    for (const forward of group.forwards) owned.push({ session: group.session, forward });
  }
  owned.sort(
    (a, b) =>
      compareBigintDesc(a.forward.createdAtUnixMs, b.forward.createdAtUnixMs) ||
      compareBigintDesc(a.forward.id, b.forward.id),
  );
  return owned;
}

/** How many forwards a bar shows before the rest sit behind "more". */
export const RECENT_FORWARDS_SHOWN = 4;

/**
 * The forwards a bar shows: the newest few, or all of them once expanded.
 * A list that only just exceeds the limit is shown whole, since a "more"
 * control would take the space of the one entry it hides.
 */
export function visibleForwards<T>(
  forwards: readonly T[],
  expanded: boolean,
  limit = RECENT_FORWARDS_SHOWN,
): { shown: readonly T[]; hidden: number } {
  if (expanded || forwards.length <= limit + 1) return { shown: forwards, hidden: 0 };
  return { shown: forwards.slice(0, limit), hidden: forwards.length - limit };
}

interface HttpUrlParts {
  origin: string;
  path: string;
}

const DEFAULT_PORTS: Record<string, string> = { http: "80", https: "443" };

/**
 * Splits an http(s) URL into a normalized origin and path without relying
 * on a full URL implementation, which React Native does not ship. A URL with
 * userinfo is refused: it never names a forward, and the server refuses it
 * as a destination too.
 */
function splitHttpUrl(url: string): HttpUrlParts | null {
  const match = /^(https?):\/\/([^/?#]*)([^?#]*)/i.exec(url.trim());
  if (!match) return null;
  const scheme = match[1].toLowerCase();
  const authority = match[2].toLowerCase();
  if (!authority || authority.includes("@")) return null;
  const portAt = authority.lastIndexOf(":");
  const host = portAt > authority.lastIndexOf("]") && portAt !== -1 ? authority.slice(0, portAt) : authority;
  const port = host === authority ? "" : authority.slice(portAt + 1);
  const origin = `${scheme}://${host}${port && port !== DEFAULT_PORTS[scheme] ? `:${port}` : ""}`;
  return { origin, path: match[3] || "/" };
}

/**
 * The published forward a URL points into, if any: same origin as the
 * forward's URL and a path under its root. A link tapped outside the
 * browser that holds the dashboard session can then be opened with a token
 * scoped to that forward. The server checks the destination again when it
 * mints the token, so a loose match here can never send a token elsewhere.
 */
export function forwardForUrl(forwards: Iterable<SessionForward>, url: string): SessionForward | undefined {
  const target = splitHttpUrl(url);
  if (!target) return undefined;
  let best: { forward: SessionForward; rootLength: number } | undefined;
  for (const forward of forwards) {
    if (!forward.url) continue;
    const root = splitHttpUrl(forward.url);
    if (!root || root.origin !== target.origin) continue;
    const rootPath = root.path.endsWith("/") ? root.path : `${root.path}/`;
    const under = target.path === rootPath.slice(0, -1) || target.path.startsWith(rootPath);
    if (under && (!best || rootPath.length > best.rootLength)) {
      best = { forward, rootLength: rootPath.length };
    }
  }
  return best?.forward;
}
