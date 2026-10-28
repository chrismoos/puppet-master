import { parsePmLink } from "@puppet-master/client-core/pmlink";
import { navigate, parseRoute } from "../router";
import { isReviewEntryRoute } from "../views/reviewWindow";
import { itemKey, type AppState } from "@puppet-master/client-core/state/reducer";

export const TERMINAL_PM_LINK_ERROR_EVENT = "pm-terminal-link-error";

export type TerminalLinkSource =
  | { kind: "session"; id: string }
  | { kind: "terminal"; id: string };

export interface TerminalPmLinkResolution {
  path?: string;
  error?: string;
}

function sourceBucketId(state: AppState, source: TerminalLinkSource): string | null {
  const session = source.kind === "session"
    ? state.sessions.get(source.id)
    : (() => {
        const terminal = state.terminals.get(source.id);
        return terminal ? state.sessions.get(terminal.sessionId.toString()) : undefined;
      })();
  if (!session) return null;
  return state.projects.get(session.projectId.toString())?.bucketId.toString() ?? null;
}

function sessionBucketId(state: AppState, sessionId: string): string | null {
  const session = state.sessions.get(sessionId);
  if (!session) return null;
  return state.projects.get(session.projectId.toString())?.bucketId.toString() ?? null;
}

export function resolveTerminalPmLink(
  state: AppState,
  source: TerminalLinkSource,
  href: string,
): TerminalPmLinkResolution {
  if (!state.hydrated) return { error: "Puppet Master is still loading; try the link again." };
  const bucketId = sourceBucketId(state, source);
  if (bucketId === null) return { error: "This terminal is no longer available." };

  const link = parsePmLink(href);
  if (!link || (link.kind !== "item" && link.kind !== "legacyItem" && link.kind !== "session")) {
    return { error: `Unsupported Puppet Master link: ${href}` };
  }
  if (link.kind === "legacyItem") {
    return { error: `Legacy link pm:item/${link.legacyId} is unqualified; use pm:item/${bucketId}/<item>.` };
  }
  if (link.kind === "item") {
    if (link.bucketId !== bucketId) return { error: `Item ${link.bucketId}/${link.id} is outside this terminal's bucket.` };
    const item = state.items.get(itemKey(link.bucketId, link.id));
    if (!item) return { error: `Item ${link.id} is missing or inaccessible.` };
    if (item.bucketId.toString() !== bucketId) return { error: `Item ${link.id} is outside this terminal's bucket.` };
    return { path: `/bucket/${link.bucketId}/item/${link.id}` };
  }

  const targetBucketId = sessionBucketId(state, link.id);
  if (targetBucketId === null) return { error: `Session ${link.id} is missing or inaccessible.` };
  if (targetBucketId !== bucketId) return { error: `Session ${link.id} is outside this terminal's bucket.` };
  return { path: `/session/${link.id}` };
}

export function isSafeHttpUrl(href: string): boolean {
  if (/%(?![\dA-Fa-f]{2})/.test(href)) return false;
  try {
    const parsed = new URL(href);
    return (parsed.protocol === "http:" || parsed.protocol === "https:") && parsed.hostname.length > 0;
  } catch {
    return false;
  }
}

function reportLinkError(message: string): void {
  window.dispatchEvent(new CustomEvent<string>(TERMINAL_PM_LINK_ERROR_EVENT, { detail: message }));
}

export function openTerminalHttpLink(href: string): void {
  const destination = new URL(href, location.href);
  if (destination.origin === location.origin && destination.pathname === location.pathname
    && isReviewEntryRoute(parseRoute(destination.hash))) {
    window.open(destination.href, "_blank");
  } else {
    window.open(href, "_blank", "noopener,noreferrer");
  }
}

export function activateTerminalLink(
  getState: () => AppState,
  source: TerminalLinkSource,
  href: string,
  prepareNavigation: () => void,
): void {
  if (href.startsWith("pm:")) {
    const resolution = resolveTerminalPmLink(getState(), source, href);
    if (resolution.path) {
      prepareNavigation();
      queueMicrotask(() => navigate(resolution.path!));
    } else if (resolution.error) reportLinkError(resolution.error);
    return;
  }
  if (!isSafeHttpUrl(href)) return;
  prepareNavigation();
  openTerminalHttpLink(href);
}
