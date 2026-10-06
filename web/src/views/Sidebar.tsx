import { Fragment, useEffect, useId, useRef, useState, type ReactNode } from "react";
import { ContextChip } from "../components/ContextChip";
import { LinkedItemSummary } from "../components/LinkedItemSummary";
import { Popover } from "../components/Popover";
import { toggleMenu, type OpenMenu } from "../components/menuState";
import { StateBadge } from "../components/StateBadge";
import { WorkerChip } from "../components/WorkerChip";
import {
  LOCAL_WORKER_ID,
  sessionDisplayName,
  formatAgo,
  sessionEnded,
  sessionLastActivity,
  sessionLastActivityTitle,
  sessionStatusLine,
  stateStyle,
  unescapeHtml,
} from "@puppet-master/client-core/format";
import {
  PermissionMode,
  SessionRole,
  SessionState,
  type Bucket,
  type ContextField,
  type Session,
  type SessionGit,
} from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import type { PmLink } from "@puppet-master/client-core/pmlink";
import { useAppState, useClient, useNow } from "../state/hooks";
import {
  permissionModeLabel,
} from "@puppet-master/client-core/state/permission";
import { notifyUnsupportedLabel, notifyUnsupportedMessage, type NotifySupport } from "@puppet-master/client-core/state/notify";
import { needsYouCount } from "@puppet-master/client-core/state/board";
import { displaySessionState } from "@puppet-master/client-core/state/displayState";
import {
  buildSidebar,
  type SidebarBucket,
  type SidebarModel,
} from "@puppet-master/client-core/state/sidebar";
import { linkedItemSummary } from "@puppet-master/client-core/state/linkedItems";
import { AGENT_TERMINAL_DRAG_TYPE, agentTerminalForSession, SESSION_DRAG_TYPE } from "@puppet-master/client-core/state/workspace";
import {
  COLLAPSED_BUCKETS_KEY,
  COLLAPSED_SUPERVISORS_KEY,
  SUPERVISOR_FILTERS_KEY,
  readStringMap,
  readStringSet,
  writeStringMap,
  writeStringSet,
} from "../storage";
import { overNativeVerticalScrollbar, wireTransientScrollbar } from "../transientScrollbar";
import { SessionInfoDialog } from "./SessionInfoDialog";
import { SpawnPopover } from "./SpawnPopover";

export interface NotifyControls {
  support: NotifySupport;
  permission: NotificationPermission;
  enabled: boolean;
  sound: boolean;
  onEnable: () => void;
  onToggleSound: () => void;
}

/**
 * Keeps the compact row's name and live status explicit to assistive
 * technology. `display` is the derived render state, so the spoken label and
 * the dot never disagree.
 */
export function sessionAccessibleLabel(
  session: Session,
  display: SessionState = session.state,
): string {
  const attention = display === SessionState.NEEDS_INPUT
    ? session.needsInputUnseen ? ", unseen attention" : ", viewed but unanswered"
    : display === SessionState.IDLE && session.idleUnseen
      ? ", freshly finished"
      : "";
  return `${sessionDisplayName(session)}, ${stateStyle(display).label}${attention}`;
}

/** The row container's state classes, shared by top-level and child rows. */
export function sessionRowClass(
  session: Session,
  options: { child?: boolean; selected?: boolean; display?: SessionState } = {},
): string {
  const display = options.display ?? session.state;
  const needsInput = display === SessionState.NEEDS_INPUT;
  return [
    "sb-session-row",
    options.child ? "sb-child" : "",
    options.selected ? "is-selected" : "",
    needsInput
      ? `is-needs-input ${session.needsInputUnseen ? "is-attention-unseen" : "is-attention-seen"}`
      : "",
    display === SessionState.IDLE && session.idleUnseen ? "is-idle-fresh" : "",
  ].filter(Boolean).join(" ");
}

export interface NeedsInputAttention { unseen: number; seen: number }

export function needsInputAttention(sessions: Iterable<Session>): NeedsInputAttention {
  const attention = { unseen: 0, seen: 0 };
  for (const session of sessions) {
    if (session.state !== SessionState.NEEDS_INPUT) continue;
    if (session.needsInputUnseen) attention.unseen += 1;
    else attention.seen += 1;
  }
  return attention;
}

export function DescendantAttention({ attention }: { attention: NeedsInputAttention }) {
  const total = attention.unseen + attention.seen;
  if (total === 0) return null;
  const detail = attention.unseen > 0
    ? `${total} descendant session${total === 1 ? "" : "s"} need input, ${attention.unseen} unseen`
    : `${total} viewed descendant session${total === 1 ? "" : "s"} still need input`;
  return (
    <span
      className={`sb-descendant-attention ${attention.unseen > 0 ? "is-unseen" : "is-seen"}`}
      role="img"
      aria-label={detail}
      title={detail}
    >
      {total}
    </span>
  );
}

/** The states a supervisor's rollup meters, summarizes and filters by. */
export type RollupState = "working" | "needs-input" | "idle" | "failed" | "ended";

export type RollupCounts = Record<RollupState, number>;

const ROLLUP_STATES: readonly RollupState[] = [
  "working",
  "needs-input",
  "idle",
  "failed",
  "ended",
];

const ROLLUP_LABEL: Record<RollupState, string> = {
  working: "running",
  "needs-input": "blocked",
  idle: "ready",
  failed: "failed",
  ended: "ended",
};

/** The whole-fleet tallies, most demanding of attention first. */
export const TALLY_STATES: readonly RollupState[] = ["needs-input", "failed", "working", "idle"];

export function rollupState(state: SessionState): RollupState {
  switch (state) {
    case SessionState.NEEDS_INPUT:
      return "needs-input";
    case SessionState.IDLE:
      return "idle";
    case SessionState.FAILED:
      return "failed";
    case SessionState.EXITED:
      return "ended";
    default:
      return "working";
  }
}

