import type { KeyboardEvent, MouseEvent } from "react";
import {
  ContextKind,
  ContextSeverity,
  type ContextField,
} from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { classifyHref, type PmLink } from "@puppet-master/client-core/pmlink";

export function severityClass(severity: ContextSeverity): string {
  switch (severity) {
    case ContextSeverity.GOOD:
      return "ctx-good";
    case ContextSeverity.WARN:
      return "ctx-warn";
    case ContextSeverity.BAD:
      return "ctx-bad";
    case ContextSeverity.INFO:
      return "ctx-info";
    default:
      return "ctx-neutral";
  }
}

/** Parses a progress value into a 0-100 percent, or null if not a number. */
export function progressPercent(value: string): number | null {
  const n = Number.parseFloat(value);
  if (Number.isNaN(n)) return null;
  return Math.max(0, Math.min(100, n));
}

/**
 * One glance field rendered as a compact typed chip. Each chip carries
 * its label so a bare value like "n/a" is never ambiguous. `url` chips
 * are links that stop propagation so opening one never selects the row.
 */
function pmLinkHref(link: PmLink): string {
  switch (link.kind) {
    case "item":
      return `#/bucket/${link.bucketId}/item/${link.id}`;
    case "legacyItem":
      return `#/item/${link.legacyId}`;
    case "session":
      return `#/session/${link.id}`;
    case "bucket":
      return `#/bucket/${link.id}/board`;
    case "project":
    case "spawn":
      return "#pm-link";
  }
}

export function ContextChip({
  field,
  onPmLink,
}: {
  field: ContextField;
  onPmLink?: (link: PmLink) => void;
}) {
  const label = field.label || field.key;
  const title = `${label}: ${field.value}`;

  if (field.kind === ContextKind.URL) {
    const classified = classifyHref(field.value);
    if (classified.kind === "pm") {
      const activate = (
        event: MouseEvent<HTMLAnchorElement> | KeyboardEvent<HTMLAnchorElement>,
      ) => {
        event.preventDefault();
        event.stopPropagation();
        onPmLink?.(classified.link);
      };
      return (
        <a
          className="ctx-chip ctx-url"
          href={pmLinkHref(classified.link)}
          title={title}
          onClick={activate}
          onKeyDown={(event) => {
            if (event.key === "Enter" || event.key === " ") activate(event);
          }}
        >
          <span className="ctx-chip-label">{label}</span>
          <span className="ctx-chip-value">↗</span>
        </a>
      );
    }
    if (classified.kind === "inert") {
      return (
        <span className="ctx-chip" title={title}>
          <span className="ctx-chip-label">{label}</span>
          <span className="ctx-chip-value">{field.value}</span>
        </span>
      );
    }
    return (
      <a
        className="ctx-chip ctx-url"
        href={field.value}
        target="_blank"
        rel="noopener noreferrer"
        title={title}
        onClick={(e) => e.stopPropagation()}
      >
        <span className="ctx-chip-label">{label}</span>
        <span className="ctx-chip-value">↗</span>
      </a>
    );
  }

  if (field.kind === ContextKind.PROGRESS) {
    const pct = progressPercent(field.value);
    return (
      <span className="ctx-chip ctx-progress" title={title}>
        <span className="ctx-chip-label">{label}</span>
        {pct === null ? (
          <span className="ctx-chip-value">{field.value}</span>
        ) : (
          <>
            <span className="ctx-bar">
              <span className="ctx-bar-fill" style={{ width: `${pct}%` }} />
            </span>
            <span className="ctx-chip-value">{Math.round(pct)}%</span>
          </>
        )}
      </span>
    );
  }

  const sev = severityClass(field.severity);
  const valueClass =
    field.kind === ContextKind.CODE
      ? "ctx-chip-value ctx-code"
      : field.kind === ContextKind.BADGE || field.kind === ContextKind.METRIC
        ? `ctx-chip-value ${sev}`
        : "ctx-chip-value";
  return (
    <span className="ctx-chip" title={title}>
      <span className="ctx-chip-label">{label}</span>
      <span className={valueClass}>{field.value}</span>
    </span>
  );
}
