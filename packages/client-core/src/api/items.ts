const HTTP_UNAUTHORIZED = 401;

import { create } from "@bufbuild/protobuf";
import {
  ItemPriority,
  ItemSchema,
  ItemSourceKind,
  ItemStatus,
  type Item,
} from "../gen/pm/v1/pm_pb";
import type { AbortSignalLike, JsonFetch, JsonResponse } from "../platform";

export type ItemNoteKind = "created" | "status" | "note" | "user_reply";

export interface ItemNote {
  id: number;
  session_id: number | null;
  ts_unix_ms: number;
  kind: ItemNoteKind;
  text: string;
}

export async function fetchItemNotes(
  http: JsonFetch,
  bucketId: string,
  itemId: string,
  signal?: AbortSignalLike,
): Promise<ItemNote[]> {
  const res = await http(`/api/buckets/${bucketId}/items/${itemId}/notes`, { signal });
  if (res.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  if (!res.ok) {
    const body = (await res.json().catch(() => null)) as { error?: string } | null;
    throw new Error(body?.error || `item notes fetch failed (${res.status})`);
  }
  const body = (await res.json()) as { notes: ItemNote[] };
  return body.notes;
}

export interface ItemAttachment {
  id: string;
  bucketId: string;
  itemId: string;
  filename: string;
  mediaType: string;
  byteLength: string;
  sha256: string;
  createdAtUnixMs: string;
  createdBySessionId?: string | null;
}

async function responseError(response: JsonResponse, fallback: string): Promise<Error> {
  const body = (await response.json().catch(() => null)) as { error?: string } | null;
  return new Error(body?.error || `${fallback} (${response.status})`);
}

export async function fetchItemAttachments(
  http: JsonFetch,
  bucketId: string,
  itemId: string,
  signal?: AbortSignalLike,
): Promise<ItemAttachment[]> {
  const response = await http(`/api/buckets/${bucketId}/items/${itemId}/attachments`, { signal });
  if (response.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  if (!response.ok) throw await responseError(response, "attachment list failed");
  const body = (await response.json()) as { attachments: ItemAttachment[] };
  return body.attachments;
}

export function attachmentUploadPath(bucketId: string, itemId: string, filename: string, mediaType: string): string {
  const query = new URLSearchParams({ filename, media_type: mediaType });
  return `/api/buckets/${bucketId}/items/${itemId}/attachments?${query}`;
}

export async function deleteItemAttachment(
  http: JsonFetch,
  bucketId: string,
  itemId: string,
  attachmentId: string,
): Promise<void> {
  const response = await http(`/api/buckets/${bucketId}/items/${itemId}/attachments/${attachmentId}`, { method: "DELETE" });
  if (response.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  if (!response.ok) throw await responseError(response, "attachment delete failed");
}

export function attachmentDownloadUrl(bucketId: string, itemId: string, attachmentId: string): string {
  return `/api/buckets/${bucketId}/items/${itemId}/attachments/${attachmentId}`;
}

export interface ItemSearchFilters {
  query: string;
  project: string;
  status: string;
  priority: string;
  source: string;
  includeDone: boolean;
  includeSnoozed: boolean;
  summary: string;
}

export interface ItemSearchPage {
  items: Item[];
  nextOffset?: number;
  counts?: ItemSearchCounts;
}

export interface ItemSearchCounts {
  bucketTotal: number;
  matchingTotal: number;
  byStatus: Partial<Record<keyof typeof statuses, number>>;
}

const statuses: Record<string, ItemStatus> = {
  inbox: ItemStatus.INBOX, planned: ItemStatus.PLANNED, in_progress: ItemStatus.IN_PROGRESS,
  blocked: ItemStatus.BLOCKED, blocked_external: ItemStatus.BLOCKED_EXTERNAL,
  done: ItemStatus.DONE, dropped: ItemStatus.DROPPED,
};
const priorities: Record<string, ItemPriority> = {
  urgent: ItemPriority.URGENT, high: ItemPriority.HIGH,
  normal: ItemPriority.NORMAL, low: ItemPriority.LOW,
};
const sources: Record<string, ItemSourceKind> = {
  email: ItemSourceKind.EMAIL, slack: ItemSourceKind.SLACK, github: ItemSourceKind.GITHUB,
  jira: ItemSourceKind.JIRA, teams: ItemSourceKind.TEAMS, telegram: ItemSourceKind.TELEGRAM,
  human: ItemSourceKind.HUMAN, agent: ItemSourceKind.AGENT, other: ItemSourceKind.OTHER,
};

interface ItemJson {
  id: string; bucket_id: string; project_id?: string | null; external_key?: string | null;
  title: string; body: string; question: string; status: string; priority: string;
  source_kind: string; source_detail: string; url: string; due_at_unix_ms?: string | null;
  snoozed_until_unix_ms?: string | null; created_by_session_id?: string | null;
  created_at_unix_ms: string; updated_at_unix_ms: string; done_at_unix_ms?: string | null;
  blocked_by: string[]; session_ids: string[];
}

function decodeItem(item: ItemJson): Item {
  return create(ItemSchema, {
    id: BigInt(item.id), bucketId: BigInt(item.bucket_id),
    projectId: item.project_id == null ? undefined : BigInt(item.project_id),
    externalKey: item.external_key ?? undefined, title: item.title, body: item.body,
    question: item.question, status: statuses[item.status], priority: priorities[item.priority],
    sourceKind: sources[item.source_kind], sourceDetail: item.source_detail, url: item.url,
    dueAtUnixMs: item.due_at_unix_ms == null ? undefined : BigInt(item.due_at_unix_ms),
    snoozedUntilUnixMs: item.snoozed_until_unix_ms == null ? undefined : BigInt(item.snoozed_until_unix_ms),
    createdBySessionId: item.created_by_session_id == null ? undefined : BigInt(item.created_by_session_id),
    createdAtUnixMs: BigInt(item.created_at_unix_ms), updatedAtUnixMs: BigInt(item.updated_at_unix_ms),
    doneAtUnixMs: item.done_at_unix_ms == null ? undefined : BigInt(item.done_at_unix_ms),
    blockedBy: item.blocked_by.map(BigInt), sessionIds: item.session_ids.map(BigInt),
  });
}

export async function fetchItem(
  http: JsonFetch,
  bucketId: string,
  itemId: string,
  signal?: AbortSignalLike,
): Promise<Item> {
  const res = await http(`/api/buckets/${bucketId}/items/${itemId}`, { signal });
  if (res.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  if (!res.ok) {
    const body = (await res.json().catch(() => null)) as { error?: string } | null;
    throw new Error(body?.error || `item fetch failed (${res.status})`);
  }
  const body = (await res.json()) as { item: ItemJson };
  return decodeItem(body.item);
}

export async function fetchItems(
  http: JsonFetch,
  bucketId: string,
  filters: ItemSearchFilters,
  offset = 0,
  signal?: AbortSignalLike,
): Promise<ItemSearchPage> {
  const query = new URLSearchParams({ limit: "50", offset: String(offset) });
  if (filters.query) query.set("q", filters.query);
  if (filters.project) query.set("project", filters.project);
  if (filters.status) query.set("status", filters.status);
  if (filters.priority) query.set("priority", filters.priority);
  if (filters.source) query.set("source", filters.source);
  // The HTTP endpoint defaults to completed history included. Send only the
  // explicit opt-out; this keeps the browser URL and request semantics aligned.
  if (!filters.includeDone) query.set("done", "false");
  if (filters.includeSnoozed) query.set("snoozed", "true");
  if (filters.summary) query.set("summary", filters.summary);
  const res = await http(`/api/buckets/${bucketId}/items?${query}`, { signal });
  if (res.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  if (!res.ok) {
    const body = (await res.json().catch(() => null)) as { error?: string } | null;
    throw new Error(body?.error || `item search failed (${res.status})`);
  }
  const body = (await res.json()) as { items: ItemJson[]; nextOffset?: number | null; counts?: ItemSearchCounts };
  return { items: body.items.map(decodeItem), nextOffset: body.nextOffset ?? undefined, counts: body.counts };
}

/**
 * Fetches whole pages from the start until they cover `size` rows, so a refresh
 * can replace an already paged list without discarding the pages behind it. The
 * HTTP endpoint bounds a single request well below a deeply paged window, hence
 * the walk rather than one oversized query.
 */
export async function fetchItemWindow(
  http: JsonFetch,
  bucketId: string,
  filters: ItemSearchFilters,
  size: number,
  signal?: AbortSignalLike,
): Promise<ItemSearchPage> {
  const items: Item[] = [];
  const seen = new Set<bigint>();
  let page = await fetchItems(http, bucketId, filters, 0, signal);
  let counts = page.counts;
  for (;;) {
    for (const item of page.items) {
      if (seen.has(item.id)) continue;
      seen.add(item.id);
      items.push(item);
    }
    if (page.nextOffset === undefined || page.nextOffset >= size) break;
    page = await fetchItems(http, bucketId, filters, page.nextOffset, signal);
    counts = page.counts ?? counts;
  }
  return { items, nextOffset: page.nextOffset, counts };
}

export interface BriefingEntry {
  id: number;
  bucketId: number;
  sessionId: number | null;
  tsUnixMs: number;
  markdown: string;
}

export async function fetchBriefings(
  http: JsonFetch,
  bucketId: string,
  signal?: AbortSignalLike,
): Promise<BriefingEntry[]> {
  const res = await http(`/api/buckets/${bucketId}/briefings`, { signal });
  if (res.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  if (!res.ok) {
    const body = (await res.json().catch(() => null)) as { error?: string } | null;
    throw new Error(body?.error || `briefing fetch failed (${res.status})`);
  }
  const body = (await res.json()) as { briefings: BriefingEntry[] };
  return body.briefings;
}
