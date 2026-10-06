import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import {
  ItemSchema,
  ItemStatus,
  ProjectSchema,
  SessionSchema,
  type Item,
} from "../gen/pm/v1/pm_pb";
import { compareLinkedItems, linkedItemSummary } from "./linkedItems";

const SESSION_ID = 900719925474099312345n;

function item(
  id: bigint,
  status: ItemStatus,
  updatedAtUnixMs: bigint,
  overrides: Partial<Omit<Item, "$typeName" | "$unknown">> = {},
) {
  return create(ItemSchema, {
    id,
    bucketId: 7n,
    title: `Item ${id}`,
    status,
    updatedAtUnixMs,
    sessionIds: [SESSION_ID],
    ...overrides,
  });
}

describe("linkedItemSummary", () => {
  const session = create(SessionSchema, { id: SESSION_ID, projectId: 3n });
  const projects = new Map([
    ["3", create(ProjectSchema, { id: 3n, bucketId: 7n })],
  ]);

  it("selects attention, active, waiting, planned, then closed items deterministically", () => {
    const done = item(10n, ItemStatus.DONE, 500n);
    const planned = item(11n, ItemStatus.PLANNED, 400n);
    const waiting = item(12n, ItemStatus.BLOCKED_EXTERNAL, 300n);
    const active = item(13n, ItemStatus.IN_PROGRESS, 200n);
    const attention = item(14n, ItemStatus.PLANNED, 100n, { question: "Choose one" });
    const items = new Map([done, planned, waiting, active, attention].map((value) => [value.id.toString(), value]));

    const summary = linkedItemSummary({ items, projects }, session);

    expect(summary?.primary.id).toBe(14n);
    expect(summary?.remaining.map(({ id }) => id)).toEqual([13n, 12n, 11n, 10n]);
  });

  it("uses recency and the lossless id as stable tie breakers", () => {
    const older = item(900719925474099312346n, ItemStatus.IN_PROGRESS, 100n);
    const lower = item(900719925474099312347n, ItemStatus.IN_PROGRESS, 200n);
    const higher = item(900719925474099312348n, ItemStatus.IN_PROGRESS, 200n);

    expect([older, lower, higher].sort(compareLinkedItems).map(({ id }) => id)).toEqual([
      900719925474099312348n,
      900719925474099312347n,
      900719925474099312346n,
    ]);
  });

  it("keeps completed-only history and excludes missing, cross-bucket, and unrelated links", () => {
    const completed = item(21n, ItemStatus.DONE, 200n);
    const dropped = item(22n, ItemStatus.DROPPED, 100n);
    const crossBucket = item(23n, ItemStatus.IN_PROGRESS, 300n, { bucketId: 8n });
    const unrelated = item(24n, ItemStatus.IN_PROGRESS, 400n, { sessionIds: [1n] });
    const items = new Map([completed, dropped, crossBucket, unrelated].map((value) => [value.id.toString(), value]));

    expect(linkedItemSummary({ items, projects }, session)?.primary.id).toBe(21n);
    expect(linkedItemSummary({ items: new Map(), projects }, session)).toBeNull();
    expect(linkedItemSummary({ items, projects: new Map() }, session)).toBeNull();
  });
});
