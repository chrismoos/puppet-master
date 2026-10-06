import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useSyncExternalStore } from "react";
import { Pressable, RefreshControl, ScrollView, StyleSheet, Text, TextInput, View } from "react-native";

import { KeyboardAwareFlatList } from "../components/KeyboardAwareScrollables";

import type { PmClient } from "@puppet-master/client-core/ws/client";
import { SessionState, TerminalKind, type Session } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { sessionEnded, sessionLastActive, sessionDisplayName, sessionMatchesSearch } from "@puppet-master/client-core/format";
import { sessionRowSubtitle } from "./sessionRow";
import { beginRefresh, endRefresh, idleRefresh, isRefreshing } from "./manualRefresh";
import type { KeyValueStorage } from "@puppet-master/client-core/platform";
import {
  buildMobileSessionList,
  filterSessions,
  projectIdsForBucket,
  type MobileSessionFilter,
} from "@puppet-master/client-core/state/mobileSessionList";
import { displaySessionState } from "@puppet-master/client-core/state/displayState";
import type { ConnectionBanner } from "../status";
import { colors, stateColor, stateBackgroundColor } from "../theme";
import { OverflowButton, OverflowMenu, type OverflowMenuItem } from "./OverflowMenu";
import { SpawnSheet } from "./SpawnSheet";

export interface TerminalTarget {
  sessionId: bigint;
  terminalId: bigint;
  generation: bigint;
  title: string;
}

const EXPANSION_STORAGE_KEY = "pm:session-expansion";
const FILTER_STORAGE_KEY = "pm:session-filter";
const SHOW_ENDED_STORAGE_KEY = "pm:show-ended";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function useNow(intervalMs: number): number {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(id);
  }, [intervalMs]);
  return now;
}

/** Row tint for attention states. */
function rowStateStyle(state: SessionState): { backgroundColor: string } | null {
  const bg = stateBackgroundColor(state);
  return bg ? { backgroundColor: bg } : null;
}

/** Dot indicator showing session state — always visible so colour is not
 *  the only attention signal. Uses the semantic state colour. */
function StateDot({ state }: { state: SessionState }) {
  return <View style={[styles.dot, { backgroundColor: stateColor(state) }]} />;
}

/** Renders text with matching portions highlighted. */
function HighlightedText({
  text,
  query,
  style,
}: {
  text: string;
  query: string;
  style: object;
}) {
  if (!query) return <Text style={style} numberOfLines={1}>{text}</Text>;

  const lower = text.toLowerCase();
  const qLower = query.toLowerCase();
  const parts: { text: string; highlight: boolean }[] = [];
  let cursor = 0;

  while (cursor < text.length) {
    const idx = lower.indexOf(qLower, cursor);
    if (idx < 0) {
      parts.push({ text: text.slice(cursor), highlight: false });
      break;
    }
    if (idx > cursor) parts.push({ text: text.slice(cursor, idx), highlight: false });
    parts.push({ text: text.slice(idx, idx + query.length), highlight: true });
    cursor = idx + query.length;
  }

  return (
    <Text style={style} numberOfLines={1}>
      {parts.map((p, i) =>
        p.highlight ? (
          <Text key={i} style={{ color: colors.blue, fontWeight: "600" }}>{p.text}</Text>
        ) : (
          <Text key={i}>{p.text}</Text>
        ),
      )}
    </Text>
  );
}