export function rollupCounts(sessions: Iterable<Session>): RollupCounts {
  const counts: RollupCounts = { working: 0, "needs-input": 0, idle: 0, failed: 0, ended: 0 };
  for (const session of sessions) counts[rollupState(session.state)] += 1;
  return counts;
}

function RollupTally({
  state,
  count,
  text,
  active,
  fresh,
  title,
  onPick,
}: {
  state: RollupState;
  count: number;
  text: string;
  active: boolean;
  fresh: boolean;
  title: string;
  onPick: () => void;
}) {
  return (
    <button
      type="button"
      className={`sb-rollup-tally c-${state} ${fresh ? "is-fresh" : ""} ${active ? "is-active" : ""}`}
      aria-pressed={active}
      title={title}
      onClick={onPick}
    >
      <b>{count}</b> {text}
    </button>
  );
}

/** The supervisor rollup vocabulary, counted over every session the sidebar can list. */
export function StatusTallies({
  sessions,
  active,
  onPick,
}: {
  sessions: readonly Session[];
  active: RollupState | null;
  onPick: (state: RollupState) => void;
}) {
  const counts = rollupCounts(sessions);
  const present = TALLY_STATES.filter((state) => counts[state] > 0);
  if (present.length === 0) return null;
  const anyIdleFresh = sessions.some(
    (session) => session.state === SessionState.IDLE && session.idleUnseen,
  );

  return (
    <span className="sb-rollup-summary topbar-tallies" role="group" aria-label="session states">
      {present.map((state, index) => (
        <Fragment key={state}>
          {index > 0 && <span className="sb-rollup-sep">·</span>}
          <RollupTally
            state={state}
            count={counts[state]}
            text={ROLLUP_LABEL[state]}
            active={active === state}
            fresh={state === "idle" && anyIdleFresh}
            title={active === state
              ? "show every session again"
              : `show only the ${ROLLUP_LABEL[state]} sessions`}
            onPick={() => onPick(state)}
          />
        </Fragment>
      ))}
    </span>
  );
}

export interface SupervisorGroup {
  supervisor: Session;
  workers: Session[];
}

/** One row of the bucket: a supervisor with its workers, or a session on its own. */
export type SupervisorEntry =
  | { kind: "group"; group: SupervisorGroup }
  | { kind: "session"; session: Session };

export interface SupervisorTree {
  supervisors: SupervisorGroup[];
  /** Sessions no rendered supervisor claims, listed directly under the bucket. */
  loose: Session[];
  /** Groups and loose sessions in one order, which is what the bucket renders. */
  entries: SupervisorEntry[];
}

/**
 * Hangs every spawned session off the supervisor that spawned it, leaving the
 * rest at bucket level.
 *
 * Groups and loose sessions are ordered together. Rendering every supervisor
 * before every loose session made the order say more about who spawned what
 * than about what just happened: a session working now sat below a group
 * nobody had touched for an hour. A group takes the position of its most
 * salient member, supervisor or worker, so a quiet supervisor whose worker is
 * busy rises with it.
 */
export function supervisorTree(
  bucket: SidebarBucket,
  order: (a: Session, b: Session) => number,
): SupervisorTree {
  const spawned = new Map<string, Session[]>();
  for (const session of bucket.sessions) {
    const parent = session.spawnedBySessionId?.toString();
    if (parent === undefined) continue;
    const siblings = spawned.get(parent) ?? [];
    siblings.push(session);
    spawned.set(parent, siblings);
  }

  const nested = new Set<string>();
  const supervisors = bucket.supervisors.map((supervisor) => {
    const workers = [...(spawned.get(supervisor.id.toString()) ?? [])].sort(order);
    for (const worker of workers) nested.add(worker.id.toString());
    return { supervisor, workers };
  });
  const loose = bucket.sessions.filter((session) => !nested.has(session.id.toString()));

  const standsFor = (entry: SupervisorEntry): Session =>
    entry.kind === "session"
      ? entry.session
      : [entry.group.supervisor, ...entry.group.workers].sort(order)[0];

  const entries: SupervisorEntry[] = [
    ...supervisors.map((group) => ({ kind: "group", group }) as SupervisorEntry),
    ...loose.map((session) => ({ kind: "session", session }) as SupervisorEntry),
  ].sort((a, b) => order(standsFor(a), standsFor(b)));

  return { supervisors, loose, entries };
}

/**
 * Narrows the whole sidebar to one state. A supervisor is kept when it matches
 * or still owns a matching worker, so a match never loses the row it hangs off.
 */
export function filterSidebarByState(model: SidebarModel, filter: RollupState): SidebarModel {
  const matches = (session: Session) => rollupState(session.state) === filter;
  const buckets = model.buckets
    .map(({ bucket, supervisors, sessions }) => {
      const kept = sessions.filter(matches);
      const parents = new Set(kept.map((session) => session.spawnedBySessionId?.toString()));
      return {
        bucket,
        sessions: kept,
        supervisors: supervisors.filter(
          (supervisor) => matches(supervisor) || parents.has(supervisor.id.toString()),
        ),
      };
    })
    .filter(({ supervisors, sessions }) => supervisors.length > 0 || sessions.length > 0);
  return { buckets, orphans: model.orphans.filter(matches), order: model.order };
}

/** A tally opens the supervisor filtered to that state; clicking the active one returns to the full list. */
export function nextSupervisorFilter(
  filters: ReadonlyMap<string, RollupState>,
  supervisorId: string,
  picked: RollupState,
): Map<string, RollupState> {
  const next = new Map(filters);
  if (next.get(supervisorId) === picked) next.delete(supervisorId);
  else next.set(supervisorId, picked);
  return next;
}

