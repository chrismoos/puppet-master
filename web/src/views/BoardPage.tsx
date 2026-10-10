import { create } from "@bufbuild/protobuf";
import { useEffect, useRef, useState } from "react";
import {
  attachmentDownloadUrl,
  deleteItemAttachment,
  fetchItemAttachments,
  fetchItemNotes,
  fetchItems,
  fetchItemWindow,
  uploadItemAttachment,
  type ItemAttachment,
  type ItemNote,
  type ItemSearchCounts,
  type ItemSearchFilters,
} from "../api/items";
import { Markdown } from "../components/Markdown";
import { ItemReference } from "../components/ItemReference";
import { Popover } from "../components/Popover";
import { StateBadge } from "../components/StateBadge";
import { formatAgo, sessionDisplayName } from "@puppet-master/client-core/format";
import {
  ItemPriority,
  ItemSchema,
  ItemSourceKind,
  ItemStatus,
  PlanState,
  type Item,
  type UpsertItem,
} from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { classifyHref, type PmLink } from "@puppet-master/client-core/pmlink";
import {
  buildBoard,
  boardMetricCounts,
  PRIORITY_LABELS,
  resolveBlockerIndicator,
  STATUS_LABELS,
} from "@puppet-master/client-core/state/board";
import {
  boardHash,
  EMPTY_ITEM_FILTERS,
  hasItemFilters,
  itemFiltersEqual,
  parseBoardSearch,
} from "@puppet-master/client-core/state/boardSearch";
import { boardIndexStatus } from "@puppet-master/client-core/state/boardIndexStatus";
import {
  completedGroupCollapsed,
  completedMatchingCount,
  preferNewerLiveItem,
  readCompletedCollapse,
  writeCompletedCollapse,
} from "@puppet-master/client-core/state/boardCompleted";
import {
  buildItemResponseOptions,
  resolvePrimaryResponseTarget,
  type ItemResponseOption,
  type ItemResponseTarget,
} from "@puppet-master/client-core/state/itemResponse";
import {
  ITEM_BODY_MAX_CHARACTERS,
  itemBodyCharacterCount,
  itemBodyLengthError,
} from "@puppet-master/client-core/state/itemBodyLimit";
import { useAppState, useClient, useNow } from "../state/hooks";
import { itemKey } from "@puppet-master/client-core/state/reducer";

const SOURCE_LABELS = new Map<ItemSourceKind, string>([
  [ItemSourceKind.EMAIL, "email"], [ItemSourceKind.SLACK, "slack"],
  [ItemSourceKind.GITHUB, "github"], [ItemSourceKind.JIRA, "jira"],
  [ItemSourceKind.TEAMS, "teams"], [ItemSourceKind.TELEGRAM, "telegram"],
  [ItemSourceKind.HUMAN, "you"], [ItemSourceKind.AGENT, "agent"],
  [ItemSourceKind.OTHER, "other"],
]);

function ItemBodyLength({ value }: { value: string }) {
  const error = itemBodyLengthError(value);
  return <small className={error ? "field-error" : "muted-line"} role={error ? "alert" : undefined}>
    {error ?? `${itemBodyCharacterCount(value).toLocaleString()} / ${ITEM_BODY_MAX_CHARACTERS.toLocaleString()} characters`}
  </small>;
}
const PRIORITY_CLASSES = new Map<ItemPriority, string>([
  [ItemPriority.URGENT, "prio-urgent"], [ItemPriority.HIGH, "prio-high"],
  [ItemPriority.NORMAL, "prio-normal"], [ItemPriority.LOW, "prio-low"],
]);
const FILTER_STATUS = ["inbox", "planned", "in_progress", "blocked", "blocked_external", "done", "dropped"];
const FILTER_PRIORITY = ["urgent", "high", "normal", "low"];
const FILTER_SOURCE = ["email", "slack", "github", "jira", "teams", "telegram", "human", "agent", "other"];
const SUMMARY_METRICS = [
  { value: "needs_you", label: "needs you", count: "needsYou" },
  { value: "in_progress", label: "in progress", count: "inProgress" },
  { value: "planned", label: "planned", count: "planned" },
  { value: "blocked_external", label: "waiting external", count: "blockedExternal" },
  { value: "done_recently", label: "done 7d", count: "doneRecently" },
  { value: "live_linked", label: "live linked", count: "liveLinked" },
] as const;

type SaveState = "idle" | "saving" | "saved" | "error";

