import { describe, expect, it, vi } from "vitest";
import { ItemPriority, ItemSourceKind, ItemStatus } from "../gen/pm/v1/pm_pb";
import type { AbortSignalLike } from "../platform";
import { fetchItem, fetchItems, fetchItemWindow, type ItemSearchFilters } from "./items";

const filters: ItemSearchFilters = {
  query: "Needle & URL", project: "9007199254740993", status: "planned",
  priority: "high", source: "github", includeDone: true, includeSnoozed: true, summary: "live_linked",
};

function jsonResponse(status: number, body: unknown) {
  return { ok: status >= 200 && status < 300, status, json: () => Promise.resolve(body) };
}

describe("fetchItems", () => {
  it("sends every composable filter and decodes ids without Number coercion", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, {
      items: [{
        id: "9007199254740993", bucket_id: "1", project_id: "9007199254740994",
        external_key: "jira:NEEDLE", title: "Needle", body: "body", question: "question",
        status: "planned", priority: "high", source_kind: "github", source_detail: "acme/api",
        url: "https://example.test", due_at_unix_ms: null, snoozed_until_unix_ms: null,
        created_by_session_id: null, created_at_unix_ms: "1700000000000",
        updated_at_unix_ms: "1700000000001", done_at_unix_ms: null,
        blocked_by: ["9007199254740995"], session_ids: ["9007199254740996"],
      }], nextOffset: 50, counts: { bucketTotal: 144, matchingTotal: 55, byStatus: { planned: 55 } },
    }));
    const signal: AbortSignalLike = { aborted: false };
    const page = await fetchItems(fetchMock, "1", filters, 0, signal);
    const url = String(fetchMock.mock.calls[0][0]);
    expect(url).toContain("q=Needle+%26+URL");
    expect(url).toContain("project=9007199254740993");
    expect(url).not.toContain("done=");
    expect(url).toContain("snoozed=true");
    expect(url).toContain("summary=live_linked");
    expect(fetchMock.mock.calls[0][1]).toEqual({ signal });
    expect(page.items[0]).toMatchObject({ id: 9007199254740993n, projectId: 9007199254740994n, status: ItemStatus.PLANNED, priority: ItemPriority.HIGH, sourceKind: ItemSourceKind.GITHUB });
    expect(page.items[0].blockedBy).toEqual([9007199254740995n]);
    expect(page.nextOffset).toBe(50);
    expect(page.counts).toEqual({ bucketTotal: 144, matchingTotal: 55, byStatus: { planned: 55 } });
  });

  it("sends an explicit completed-history opt-out", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, { items: [], nextOffset: null }));
    await fetchItems(fetchMock, "1", { ...filters, includeDone: false });
    expect(String(fetchMock.mock.calls[0][0])).toContain("done=false");
  });

  it("surfaces server errors", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(400, { error: "bad status" }));
    await expect(fetchItems(fetchMock, "1", { ...filters, status: "bad" })).rejects.toThrow("bad status");
  });

  it("hydrates a direct historical item route", async () => {
    const item = {
      id: "91", bucket_id: "7", project_id: null, external_key: null,
      title: "Historical result", body: "", question: "", status: "done",
      priority: "normal", source_kind: "human", source_detail: "", url: "",
      due_at_unix_ms: null, snoozed_until_unix_ms: null, created_by_session_id: null,
      created_at_unix_ms: "1", updated_at_unix_ms: "2", done_at_unix_ms: "2",
      blocked_by: [], session_ids: [],
    };
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, { item }));
    await expect(fetchItem(fetchMock, "7", "91")).resolves.toMatchObject({ id: 91n, bucketId: 7n, status: ItemStatus.DONE });
  });
});

