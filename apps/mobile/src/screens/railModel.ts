import type { AppState } from "@puppet-master/client-core/state/reducer";
import { bucketTallies, railMonograms, type BucketTally } from "@puppet-master/client-core/state/buckets";
import type { MobileSessionFilter } from "@puppet-master/client-core/state/mobileSessionList";

/** The one dot a rail tile shows, most urgent first: a session waiting on
 * you, then one mid-turn, then live sessions sitting idle. */
export type RailDot = "needs" | "working" | "idle" | null;

export interface RailTile {
  id: string;
  name: string;
  letters: string;
  dot: RailDot;
}

export function railDot(tally: BucketTally | undefined): RailDot {
  if (!tally) return null;
  if (tally.needsInput > 0) return "needs";
  if (tally.working > 0) return "working";
  return tally.live > 0 ? "idle" : null;
}

/** A tile per bucket, in the buckets' own order. */
export function railTiles(state: Pick<AppState, "buckets" | "projects" | "sessions">): RailTile[] {
  const buckets = [...state.buckets.values()]
    .sort((a, b) => a.position - b.position)
    .map((bucket) => ({ id: bucket.id.toString(), name: bucket.name }));
  const letters = railMonograms(buckets);
  const tallies = bucketTallies(state);
  return buckets.map((bucket) => ({
    ...bucket,
    letters: letters.get(bucket.id) ?? "?",
    dot: railDot(tallies.get(bucket.id)),
  }));
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
