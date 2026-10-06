import type {
  AgentDialects,
  Bucket,
  BucketBriefing,
  Event,
  Item,
  Review,
  ReviewViewerState,
  InstructionLayer,
  ModelProfile,
  Plan,
  Project,
  Session,
  SessionContext,
  SessionForward,
  Snapshot,
  Terminal,
  Worker,
} from "../gen/pm/v1/pm_pb";

export type ConnPhase = "connecting" | "online" | "offline";

export interface AppState {
  conn: ConnPhase;
  /** True once the first snapshot of the current connection has arrived. */
  hydrated: boolean;
  buckets: ReadonlyMap<string, Bucket>;
  projects: ReadonlyMap<string, Project>;
  sessions: ReadonlyMap<string, Session>;
  /** View/subscription owners for each canonical Session record. */
  sessionSources: ReadonlyMap<string, number>;
  workers: ReadonlyMap<string, Worker>;
  terminals: ReadonlyMap<string, Terminal>;
  /** Agent-authored context bags, keyed by session id. */
  contexts: ReadonlyMap<string, SessionContext>;
  /** Published port forwards, keyed by forward id. */
  forwards: ReadonlyMap<string, SessionForward>;
  /** Work items, keyed by `bucket_id/item_number`. */
  items: ReadonlyMap<string, Item>;
  /** Open reviews. Threads and diffs are fetched over HTTP, not here. */
  reviews: ReadonlyMap<string, Review>;
  plans: ReadonlyMap<string, Plan>;
  /** This user's own place in each review, including what they have
   * seen. Per-user, so it arrives with the snapshot rather than on the
   * broadcast event bus. */
  reviewViewerStates: ReadonlyMap<string, ReviewViewerState>;
  /** The latest briefing per bucket, keyed by bucket id. */
  briefings: ReadonlyMap<string, BucketBriefing>;
  /** Authenticated user's server-synced preferences, keyed by typed setting key. */
  userSettings: ReadonlyMap<string, string>;
  instructionLayers: ReadonlyMap<string, InstructionLayer>;
  /** Controller-global provider accounts, keyed by profile id. */
  modelProfiles: ReadonlyMap<string, ModelProfile>;
  /** Which dialects each agent adapter speaks, as the daemon reports them. */
  agentDialects: readonly AgentDialects[];
}

export type Action =
  | { type: "conn"; phase: ConnPhase }
  | { type: "snapshot"; snapshot: Snapshot }
  | { type: "event"; event: Event }
  | { type: "sessionPage"; sessions: readonly Session[]; source: SessionSource; replace: boolean }
  | { type: "selectedSession"; id: string | null; session?: Session }
  | { type: "expireSessions"; nowUnixMs: number };

export type SessionSource = "subscription" | "history" | "search" | "selected";
const SOURCE_BITS: Record<SessionSource, number> = { subscription: 1, history: 2, search: 4, selected: 8 };
export const SESSION_RECENT_GRACE_MS = 60_000;

export function sessionHasSource(state: AppState, id: string, source: SessionSource): boolean {
  return ((state.sessionSources.get(id) ?? 0) & SOURCE_BITS[source]) !== 0;
}

export const initialState: AppState = {
  conn: "connecting",
  hydrated: false,
  buckets: new Map(),
  projects: new Map(),
  sessions: new Map(),
  sessionSources: new Map(),
  workers: new Map(),
  terminals: new Map(),
  contexts: new Map(),
  forwards: new Map(),
  reviews: new Map(),
  plans: new Map(),
  reviewViewerStates: new Map(),
  items: new Map(),
  briefings: new Map(),
  userSettings: new Map(),
  instructionLayers: new Map(),
  modelProfiles: new Map(),
  agentDialects: [],
};

function byId<T extends { id: bigint }>(items: readonly T[]): Map<string, T> {
  return new Map(items.map((item) => [item.id.toString(), item]));
}

export function itemKey(bucketId: bigint | string, itemId: bigint | string): string {
  return `${bucketId.toString()}/${itemId.toString()}`;
}

function byItemRef(items: readonly Item[]): Map<string, Item> {
  return new Map(items.map((item) => [itemKey(item.bucketId, item.id), item]));
}

function bySessionId(items: readonly SessionContext[]): Map<string, SessionContext> {
  return new Map(items.map((item) => [item.sessionId.toString(), item]));
}

