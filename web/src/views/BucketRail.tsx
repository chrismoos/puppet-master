import type { CSSProperties, ReactNode } from "react";
import { ALL_SCOPE, scopeKey, type SidebarScope } from "./sidebarScope";
import type { RailIcon, SwitcherBucket } from "./BucketSwitcher";

const MONOGRAM_LETTERS = 2;
const LONG_FACE_CHARS = 3;

function words(name: string): string[] {
  return name.split(/[\s._-]+/).filter(Boolean);
}

/** Two letters for a rail button: the first two words' initials, or the first two letters of one word. */
export function bucketMonogram(name: string): string {
  const parts = words(name);
  const letters = parts.length >= MONOGRAM_LETTERS
    ? parts.slice(0, MONOGRAM_LETTERS).map((word) => [...word][0]).join("")
    : [...(parts[0] ?? "?")].slice(0, MONOGRAM_LETTERS).join("");
  return letters.toUpperCase();
}

/** Letters to try when the plain monogram is taken: the first letter with each later one, then three letters. */
function monogramCandidates(name: string): string[] {
  const letters = [...words(name).join("")].filter((char) => /\p{L}|\p{N}/u.test(char));
  const first = letters[0] ?? "?";
  const pairs = letters.slice(1).map((letter) => `${first}${letter}`);
  const parts = words(name);
  const triple = parts.length >= LONG_FACE_CHARS
    ? parts.slice(0, LONG_FACE_CHARS).map((word) => [...word][0]).join("")
    : letters.slice(0, LONG_FACE_CHARS).join("");
  return [bucketMonogram(name), ...pairs, triple].map((candidate) => candidate.toUpperCase());
}

/** Letters for every bucket, distinct wherever the names allow, earlier buckets keeping their plain monogram. */
export function railMonograms(buckets: readonly { id: string; name: string }[]): Map<string, string> {
  const plain = new Map<string, number>();
  for (const bucket of buckets) {
    const monogram = bucketMonogram(bucket.name);
    plain.set(monogram, (plain.get(monogram) ?? 0) + 1);
  }
  const taken = new Set<string>();
  const result = new Map<string, string>();
  for (const bucket of buckets) {
    const monogram = bucketMonogram(bucket.name);
    const candidates = plain.get(monogram) === 1 ? [monogram] : monogramCandidates(bucket.name);
    const pick = candidates.find((candidate) => !taken.has(candidate)) ?? monogram;
    taken.add(pick);
    result.set(bucket.id, pick);
  }
  return result;
}

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