function RowSubtitle({
  session,
  workers,
  showHost,
}: {
  session: Session;
  workers: ReadonlyMap<string, { hostname: string; name: string; id: bigint }>;
  showHost: boolean;
}) {
  const worker = workers.get(session.workerId.toString());
  const host = worker
    ? worker.id === 0n ? "local" : worker.hostname || worker.name
    : "";
  const subtitle = sessionRowSubtitle(session, host, showHost);
  if (!subtitle) return null;
  return (
    <Text style={{ color: colors.textMuted, fontSize: 11, marginTop: 1 }} numberOfLines={1}>
      {subtitle}
    </Text>
  );
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function SessionsScreen({
  client,
  storage,
  banner,
  onOpenTerminal,
  onOpenSettings,
  onOpenApprovals,
  pendingApprovals = 0,
}: {
  client: PmClient;
  storage: KeyValueStorage;
  banner: ConnectionBanner;
  onOpenTerminal: (target: TerminalTarget) => void;
  onOpenSettings: () => void;
  onOpenApprovals: () => void;
  pendingApprovals?: number;
}) {
  const state = useSyncExternalStore(client.subscribe, client.getState);
  const now = useNow(15_000); // refresh timestamps every 15s

  // Filter state: persisted bucket/project selection
  const [filter, setFilter] = useState<MobileSessionFilter>(() => {
    const raw = storage.getItem(FILTER_STORAGE_KEY);
    if (!raw) return {};
    try { return JSON.parse(raw) as MobileSessionFilter; } catch { return {}; }
  });
  const [filterOpen, setFilterOpen] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [spawnOpen, setSpawnOpen] = useState(false);
  const [showEnded, setShowEnded] = useState(() => storage.getItem(SHOW_ENDED_STORAGE_KEY) === "1");
  const [searchActive, setSearchActive] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const searchInputRef = useRef<TextInput>(null);
  const [refreshState, setRefreshState] = useState(idleRefresh);

  const onPullToRefresh = useCallback(() => {
    const begun = beginRefresh(refreshState);
    if (!begun) return;
    setRefreshState(begun.state);
    const { token } = begun;
    const done = () => setRefreshState((prev) => endRefresh(prev, token));
    client.refresh().then(done, done);
  }, [client, refreshState]);

  const updateFilter = useCallback((next: MobileSessionFilter) => {
    setFilter(next);
    storage.setItem(FILTER_STORAGE_KEY, JSON.stringify(next));
  }, [storage]);

  const filterCount = (filter.bucketId ? 1 : 0) + (filter.projectId ? 1 : 0);

  const toggleShowEnded = useCallback(() => {
    setShowEnded((prev) => {
      const next = !prev;
      storage.setItem(SHOW_ENDED_STORAGE_KEY, next ? "1" : "0");
      return next;
    });
  }, [storage]);

  // Compute filtered sessions, then build the grouped/sorted list
  const filteredSessions = useMemo(() => {
    let sessions: Session[];
    if (!filter.bucketId && !filter.projectId) {
      sessions = [...state.sessions.values()];
    } else {
      const projectsInBucket = filter.bucketId
        ? projectIdsForBucket(state.projects.values(), filter.bucketId)
        : null;
      sessions = filterSessions(state.sessions.values(), filter, projectsInBucket);
    }
    if (!showEnded) sessions = sessions.filter((s) => !sessionEnded(s));
    return sessions;
  }, [state.sessions, state.projects, filter, showEnded]);

  const entries = useMemo(
    () => buildMobileSessionList(filteredSessions),
    [filteredSessions],
  );

  // Available buckets and projects for the filter UI
  const buckets = useMemo(
    () => [...state.buckets.values()].sort((a, b) => a.position - b.position),
    [state.buckets],
  );
  const projects = useMemo(
    () => [...state.projects.values()].sort((a, b) => a.name.localeCompare(b.name)),
    [state.projects],
  );
  const filteredProjects = useMemo(
    () => filter.bucketId
      ? projects.filter((p) => p.bucketId.toString() === filter.bucketId)
      : projects,
    [projects, filter.bucketId],
  );

  // Expansion state: which supervisor groups are expanded, persisted via storage
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => {
    const raw = storage.getItem(EXPANSION_STORAGE_KEY);
    if (!raw) return new Set<string>();
    try { return new Set<string>(JSON.parse(raw)); } catch { return new Set<string>(); }
  });

  const toggleExpanded = useCallback((supervisorId: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(supervisorId)) next.delete(supervisorId);
      else next.add(supervisorId);
      storage.setItem(EXPANSION_STORAGE_KEY, JSON.stringify([...next]));
      return next;
    });
  }, [storage]);

  const openSession = useCallback((session: Session) => {
    const terminal = [...state.terminals.values()].find(
      (t) => t.sessionId === session.id && t.kind === TerminalKind.AGENT,
    );
    if (!terminal) return;
    if (session.needsInputUnseen || session.idleUnseen) {
      void client.markSessionSeen(session.id).catch(() => {});
    }
    onOpenTerminal({
      sessionId: session.id,
      terminalId: terminal.id,
      generation: terminal.generation,
      title: sessionDisplayName(session),
    });
  }, [state.terminals, client, onOpenTerminal]);

  // Flatten entries for FlatList: groups expand to show children
  const flatItems = useMemo(() => {
    const items: Array<
      | { kind: "standalone"; session: Session }
      | { kind: "supervisor"; session: Session; displayState: SessionState; workerCount: number; needsAttention: boolean; isExpanded: boolean }
      | { kind: "worker"; session: Session; isLast: boolean }
    > = [];

    for (const entry of entries) {
      if (entry.kind === "standalone") {
        items.push({ kind: "standalone", session: entry.session });
      } else {
        const supId = entry.supervisor.id.toString();
        const isExpanded = expanded.has(supId);
        items.push({
          kind: "supervisor",
          session: entry.supervisor,
          displayState: displaySessionState(entry.supervisor, entry.workers),
          workerCount: entry.workers.length,
          needsAttention: entry.needsAttention,
          isExpanded,
        });
        if (isExpanded) {
          for (let i = 0; i < entry.workers.length; i++) {
            items.push({
              kind: "worker",
              session: entry.workers[i],
              isLast: i === entry.workers.length - 1,
            });
          }
        }
      }
    }
    return items;
  }, [entries, expanded]);

  // Search-narrowed items: filter flatItems by search query on goal and headline
  const searchedItems = useMemo(() => {
    if (!searchActive || !searchQuery) return flatItems;
    return flatItems.filter((item) => sessionMatchesSearch(item.session, searchQuery));
  }, [flatItems, searchActive, searchQuery]);

  const totalCount = flatItems.length;
  const matchCount = searchedItems.length;

  const renderItem = useCallback(({ item }: { item: (typeof flatItems)[number] }) => {
    switch (item.kind) {
      case "standalone":
        return (
          <Pressable
            style={[styles.row, rowStateStyle(item.session.state)]}
            onPress={() => openSession(item.session)}
          >
            <StateDot state={item.session.state} />
            <View style={styles.rowTextCol}>
              {searchActive && searchQuery ? (
                <HighlightedText text={sessionDisplayName(item.session)} query={searchQuery} style={styles.rowTitle} />
              ) : (
                <Text style={styles.rowTitle} numberOfLines={1}>{sessionDisplayName(item.session)}</Text>
              )}
              <RowSubtitle session={item.session} workers={state.workers} showHost={searchActive} />
            </View>
            <Text style={styles.rowTimestamp}>{sessionLastActive(item.session, now)}</Text>
          </Pressable>
        );

      case "supervisor":
        return (
          <Pressable
            style={[
              styles.row,
              item.needsAttention && !item.isExpanded
                ? { backgroundColor: stateBackgroundColor(SessionState.NEEDS_INPUT) ?? undefined }
                : null,
            ]}
            onPress={() => openSession(item.session)}
          >
            {item.needsAttention && !item.isExpanded ? (
              <View style={[styles.dot, { backgroundColor: stateColor(SessionState.NEEDS_INPUT) }]} />
            ) : (
              <StateDot state={item.displayState} />
            )}
            <View style={styles.rowTextCol}>
              {searchActive && searchQuery ? (
                <HighlightedText text={sessionDisplayName(item.session)} query={searchQuery} style={styles.rowTitle} />
              ) : (
                <Text style={styles.rowTitle} numberOfLines={1}>{sessionDisplayName(item.session)}</Text>
              )}
              <RowSubtitle session={item.session} workers={state.workers} showHost={searchActive} />
            </View>
            <Pressable
              onPress={(e) => {
                e.stopPropagation();
                toggleExpanded(item.session.id.toString());
              }}
              hitSlop={{ top: 8, bottom: 8, left: 4, right: 4 }}
              style={styles.pillTouch}
            >
              <View style={[styles.pill, item.needsAttention && !item.isExpanded && styles.pillAttention]}>
                <Text style={[styles.pillText, item.needsAttention && !item.isExpanded && styles.pillTextAttention]}>
                  {item.workerCount}{item.isExpanded ? " ▾" : " ▸"}
                </Text>
              </View>
            </Pressable>
            <Text style={styles.rowTimestamp}>{sessionLastActive(item.session, now)}</Text>
          </Pressable>
        );

      case "worker":
        return (
          <Pressable
            style={[styles.row, styles.workerRow, rowStateStyle(item.session.state)]}
            onPress={() => openSession(item.session)}
          >
            <StateDot state={item.session.state} />
            <View style={styles.rowTextCol}>
              {searchActive && searchQuery ? (
                <HighlightedText text={sessionDisplayName(item.session)} query={searchQuery} style={styles.rowTitle} />
              ) : (
                <Text style={styles.rowTitle} numberOfLines={1}>{sessionDisplayName(item.session)}</Text>
              )}
              <RowSubtitle session={item.session} workers={state.workers} showHost={searchActive} />
            </View>
            <Text style={styles.rowTimestamp}>{sessionLastActive(item.session, now)}</Text>
          </Pressable>
        );

    }
  }, [now, openSession, toggleExpanded, searchActive, searchQuery, state.workers]);

  const keyExtractor = useCallback((item: (typeof flatItems)[number]) => {
    const prefix = item.kind === "worker" ? "w:" : item.kind === "supervisor" ? "s:" : "";
    return `${prefix}${item.session.id}`;
  }, []);

  const menuItems: OverflowMenuItem[] = useMemo(() => [
    { key: "spawn", label: "New session", icon: "dot", color: colors.blue, onPress: () => setSpawnOpen(true) },
    { key: "filter", label: "Filter", icon: "filter", badge: filterCount || undefined, onPress: () => setFilterOpen((v) => !v) },
    { key: "search", label: "Search", icon: "search", onPress: () => { setSearchActive(true); setTimeout(() => searchInputRef.current?.focus(), 100); } },
    { key: "ended", label: showEnded ? "Hide ended" : "Show ended", icon: "eye", onPress: toggleShowEnded },
    { key: "approvals", label: "Approvals", icon: "shieldCheck", badge: pendingApprovals || undefined, onPress: onOpenApprovals },
    { key: "settings", label: "Settings", icon: "gear", onPress: onOpenSettings },
  ], [filterCount, showEnded, toggleShowEnded, onOpenSettings, onOpenApprovals, pendingApprovals]);

  return (
    <View style={styles.root}>
      {searchActive ? (
        <View style={styles.header}>
          <TextInput
            ref={searchInputRef}
            style={styles.searchInput}
            placeholder="Search sessions…"
            placeholderTextColor={colors.textMuted}
            value={searchQuery}
            onChangeText={setSearchQuery}
            autoFocus
            returnKeyType="search"
          />
          {searchQuery ? (
            <Pressable onPress={() => setSearchQuery("")} hitSlop={8}>
              <Text style={styles.searchClear}>✕</Text>
            </Pressable>
          ) : null}
          <Pressable onPress={() => { setSearchActive(false); setSearchQuery(""); }} hitSlop={8}>
            <Text style={styles.searchCancel}>Cancel</Text>
          </Pressable>
        </View>
      ) : (
        <View style={styles.header}>
          <Text style={styles.title}>Sessions</Text>
          <View style={[styles.connDot, { backgroundColor: banner.label === "connected" ? colors.green : colors.textMuted }]} />
          <OverflowButton onPress={() => setMenuOpen(true)} />
        </View>
      )}
      {searchActive && (filterCount > 0 || searchQuery) ? (
        <View style={styles.searchStatus}>
          {filterCount > 0 ? (
            <View style={styles.searchChips}>
              {filter.bucketId ? (
                <Pressable
                  style={styles.searchChip}
                  onPress={() => updateFilter({ projectId: filter.projectId })}
                >
                  <Text style={styles.searchChipText}>
                    {buckets.find((b) => b.id.toString() === filter.bucketId)?.name ?? "bucket"}
                  </Text>
                  <Text style={styles.searchChipX}>✕</Text>
                </Pressable>
              ) : null}
              {filter.projectId ? (
                <Pressable
                  style={styles.searchChip}
                  onPress={() => updateFilter({ bucketId: filter.bucketId })}
                >
                  <Text style={styles.searchChipText}>
                    {projects.find((p) => p.id.toString() === filter.projectId)?.name ?? "project"}
                  </Text>
                  <Text style={styles.searchChipX}>✕</Text>
                </Pressable>
              ) : null}
            </View>
          ) : null}
          {searchQuery ? (
            <Text style={styles.matchCount}>{matchCount} of {totalCount}</Text>
          ) : null}
        </View>
      ) : null}
      <OverflowMenu visible={menuOpen} items={menuItems} onClose={() => setMenuOpen(false)} />
      <SpawnSheet visible={spawnOpen} client={client} onClose={() => setSpawnOpen(false)} />
      {filterOpen ? (
        <View style={styles.filterBar}>
          {filterCount > 0 ? (
            <Pressable style={styles.clearFilters} onPress={() => updateFilter({})}>
              <Text style={styles.clearFiltersText}>Clear filters</Text>
            </Pressable>
          ) : null}
          <Text style={styles.filterLabel}>Bucket</Text>
          <ScrollView horizontal showsHorizontalScrollIndicator={false} style={styles.chipScroll}>
            {buckets.map((b) => {
              const id = b.id.toString();
              const selected = filter.bucketId === id;
              return (
                <Pressable
                  key={id}
                  style={[styles.chip, selected && styles.chipSelected]}
                  onPress={() => updateFilter(selected ? {} : { bucketId: id })}
                >
                  <Text style={[styles.chipText, selected && styles.chipTextSelected]}>{b.name}</Text>
                </Pressable>
              );
            })}
          </ScrollView>
          {filteredProjects.length > 0 ? (
            <>
              <Text style={styles.filterLabel}>Project</Text>
              <ScrollView horizontal showsHorizontalScrollIndicator={false} style={styles.chipScroll}>
                {filteredProjects.map((p) => {
                  const id = p.id.toString();
                  const selected = filter.projectId === id;
                  return (
                    <Pressable
                      key={id}
                      style={[styles.chip, selected && styles.chipSelected]}
                      onPress={() =>
                        updateFilter(
                          selected
                            ? { bucketId: filter.bucketId }
                            : { bucketId: filter.bucketId, projectId: id },
                        )
                      }
                    >
                      <Text style={[styles.chipText, selected && styles.chipTextSelected]}>{p.name}</Text>
                    </Pressable>
                  );
                })}
              </ScrollView>
            </>
          ) : null}
        </View>
      ) : null}
      {banner.detail ? (
        <View style={styles.notice}>
          <Text style={styles.noticeText}>{banner.detail}</Text>
          {banner.needsEnrollment ? (
            <Pressable style={styles.noticeButton} onPress={onOpenSettings}>
              <Text style={styles.noticeButtonText}>Log in</Text>
            </Pressable>
          ) : null}
        </View>
      ) : null}
      <KeyboardAwareFlatList
        data={searchedItems}
        keyExtractor={keyExtractor}
        renderItem={renderItem}
        refreshControl={
          <RefreshControl
            refreshing={isRefreshing(refreshState)}
            onRefresh={onPullToRefresh}
            tintColor={colors.textMuted}
          />
        }
        ListEmptyComponent={
          <Text style={styles.empty}>
            {state.hydrated ? "No sessions." : "Waiting for snapshot\u2026"}
          </Text>
        }
      />
    </View>
  );
}