/** Drops stored values that are no longer states, so a stale one cannot pin a supervisor to an empty list. */
export function readSupervisorFilters(): Map<string, RollupState> {
  const filters = new Map<string, RollupState>();
  for (const [id, value] of readStringMap(SUPERVISOR_FILTERS_KEY)) {
    if ((ROLLUP_STATES as readonly string[]).includes(value)) filters.set(id, value as RollupState);
  }
  return filters;
}

export function SupervisorNode({
  row,
  workers,
  label,
  collapsed,
  filter,
  emptyNote,
  onToggleCollapse,
  onPickState,
  onShowAll,
  renderWorker,
}: {
  row: ReactNode;
  workers: Session[];
  label: string;
  collapsed: boolean;
  filter: RollupState | null;
  /** Why the list is empty, when a state filter is the reason. A supervisor
   * that simply has no workers yet says nothing. */
  emptyNote?: string;
  onToggleCollapse: () => void;
  onPickState: (state: RollupState) => void;
  onShowAll: () => void;
  renderWorker: (worker: Session) => ReactNode;
}) {
  if (workers.length === 0) {
    return (
      <div className="sb-supervisor">
        {row}
        {emptyNote ? <p className="sb-supervisor-empty">{emptyNote}</p> : null}
      </div>
    );
  }

  const total = workers.length;
  const counts = rollupCounts(workers);
  // A filter is dropped once its last worker moves on, rather than left showing nothing.
  const active = filter && counts[filter] > 0 ? filter : null;
  const present = ROLLUP_STATES.filter((state) => counts[state] > 0);
  const shown = active ? workers.filter((worker) => rollupState(worker.state) === active) : workers;
  const plural = `worker${total === 1 ? "" : "s"}`;
  const anyIdleFresh = workers.some(
    (worker) => worker.state === SessionState.IDLE && worker.idleUnseen,
  );

  const tally = (state: RollupState, text: string) => (
    <RollupTally
      key={state}
      state={state}
      count={counts[state]}
      text={text}
      active={active === state}
      fresh={state === "idle" && anyIdleFresh}
      title={active === state
        ? "show all workers again"
        : `show only the ${ROLLUP_LABEL[state]} workers`}
      onPick={() => onPickState(state)}
    />
  );

  const lead: RollupState | null = counts["needs-input"] > 0
    ? "needs-input"
    : counts.failed > 0
      ? "failed"
      : counts.working > 0
        ? "working"
        : null;

  return (
    <div className={`sb-supervisor ${collapsed ? "" : "is-open"}`}>
      {row}
      <div className="sb-rollup">
        <span
          className="sb-rollup-meter"
          role="img"
          aria-label={present.map((state) => `${counts[state]} ${ROLLUP_LABEL[state]}`).join(", ")}
        >
          {present.map((state) => (
            <i key={state} className={`m-${state}`} style={{ flex: counts[state] }} />
          ))}
        </span>
        <span className="sb-rollup-summary">
          <span className="sb-rollup-summary-brief">
            {lead ? (
              <>
                {tally(lead, lead === "needs-input" ? "need you" : ROLLUP_LABEL[lead])}
                <span className="sb-rollup-sep">·</span>
                <span className="sb-rollup-quiet">{total} {plural}</span>
              </>
            ) : (
              <span className="sb-rollup-quiet">{total} {plural}, all quiet</span>
            )}
          </span>
          <span className="sb-rollup-summary-full">
            {present.map((state, index) => (
              <Fragment key={state}>
                {index > 0 && <span className="sb-rollup-sep">·</span>}
                {tally(state, ROLLUP_LABEL[state])}
              </Fragment>
            ))}
          </span>
        </span>
        <button
          type="button"
          className="sb-rollup-toggle"
          aria-expanded={!collapsed}
          aria-label={`${collapsed ? "show" : "hide"} the ${total} ${plural} under ${label}`}
          onClick={onToggleCollapse}
        >
          <span aria-hidden="true">{collapsed ? "▸" : "▾"}</span>
        </button>
      </div>
      {!collapsed && (
        <div className="sb-children">
          {shown.map(renderWorker)}
          {active && (
            <button type="button" className="sb-children-showall" onClick={onShowAll}>
              showing {shown.length} of {total} · show all
            </button>
          )}
        </div>
      )}
    </div>
  );
}

/** A blank key or value renders as an empty chip, so such fields never
    reach the row; a daemon predating ingestion-side dropping may still
    deliver them. */
export function visibleGlanceFields(
  fields: readonly ContextField[],
): readonly ContextField[] {
  return fields.filter(
    (field) => field.key.trim() !== "" && field.value.trim() !== "",
  );
}

export function SessionGlance({
  fields: allFields,
  sessionId,
  onPmLink,
}: {
  fields: readonly ContextField[];
  sessionId: string;
  onPmLink: (link: PmLink, sourceSessionId: string) => void;
}) {
  const fields = visibleGlanceFields(allFields);
  if (fields.length === 0) return null;
  return (
    <span className="sb-session-chips">
      {fields.map((field) => (
        <ContextChip
          key={field.key}
          field={field}
          onPmLink={(link) => onPmLink(link, sessionId)}
        />
      ))}
    </span>
  );
}

/** A row's text: the goal as its name, the headline under it, then any state detail. */
export function SessionRowText({ session }: { session: Session }) {
  const status = sessionStatusLine(session);
  return (
    <>
      <span className="sb-session-title">{sessionDisplayName(session)}</span>
      {status && <span className="sb-session-status">{status}</span>}
      {session.stateDetail && <span className="sb-session-detail">{unescapeHtml(session.stateDetail)}</span>}
    </>
  );
}

/**
 * The branch a session reports itself on, as a compact row chip. A
 * worktree that is not the main checkout is named in the tooltip, since
 * the branch alone does not say which tree it is checked out in.
 */