function byBucketId(briefings: readonly BucketBriefing[]): Map<string, BucketBriefing> {
  return new Map(briefings.map((b) => [b.bucketId.toString(), b]));
}

function upsert<T>(map: ReadonlyMap<string, T>, key: string, value: T): Map<string, T> {
  const next = new Map(map);
  next.set(key, value);
  return next;
}

function remove<T>(map: ReadonlyMap<string, T>, key: string): Map<string, T> {
  const next = new Map(map);
  next.delete(key);
  return next;
}

export function reduce(state: AppState, action: Action): AppState {
  switch (action.type) {
    case "conn":
      return { ...state, conn: action.phase };
    case "snapshot":
      return {
        ...state,
        hydrated: true,
        buckets: byId(action.snapshot.buckets),
        projects: byId(action.snapshot.projects),
        sessions: byId(action.snapshot.sessions),
        sessionSources: new Map(action.snapshot.sessions.map((session) => [session.id.toString(), SOURCE_BITS.subscription])),
        workers: byId(action.snapshot.workers),
        terminals: byId(action.snapshot.terminals),
        contexts: bySessionId(action.snapshot.contexts),
        forwards: byId(action.snapshot.forwards),
        reviews: byId(action.snapshot.reviews),
        plans: byId(action.snapshot.plans),
        reviewViewerStates: new Map(
          action.snapshot.reviewViewerStates.map((s) => [s.reviewId.toString(), s]),
        ),
        items: byItemRef(action.snapshot.items),
        briefings: byBucketId(action.snapshot.briefings),
        userSettings: new Map(
          action.snapshot.userSettings.map((setting) => [setting.key, setting.valueJson]),
        ),
        instructionLayers: byId(action.snapshot.instructionLayers),
        modelProfiles: byId(action.snapshot.modelProfiles),
        agentDialects: action.snapshot.agentDialects,
      };
    case "event":
      return applyEvent(state, action.event);
    case "sessionPage":
      return mergeSessionSource(state, action.sessions, action.source, action.replace);
    case "selectedSession":
      return setSelectedSession(state, action.id, action.session);
    case "expireSessions":
      return expireSubscriptionSessions(state, action.nowUnixMs);
  }
}

function applyEvent(state: AppState, event: Event): AppState {
  const ev = event.event;
  switch (ev.case) {
    case "sessionChanged":
      return mergeSessionSource(state, [ev.value], "subscription", false);
    case "sessionRemoved":
      return { ...state, sessions: remove(state.sessions, ev.value.toString()), sessionSources: remove(state.sessionSources, ev.value.toString()) };
    case "bucketChanged":
      return { ...state, buckets: upsert(state.buckets, ev.value.id.toString(), ev.value) };
    case "bucketRemoved":
      return { ...state, buckets: remove(state.buckets, ev.value.toString()) };
    case "projectChanged":
      return { ...state, projects: upsert(state.projects, ev.value.id.toString(), ev.value) };
    case "projectRemoved":
      return { ...state, projects: remove(state.projects, ev.value.toString()) };
    case "workerChanged":
      return { ...state, workers: upsert(state.workers, ev.value.id.toString(), ev.value) };
    case "workerRemoved":
      return { ...state, workers: remove(state.workers, ev.value.toString()) };
    case "terminalChanged":
      return { ...state, terminals: upsert(state.terminals, ev.value.id.toString(), ev.value) };
    case "terminalRemoved":
      return { ...state, terminals: remove(state.terminals, ev.value.toString()) };
    case "reviewChanged":
      return { ...state, reviews: upsert(state.reviews, ev.value.id.toString(), ev.value) };
    case "reviewRemoved":
      return { ...state, reviews: remove(state.reviews, ev.value.toString()) };
    case "planChanged":
      return { ...state, plans: upsert(state.plans, ev.value.id.toString(), ev.value) };
    case "planRemoved":
      return { ...state, plans: remove(state.plans, ev.value.toString()) };
    // Carries no state: the daemon raised it so a connected client can
    // render an alert, which PmClient hands to its listeners.
    case "sessionAlert":
      return state;
    // Rendered as it arrives rather than held in state: it is a thing to be
    // told, not a thing to be listed.
    case "securityNotice":
      return state;
    case "contextChanged": {
      const key = ev.value.sessionId.toString();
      if (!state.sessions.has(key)) return state;
      const empty = ev.value.glance.length === 0 && ev.value.detail.length === 0;
      return {
        ...state,
        contexts: empty ? remove(state.contexts, key) : upsert(state.contexts, key, ev.value),
      };
    }
    case "forwardChanged":
      return { ...state, forwards: upsert(state.forwards, ev.value.id.toString(), ev.value) };
    case "forwardRemoved":
      return { ...state, forwards: remove(state.forwards, ev.value.toString()) };
    case "itemChanged":
      return { ...state, items: upsert(state.items, itemKey(ev.value.bucketId, ev.value.id), ev.value) };
    case "itemRemoved":
      return { ...state, items: remove(state.items, itemKey(ev.value.bucketId, ev.value.itemId)) };
    case "briefingChanged":
      return {
        ...state,
        briefings: upsert(state.briefings, ev.value.bucketId.toString(), ev.value),
      };
    case "userSettingChanged":
      return {
        ...state,
        userSettings: ev.value.valueJson === undefined
          ? remove(state.userSettings, ev.value.key)
          : upsert(state.userSettings, ev.value.key, ev.value.valueJson),
      };
    case "instructionLayerChanged":
      return { ...state, instructionLayers: upsert(state.instructionLayers,ev.value.id.toString(),ev.value) };
    case "modelProfileChanged":
      return { ...state, modelProfiles: upsert(state.modelProfiles, ev.value.id.toString(), ev.value) };
    case "modelProfileRemoved":
      return { ...state, modelProfiles: remove(state.modelProfiles, ev.value.toString()) };
    case undefined:
      return state;
  }
}

