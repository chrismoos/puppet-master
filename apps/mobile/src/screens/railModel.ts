import type { AppState } from "@puppet-master/client-core/state/reducer";
import { bucketTallies, railMonograms } from "@puppet-master/client-core/state/buckets";
import type { MobileSessionFilter } from "@puppet-master/client-core/state/mobileSessionList";

/** A bucket's own rail face, as set on the web: a picture, short text, a color. */
export interface RailIcon {
  text?: string;
  color?: string;
  image?: string;
}

/** What a rail tile shows, by the web rail's rules: a count of sessions
 * waiting on you, a dot while one is mid-turn, and a dot for finished turns
 * you have not looked at. */
export interface RailTile {
  id: string;
  name: string;
  letters: string;
  icon?: RailIcon;
  needs: number;
  working: boolean;
  finishedUnseen: boolean;
}

/** A tile per bucket, in the buckets' own order. */
export function railTiles(state: Pick<AppState, "buckets" | "projects" | "sessions">): RailTile[] {
  const buckets = [...state.buckets.values()].sort((a, b) => a.position - b.position);
  const letters = railMonograms(buckets.map((bucket) => ({ id: bucket.id.toString(), name: bucket.name })));
  const tallies = bucketTallies(state);
  return buckets.map((bucket) => {
    const id = bucket.id.toString();
    const tally = tallies.get(id);
    const icon = bucket.icon && (bucket.icon.text || bucket.icon.color || bucket.icon.image)
      ? { text: bucket.icon.text || undefined, color: bucket.icon.color || undefined, image: bucket.icon.image || undefined }
      : undefined;
    return {
      id,
      name: bucket.name,
      letters: letters.get(id) ?? "?",
      ...(icon ? { icon } : {}),
      needs: tally?.needsInput ?? 0,
      working: (tally?.working ?? 0) > 0,
      finishedUnseen: (tally?.finishedUnseen ?? 0) > 0,
    };
  });
}

/** The filter after tapping a tile: the selected bucket again shows every
 * bucket, any other narrows to it. A project choice never carries across. */
export function tapBucket(filter: MobileSessionFilter, bucketId: string): MobileSessionFilter {
  return filter.bucketId === bucketId ? {} : { bucketId };
}

/** The line under the header: what is working and waiting in view. */
export function railSummary(
  state: Pick<AppState, "projects" | "sessions">,
  bucketId: string | undefined,
): string {
  let working = 0;
  let needs = 0;
  for (const [id, tally] of bucketTallies(state)) {
    if (bucketId && id !== bucketId) continue;
    working += tally.working;
    needs += tally.needsInput;
  }
  return [
    working > 0 ? `${working} working` : "",
    needs > 0 ? `${needs} ${needs === 1 ? "needs" : "need"} you` : "",
  ].filter(Boolean).join(" · ");
}