export function SessionBranch({ git }: { git?: SessionGit }) {
  if (!git?.branch) return null;
  const linked = git.worktree && git.repoRoot && git.worktree !== git.repoRoot;
  const title = linked
    ? `${git.branch} in worktree ${git.worktree}`
    : `Branch ${git.branch}`;
  return (
    <span className="sb-session-branch" title={title}>
      <svg viewBox="0 0 12 12" aria-hidden="true">
        <circle cx="3.2" cy="2.6" r="1.4" />
        <circle cx="3.2" cy="9.4" r="1.4" />
        <circle cx="8.8" cy="4.4" r="1.4" />
        <path d="M3.2 4v4M4.6 3.2h1.6a2.6 2.6 0 012.6 2.6v0" />
      </svg>
      <span className="sb-session-branch-name">{git.branch}</span>
      {linked && <span className="sb-session-branch-wt" aria-hidden="true">⌥</span>}
    </span>
  );
}

export function SessionRoleIcon({ role }: { role: SessionRole }) {
  const [open, setOpen] = useState(false);
  const containerRef = useRef<HTMLSpanElement>(null);
  const pointerFocus = useRef(false);
  const lastPointerType = useRef("");
  const tooltipId = useId();
  const supervisor = role === SessionRole.SUPERVISOR;
  const label = supervisor
    ? "Supervisor role — coordinates bucket work and may dispatch child sessions"
    : "Worker role — executes a scoped task in a project";

  useEffect(() => {
    if (!open) return;
    const dismiss = (event: Event) => {
      if (
        (event.type === "pointerdown" || event.type === "focusin") &&
        containerRef.current?.contains(event.target as Node)
      ) {
        return;
      }
      setOpen(false);
    };
    document.addEventListener("pointerdown", dismiss, true);
    document.addEventListener("focusin", dismiss, true);
    document.addEventListener("scroll", dismiss, true);
    window.addEventListener("hashchange", dismiss);
    window.addEventListener("popstate", dismiss);
    return () => {
      document.removeEventListener("pointerdown", dismiss, true);
      document.removeEventListener("focusin", dismiss, true);
      document.removeEventListener("scroll", dismiss, true);
      window.removeEventListener("hashchange", dismiss);
      window.removeEventListener("popstate", dismiss);
    };
  }, [open]);

  return (
    <span
      ref={containerRef}
      className={`sb-session-role ${supervisor ? "is-supervisor" : "is-worker"}${open ? " is-open" : ""}`}
    >
      <span
        className="sb-session-role-trigger"
        role="img"
        tabIndex={0}
        aria-label={label}
        aria-describedby={open ? tooltipId : undefined}
        onPointerEnter={(event) => {
          if (event.pointerType !== "touch") setOpen(true);
        }}
        onPointerLeave={(event) => {
          if (event.pointerType !== "touch") setOpen(false);
        }}
        onPointerDown={(event) => {
          pointerFocus.current = true;
          lastPointerType.current = event.pointerType;
          event.stopPropagation();
        }}
        onPointerUp={() => {
          pointerFocus.current = false;
        }}
        onPointerCancel={() => {
          pointerFocus.current = false;
          setOpen(false);
        }}
        onClick={(event) => {
          event.stopPropagation();
          if (lastPointerType.current === "touch") setOpen((value) => !value);
        }}
        onFocus={() => {
          if (!pointerFocus.current) setOpen(true);
        }}
        onBlur={() => {
          pointerFocus.current = false;
          setOpen(false);
        }}
        onKeyDown={(event) => {
          if (event.key !== "Escape") return;
          event.preventDefault();
          event.stopPropagation();
          setOpen(false);
          event.currentTarget.blur();
        }}
      >
        {supervisor ? (
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path d="M8 3v3m-4 6V9h8v3M4 12v1m8-1v1" />
            <circle cx="8" cy="2.5" r="1.4" />
            <circle cx="4" cy="13" r="1.4" />
            <circle cx="12" cy="13" r="1.4" />
          </svg>
        ) : (
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <circle cx="8" cy="5" r="2.4" />
            <path d="M3.8 13c.5-2.4 2-3.6 4.2-3.6s3.7 1.2 4.2 3.6" />
          </svg>
        )}
      </span>
      <span className="sb-session-role-tooltip" id={tooltipId} role="tooltip">
        {label}
      </span>
    </span>
  );
}

