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
}: {
  scope: SidebarScope;
  buckets: readonly SwitcherBucket[];
  searchOpen: boolean;
  onPickScope: (scope: SidebarScope) => void;
  onOpenSearch: () => void;
}) {
  const needsTotal = buckets.reduce((sum, bucket) => sum + bucket.needsInput, 0);
  const current = scopeKey(scope);
  const monograms = railMonograms(buckets);
  const button = (target: SidebarScope, label: string, face: ReactNode, needs: number, icon?: RailIcon) => {
    const isCurrent = scopeKey(target) === current;
    return (
      <button
        key={scopeKey(target)}
        type="button"
        className={`sb-rail-btn ${isCurrent ? "is-current" : ""} ${icon?.color ? "has-color" : ""} ${icon?.image ? "has-image" : ""}`}
        style={railColorStyle(icon)}
        aria-pressed={isCurrent}
        aria-label={needs > 0 ? `${label}, ${needs} need input` : label}
        title={label}
        onClick={() => onPickScope(target)}
      >
        {face}
        {needs > 0 && <span className="sb-rail-badge" aria-hidden="true">{needs}</span>}
      </button>
    );
  };
  return (
    <nav className="sb-rail" aria-label="buckets">
      {button(ALL_SCOPE, "All buckets", <span className="sb-rail-glyph" aria-hidden="true">All</span>, 0)}
      {button({ kind: "needs" }, "Needs you", <span className="sb-rail-glyph" aria-hidden="true">!</span>, needsTotal)}
      <button
        type="button"
        className={`sb-rail-btn ${searchOpen ? "is-current" : ""}`}
        aria-label="Search sessions"
        aria-expanded={searchOpen}
        data-switcher-toggle
        title="Find a bucket or session (/)"
        onClick={onOpenSearch}
      >
        <svg className="sb-rail-glyph" viewBox="0 0 16 16" aria-hidden="true">
          <circle cx="7" cy="7" r="4.5" />
          <path d="M10.5 10.5 14 14" />
        </svg>
      </button>
      <span className="sb-rail-sep" aria-hidden="true" />
      {buckets.map((bucket) => button(
        { kind: "bucket", bucketId: bucket.id },
        bucket.name,
        <RailFace icon={bucket.icon} letters={monograms.get(bucket.id) ?? bucketMonogram(bucket.name)} />,
        bucket.needsInput,
        bucket.icon,
      ))}
    </nav>
  );
}