// ---------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------

const styles = StyleSheet.create({
  root: { flex: 1 },
  header: {
    flexDirection: "row",
    alignItems: "center",
    gap: 12,
    paddingHorizontal: 16,
    paddingVertical: 12,
  },
  title: { color: colors.textBright, fontSize: 20, fontWeight: "600", flex: 1 },
  connDot: { width: 8, height: 8, borderRadius: 4 },
  searchInput: {
    flex: 1,
    color: colors.text,
    fontSize: 16,
    paddingVertical: 8,
    paddingHorizontal: 4,
  },
  searchClear: { color: colors.textMuted, fontSize: 16, paddingHorizontal: 4 },
  searchCancel: { color: colors.blue, fontSize: 14, marginLeft: 8 },
  searchStatus: {
    flexDirection: "row",
    alignItems: "center",
    paddingHorizontal: 16,
    paddingBottom: 6,
    gap: 8,
  },
  searchChips: { flexDirection: "row", gap: 6, flex: 1 },
  searchChip: {
    flexDirection: "row",
    alignItems: "center",
    gap: 4,
    backgroundColor: colors.blue,
    borderRadius: 12,
    paddingHorizontal: 10,
    paddingVertical: 3,
  },
  searchChipText: { color: "#fff", fontSize: 12, fontWeight: "600" },
  searchChipX: { color: "rgba(255,255,255,0.7)", fontSize: 10 },
  matchCount: { color: colors.textMuted, fontSize: 12 },
  notice: {
    marginHorizontal: 16,
    marginBottom: 8,
    padding: 12,
    backgroundColor: colors.panelAlt,
    borderRadius: 8,
    gap: 8,
  },
  noticeText: { color: colors.amber, fontSize: 13 },
  noticeButton: {
    alignSelf: "flex-start",
    backgroundColor: colors.blue,
    borderRadius: 6,
    paddingHorizontal: 14,
    paddingVertical: 6,
  },
  noticeButtonText: { color: "#fff", fontSize: 13, fontWeight: "600" },
  row: {
    paddingHorizontal: 16,
    paddingVertical: 12,
    borderTopWidth: StyleSheet.hairlineWidth,
    borderTopColor: colors.line,
    flexDirection: "row",
    alignItems: "center",
    gap: 8,
  },
  rowTextCol: { flex: 1 },
  rowTitle: { color: colors.text, fontSize: 15 },
  rowTimestamp: { color: colors.textMuted, fontSize: 12 },
  workerRow: {
    paddingLeft: 40,
  },
  pillTouch: {
    minWidth: 36,
    height: 28,
    alignItems: "center" as const,
    justifyContent: "center" as const,
  },
  pill: {
    flexDirection: "row" as const,
    alignItems: "center" as const,
    backgroundColor: colors.line,
    borderRadius: 10,
    paddingHorizontal: 8,
    paddingVertical: 2,
  },
  pillAttention: {
    backgroundColor: "rgba(255, 178, 36, 0.18)",
  },
  pillText: {
    color: colors.textMuted,
    fontSize: 11,
    fontWeight: "600" as const,
  },
  pillTextAttention: {
    color: colors.amber,
  },
  dot: {
    width: 8,
    height: 8,
    borderRadius: 4,
  },
  empty: { color: colors.textMuted, textAlign: "center", marginTop: 48 },
  filterBar: {
    paddingHorizontal: 16,
    paddingBottom: 8,
    gap: 6,
  },
  clearFilters: {
    alignSelf: "flex-start",
    paddingVertical: 4,
  },
  clearFiltersText: {
    color: colors.blue,
    fontSize: 12,
    fontWeight: "600",
  },
  filterLabel: { color: colors.textMuted, fontSize: 11, marginTop: 2 },
  chipScroll: { flexGrow: 0 },
  chip: {
    backgroundColor: colors.line,
    borderRadius: 12,
    paddingHorizontal: 10,
    paddingVertical: 4,
    marginRight: 6,
  },
  chipSelected: {
    backgroundColor: colors.blue,
  },
  chipText: { color: colors.text, fontSize: 12 },
  chipTextSelected: { color: "#fff", fontWeight: "600" },
});
