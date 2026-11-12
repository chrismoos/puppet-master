import { useEffect, useId, useRef, useState, type KeyboardEvent } from "react";
import { ALL_SCOPE, scopeKey, type SidebarScope } from "./sidebarScope";

export interface SwitcherBucket {
  id: string;
  name: string;
  /** Sessions in the bucket that are not ended. */
  live: number;
  needsInput: number;
  needsInputUnseen?: number;
  /** Idle sessions whose finished turn has not been viewed. */
  finishedUnseen?: number;
  icon?: RailIcon;
}

/** A bucket's own rail face; absent parts fall back to its letters and the default color. */
export interface RailIcon {
  text?: string;
  color?: string;
  image?: string;
}

export interface SwitcherSession {
  id: string;
  name: string;
  bucketName: string | null;
}

/** Session results the menu lists before offering the full result list. */
export const SWITCHER_SESSION_LIMIT = 8;

type MenuItem =
  | { kind: "scope"; scope: SidebarScope }
  | { kind: "session"; id: string }
  | { kind: "all-results" };

export function matchingBuckets(buckets: readonly SwitcherBucket[], query: string): SwitcherBucket[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return [...buckets];
  return buckets.filter((bucket) => bucket.name.toLowerCase().includes(needle));
}

/** Badge for the closed switcher: sessions needing input that the current scope does not list. */
export function needsElsewhere(scope: SidebarScope, buckets: readonly SwitcherBucket[]): number {
  if (scope.kind !== "bucket") return 0;
  return buckets.reduce((sum, bucket) => sum + (bucket.id === scope.bucketId ? 0 : bucket.needsInput), 0);
}

