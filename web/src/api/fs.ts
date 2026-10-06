import { LOCAL_WORKER_ID } from "@puppet-master/client-core/format";
import { authedFetch } from "./token";

const HTTP_UNAUTHORIZED = 401;

export interface DirEntry {
  name: string;
  path: string;
}

export interface DirListing {
  dir: string;
  parent: string | null;
  entries: DirEntry[];
}

/**
 * Trailing path segment being typed, used to filter the parent
 * directory's entries. Empty when the input ends in a separator.
 */
export function trailingSegment(input: string): string {
  const slash = input.lastIndexOf("/");
  return slash < 0 ? input : input.slice(slash + 1);
}

/** Suggestions whose name matches the partial segment, case-insensitive. */
export function filterEntries(entries: readonly DirEntry[], input: string): DirEntry[] {
  const seg = trailingSegment(input).toLowerCase();
  if (seg === "") return [...entries];
  return entries.filter((e) => e.name.toLowerCase().startsWith(seg));
}

/** The listing key to fetch for an input: its parent when mid-segment. */
export function listingPath(input: string): string {
  const trimmed = input.trim();
  if (trimmed === "" || trimmed.endsWith("/")) return trimmed;
  const slash = trimmed.lastIndexOf("/");
  if (slash <= 0) return slash === 0 ? "/" : trimmed;
  return trimmed.slice(0, slash);
}

/** Lists a directory on the given worker; the local worker when omitted. */
export async function fetchDirectory(
  path: string,
  workerId?: bigint,
  signal?: AbortSignal,
): Promise<DirListing> {
  const query = new URLSearchParams({ path });
  if (workerId !== undefined && workerId !== LOCAL_WORKER_ID) {
    query.set("worker", workerId.toString());
  }
  const res = await authedFetch(`/api/fs?${query.toString()}`, { signal });
  if (res.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  if (!res.ok) {
    const body = (await res.json().catch(() => null)) as { error?: string } | null;
    throw new Error(body?.error || `directory listing failed (${res.status})`);
  }
  return (await res.json()) as DirListing;
}