function mergeSessionSource(
  state: AppState,
  incoming: readonly Session[],
  source: SessionSource,
  replace: boolean,
): AppState {
  const bit = SOURCE_BITS[source];
  const sessions = new Map(state.sessions);
  const sources = new Map(state.sessionSources);
  const removed = new Set<string>();
  if (replace) {
    for (const [id, owned] of sources) {
      const next = owned & ~bit;
      if (next === 0) { sources.delete(id); sessions.delete(id); removed.add(id); }
      else sources.set(id, next);
    }
  }
  for (const session of incoming) {
    const id = session.id.toString();
    sessions.set(id, session);
    sources.set(id, (sources.get(id) ?? 0) | bit);
  }
  return withPrunedSessionDependents({ ...state, sessions, sessionSources: sources }, removed);
}

function setSelectedSession(state: AppState, id: string | null, session?: Session): AppState {
  const retained = session ?? (id ? state.sessions.get(id) : undefined);
  let next = mergeSessionSource(state, retained ? [retained] : [], "selected", true);
  if (id && next.sessions.has(id)) {
    const sources = new Map(next.sessionSources);
    sources.set(id, (sources.get(id) ?? 0) | SOURCE_BITS.selected);
    next = { ...next, sessionSources: sources };
  }
  return next;
}

function expireSubscriptionSessions(state: AppState, nowUnixMs: number): AppState {
  const sessions = new Map(state.sessions);
  const sources = new Map(state.sessionSources);
  const removed = new Set<string>();
  let changed = false;
  for (const [id, session] of sessions) {
    const owned = sources.get(id) ?? 0;
    const ended = session.endedAtUnixMs;
    if ((owned & SOURCE_BITS.subscription) === 0 || ended === undefined ||
        Number(ended) > nowUnixMs - SESSION_RECENT_GRACE_MS) continue;
    changed = true;
    const next = owned & ~SOURCE_BITS.subscription;
    if (next === 0) { sessions.delete(id); sources.delete(id); removed.add(id); }
    else sources.set(id, next);
  }
  return changed ? withPrunedSessionDependents({ ...state, sessions, sessionSources: sources }, removed) : state;
}

function withPrunedSessionDependents(state: AppState, removed: ReadonlySet<string>): AppState {
  if (removed.size === 0) return state;
  return {
    ...state,
    contexts: new Map([...state.contexts].filter(([sessionId]) => !removed.has(sessionId))),
    terminals: new Map([...state.terminals].filter(([, terminal]) => !removed.has(terminal.sessionId.toString()))),
    forwards: new Map([...state.forwards].filter(([, forward]) => !removed.has(forward.sessionId.toString()))),
  };
}