export function BucketSwitcher({
  scope,
  buckets,
  open,
  onOpenChange,
  query,
  onQuery,
  sessions,
  resultTotal,
  loading,
  onPickScope,
  onPickSession,
  onShowAllResults,
  inline = false,
}: {
  /** Shows only the open menu, laid over whatever holds it, with no trigger button. */
  inline?: boolean;
  scope: SidebarScope;
  buckets: readonly SwitcherBucket[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
  query: string;
  onQuery: (query: string) => void;
  /** Server search results for the query, in rank order. */
  sessions: readonly SwitcherSession[];
  resultTotal: number | null;
  loading: boolean;
  onPickScope: (scope: SidebarScope) => void;
  onPickSession: (id: string) => void;
  onShowAllResults: () => void;
}) {
  const rootRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const listId = useId();
  const [active, setActive] = useState(0);
  const searching = Boolean(query.trim());
  const needsTotal = buckets.reduce((sum, bucket) => sum + bucket.needsInput, 0);
  const liveTotal = buckets.reduce((sum, bucket) => sum + bucket.live, 0);
  const shownBuckets = matchingBuckets(buckets, query);
  const shownSessions = searching ? sessions.slice(0, SWITCHER_SESSION_LIMIT) : [];
  const moreResults = searching && resultTotal !== null && resultTotal > 0;

  const items: MenuItem[] = [
    ...(searching ? [] : [
      { kind: "scope", scope: ALL_SCOPE } as MenuItem,
      { kind: "scope", scope: { kind: "needs" } } as MenuItem,
    ]),
    ...shownBuckets.map((bucket) => ({ kind: "scope", scope: { kind: "bucket", bucketId: bucket.id } }) as MenuItem),
    ...shownSessions.map((session) => ({ kind: "session", id: session.id }) as MenuItem),
    ...(moreResults ? [{ kind: "all-results" } as MenuItem] : []),
  ];
  const activeIndex = Math.min(active, Math.max(0, items.length - 1));

  useEffect(() => setActive(0), [query, open]);

  useEffect(() => {
    if (!open) return;
    inputRef.current?.focus();
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Element;
      if (rootRef.current?.contains(target) || target.closest?.("[data-switcher-toggle]")) return;
      onOpenChange(false);
    };
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, [open, onOpenChange]);

  const activate = (item: MenuItem | undefined) => {
    if (!item) return;
    if (item.kind === "scope") onPickScope(item.scope);
    else if (item.kind === "session") onPickSession(item.id);
    else onShowAllResults();
  };

  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActive((activeIndex + 1) % Math.max(1, items.length));
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActive((activeIndex - 1 + items.length) % Math.max(1, items.length));
    } else if (event.key === "Enter") {
      event.preventDefault();
      activate(items[activeIndex]);
    } else if (event.key === "Escape") {
      event.preventDefault();
      onOpenChange(false);
    }
  };

  const optionId = (index: number) => `${listId}-${index}`;
  const isCurrent = (candidate: SidebarScope) => scopeKey(candidate) === scopeKey(scope);
  const rowProps = (item: MenuItem, index: number, columns = false) => ({
    id: optionId(index),
    role: "option" as const,
    "aria-selected": index === activeIndex,
    className: [
      "sb-switcher-item",
      columns ? "has-columns" : "",
      index === activeIndex ? "is-active" : "",
      item.kind === "scope" && isCurrent(item.scope) ? "is-current" : "",
    ].filter(Boolean).join(" "),
    onPointerMove: () => setActive(index),
    onClick: () => activate(item),
  });

  const current = scope.kind === "all"
    ? "All buckets"
    : scope.kind === "needs"
      ? "Needs you"
      : buckets.find((bucket) => bucket.id === scope.bucketId)?.name ?? "All buckets";
  const elsewhere = needsElsewhere(scope, buckets);
  let index = 0;

  return (
    <div className={`sb-switcher ${inline ? "is-inline" : ""}`} ref={rootRef}>
      {!inline && <button
        type="button"
        className="sb-switcher-trigger"
        aria-haspopup="listbox"
        aria-expanded={open}
        title="Switch bucket or search sessions (/)"
        onClick={() => onOpenChange(!open)}
      >
        <span className="sb-switcher-current">{current}</span>
        {elsewhere > 0 && (
          <span
            className="sb-switcher-elsewhere"
            title={`${elsewhere} session${elsewhere === 1 ? "" : "s"} in other buckets need input`}
          >
            +{elsewhere}
          </span>
        )}
        <span className="sb-caret" aria-hidden="true">▾</span>
      </button>}
      {open && (
        <div className="sb-switcher-menu">
          <div className="sb-switcher-search">
            <input
              ref={inputRef}
              type="search"
              role="combobox"
              aria-label="Find a bucket or session"
              aria-expanded="true"
              aria-controls={listId}
              aria-activedescendant={items.length > 0 ? optionId(activeIndex) : undefined}
              placeholder="Find a bucket or session"
              value={query}
              onChange={(event) => onQuery(event.target.value)}
              onKeyDown={onKeyDown}
            />
          </div>
          <div className="sb-switcher-list" id={listId} role="listbox" aria-label="buckets and sessions">
            {!searching && (
              <>
                <div {...rowProps(items[index], index++, true)}>
                  <span className="sb-switcher-name">All buckets</span>
                  <NeedsCell count={0} />
                  <span className="sb-switcher-count">{liveTotal}</span>
                </div>
                <div {...rowProps(items[index], index++, true)}>
                  <span className="sb-switcher-name">Needs you</span>
                  <NeedsCell count={needsTotal} />
                  <span className="sb-switcher-count" />
                </div>
                <div className="sb-switcher-sep" role="separator" />
              </>
            )}
            {searching && shownBuckets.length > 0 && <div className="sb-switcher-label">Buckets</div>}
            {shownBuckets.map((bucket) => (
              <div key={bucket.id} {...rowProps(items[index], index++, true)}>
                <span className="sb-switcher-name">{bucket.name}</span>
                <NeedsCell count={bucket.needsInput} />
                <span className="sb-switcher-count" title={`${bucket.live} live session${bucket.live === 1 ? "" : "s"}`}>
                  {bucket.live}
                </span>
              </div>
            ))}
            {shownSessions.length > 0 && <div className="sb-switcher-label">Sessions</div>}
            {shownSessions.map((session) => (
              <div key={session.id} {...rowProps(items[index], index++)}>
                <span className="sb-switcher-name">{session.name}</span>
                {session.bucketName && <span className="sb-switcher-bucket">{session.bucketName}</span>}
              </div>
            ))}
            {moreResults && (
              <div {...rowProps(items[index], index++)}>
                <span className="sb-switcher-name sb-switcher-more">
                  Show all {resultTotal} result{resultTotal === 1 ? "" : "s"}
                </span>
              </div>
            )}
            {searching && (
              <div className="sb-switcher-status" role="status" aria-live="polite">
                {loading ? "searching…" : items.length === 0 ? "no matches" : ""}
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function NeedsCell({ count }: { count: number }) {
  if (count === 0) return <span className="sb-switcher-needs-cell" />;
  return (
    <span className="sb-switcher-needs-cell">
      <span className="sb-switcher-needs" title={`${count} session${count === 1 ? "" : "s"} need input`}>
        {count}
      </span>
    </span>
  );
}
