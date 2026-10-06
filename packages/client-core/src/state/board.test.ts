import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import {
  ItemPriority,
  ItemSchema,
  ItemSourceKind,
  ItemStatus,
  ProjectSchema,
  SessionSchema,
  SessionState,
  type Item,
} from "../gen/pm/v1/pm_pb";
import {
  boardMetricCounts,
  buildBoard,
  isSnoozed,
  needsYouCount,
  resolveBlockerIndicator,
  resolveProjectName,
} from "./board";
import { itemKey } from "./reducer";

const NOW = 1_700_000_000_000;

function item(
  id: number,
  status: ItemStatus,
  overrides: Partial<Omit<Item, "$typeName" | "$unknown">> = {},
): Item {
  return create(ItemSchema, {
    id: BigInt(id),
    bucketId: 1n,
    title: `item ${id}`,
    status,
    priority: ItemPriority.NORMAL,
    sourceKind: ItemSourceKind.AGENT,
    createdAtUnixMs: BigInt(NOW - 1000),
    updatedAtUnixMs: BigInt(NOW - 1000),
    ...overrides,
  });
}

describe("buildBoard", () => {
  it("groups by status with inbox and blocked needing the user", () => {
    const board = buildBoard(
      [
        item(1, ItemStatus.INBOX),
        item(2, ItemStatus.BLOCKED),
        item(3, ItemStatus.IN_PROGRESS),
        item(4, ItemStatus.BLOCKED_EXTERNAL),
        item(5, ItemStatus.PLANNED),
        item(6, ItemStatus.DONE),
        item(7, ItemStatus.DROPPED),
      ],
      "1",
      NOW,
    );
    expect(board.needsYou.map((i) => Number(i.id))).toEqual([1, 2]);
    expect(board.inProgress.map((i) => Number(i.id))).toEqual([3]);
    expect(board.blockedExternal.map((i) => Number(i.id))).toEqual([4]);
    expect(board.planned.map((i) => Number(i.id))).toEqual([5]);
    // Closed items render newest first.
    expect(board.closed.map((i) => Number(i.id))).toEqual([7, 6]);
  });

  it("puts items with questions in needs you regardless of status", () => {
    const board = buildBoard(
      [
        item(1, ItemStatus.PLANNED, { question: "Which approach?" }),
        item(2, ItemStatus.DONE, { question: "Can I ship this?" }),
        item(3, ItemStatus.IN_PROGRESS),
      ],
      "1",
      NOW,
    );
    expect(board.needsYou.map((i) => Number(i.id))).toEqual([1, 2]);
    expect(board.planned).toEqual([]);
    expect(board.closed).toEqual([]);
    expect(board.inProgress.map((i) => Number(i.id))).toEqual([3]);
  });

  it("only shows the requested bucket", () => {
    const other = item(9, ItemStatus.INBOX, { bucketId: 2n });
    const board = buildBoard([item(1, ItemStatus.INBOX), other], "1", NOW);
    expect(board.needsYou.map((i) => Number(i.id))).toEqual([1]);
  });

  it("narrows every group to the selected project without losing bigint precision", () => {
    const selectedProjectId = 9_007_199_254_740_993n;
    const otherProjectId = 9_007_199_254_740_994n;
    const board = buildBoard(
      [
        item(1, ItemStatus.INBOX, { projectId: selectedProjectId }),
        item(2, ItemStatus.IN_PROGRESS, { projectId: otherProjectId }),
        item(3, ItemStatus.DONE, { projectId: selectedProjectId }),
        item(4, ItemStatus.PLANNED),
      ],
      "1",
      NOW,
      selectedProjectId.toString(),
    );

    expect(board.needsYou.map((i) => i.id)).toEqual([1n]);
    expect(board.inProgress).toEqual([]);
    expect(board.planned).toEqual([]);
    expect(board.closed.map((i) => i.id)).toEqual([3n]);
  });

  it("shows tagged and untagged items when all projects are selected", () => {
    const board = buildBoard(
      [
        item(1, ItemStatus.INBOX, { projectId: 10n }),
        item(2, ItemStatus.IN_PROGRESS, { projectId: 11n }),
        item(3, ItemStatus.PLANNED),
      ],
      "1",
      NOW,
    );

    expect(board.needsYou.map((i) => i.id)).toEqual([1n]);
    expect(board.inProgress.map((i) => i.id)).toEqual([2n]);
    expect(board.planned.map((i) => i.id)).toEqual([3n]);
  });

  it("sorts by priority then due date", () => {
    const board = buildBoard(
      [
        item(1, ItemStatus.INBOX, { priority: ItemPriority.LOW }),
        item(2, ItemStatus.INBOX, { priority: ItemPriority.URGENT }),
        item(3, ItemStatus.INBOX, { dueAtUnixMs: BigInt(NOW + 1000) }),
        item(4, ItemStatus.INBOX),
      ],
      "1",
      NOW,
    );
    expect(board.needsYou.map((i) => Number(i.id))).toEqual([2, 3, 4, 1]);
  });

  it("parks active snoozed items but not closed ones", () => {
    const snoozed = item(1, ItemStatus.INBOX, { snoozedUntilUnixMs: BigInt(NOW + 60_000) });
    const expired = item(2, ItemStatus.INBOX, { snoozedUntilUnixMs: BigInt(NOW - 60_000) });
    const doneSnoozed = item(3, ItemStatus.DONE, { snoozedUntilUnixMs: BigInt(NOW + 60_000) });
    const board = buildBoard([snoozed, expired, doneSnoozed], "1", NOW);
    expect(board.snoozed.map((i) => Number(i.id))).toEqual([1]);
    expect(board.needsYou.map((i) => Number(i.id))).toEqual([2]);
    expect(board.closed.map((i) => Number(i.id))).toEqual([3]);
    expect(isSnoozed(snoozed, NOW)).toBe(true);
    expect(isSnoozed(expired, NOW)).toBe(false);
  });
});

