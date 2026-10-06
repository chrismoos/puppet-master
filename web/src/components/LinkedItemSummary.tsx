import type { KeyboardEvent, MouseEvent } from "react";
import { ItemStatus, type Item } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import type { PmLink } from "@puppet-master/client-core/pmlink";
import { STATUS_LABELS } from "@puppet-master/client-core/state/board";
import type { LinkedItemSummary as Summary } from "@puppet-master/client-core/state/linkedItems";

function presentation(item: Item): { label: string; className: string } {
  if (item.question.length > 0) return { label: "needs you", className: "is-needs-you" };
  switch (item.status) {
    case ItemStatus.INBOX:
    case ItemStatus.BLOCKED:
      return { label: "needs you", className: "is-needs-you" };
    case ItemStatus.IN_PROGRESS:
      return { label: "in progress", className: "is-in-progress" };
    case ItemStatus.BLOCKED_EXTERNAL:
      return { label: "waiting", className: "is-waiting" };
    case ItemStatus.DONE:
    case ItemStatus.DROPPED:
      return { label: STATUS_LABELS.get(item.status) ?? "closed", className: "is-closed" };
    default:
      return { label: "planned", className: "is-planned" };
  }
}

function itemDescription(item: Item): string {
  return `#${item.id.toString()} · ${item.title} — ${presentation(item).label}`;
}

export function LinkedItemSummary({
  summary,
  sessionId,
  onPmLink,
  showStatus = true,
}: {
  summary: Summary;
  sessionId: string;
  onPmLink: (link: PmLink, sourceSessionId: string) => void;
  showStatus?: boolean;
}) {
  const { primary, remaining } = summary;
  const status = presentation(primary);
  const primaryDescription = itemDescription(primary);
  const remainingDescription = remaining.map(itemDescription).join("\n");
  const activate = (
    event: MouseEvent<HTMLAnchorElement> | KeyboardEvent<HTMLAnchorElement>,
  ) => {
    event.preventDefault();
    event.stopPropagation();
    onPmLink({ kind: "item", bucketId: primary.bucketId.toString(), id: primary.id.toString() }, sessionId);
  };

  return (
    <span className="sb-linked-item-row">
      <a
        className="sb-linked-item"
        href={`#/bucket/${primary.bucketId.toString()}/item/${primary.id.toString()}`}
        title={primaryDescription}
        aria-label={primaryDescription}
        onClick={activate}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === " ") activate(event);
        }}
      >
        <span className="sb-linked-item-ref">#{primary.id.toString()}</span>
        <span className="sb-linked-item-separator" aria-hidden="true">·</span>
        <span className="sb-linked-item-title">{primary.title}</span>
        {showStatus && (
          <span className={`sb-linked-item-status ${status.className}`}>{status.label}</span>
        )}
      </a>
      {remaining.length > 0 && (
        <span
          className="sb-linked-item-more"
          title={remainingDescription}
          tabIndex={0}
          aria-label={`${remaining.length} additional linked item${remaining.length === 1 ? "" : "s"}: ${remainingDescription.replaceAll("\n", "; ")}`}
        >
          +{remaining.length}
        </span>
      )}
    </span>
  );
}
