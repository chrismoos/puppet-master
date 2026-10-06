import { ItemStatus, type Item, type Session } from "../gen/pm/v1/pm_pb";
import type { AppState } from "./reducer";

export interface LinkedItemSummary {
  primary: Item;
  remaining: Item[];
}

function itemRank(item: Item): number {
  if (item.question.length > 0) return 0;
  switch (item.status) {
    case ItemStatus.INBOX:
    case ItemStatus.BLOCKED:
      return 0;
    case ItemStatus.IN_PROGRESS:
      return 1;
    case ItemStatus.BLOCKED_EXTERNAL:
      return 2;
    case ItemStatus.PLANNED:
    case ItemStatus.UNSPECIFIED:
      return 3;
    case ItemStatus.DONE:
    case ItemStatus.DROPPED:
      return 4;
  }
}

function compareBigintDesc(a: bigint, b: bigint): number {
  return a === b ? 0 : a > b ? -1 : 1;
}

export function compareLinkedItems(a: Item, b: Item): number {
  return (
    itemRank(a) - itemRank(b) ||
    compareBigintDesc(a.updatedAtUnixMs, b.updatedAtUnixMs) ||
    compareBigintDesc(a.id, b.id)
  );
}

export function linkedItemSummary(
  state: Pick<AppState, "items" | "projects">,
  session: Session,
): LinkedItemSummary | null {
  const bucketId = state.projects.get(session.projectId.toString())?.bucketId.toString();
  if (!bucketId) return null;

  const sessionId = session.id.toString();
  const linked = [...state.items.values()]
    .filter(
      (item) =>
        item.bucketId.toString() === bucketId &&
        item.sessionIds.some((id) => id.toString() === sessionId),
    )
    .sort(compareLinkedItems);
  const primary = linked[0];
  return primary ? { primary, remaining: linked.slice(1) } : null;
}