describe("boardMetricCounts", () => {
  it("keeps bucket totals live and applies the metric meanings", () => {
    const live = create(SessionSchema, { id: 21n, state: SessionState.IDLE });
    const exited = create(SessionSchema, { id: 22n, state: SessionState.EXITED });
    const counts = boardMetricCounts([
      item(1, ItemStatus.INBOX),
      item(2, ItemStatus.PLANNED, { question: "decide", sessionIds: [21n] }),
      item(3, ItemStatus.IN_PROGRESS, { sessionIds: [22n] }),
      item(4, ItemStatus.BLOCKED_EXTERNAL),
      item(5, ItemStatus.DONE, { updatedAtUnixMs: BigInt(NOW - 1000) }),
      item(6, ItemStatus.DONE, { updatedAtUnixMs: BigInt(NOW - 8 * 24 * 60 * 60 * 1000) }),
      item(7, ItemStatus.BLOCKED, { snoozedUntilUnixMs: BigInt(NOW + 1000) }),
      item(8, ItemStatus.PLANNED, { bucketId: 2n, sessionIds: [21n] }),
    ], [live, exited], "1", NOW);

    expect(counts).toEqual({
      needsYou: 2,
      inProgress: 1,
      planned: 1,
      blockedExternal: 1,
      doneRecently: 1,
      liveLinked: 1,
    });
  });
});

describe("resolveProjectName", () => {
  it("resolves a project chip label and omits it from untagged items", () => {
    const projectId = 9_007_199_254_740_993n;
    const project = create(ProjectSchema, { id: projectId, bucketId: 1n, name: "frontend" });
    const projects = new Map([[project.id.toString(), project]]);

    expect(
      resolveProjectName(item(1, ItemStatus.PLANNED, { projectId }), projects),
    ).toBe("frontend");
    expect(resolveProjectName(item(2, ItemStatus.PLANNED), projects)).toBeNull();
  });
});

describe("needsYouCount", () => {
  it("counts statuses and questions needing the user, skipping snoozed and other buckets", () => {
    const items = [
      item(1, ItemStatus.INBOX),
      item(2, ItemStatus.BLOCKED),
      item(3, ItemStatus.PLANNED, { question: "What next?" }),
      item(4, ItemStatus.INBOX, { snoozedUntilUnixMs: BigInt(NOW + 1000) }),
      item(5, ItemStatus.INBOX, { bucketId: 2n }),
    ];
    expect(needsYouCount(items, "1", NOW)).toBe(3);
    expect(needsYouCount(items, "2", NOW)).toBe(1);
  });
});

describe("resolveBlockerIndicator", () => {
  it("labels a single blocker with its title and pm item link", () => {
    const blocker = item(1, ItemStatus.IN_PROGRESS, { title: "Water the office plants" });
    const blocked = item(2, ItemStatus.PLANNED, { blockedBy: [blocker.id] });

    expect(resolveBlockerIndicator(blocked, new Map([[itemKey(blocker.bucketId, blocker.id), blocker]]))).toEqual({
      blockers: [{ id: 1n, title: "Water the office plants" }],
      label: "blocked by: Water the office plants",
      title: "Water the office plants — pm:item/1/1",
    });
  });

  it("uses an item count when several blockers remain open", () => {
    const first = item(1, ItemStatus.IN_PROGRESS, { title: "First dependency" });
    const second = item(2, ItemStatus.PLANNED, { title: "Second dependency" });
    const blocked = item(3, ItemStatus.PLANNED, { blockedBy: [first.id, second.id] });
    const items = new Map([
      [itemKey(first.bucketId, first.id), first],
      [itemKey(second.bucketId, second.id), second],
    ]);

    expect(resolveBlockerIndicator(blocked, items)).toMatchObject({
      label: "blocked by 2 items",
      title: "First dependency — pm:item/1/1\nSecond dependency — pm:item/1/2",
    });
  });

  it("falls back to an unknown blocker id without losing bigint precision", () => {
    const unknownId = 9_007_199_254_740_993n;
    const blocked = item(1, ItemStatus.PLANNED, { blockedBy: [unknownId] });

    expect(resolveBlockerIndicator(blocked, new Map())).toEqual({
      blockers: [{ id: unknownId, title: unknownId.toString() }],
      label: `blocked by: ${unknownId}`,
      title: `${unknownId} — pm:item/1/${unknownId}`,
    });
  });

  it("filters out done and dropped blockers", () => {
    const open = item(1, ItemStatus.IN_PROGRESS, { title: "Still open" });
    const done = item(2, ItemStatus.DONE);
    const dropped = item(3, ItemStatus.DROPPED);
    const blocked = item(4, ItemStatus.PLANNED, {
      blockedBy: [open.id, done.id, dropped.id],
    });
    const items = new Map(
      [open, done, dropped].map((candidate) => [itemKey(candidate.bucketId, candidate.id), candidate] as const),
    );

    expect(resolveBlockerIndicator(blocked, items)).toMatchObject({
      blockers: [{ id: 1n, title: "Still open" }],
      label: "blocked by: Still open",
    });
  });

  it("returns no indicator when every blocker is satisfied", () => {
    const done = item(1, ItemStatus.DONE);
    const dropped = item(2, ItemStatus.DROPPED);
    const blocked = item(3, ItemStatus.PLANNED, { blockedBy: [done.id, dropped.id] });
    const items = new Map([
      [itemKey(done.bucketId, done.id), done],
      [itemKey(dropped.bucketId, dropped.id), dropped],
    ]);

    expect(resolveBlockerIndicator(blocked, items)).toBeNull();
  });
});