export function Sidebar({
  selectedId,
  onSelect,
  onAddToWorkspace,
  onNewProject,
  onNewSession,
  onOpenBucketSettings,
  onOpenBoard,
  onPmLink,
  activeBoardBucketId,
  showEnded,
  onToggleEnded,
  stateFilter,
  onClearStateFilter,
  notify,
}: {
  selectedId: string | null;
  onSelect: (id: string) => void;
  onAddToWorkspace: (id: string) => boolean;
  onNewProject: (bucketId: string) => void;
  onNewSession: (bucketId: string) => void;
  onOpenBucketSettings: (bucketId: string) => void;
  onOpenBoard: (bucketId: string) => void;
  onPmLink: (link: PmLink, sourceSessionId: string) => void;
  activeBoardBucketId?: string;
  showEnded: boolean;
  onToggleEnded: () => void;
  /** The top bar's whole-fleet tally filter, already reduced to a state that still has sessions. */
  stateFilter: RollupState | null;
  onClearStateFilter: () => void;
  notify: NotifyControls;
}) {
  const state = useAppState();
  const client = useClient();
  const now = useNow();
  const [collapsedBuckets, setCollapsedBuckets] = useState(() =>
    readStringSet(COLLAPSED_BUCKETS_KEY),
  );
  const [collapsedSupervisors, setCollapsedSupervisors] = useState(() =>
    readStringSet(COLLAPSED_SUPERVISORS_KEY),
  );
  const [supervisorFilters, setSupervisorFilters] = useState(readSupervisorFilters);
  const [openMenu, setOpenMenu] = useState<OpenMenu>(null);
  const [sessionInfo, setSessionInfo] = useState<{
    session: Session;
    returnFocus: HTMLElement | null;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [searchIds, setSearchIds] = useState<Set<string> | null>(null);
  const [historyCursor, setHistoryCursor] = useState("");
  const [searchCursor, setSearchCursor] = useState("");
  const [historyTotal, setHistoryTotal] = useState<number | null>(null);
  const [searchTotal, setSearchTotal] = useState<number | null>(null);
  const [loading, setLoading] = useState(false);
  const [offlineHostsOpen, setOfflineHostsOpen] = useState<boolean | null>(null);
  const historyRequestVersion = useRef(0);
  const searchRequestVersion = useRef(0);
  const scrollRef = useRef<HTMLDivElement>(null);
  const offlineHostsId = useId();

  useEffect(() => {
    const host = scrollRef.current;
    if (!host) return;
    return wireTransientScrollbar(host, {
      activeClass: "is-sb-scrollbar-active",
      onScrollbar: (event) => overNativeVerticalScrollbar(host, event),
      revealOnHover: true,
      revealOn: ["scroll", "wheel", "touchmove"],
    });
  }, []);

  useEffect(() => {
    if (!state.hydrated || state.conn !== "online") return;
    if (!showEnded) {
      ++historyRequestVersion.current;
      client.ownSessionPage("history", [], true);
      setHistoryCursor("");
      setHistoryTotal(null);
      return;
    }
    if (search.trim()) return;
    const version = ++historyRequestVersion.current;
    setLoading(true);
    client.listEndedSessions().then((page) => {
      if (historyRequestVersion.current !== version) return;
      client.ownSessionPage("history", page.sessions, true);
      setHistoryCursor(page.nextCursor);
      setHistoryTotal(Number(page.total));
    }).catch((cause: unknown) => {
      if (historyRequestVersion.current === version) setError(cause instanceof Error ? cause.message : String(cause));
    }).finally(() => {
      if (historyRequestVersion.current === version) setLoading(false);
    });
  }, [client, search, showEnded, state.conn, state.hydrated]);

  useEffect(() => {
    if (!state.hydrated || state.conn !== "online") return;
    const query = search.trim();
    if (!query) {
      ++searchRequestVersion.current;
      client.ownSessionPage("search", [], true);
      setSearchIds(null);
      setSearchCursor("");
      setSearchTotal(null);
      setLoading(false);
      return;
    }
    const version = ++searchRequestVersion.current;
    setLoading(true);
    const timer = window.setTimeout(() => {
      client.searchSessions(query).then((page) => {
        if (searchRequestVersion.current !== version) return;
        client.ownSessionPage("search", page.sessions, true);
        setSearchIds(new Set(page.sessions.map((session) => session.id.toString())));
        setSearchCursor(page.nextCursor);
        setSearchTotal(Number(page.total));
      }).catch((cause: unknown) => {
        if (searchRequestVersion.current === version) setError(cause instanceof Error ? cause.message : String(cause));
      }).finally(() => {
        if (searchRequestVersion.current === version) setLoading(false);
      });
    }, 250);
    return () => window.clearTimeout(timer);
  }, [client, search, state.conn, state.hydrated]);

  const loadMore = () => {
    const query = search.trim();
    const pageCursor = query ? searchCursor : historyCursor;
    if (!pageCursor || loading) return;
    const version = query ? ++searchRequestVersion.current : ++historyRequestVersion.current;
    setLoading(true);
    const request = query ? client.searchSessions(query, pageCursor) : client.listEndedSessions(pageCursor);
    request.then((page) => {
      if ((query ? searchRequestVersion.current : historyRequestVersion.current) !== version) return;
      const source = query ? "search" : "history";
      client.ownSessionPage(source, page.sessions, false);
      if (query) setSearchIds((current) => new Set([...(current ?? []), ...page.sessions.map((session) => session.id.toString())]));
      if (query) {
        setSearchCursor(page.nextCursor);
        setSearchTotal(Number(page.total));
      } else {
        setHistoryCursor(page.nextCursor);
        setHistoryTotal(Number(page.total));
      }
    }).catch((cause: unknown) => {
      if ((query ? searchRequestVersion.current : historyRequestVersion.current) === version) setError(cause instanceof Error ? cause.message : String(cause));
    }).finally(() => {
      if ((query ? searchRequestVersion.current : historyRequestVersion.current) === version) setLoading(false);
    });
  };

  const searching = Boolean(search.trim());
  const pageCursor = searching ? searchCursor : historyCursor;
  const resultTotal = searching ? searchTotal : historyTotal;
  const searchOrder = searchIds
    ? new Map([...searchIds].map((id, index) => [id, index]))
    : undefined;
  const fullModel = buildSidebar(state, searchIds ? true : showEnded, selectedId, searchIds, searchOrder);
  const model = stateFilter ? filterSidebarByState(fullModel, stateFilter) : fullModel;
  const remoteWorkers = [...state.workers.values()]
    .filter((worker) => worker.id !== LOCAL_WORKER_ID)
    .sort((a, b) => a.name.localeCompare(b.name));
  const onlineHosts = remoteWorkers.filter((worker) => worker.online);
  const offlineHosts = remoteWorkers.filter((worker) => !worker.online);
  // An empty host row is worse than a long one, so a fleet that is entirely
  // offline opens the group until the user says otherwise.
  const showOfflineHosts = offlineHostsOpen ?? onlineHosts.length === 0;

  const toggle = (
    set: Set<string>,
    key: string,
    id: string,
    apply: (next: Set<string>) => void,
  ) => {
    const next = new Set(set);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    writeStringSet(key, next);
    apply(next);
  };

  const storeFilters = (next: Map<string, RollupState>) => {
    writeStringMap(SUPERVISOR_FILTERS_KEY, next);
    setSupervisorFilters(next);
  };

  const clearSupervisorFilter = (id: string) => {
    if (!supervisorFilters.has(id)) return;
    const next = new Map(supervisorFilters);
    next.delete(id);
    storeFilters(next);
  };

  const pickSupervisorState = (id: string, picked: RollupState) => {
    storeFilters(nextSupervisorFilter(supervisorFilters, id, picked));
    if (collapsedSupervisors.has(id)) {
      toggle(collapsedSupervisors, COLLAPSED_SUPERVISORS_KEY, id, setCollapsedSupervisors);
    }
  };

  // Collapsing drops the filter so the caret always reopens to the full list.
  const toggleSupervisor = (id: string) => {
    clearSupervisorFilter(id);
    toggle(collapsedSupervisors, COLLAPSED_SUPERVISORS_KEY, id, setCollapsedSupervisors);
  };

  const run = (action: Promise<unknown>) => {
    setOpenMenu(null);
    action.catch((err: unknown) => {
      setError(err instanceof Error ? err.message : String(err));
    });
  };

  const renderSessionMenu = (session: Session) => {
    const id = session.id.toString();
    const key = `session:${id}`;
    return (
      <Popover
        open={openMenu === key}
        onToggle={() => setOpenMenu((cur) => toggleMenu(cur, key))}
        onClose={() => setOpenMenu(null)}
        triggerLabel="⋮"
        triggerTitle={`actions for ${sessionDisplayName(session)}`}
        triggerClassName="sb-menu-btn"
      >
        <button
          type="button"
          role="menuitem"
          className="popover-item"
          onClick={(event) => {
            const returnFocus = event.currentTarget
              .closest(".popover")
              ?.querySelector<HTMLElement>(".popover-trigger") ?? null;
            setOpenMenu(null);
            setSessionInfo({ session, returnFocus });
          }}
        >
          Info
        </button>
        <div className="popover-sep" />
        <div className="popover-group" role="group" aria-label="session role">
          <span className="popover-group-label">role</span>
          <button
            type="button"
            role="menuitemradio"
            aria-checked={session.role === SessionRole.WORKER}
            className="popover-item popover-radio"
            onClick={() => run(client.updateSessionApis(session.id, { role: SessionRole.WORKER, supervisorApi:false }))}
          >
            <span className="popover-radio-dot" aria-hidden="true">
              {session.role === SessionRole.WORKER ? "●" : "○"}
            </span>
            Worker
          </button>
          <button
            type="button"
            role="menuitemradio"
            aria-checked={session.role === SessionRole.SUPERVISOR}
            className="popover-item popover-radio"
            onClick={() =>
              run(client.updateSessionApis(session.id, { role: SessionRole.SUPERVISOR, itemsApi:true, supervisorApi:true }))
            }
          >
            <span className="popover-radio-dot" aria-hidden="true">
              {session.role === SessionRole.SUPERVISOR ? "●" : "○"}
            </span>
            Supervisor
          </button>
          {session.role === SessionRole.WORKER && <button type="button" role="menuitemcheckbox" aria-checked={session.itemsApi} className="popover-item popover-radio" onClick={()=>run(client.updateSessionApis(session.id,{itemsApi:!session.itemsApi}))}><span className="popover-radio-dot" aria-hidden="true">{session.itemsApi?"●":"○"}</span><span className="sentence">advanced: Items API</span></button>}
          <span className="popover-hint">
            a live agent may need an MCP reconnect or resume to notice newly granted tools
          </span>
        </div>
      </Popover>
    );
  };

  const renderRow = (
    session: Session,
    nesting: { child?: boolean; workers?: readonly Session[] } = {},
  ) => {
    const id = session.id.toString();
    const name = sessionDisplayName(session);
    const glance = state.contexts.get(id)?.glance ?? [];
    const linkedItem = linkedItemSummary(state, session);
    const selected = id === selectedId;
    const display = displaySessionState(session, nesting.workers ?? []);
    const needsInput = display === SessionState.NEEDS_INPUT;
    const unseen = needsInput && session.needsInputUnseen;
    const ended = sessionEnded(session);
    const activity = ended
      ? `${session.state === SessionState.FAILED ? "failed" : "ended"} · ${formatAgo(now - Number(session.endedAtUnixMs ?? BigInt(now)))} ago`
      : sessionLastActivity(session, now);
    const activityTitle = ended ? activity : sessionLastActivityTitle(session, now);
    return (
      <div className={sessionRowClass(session, { child: nesting.child, selected, display })} key={id}>
        <div className="sb-session-main">
          <button
            type="button"
            draggable
            className={`sb-session ${selected ? "is-selected" : ""} ${needsInput ? "is-needs-input" : ""}`}
            aria-label={sessionAccessibleLabel(session, display)}
            aria-current={selected ? "page" : undefined}
            title="Open session home. Shift-click to add its agent to the active workspace."
            onClick={(event) => {
              if (!event.shiftKey || !onAddToWorkspace(id)) onSelect(id);
            }}
            onDragStart={(event) => {
              const agentTerminal = agentTerminalForSession(state.terminals.values(), id);
              event.dataTransfer.effectAllowed = "copyMove";
              event.dataTransfer.setData(SESSION_DRAG_TYPE, id);
              if (agentTerminal) {
                event.dataTransfer.setData(
                  AGENT_TERMINAL_DRAG_TYPE,
                  agentTerminal.id.toString(),
                );
              }
              event.dataTransfer.setData("text/plain", name);
            }}
          >
            <span className="sb-session-gutter">
              <StateBadge state={display} dot seen={needsInput && !unseen} />
            </span>
            <span className="sb-session-content">
              <SessionRowText session={session} />
              <SessionGlance fields={glance} sessionId={id} onPmLink={onPmLink} />
              <span className="sb-session-meta">
                <SessionRoleIcon role={session.role} />
                <SessionBranch git={session.git} />
                <span
                  className={`sb-session-elapsed ${activity === "now" ? "is-now" : ""} ${activity === "unknown" || activity === "inactive" ? "is-muted" : ""}`}
                  aria-label={activityTitle}
                  title={activityTitle}
                >
                  {activity}
                </span>
              </span>
            </span>
          </button>
          {linkedItem && (
            <LinkedItemSummary summary={linkedItem} sessionId={id} onPmLink={onPmLink} showStatus={false} />
          )}
        </div>
        {renderSessionMenu(session)}
      </div>
    );
  };

  const renderBucketMenu = (bucket: Bucket) => {
    const key = `bucket:${bucket.id.toString()}`;
    return (
      <Popover
        open={openMenu === key}
        onToggle={() => setOpenMenu((cur) => toggleMenu(cur, key))}
        onClose={() => setOpenMenu(null)}
        triggerLabel="⋯"
        triggerTitle={`actions for ${bucket.name}`}
        triggerClassName="sb-menu-btn"
      >
        <button
          type="button"
          role="menuitem"
          className="popover-item"
          onClick={() => {
            setOpenMenu(null);
            onOpenBucketSettings(bucket.id.toString());
          }}
        >
          Settings
        </button>
        <button
          type="button"
          role="menuitem"
          className="popover-item"
          onClick={() => {
            setOpenMenu(null);
            onNewSession(bucket.id.toString());
          }}
        >
          <span className="sentence">new session…</span>
        </button>
        <button
          type="button"
          role="menuitem"
          className="popover-item"
          onClick={() => {
            setOpenMenu(null);
            onNewProject(bucket.id.toString());
          }}
        >
          <span className="sentence">new project…</span>
        </button>
        <div className="popover-sep" />
        <PermissionOptions
          current={bucket.permissionMode}
          includeInherit={false}
          onPick={(mode) => run(client.setBucketPermissionMode(bucket.id, mode))}
        />
        <div className="popover-sep" />
        <button
          type="button"
          role="menuitem"
          className="popover-item popover-item-danger"
          onClick={() => run(client.deleteBucket(bucket.id))}
        >
          <span className="sentence">delete bucket</span>
        </button>
      </Popover>
    );
  };


  return (
    <nav className="sidebar" aria-label="sessions">
      {error && (
        <div className="sb-flash" role="alert">
          <span>{error}</span>
          <button type="button" className="link-btn" onClick={() => setError(null)}>
            dismiss
          </button>
        </div>
      )}
      <div className="sb-search">
        <label className="visually-hidden" htmlFor="session-search">Search sessions</label>
        <input
          id="session-search"
          type="search"
          value={search}
          placeholder="Search sessions"
          onChange={(event) => setSearch(event.target.value)}
        />
        <span role="status" aria-live="polite">
          {loading ? "loading…" : searching && resultTotal === 0 ? "no matches" :
            searching && resultTotal !== null ? `${resultTotal} results` : ""}
        </span>
      </div>

      <div className="sb-scroll" ref={scrollRef}>
        {model.buckets.length === 0 && model.orphans.length === 0 && (
          stateFilter ? (
            <p className="muted-line sb-empty">
              no {ROLLUP_LABEL[stateFilter]} sessions here —{" "}
              <button type="button" className="link-btn" onClick={onClearStateFilter}>
                show every session
              </button>
            </p>
          ) : (
            <p className="muted-line sb-empty">
              no sessions yet — create a project in <a href="#/settings/projects">Settings</a>, then spawn one
            </p>
          )
        )}
        {model.buckets.map(({ bucket, supervisors, sessions }) => {
          const bid = bucket.id.toString();
          const bucketCollapsed = collapsedBuckets.has(bid);
          const bucketAttention = needsInputAttention([...supervisors, ...sessions]);
          const tree = supervisorTree({ bucket, supervisors, sessions }, model.order);
          return (
            <div className="sb-bucket" key={bid}>
              <div className="sb-bucket-row">
                <button
                  type="button"
                  className="sb-bucket-head"
                  aria-expanded={!bucketCollapsed}
                  onClick={() =>
                    toggle(collapsedBuckets, COLLAPSED_BUCKETS_KEY, bid, setCollapsedBuckets)
                  }
                >
                  <span className="sb-caret" aria-hidden="true">
                    {bucketCollapsed ? "▸" : "▾"}
                  </span>
                  {bucket.name}
                  {bucketCollapsed && <DescendantAttention attention={bucketAttention} />}
                </button>
                <BoardLink
                  bucketId={bid}
                  active={activeBoardBucketId === bid}
                  count={needsYouCount(state.items.values(), bid, now)}
                  onOpen={onOpenBoard}
                />
                <SpawnPopover
                  open={openMenu === `spawn-bucket:${bid}`}
                  onToggle={() => setOpenMenu((cur) => toggleMenu(cur, `spawn-bucket:${bid}`))}
                  onClose={() => setOpenMenu(null)}
                  onCreated={onSelect}
                  triggerTitle={`new session in ${bucket.name}`}
                  target={{ kind: "bucket", bucketId: bid }}
                />
                {renderBucketMenu(bucket)}
              </div>
              {!bucketCollapsed &&
                tree.entries.map((entry) => {
                  if (entry.kind === "session") return renderRow(entry.session);
                  const { supervisor, workers } = entry.group;
                  const sid = supervisor.id.toString();
                  return (
                    <SupervisorNode
                      key={sid}
                      row={renderRow(supervisor, { workers })}
                      workers={workers}
                      label={sessionDisplayName(supervisor)}
                      collapsed={collapsedSupervisors.has(sid)}
                      filter={supervisorFilters.get(sid) ?? null}
                      emptyNote={stateFilter ? `no ${ROLLUP_LABEL[stateFilter]} workers` : undefined}
                      onToggleCollapse={() => toggleSupervisor(sid)}
                      onPickState={(picked) => pickSupervisorState(sid, picked)}
                      onShowAll={() => clearSupervisorFilter(sid)}
                      renderWorker={(worker) => renderRow(worker, { child: true })}
                    />
                  );
                })}
            </div>
          );
        })}
        {model.orphans.length > 0 && (
          <div className="sb-bucket">
            <div className="sb-bucket-head sb-bucket-head-static">unfiled</div>
            {model.orphans.map((session) => renderRow(session))}
          </div>
        )}
        {pageCursor && (
          <button type="button" className="sb-load-more" onClick={loadMore} disabled={loading}>
            {loading ? "loading…" : "load more"}
          </button>
        )}
        {!loading && resultTotal !== null && !pageCursor && resultTotal > 0 && (
          <p className="muted-line sb-page-end">all {resultTotal} shown</p>
        )}
      </div>

      {remoteWorkers.length > 0 && (
        <section className="sb-workers" aria-label="workers">
          {onlineHosts.map((worker) => (
            <WorkerChip key={worker.id.toString()} worker={worker} />
          ))}
          {offlineHosts.length > 0 && (
            <button
              type="button"
              className="sb-workers-offline-toggle"
              aria-expanded={showOfflineHosts}
              aria-controls={offlineHostsId}
              onClick={() => setOfflineHostsOpen(!showOfflineHosts)}
            >
              <span aria-hidden="true">{showOfflineHosts ? "▾" : "▸"}</span>
              {offlineHosts.length} offline
            </button>
          )}
          <span className="sb-workers-offline" id={offlineHostsId}>
            {showOfflineHosts && offlineHosts.map((worker) => (
              <WorkerChip key={worker.id.toString()} worker={worker} />
            ))}
          </span>
        </section>
      )}

      <footer className="sb-footer">
        <label className="sb-check">
          <input type="checkbox" checked={showEnded} onChange={onToggleEnded} />
          <span className="sentence">show ended</span>
        </label>
        <NotifyToggle notify={notify} />
      </footer>
      {sessionInfo && (
        <SessionInfoDialog
          session={sessionInfo.session}
          project={state.projects.get(sessionInfo.session.projectId.toString())}
          worker={state.workers.get(sessionInfo.session.workerId.toString())}
          modelProfile={sessionInfo.session.modelProfileId === undefined
            ? undefined
            : state.modelProfiles.get(sessionInfo.session.modelProfileId.toString())}
          agentDialects={state.agentDialects}
          returnFocus={sessionInfo.returnFocus}
          onClose={() => setSessionInfo(null)}
        />
      )}
    </nav>
  );
}

export function BoardLink({
  bucketId,
  active,
  count,
  onOpen,
}: {
  bucketId: string;
  active: boolean;
  count: number;
  onOpen: (bucketId: string) => void;
}) {
  return (
    <button
      type="button"
      className={`sb-board-link ${active ? "is-selected" : ""}`}
      title={active ? "return to this bucket's previous view" : "open this bucket's work-item board"}
      aria-label={active ? "board, active — return to previous view" : "open board"}
      aria-pressed={active}
      onClick={() => onOpen(bucketId)}
    >
      <span className="sentence">board</span>
      {count > 0 && <span className="sb-board-count">{count}</span>}
    </button>
  );
}

function PermissionOptions({
  current,
  includeInherit,
  onPick,
}: {
  current: PermissionMode;
  includeInherit: boolean;
  onPick: (mode: PermissionMode) => void;
}) {
  const options: PermissionMode[] = [
    ...(includeInherit ? [PermissionMode.UNSPECIFIED] : []),
    PermissionMode.DEFAULT,
    PermissionMode.AUTO,
    PermissionMode.BYPASS,
  ];
  return (
    <div className="popover-group" role="group" aria-label="permission mode">
      <span className="popover-group-label">permission mode</span>
      {options.map((mode) => {
        const isCurrent = mode === current;
        const danger = mode === PermissionMode.BYPASS;
        return (
          <button
            key={mode}
            type="button"
            role="menuitemradio"
            aria-checked={isCurrent}
            className={`popover-item popover-radio ${danger ? "popover-item-danger" : ""}`}
            onClick={() => onPick(mode)}
          >
            <span className="popover-radio-dot" aria-hidden="true">
              {isCurrent ? "●" : "○"}
            </span>
            <span className="sentence">{permissionModeLabel(mode)}</span>
          </button>
        );
      })}
    </div>
  );
}

function NotifyToggle({ notify }: { notify: NotifyControls }) {
  if (!notify.support.supported) {
    if (notify.support.reason === "unavailable") return null;
    return (
      <span
        className="sb-notify-unavailable"
        role="note"
        title={notifyUnsupportedMessage(notify.support.reason)}
      >
        {notifyUnsupportedLabel(notify.support.reason)}
      </span>
    );
  }
  const granted = notify.permission === "granted";
  if (!granted || !notify.enabled) {
    return (
      <button type="button" className="link-btn sb-notify-enable" onClick={notify.onEnable}>
        {notify.permission === "denied" ? "notifications blocked" : "enable notifications"}
      </button>
    );
  }
  return (
    <span className="sb-notify-on">
      <span className="sb-notify-label">notifications on</span>
      <label className="sb-check">
        <input type="checkbox" checked={notify.sound} onChange={notify.onToggleSound} />
        <span className="sentence">sound</span>
      </label>
    </span>
  );
}

export function needsInputCount(sessions: Iterable<Session>): number {
  let count = 0;
  for (const s of sessions) if (s.needsInputUnseen) count += 1;
  return count;
}
