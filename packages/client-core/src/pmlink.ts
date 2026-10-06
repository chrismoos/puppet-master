// The surface-neutral URI scheme agents write in briefings and item
// bodies (pm:item/2/12, pm:session/37, ...). Links navigate; the one
// action-shaped link, pm:spawn, only prefills the spawn dialog.

import type { AppState } from "./state/reducer";
import { itemKey } from "./state/reducer";

export type PmLink =
  | { kind: "session"; id: string }
  | { kind: "item"; bucketId: string; id: string }
  | { kind: "legacyItem"; legacyId: string }
  | { kind: "project"; id: string }
  | { kind: "bucket"; id: string }
  | { kind: "spawn"; projectId?: string; prompt?: string };

export function parsePmLink(href: string): PmLink | null {
  const rest = href.startsWith("pm:") ? href.slice("pm:".length) : null;
  if (rest === null) return null;
  const item = rest.match(/^item\/(\d+)\/(\d+)$/);
  if (item) return { kind: "item", bucketId: item[1], id: item[2] };
  const legacyItem = rest.match(/^item\/(\d+)$/);
  if (legacyItem) return { kind: "legacyItem", legacyId: legacyItem[1] };
  const entity = rest.match(/^(session|project|bucket)\/(\d+)$/);
  if (entity) {
    return { kind: entity[1] as "session" | "project" | "bucket", id: entity[2] };
  }
  if (rest === "spawn" || rest.startsWith("spawn?")) {
    const params = new URLSearchParams(rest.slice("spawn".length).replace(/^\?/, ""));
    const projectId = params.get("project") ?? undefined;
    return {
      kind: "spawn",
      projectId: projectId && /^\d+$/.test(projectId) ? projectId : undefined,
      prompt: params.get("prompt") ?? undefined,
    };
  }
  return null;
}

export type ClassifiedHref =
  { kind: "pm"; link: PmLink } | { kind: "external" } | { kind: "inert" };

export function classifyHref(href: string): ClassifiedHref {
  const pm = parsePmLink(href);
  if (pm) return { kind: "pm", link: pm };
  if (/^https?:\/\//.test(href)) return { kind: "external" };
  return { kind: "inert" };
}

function sessionBucketId(state: AppState, sessionId: string): string | null {
  const session = state.sessions.get(sessionId);
  if (!session) return null;
  return (
    state.projects.get(session.projectId.toString())?.bucketId.toString() ??
    null
  );
}

/** Revalidates an internal link against the latest hydrated state. */
export function resolvePmLink(
  state: AppState,
  link: PmLink,
  sourceSessionId?: string,
): PmLink | null {
  if (!state.hydrated) return null;
  const sourceBucketId = sourceSessionId
    ? sessionBucketId(state, sourceSessionId)
    : undefined;
  if (sourceSessionId && !sourceBucketId) return null;

  switch (link.kind) {
    case "item": {
      const item = state.items.get(itemKey(link.bucketId, link.id));
      return item &&
        item.bucketId.toString() === link.bucketId &&
        (!sourceBucketId || item.bucketId.toString() === sourceBucketId)
        ? link
        : null;
    }
    case "legacyItem":
      // The explicit compatibility route only displays a non-resolving
      // migration message; it never looks this number up.
      return link;
    case "session": {
      const bucketId = sessionBucketId(state, link.id);
      return bucketId && (!sourceBucketId || bucketId === sourceBucketId)
        ? link
        : null;
    }
    case "project": {
      const project = state.projects.get(link.id);
      return project &&
        (!sourceBucketId || project.bucketId.toString() === sourceBucketId)
        ? link
        : null;
    }
    case "bucket":
      return state.buckets.has(link.id) &&
        (!sourceBucketId || link.id === sourceBucketId)
        ? link
        : null;
    case "spawn": {
      if (!link.projectId) return link;
      const project = state.projects.get(link.projectId);
      return project &&
        (!sourceBucketId || project.bucketId.toString() === sourceBucketId)
        ? link
        : null;
    }
  }
}
