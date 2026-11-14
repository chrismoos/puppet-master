import type { CSSProperties, ReactNode } from "react";
import { ALL_SCOPE, scopeKey, type SidebarScope } from "./sidebarScope";
import type { RailIcon, SwitcherBucket } from "./BucketSwitcher";
import { bucketMonogram, railMonograms } from "@puppet-master/client-core/state/buckets";

export { bucketMonogram, railMonograms } from "@puppet-master/client-core/state/buckets";

const LONG_FACE_CHARS = 3;

/** What a rail button shows: the bucket's picture, its own text, or its letters. */
export function RailFace({ icon, letters }: { icon?: RailIcon; letters: string }) {
  if (icon?.image) return <img className="sb-rail-image" src={icon.image} alt="" />;
  const text = icon?.text || letters;
  return (
    <span className={`sb-rail-glyph ${[...text].length >= LONG_FACE_CHARS ? "is-long" : ""}`} aria-hidden="true">
      {text}
    </span>
  );
}

export function railColorStyle(icon?: RailIcon): CSSProperties | undefined {
  return icon?.color ? ({ "--rail-color": icon.color } as CSSProperties) : undefined;
}

/** A narrow column of scopes beside the sidebar: every bucket, the needs-you list, search, and each bucket. */
export function BucketRail({
  scope,
  buckets,
  searchOpen,
  onPickScope,
  onOpenSearch,
  header,
}: {
  scope: SidebarScope;
  buckets: readonly SwitcherBucket[];
  searchOpen: boolean;
  onPickScope: (scope: SidebarScope) => void;
  onOpenSearch: () => void;
  /** Above the rail's buttons. */
  header?: ReactNode;
}) {
  const current = scopeKey(scope);
  const monograms = railMonograms(buckets);
  const button = (
    target: SidebarScope,
    label: string,
    face: ReactNode,
    counts: { needs: number; needsUnseen: number; finished: number; working: number },
    icon?: RailIcon,
  ) => {
    const isCurrent = scopeKey(target) === current;
    const { needs, needsUnseen, finished, working } = counts;
    const status = [
      needs > 0 ? `${needs} need input` : "",
      working > 0 ? `${working} working` : "",
      finished > 0 ? `${finished} finished` : "",
    ].filter(Boolean);
    const classes = [
      "sb-rail-btn",
      isCurrent ? "is-current" : "",
      needs > 0 ? "has-needs" : "",
      needsUnseen > 0 ? "has-unseen-needs" : "",
      icon?.color ? "has-color" : "",
      icon?.image ? "has-image" : "",
    ].filter(Boolean).join(" ");
    return (
      <button
        key={scopeKey(target)}
        type="button"
        className={classes}
        style={railColorStyle(icon)}
        aria-pressed={isCurrent}
        aria-label={status.length > 0 ? `${label}, ${status.join(", ")}` : label}
        title={label}
        onClick={() => onPickScope(target)}
      >
        {face}
        {needs > 0 && <span className="sb-rail-badge" aria-hidden="true">{needs}</span>}
        {working > 0 && <span className="sb-rail-working" aria-hidden="true" />}
        {finished > 0 && <span className="sb-rail-finished" aria-hidden="true" />}
      </button>
    );
  };
  const none = { needs: 0, needsUnseen: 0, finished: 0, working: 0 };
  return (
    <nav className="sb-rail" aria-label="buckets">
      {header && <div className="sb-rail-header">{header}</div>}
      <button
        type="button"
        className={`sb-rail-btn sb-rail-quiet ${searchOpen ? "is-current" : ""}`}
        aria-label="Search sessions"
        aria-expanded={searchOpen}
        data-switcher-toggle
        title="Find a bucket or session (/)"
        onClick={onOpenSearch}
      >
        <svg className="sb-rail-quiet-glyph" viewBox="0 0 16 16" aria-hidden="true">
          <circle cx="7" cy="7" r="4.5" />
          <path d="M10.5 10.5 14 14" />
        </svg>
      </button>
      {button(
        ALL_SCOPE,
        "All buckets",
        <svg className="sb-rail-glyph sb-rail-all-glyph" viewBox="0 0 16 16" aria-hidden="true">
          <rect x="2.5" y="2.5" width="4.5" height="4.5" rx="1" />
          <rect x="9" y="2.5" width="4.5" height="4.5" rx="1" />
          <rect x="2.5" y="9" width="4.5" height="4.5" rx="1" />
          <rect x="9" y="9" width="4.5" height="4.5" rx="1" />
        </svg>,
        none,
      )}
      <span className="sb-rail-sep" aria-hidden="true" />
      {buckets.map((bucket) => button(
        { kind: "bucket", bucketId: bucket.id },
        bucket.name,
        <RailFace icon={bucket.icon} letters={monograms.get(bucket.id) ?? bucketMonogram(bucket.name)} />,
        {
          needs: bucket.needsInput,
          needsUnseen: bucket.needsInputUnseen ?? 0,
          finished: bucket.finishedUnseen ?? 0,
          working: bucket.working ?? 0,
        },
        bucket.icon,
      ))}
    </nav>
  );
}