describe("fetchItemWindow", () => {
  function row(id: number, updatedAtUnixMs = "1700000000000") {
    return {
      id: String(id), bucket_id: "1", project_id: null, external_key: null,
      title: `Item ${id}`, body: "", question: "", status: "planned", priority: "low",
      source_kind: "human", source_detail: "", url: "", due_at_unix_ms: null,
      snoozed_until_unix_ms: null, created_by_session_id: null,
      created_at_unix_ms: "1700000000000", updated_at_unix_ms: updatedAtUnixMs,
      done_at_unix_ms: null, blocked_by: [], session_ids: [],
    };
  }

  function requestedOffset(url: unknown): string | null {
    return new URLSearchParams(String(url).split("?")[1] ?? "").get("offset");
  }

  function pagedFetch(pages: Array<{ items: number[]; nextOffset: number | null }>) {
    return vi.fn().mockImplementation((url: string) => {
      const offset = Number(requestedOffset(url));
      const page = pages.find((_, index) => index * 50 === offset);
      if (!page) throw new Error(`unexpected offset ${offset}`);
      return Promise.resolve(jsonResponse(200, {
        items: page.items.map((id) => row(id)),
        nextOffset: page.nextOffset,
        counts: { bucketTotal: 200, matchingTotal: 120, byStatus: { planned: 120 } },
      }));
    });
  }

  it("asks for one page when the loaded window is a single page", async () => {
    const fetchMock = pagedFetch([{ items: [1, 2], nextOffset: 50 }]);
    const page = await fetchItemWindow(fetchMock, "1", filters, 50);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(page.items.map((item) => item.id)).toEqual([1n, 2n]);
    expect(page.nextOffset).toBe(50);
  });

  it("walks whole pages until they cover a deeper loaded window", async () => {
    const fetchMock = pagedFetch([
      { items: [1], nextOffset: 50 },
      { items: [2], nextOffset: 100 },
      { items: [3], nextOffset: 150 },
    ]);
    const page = await fetchItemWindow(fetchMock, "1", filters, 150);
    expect(fetchMock.mock.calls.map((call) => requestedOffset(call[0]))).toEqual(["0", "50", "100"]);
    expect(page.items.map((item) => item.id)).toEqual([1n, 2n, 3n]);
    expect(page.nextOffset).toBe(150);
    expect(page.counts).toEqual({ bucketTotal: 200, matchingTotal: 120, byStatus: { planned: 120 } });
  });

  it("stops at the end of the result set and reports no further page", async () => {
    const fetchMock = pagedFetch([
      { items: [1], nextOffset: 50 },
      { items: [2], nextOffset: null },
    ]);
    const page = await fetchItemWindow(fetchMock, "1", filters, 500);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(page.items.map((item) => item.id)).toEqual([1n, 2n]);
    expect(page.nextOffset).toBeUndefined();
  });

  it("returns a shorter window when rows left the filter", async () => {
    const fetchMock = pagedFetch([
      { items: [1, 2], nextOffset: 50 },
      { items: [3], nextOffset: null },
    ]);
    const page = await fetchItemWindow(fetchMock, "1", filters, 100);
    expect(page.items.map((item) => item.id)).toEqual([1n, 2n, 3n]);
  });

  it("keeps one copy of a row that a concurrent write moved between pages", async () => {
    const fetchMock = pagedFetch([
      { items: [1, 2], nextOffset: 50 },
      { items: [2, 3], nextOffset: null },
    ]);
    const page = await fetchItemWindow(fetchMock, "1", filters, 100);
    expect(page.items.map((item) => item.id)).toEqual([1n, 2n, 3n]);
  });

  it("propagates the abort signal to every page request", async () => {
    const signal: AbortSignalLike = { aborted: false };
    const fetchMock = pagedFetch([
      { items: [1], nextOffset: 50 },
      { items: [2], nextOffset: null },
    ]);
    await fetchItemWindow(fetchMock, "1", filters, 100, signal);
    expect(fetchMock.mock.calls.map((call) => call[1])).toEqual([{ signal }, { signal }]);
  });

  it("surfaces a failure on a later page", async () => {
    const fetchMock = vi.fn()
      .mockResolvedValueOnce(jsonResponse(200, { items: [], nextOffset: 50 }))
      .mockResolvedValueOnce(jsonResponse(500, { error: "page two unavailable" }));
    await expect(fetchItemWindow(fetchMock, "1", filters, 100)).rejects.toThrow("page two unavailable");
  });
});