export function BoardPage({ bucketId, active, focusItemId, focusItem, onPmLink, onSpawnFromItem, planHref }: {
  bucketId: string;
  active: boolean;
  focusItemId?: string;
  focusItem?: Item;
  onPmLink: (link: PmLink) => void;
  onSpawnFromItem: (item: Item) => void;
  planHref: (sessionId: string, planId: string) => string;
}) {
  const state = useAppState();
  const now = useNow();
  const [filters, setFilters] = useState<ItemSearchFilters>(() =>
    location.hash.includes(`/bucket/${bucketId}/board`) ? parseBoardSearch(location.hash) : EMPTY_ITEM_FILTERS,
  );
  const [requestFilters, setRequestFilters] = useState(filters);
  const [items, setItems] = useState<Item[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(focusItemId ?? null);
  const [nextOffset, setNextOffset] = useState<number | undefined>();
  const [counts, setCounts] = useState<ItemSearchCounts | undefined>();
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [flash, setFlash] = useState<string | null>(null);
  const [reload, setReload] = useState(0);
  const [captureOpen, setCaptureOpen] = useState(false);
  const [briefingOpen, setBriefingOpen] = useState(false);
  const [completedCollapsedPreference, setCompletedCollapsedPreference] = useState(() => readCompletedCollapse(bucketId, sessionStorage));
  const [undo, setUndo] = useState<{ label: string; run: () => void } | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const indexRef = useRef<HTMLElement>(null);
  const activatedRef = useRef(false);
  const focusItemIdRef = useRef(focusItemId);
  const loadedWindowRef = useRef(0);
  const queriedRef = useRef({ bucketId, filters: requestFilters });

  useEffect(() => {
    if (!active) return;
    focusItemIdRef.current = focusItemId;
    if (!activatedRef.current) {
      setFilters(location.hash.includes(`/bucket/${bucketId}/board`) ? parseBoardSearch(location.hash) : EMPTY_ITEM_FILTERS);
      activatedRef.current = true;
    } else if (!focusItemId) {
      history.replaceState(null, "", boardHash(bucketId, filters));
    }
    if (focusItemId) setSelectedId(focusItemId);
    setCompletedCollapsedPreference(readCompletedCollapse(bucketId, sessionStorage));
  }, [active, bucketId, filters, focusItemId]);

  useEffect(() => {
    const timer = window.setTimeout(() => {
      // Re-parsing the same route produces an equal but distinct object. Keep
      // the current one so it cannot re-query and discard already loaded pages.
      setRequestFilters((current) => itemFiltersEqual(current, filters) ? current : filters);
      // The route can leave this board before a re-render clears the timer.
      if (active && !focusItemId && location.hash.includes(`/bucket/${bucketId}/board`)) {
        history.replaceState(null, "", boardHash(bucketId, filters));
      }
    }, 300);
    return () => window.clearTimeout(timer);
  }, [active, bucketId, filters, focusItemId]);

  useEffect(() => {
    const controller = new AbortController();
    // A refresh keeps the pages already on screen; a different bucket or filter
    // is a new result set and starts from one page again.
    if (queriedRef.current.bucketId !== bucketId || queriedRef.current.filters !== requestFilters) {
      queriedRef.current = { bucketId, filters: requestFilters };
      loadedWindowRef.current = 0;
    }
    setLoading(true);
    setSearchError(null);
    fetchItemWindow(bucketId, requestFilters, loadedWindowRef.current, controller.signal)
      .then((page) => {
        loadedWindowRef.current = page.nextOffset ?? page.items.length;
        setItems(page.items);
        setNextOffset(page.nextOffset);
        setCounts(page.counts);
        setSelectedId((current) => focusItemIdRef.current ?? (page.items.some((item) => item.id.toString() === current)
          ? current
          : page.items[0]?.id.toString() ?? null));
      })
      .catch((error: unknown) => {
        if (controller.signal.aborted) return;
        setSearchError(error instanceof Error ? error.message : String(error));
      })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [bucketId, reload, requestFilters]);

  useEffect(() => {
    if (!active) return;
    const onKey = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault(); searchRef.current?.focus();
      }
      if (event.key === "Escape" && captureOpen) setCaptureOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [active, captureOpen]);

  useEffect(() => {
    if (!undo) return;
    const timer = window.setTimeout(() => setUndo(null), 10_000);
    return () => window.clearTimeout(timer);
  }, [undo]);

  const bucket = state.buckets.get(bucketId);
  const projects = [...state.projects.values()].filter((project) => project.bucketId.toString() === bucketId).sort((a, b) => a.name.localeCompare(b.name));
  const plans = [...state.plans.values()]
    .filter((plan) => plan.bucketId.toString() === bucketId)
    .sort((a, b) => Number(b.updatedAtUnixMs - a.updatedAtUnixMs));
  const liveItems = items.map((item) => {
    const live = state.items.get(itemKey(item.bucketId, item.id));
    // Query responses own membership and may be newer than the live snapshot
    // immediately after a mutation. Overlay only genuinely newer live data;
    // otherwise a stale status can leak a completed row out of its collapse.
    return preferNewerLiveItem(item, live);
  });
  const focusedSnapshot = focusItemId ? state.items.get(itemKey(bucketId, focusItemId)) ?? focusItem : undefined;
  if (focusedSnapshot && !liveItems.some((item) => item.id === focusedSnapshot.id)) liveItems.unshift(focusedSnapshot);
  const board = buildBoard(liveItems, bucketId, now, undefined, true);
  const completedCollapsed = completedGroupCollapsed(completedCollapsedPreference, filters, focusedSnapshot);
  const completedAutoRevealed = completedCollapsedPreference && !completedCollapsed;
  const collapsedCompletedCount = completedCollapsed ? completedMatchingCount(counts) : 0;
  const grouped = [
    ["needs you", board.needsYou], ["in progress", board.inProgress],
    ["blocked external", board.blockedExternal], ["planned", board.planned],
    ["snoozed", board.snoozed], ["done / dropped", board.closed],
  ] as const;
  const selected = selectedId ? liveItems.find((item) => item.id.toString() === selectedId) ?? state.items.get(itemKey(bucketId, selectedId)) : undefined;
  const shownCount = grouped.reduce((total, [label, values]) =>
    total + (label === "done / dropped" && completedCollapsed ? 0 : values.length), 0);
  const visibleMatchingTotal = Math.max(0, (counts?.matchingTotal ?? shownCount) - collapsedCompletedCount);
  const canLoadMore = nextOffset !== undefined && (!completedCollapsed || shownCount < visibleMatchingTotal);
  const metricCounts = boardMetricCounts(state.items.values(), state.sessions.values(), bucketId, now);

  if (state.hydrated && !bucket) return <div className="pane-empty"><p className="muted-line">this bucket no longer exists</p></div>;

  const updateFilter = <K extends keyof ItemSearchFilters>(key: K, value: ItemSearchFilters[K]) => setFilters((current) => ({ ...current, [key]: value }));
  const clearFilters = () => {
    setFilters(EMPTY_ITEM_FILTERS);
    setCompletedCollapsedPreference(true);
    writeCompletedCollapse(bucketId, true, sessionStorage);
  };
  const toggleSummary = (value: string) => {
    updateFilter("summary", filters.summary === value ? "" : value);
    indexRef.current?.scrollTo({ top: 0 });
  };
  const reportError = (error: unknown) => setFlash(error instanceof Error ? error.message : String(error));
  const loadMore = () => {
    if (nextOffset === undefined || loadingMore) return;
    const from = nextOffset;
    const controller = new AbortController();
    setLoadingMore(true);
    fetchItems(bucketId, requestFilters, from, controller.signal)
      .then((page) => { loadedWindowRef.current = page.nextOffset ?? from + page.items.length; setItems((current) => [...current, ...page.items]); setNextOffset(page.nextOffset); setCounts((current) => page.counts ?? current); })
      .catch(reportError).finally(() => setLoadingMore(false));
  };

  return (
    <section className="workbench" aria-label={`workbench for ${bucket?.name ?? "bucket"}`}>
      <header className="workbench-head">
        <div><h1>{bucket?.name ?? "…"} board</h1><span className="board-live"><i />live</span></div>
        <label className={`workbench-search ${loading ? "is-loading" : ""}`}>
          <span aria-hidden="true">⌕</span>
          <input ref={searchRef} type="search" value={filters.query} onChange={(event) => updateFilter("query", event.target.value)} placeholder="Search title, context, source, URL…" aria-label="search work items" />
          {filters.query && <button type="button" aria-label="clear search" onClick={() => updateFilter("query", "")}>×</button>}
          <kbd>⌘K</kbd>
        </label>
      </header>

      {state.briefings.get(bucketId) && <section className="workbench-briefing">
        <button type="button" aria-expanded={briefingOpen} onClick={() => setBriefingOpen((open) => !open)}><span>{briefingOpen ? "▾" : "▸"}</span><strong>briefing</strong><em>{state.briefings.get(bucketId)!.markdown.split("\n")[0]}</em></button>
        <span>{formatAgo(now - Number(state.briefings.get(bucketId)!.tsUnixMs))} ago</span>
        {briefingOpen && <Markdown text={state.briefings.get(bucketId)!.markdown} onPmLink={onPmLink} />}
      </section>}

      {plans.length > 0 && <section className="workbench-plans" aria-label="bucket plans">
        <strong>plans</strong>
        <div>{plans.map((plan) => <a key={plan.id.toString()} href={planHref(plan.owningSessionId.toString(), plan.id.toString())} target="_blank" rel="noreferrer">
          <span>{plan.name}</span>
          <small>{state.projects.get(plan.projectId.toString())?.name ?? "project"}{plan.activeDecisionId !== undefined ? " · decision ready" : plan.state === PlanState.ACCEPTED ? " · accepted" : " · active"}</small>
          {plan.activeDecisionId !== undefined && <i aria-label="decision ready" />}
        </a>)}</div>
      </section>}

      <div className="workbench-metrics" role="group" aria-label="board summary">
        {SUMMARY_METRICS.map((metric) => {
          const active = filters.summary === metric.value;
          const count = metricCounts[metric.count];
          const description = metric.value === "live_linked" ? "items with live linked sessions" : `${metric.label} items`;
          return <button
            type="button"
            key={metric.value}
            className={active ? "is-active" : ""}
            aria-pressed={active}
            aria-label={`${active ? "Clear" : "Filter to"} ${description}, ${count} in bucket`}
            onClick={() => toggleSummary(metric.value)}
          ><span>{metric.label}</span><b>{count}</b></button>;
        })}
      </div>

      <div className="workbench-filters" aria-label="item filters">
        <button className="btn btn-primary workbench-new" type="button" onClick={() => setCaptureOpen(true)}>＋ new item</button>
        <select aria-label="filter by project" value={filters.project} onChange={(e) => updateFilter("project", e.target.value)}><option value="">all projects</option>{projects.map((project) => <option key={project.id.toString()} value={project.id.toString()}>{project.name}</option>)}</select>
        <select aria-label="filter by status" value={filters.status} onChange={(e) => updateFilter("status", e.target.value)}><option value="">all statuses</option>{FILTER_STATUS.map((value) => <option key={value} value={value}>{value.replace("_", " ")}</option>)}</select>
        <select aria-label="filter by priority" value={filters.priority} onChange={(e) => updateFilter("priority", e.target.value)}><option value="">all priorities</option>{FILTER_PRIORITY.map((value) => <option key={value}>{value}</option>)}</select>
        <select aria-label="filter by source" value={filters.source} onChange={(e) => updateFilter("source", e.target.value)}><option value="">all sources</option>{FILTER_SOURCE.map((value) => <option key={value}>{value}</option>)}</select>
        <label><input type="checkbox" checked={filters.includeDone} onChange={(e) => updateFilter("includeDone", e.target.checked)} /> done</label>
        <label><input type="checkbox" checked={filters.includeSnoozed} onChange={(e) => updateFilter("includeSnoozed", e.target.checked)} /> snoozed</label>
        {hasItemFilters(filters) && <button className="link-btn" type="button" onClick={clearFilters}>clear filters</button>}
      </div>

      {flash && <div className="flash-error workbench-flash" role="alert"><span>{flash}</span><button className="link-btn" type="button" onClick={() => setFlash(null)}>dismiss</button></div>}
      {searchError && <div className="workbench-query-state is-error" role="alert"><strong>Couldn’t load this view.</strong><span>{searchError}</span><button className="btn" type="button" onClick={() => setReload((value) => value + 1)}>retry</button></div>}

      <div className="workbench-layout">
        <aside ref={indexRef} className="workbench-index" aria-label="work item index" aria-busy={loading}>
          <header><strong>issue index</strong></header>
          <div className="workbench-index-results">
            {!searchError && !loading && liveItems.length === 0 && <div className="workbench-empty"><strong>No matching items</strong><span>Try clearing a filter or capture something new.</span></div>}
            {grouped.map(([label, values]) => {
              const completed = label === "done / dropped";
              const count = completed ? completedMatchingCount(counts) || values.length : values.length;
              if (count === 0) return null;
              return <section className={`workbench-group ${completed ? "is-completed" : ""}`} key={label}>
                {completed ? <h2><button
                  type="button"
                  aria-expanded={!completedCollapsed}
                  aria-controls="workbench-completed-items"
                  aria-label={completedAutoRevealed
                    ? `Done and dropped items (${count}), revealed for the current view`
                    : `${completedCollapsed ? "Expand" : "Collapse"} done and dropped items (${count})`}
                  disabled={completedAutoRevealed}
                  onClick={() => {
                    const collapsed = !completedCollapsed;
                    setCompletedCollapsedPreference(collapsed);
                    writeCompletedCollapse(bucketId, collapsed, sessionStorage);
                  }}
                ><span aria-hidden="true">{completedCollapsed ? "▸" : "▾"}</span><span>{label}</span><b>{count}</b></button></h2>
                  : <h2><span>{label}</span><b>{count}</b></h2>}
                {(!completed || !completedCollapsed) && <div id={completed ? "workbench-completed-items" : undefined}>
                  {values.map((item) => <WorkbenchRow key={item.id.toString()} item={item} selected={item.id.toString() === selectedId} onSelect={() => setSelectedId(item.id.toString())} />)}
                </div>}
              </section>;
            })}
          </div>
          <footer className="workbench-index-status" aria-live="polite">
            <span>{boardIndexStatus({ shown: shownCount, collapsed: collapsedCompletedCount, counts, hasMore: canLoadMore, filtered: hasItemFilters(requestFilters), loading, error: searchError !== null })}</span>
            {canLoadMore && <button type="button" disabled={loadingMore} onClick={loadMore}>{loadingMore ? "loading…" : "load 50 more"}</button>}
          </footer>
        </aside>
        <main className="workbench-inspector">
          {selected ? <ItemInspector key={selected.id.toString()} item={selected} projects={projects} onPmLink={onPmLink} onSpawnFromItem={onSpawnFromItem} onError={reportError} onUndo={setUndo} onChanged={() => setReload((value) => value + 1)} /> : <div className="workbench-empty inspector-empty"><strong>Select an item</strong><span>Its context and workflow stay visible here.</span></div>}
        </main>
      </div>

      <p className="workbench-mobile-note">This Workbench is optimized for pointer, touch, and keyboard use on desktop. A dedicated phone triage surface is a deliberate follow-up.</p>
      {captureOpen && <CaptureItem bucketId={bucketId} projects={projects} onClose={() => setCaptureOpen(false)} onCreated={() => { setCaptureOpen(false); setReload((value) => value + 1); }} onError={reportError} />}
      {undo && <div className="workbench-toast" role="status"><span>{undo.label}</span><button type="button" onClick={() => { undo.run(); setUndo(null); }}>undo</button><button type="button" aria-label="dismiss" onClick={() => setUndo(null)}>×</button></div>}
    </section>
  );
}

function WorkbenchRow({ item, selected, onSelect }: { item: Item; selected: boolean; onSelect: () => void }) {
  const state = useAppState();
  const now = useNow();
  const blocker = resolveBlockerIndicator(item, state.items);
  const project = item.projectId === undefined ? undefined : state.projects.get(item.projectId.toString());
  const source = SOURCE_LABELS.get(item.sourceKind) ?? "other";
  const live = item.sessionIds.map((id) => state.sessions.get(id.toString())).find((session) => session && session.state !== 5 && session.state !== 6);
  return <button type="button" className={`workbench-row ${selected ? "is-selected" : ""}`} aria-current={selected} onClick={onSelect}>
    <i className={`prio-dot ${PRIORITY_CLASSES.get(item.priority) ?? "prio-normal"}`} />
    <span><strong>{item.title}</strong><small>{live ? <><i className="tiny-state working" />session {live.id.toString()} live</> : `${source}${project ? ` · ${project.name}` : ""}`} · {formatAgo(now - Number(item.updatedAtUnixMs))}</small></span>
    {item.question && <em className="attention-chip">reply</em>}
    {!item.question && blocker && <em title={blocker.title}>blocked</em>}
    {!item.question && !blocker && item.sessionIds.length > 0 && <em className="session-ref">s{item.sessionIds[0].toString()}</em>}
  </button>;
}

function ItemInspector({ item, projects, onPmLink, onSpawnFromItem, onError, onUndo, onChanged }: {
  item: Item; projects: ReturnType<typeof useProjects>; onPmLink: (link: PmLink) => void;
  onSpawnFromItem: (item: Item) => void; onError: (error: unknown) => void;
  onUndo: (undo: { label: string; run: () => void } | null) => void;
  onChanged: () => void;
}) {
  const client = useClient();
  const state = useAppState();
  const [draft, setDraft] = useState<Partial<Item>>({});
  const [saveState, setSaveState] = useState<SaveState>("idle");
  const [notes, setNotes] = useState<ItemNote[] | null>(null);
  const [editingBody, setEditingBody] = useState(false);
  const [question, setQuestion] = useState(item.question);
  const [notesReload, setNotesReload] = useState(0);
  const merged = create(ItemSchema, { ...item, ...draft });

  useEffect(() => {
    const controller = new AbortController();
    fetchItemNotes(item.bucketId.toString(), item.id.toString(), controller.signal).then(setNotes).catch(() => setNotes(null));
    return () => controller.abort();
  }, [item.id, item.updatedAtUnixMs, notesReload]);

  useEffect(() => setQuestion(item.question), [item.question]);

  const mutate = (write: Partial<Omit<UpsertItem, "$typeName">>, optimistic: Partial<Item>, label: string, undoWrite?: Partial<Omit<UpsertItem, "$typeName">>) => {
    setDraft((current) => ({ ...current, ...optimistic })); setSaveState("saving");
    client.upsertItem({ bucketId: item.bucketId, id: item.id, ...write }).then(() => {
      setDraft({}); setSaveState("saved"); onChanged();
      window.setTimeout(() => setSaveState("idle"), 1800);
      if (undoWrite) onUndo({ label, run: () => { setSaveState("saving"); client.upsertItem({ bucketId: item.bucketId, id: item.id, ...undoWrite }).then(() => { setDraft({}); setSaveState("saved"); onChanged(); }).catch(onError); } });
    }).catch((error: unknown) => { setDraft({}); setSaveState("error"); onError(error); });
  };
  const status = (value: ItemStatus, label = `Moved to ${STATUS_LABELS.get(value)}`) => mutate({ status: value }, { status: value }, label, { status: merged.status });
  const sessions = merged.sessionIds.map((id) => state.sessions.get(id.toString())).filter((session) => session !== undefined);
  const dueValue = merged.dueAtUnixMs === undefined ? "" : new Date(Number(merged.dueAtUnixMs)).toISOString().slice(0, 10);

  return <div className="inspector-grid">
    <article className="inspector-content">
      <header className="inspector-titlebar"><div className="source-glyph">{SOURCE_LABELS.get(merged.sourceKind)?.slice(0, 1) ?? "·"}</div><div><span>{SOURCE_LABELS.get(merged.sourceKind)}{merged.sourceDetail ? ` · ${merged.sourceDetail}` : ""}</span><input aria-label="item title" value={merged.title} onChange={(e) => setDraft((value) => ({ ...value, title: e.target.value }))} onBlur={() => { if (merged.title.trim() && merged.title !== item.title) mutate({ title: merged.title }, { title: merged.title }, "Title saved", { title: item.title }); }} /></div><div className="inspector-title-actions"><ItemReference bucketId={merged.bucketId.toString()} id={merged.id.toString()} onOpen={() => onPmLink({ kind: "item", bucketId: merged.bucketId.toString(), id: merged.id.toString() })} /><button className="btn btn-primary inspector-spawn" type="button" onClick={() => onSpawnFromItem(merged)} aria-label="spawn session"><svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 1.5a6.5 6.5 0 1 0 6.5 6.5A6.5 6.5 0 0 0 8 1.5Zm-1.4 3 4.5 3a.6.6 0 0 1 0 1l-4.5 3A.6.6 0 0 1 5.7 11V5a.6.6 0 0 1 .9-.5Z" /></svg><span>spawn</span></button></div></header>
      <div className="inspector-facts"><span className="mini-badge attention">{STATUS_LABELS.get(merged.status)}</span>{merged.projectId !== undefined && <span>project <b>{state.projects.get(merged.projectId.toString())?.name}</b></span>}<span>source <b>{SOURCE_LABELS.get(merged.sourceKind)}</b></span>{merged.url && classifyHref(merged.url).kind === "external" ? <a href={merged.url} target="_blank" rel="noopener noreferrer">open source ↗</a> : merged.url ? <span className="muted-line" title={merged.url}>source link is not a web URL</span> : null}</div>
      {merged.question && <ItemQuestionReply item={merged} onError={onError} />}
      <section className="inspector-section"><header><strong>description</strong><button className="link-btn" type="button" onClick={() => setEditingBody((value) => !value)}>{editingBody ? "cancel" : "edit"}</button></header>{editingBody ? <><textarea rows={10} value={merged.body} aria-invalid={itemBodyLengthError(merged.body) !== null} onChange={(e) => setDraft((value) => ({ ...value, body: e.target.value }))} /><ItemBodyLength value={merged.body} /><div className="section-actions"><button className="btn btn-primary" type="button" disabled={itemBodyLengthError(merged.body) !== null} onClick={() => { mutate({ body: merged.body }, { body: merged.body }, "Description saved", { body: item.body }); setEditingBody(false); }}>save description</button></div></> : merged.body ? <Markdown text={merged.body} onPmLink={onPmLink} /> : <p className="muted-line">No description yet.</p>}</section>
      <section className="inspector-section"><header><strong>question context</strong><span className="muted-line">empty means no open question</span></header><textarea rows={3} value={question} onChange={(e) => setQuestion(e.target.value)} placeholder="What decision or context is needed?" /><div className="section-actions"><button className="btn" type="button" disabled={question === item.question} onClick={() => mutate({ question }, { question }, question ? "Question saved" : "Question cleared", { question: item.question })}>save question</button></div></section>
      <AttachmentSection item={merged} onError={onError} onChanged={() => { setNotesReload((value) => value + 1); onChanged(); }} />
      <section className="inspector-section activity-section"><header><strong>activity</strong><span className="muted-line">append-only history</span></header>{notes === null ? <p className="muted-line">loading activity…</p> : notes.length === 0 ? <p className="muted-line">No activity yet.</p> : <ol>{[...notes].reverse().slice(0, 20).map((note) => <li key={note.id}><i>{note.session_id === null ? "Y" : "A"}</i><p>{note.text}<span>{new Date(note.ts_unix_ms).toLocaleString()} · {note.kind}</span></p></li>)}</ol>}</section>
    </article>
    <aside className="workflow-panel">
      <header><strong>workflow</strong><span className={`save-state is-${saveState}`} aria-live="polite">{saveState === "saving" ? "saving…" : saveState === "saved" ? "saved" : saveState === "error" ? "save failed" : "changes save here"}</span></header>
      <label className="workflow-label">status</label><div className="workflow-status">{[...STATUS_LABELS.entries()].map(([value, label]) => <button type="button" className={merged.status === value ? "active" : ""} key={value} onClick={() => status(value)}><i />{label}</button>)}</div>
      <label className="workflow-label">priority</label><div className="workflow-priority">{[...PRIORITY_LABELS.entries()].map(([value, label]) => <button type="button" className={merged.priority === value ? "active" : ""} key={value} onClick={() => mutate({ priority: value }, { priority: value }, `Priority set to ${label}`, { priority: merged.priority })}><i />{label}</button>)}</div>
      <label className="workflow-label" htmlFor={`project-${merged.id}`}>project</label><select id={`project-${merged.id}`} value={merged.projectId?.toString() ?? ""} onChange={(e) => { const id = e.target.value; mutate(id ? { projectId: BigInt(id) } : { clearProject: true }, { projectId: id ? BigInt(id) : undefined }, "Project updated", merged.projectId === undefined ? { clearProject: true } : { projectId: merged.projectId }); }}><option value="">unassigned</option>{projects.map((project) => <option key={project.id.toString()} value={project.id.toString()}>{project.name}</option>)}</select>
      <label className="workflow-label" htmlFor={`due-${merged.id}`}>due</label><input id={`due-${merged.id}`} type="date" value={dueValue} onChange={(e) => { const value = e.target.value; mutate(value ? { dueAtUnixMs: BigInt(new Date(`${value}T00:00:00`).getTime()) } : { clearDue: true }, { dueAtUnixMs: value ? BigInt(new Date(`${value}T00:00:00`).getTime()) : undefined }, value ? "Due date saved" : "Due date cleared", merged.dueAtUnixMs === undefined ? { clearDue: true } : { dueAtUnixMs: merged.dueAtUnixMs }); }} />
      {sessions.length > 0 && <><label className="workflow-label">sessions</label>{sessions.map((session) => <button className="execution-card" type="button" key={session.id.toString()} onClick={() => onPmLink({ kind: "session", id: session.id.toString() })}><StateBadge state={session.state} dot /><span><b>session {session.id.toString()}</b><small>{sessionDisplayName(session)}</small></span><em>open ↗</em></button>)}</>}
      <label className="workflow-label">park or finish</label><div className="finish-actions">{merged.snoozedUntilUnixMs !== undefined ? <button className="btn" type="button" onClick={() => client.snoozeItem(merged.bucketId, merged.id).then(onChanged).catch(onError)}>unsnooze</button> : <button className="btn" type="button" onClick={() => client.snoozeItem(merged.bucketId, merged.id, BigInt(Date.now() + 24 * 60 * 60 * 1000)).then(() => { onChanged(); onUndo({ label: "Snoozed until tomorrow", run: () => { client.snoozeItem(merged.bucketId, merged.id).then(onChanged).catch(onError); } }); }).catch(onError)}>snooze 1d</button>}<button className="btn done-btn" type="button" onClick={() => status(ItemStatus.DONE, "Marked done")}>✓ done</button></div>
      <button className="drop-action" type="button" onClick={() => status(ItemStatus.DROPPED, "Item dropped")}>drop item</button>
      <button className="drop-action delete" type="button" onClick={() => { if (window.confirm(`Delete “${merged.title}” permanently?`)) client.deleteItem(merged.bucketId, merged.id).then(onChanged).catch(onError); }}>delete permanently</button>
    </aside>
  </div>;
}

function formatBytes(value: string): string {
  const bytes = Number(value);
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
}

function AttachmentSection({ item, onError, onChanged }: {
  item: Item; onError: (error: unknown) => void; onChanged: () => void;
}) {
  const state = useAppState();
  const [attachments, setAttachments] = useState<ItemAttachment[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<{ filename: string; percent: number } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);
  const load = () => fetchItemAttachments(item.bucketId.toString(), item.id.toString()).then(setAttachments).catch((reason: unknown) => {
    const message = reason instanceof Error ? reason.message : String(reason); setError(message); onError(reason);
  });
  useEffect(() => { let active = true; fetchItemAttachments(item.bucketId.toString(), item.id.toString()).then((value) => { if (active) setAttachments(value); }).catch((reason: unknown) => { if (active) setError(reason instanceof Error ? reason.message : String(reason)); }); return () => { active = false; }; }, [item.bucketId, item.id]);
  const upload = async (files: FileList | null) => {
    if (!files?.length) return;
    setBusy(true); setError(null);
    try {
      for (const file of Array.from(files)) {
        setProgress({ filename: file.name, percent: 0 });
        await uploadItemAttachment(item.bucketId.toString(), item.id.toString(), file, (percent) => setProgress({ filename: file.name, percent }));
      }
      await load(); onChanged();
    } catch (reason) {
      const message = reason instanceof Error ? reason.message : String(reason); setError(message); onError(reason);
    } finally {
      setBusy(false); setProgress(null); if (input.current) input.current.value = "";
    }
  };
  const remove = async (attachment: ItemAttachment) => {
    if (!window.confirm(`Remove attachment “${attachment.filename}”?`)) return;
    setBusy(true); setError(null);
    try { await deleteItemAttachment(item.bucketId.toString(), item.id.toString(), attachment.id); await load(); onChanged(); }
    catch (reason) { const message = reason instanceof Error ? reason.message : String(reason); setError(message); onError(reason); }
    finally { setBusy(false); }
  };
  return <section className="inspector-section attachment-section" aria-busy={busy}>
    <header><strong>attachments</strong><span className="muted-line">10 MiB each · 50 MiB total</span></header>
    <label className={`attachment-picker btn ${busy ? "is-disabled" : ""}`}>attach files<input ref={input} type="file" multiple disabled={busy} onChange={(event) => void upload(event.target.files)} /></label>
    {progress && <div className="attachment-progress" role="status" aria-live="polite"><span>Uploading {progress.filename}</span><progress max={100} value={progress.percent}>{progress.percent}%</progress><b>{progress.percent}%</b></div>}
    {error && <p className="field-error" role="alert">{error}</p>}
    {attachments === null ? <p className="muted-line">loading attachments…</p> : attachments.length === 0 ? <p className="muted-line">No attachments yet.</p> : <ul className="attachment-list">{attachments.map((attachment) => {
      const uploaderSession = attachment.createdBySessionId ? state.sessions.get(attachment.createdBySessionId) : undefined;
      const uploader = uploaderSession ? sessionDisplayName(uploaderSession) : attachment.createdBySessionId ? `session ${attachment.createdBySessionId}` : "you";
      return <li key={attachment.id}><div><a href={attachmentDownloadUrl(item.bucketId.toString(), item.id.toString(), attachment.id)} download>{attachment.filename}</a><span>{attachment.mediaType} · {formatBytes(attachment.byteLength)} · {uploader} · {new Date(Number(attachment.createdAtUnixMs)).toLocaleString()}</span></div><button className="link-btn danger" type="button" disabled={busy} onClick={() => void remove(attachment)} aria-label={`remove ${attachment.filename}`}>remove</button></li>;
    })}</ul>}
  </section>;
}

function useProjects() { return [...useAppState().projects.values()]; }

function ItemQuestionReply({ item, onError }: { item: Item; onError: (error: unknown) => void }) {
  const client = useClient(); const state = useAppState(); const now = useNow();
  const [text, setText] = useState(""); const [menuOpen, setMenuOpen] = useState(false); const [busy, setBusy] = useState(false);
  const sessions = [...state.sessions.values()]; const projects = [...state.projects.values()];
  const primary = resolvePrimaryResponseTarget(item, sessions, projects); const options = buildItemResponseOptions(item, sessions, projects, now);
  const send = (target: ItemResponseTarget | null) => { if (!target || !text.trim()) return; setBusy(true); setMenuOpen(false); client.respondToItem(item.bucketId, item.id, text, target).then(() => setText("")).catch(onError).finally(() => setBusy(false)); };
  const option = (entry: ItemResponseOption) => <button key={`${entry.target.kind}:${entry.label}`} type="button" role="menuitem" className="popover-item" disabled={!text.trim() || busy} onClick={() => send(entry.target)}>{entry.label}</button>;
  return <section className="inspector-question"><header><span>question</span><b>needs your response</b></header><p>{item.question}</p><textarea rows={3} value={text} onChange={(e) => setText(e.target.value)} placeholder="Type a decision or context…" /><footer><span>{primary?.kind === "session" ? `routes to session ${primary.sessionId}` : "choose a route"}</span><button className="btn btn-loud" type="button" disabled={!text.trim() || busy || !primary} onClick={() => send(primary)}>{busy ? "sending…" : "reply"}</button><Popover open={menuOpen} onToggle={() => setMenuOpen((value) => !value)} onClose={() => setMenuOpen(false)} triggerLabel="⌄" triggerTitle="reply route" triggerClassName="btn">{options.existingSupervisors.map(option)}{options.newSupervisors.map(option)}{option(options.replyOnly)}</Popover></footer></section>;
}

function CaptureItem({ bucketId, projects, onClose, onCreated, onError }: { bucketId: string; projects: ReturnType<typeof useProjects>; onClose: () => void; onCreated: () => void; onError: (error: unknown) => void }) {
  const client = useClient(); const [busy, setBusy] = useState(false);
  const [body, setBody] = useState("");
  const bodyError = itemBodyLengthError(body);
  const submit = (event: React.FormEvent<HTMLFormElement>) => { event.preventDefault(); const data = new FormData(event.currentTarget); const title = String(data.get("title") ?? "").trim(); if (!title || bodyError) return; setBusy(true); client.upsertItem({ bucketId: BigInt(bucketId), title, body, question: String(data.get("question") ?? ""), status: Number(data.get("status")) as ItemStatus, priority: Number(data.get("priority")) as ItemPriority, sourceKind: ItemSourceKind.HUMAN, projectId: data.get("project") ? BigInt(String(data.get("project"))) : undefined }).then(onCreated).catch(onError).finally(() => setBusy(false)); };
  return <div className="capture-layer is-open" role="dialog" aria-modal="true" aria-labelledby="capture-title"><button className="capture-scrim" type="button" onClick={onClose} aria-label="close new item" /><form className="capture-panel" onSubmit={submit}><header><div><span>new item</span><strong id="capture-title">capture work</strong></div><button type="button" onClick={onClose} aria-label="close">×</button></header><label className="field"><span>title</span><input name="title" required autoFocus maxLength={200} /></label><div className="capture-grid"><label className="field"><span>status</span><select name="status" defaultValue={ItemStatus.PLANNED}>{[...STATUS_LABELS.entries()].map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select></label><label className="field"><span>priority</span><select name="priority" defaultValue={ItemPriority.NORMAL}>{[...PRIORITY_LABELS.entries()].map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select></label></div><label className="field"><span>project</span><select name="project"><option value="">unassigned</option>{projects.map((project) => <option key={project.id.toString()} value={project.id.toString()}>{project.name}</option>)}</select></label><label className="field"><span>description</span><textarea name="body" rows={6} value={body} aria-invalid={bodyError !== null} onChange={(event) => setBody(event.target.value)} /><ItemBodyLength value={body} /></label><label className="field"><span>open question <small>optional · puts item in needs you</small></span><textarea name="question" rows={3} maxLength={1000} /></label><footer><button className="btn" type="button" onClick={onClose}>cancel</button><button className="btn btn-primary" type="submit" disabled={busy || bodyError !== null}>{busy ? "creating…" : "create item"}</button></footer></form></div>;
}
