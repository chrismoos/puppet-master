import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import {
  ActivityIndicator,
  FlatList,
  Keyboard,
  Pressable,
  ScrollView,
  StyleSheet,
  Text,
  TextInput,
  View,
  type ListRenderItemInfo,
} from "react-native";

import {
  KeyboardAwareFlatList,
  KeyboardAwareScrollView,
} from "../components/KeyboardAwareScrollables";
import { useFooterClearance } from "../components/useFooterClearance";

import type { PmClient } from "@puppet-master/client-core/ws/client";
import type { KeyValueStorage } from "@puppet-master/client-core/platform";
import {
  readFocusedDecision,
  resolveFocusedDecision,
  writeFocusedDecision,
} from "@puppet-master/client-core/state/planFocus";

import type { ControllerConfig } from "../config";
import type { DeviceAuthSession } from "../auth/session";
import { colors } from "../theme";
import {
  fetchPlan,
  postPlanMessage,
  savePlanDraft,
  submitPlanDecisions,
  type PlanDecision,
  type PlanDetail,
  type PlanMessage,
} from "../api/plans";
import { nextPlanSelection } from "../planning/selectionHelpers";
import {
  buildDecisionDraftInput,
  buildDecisionInput,
  initialDraft,
  isDecisionComplete,
  reconcileSelection,
  validateDecision,
  type DecisionDraftState,
} from "../planning/decisionHelpers";
import { PlanMarkdown } from "../planning/PlanMarkdown";

type DecisionDraft = DecisionDraftState;

interface BatchEntry {
  id: number;
  title: string;
  complete: boolean;
}

function activeDecisionIds(plan: PlanDetail["plan"]): number[] {
  if (plan.activeDecisionIds?.length) return plan.activeDecisionIds;
  return plan.activeDecisionId != null ? [plan.activeDecisionId] : [];
}

type PlanTab = "decision" | "dialogue" | "plan";

export interface PlanTarget {
  planId: string;
  sessionId: bigint;
  planName: string;
}

function planStateLabel(state: string): string {
  switch (state) {
    case "active": return "Active";
    case "accepted": return "Accepted";
    case "archived": return "Archived";
    default: return state;
  }
}

function planStateColor(state: string): string {
  switch (state) {
    case "active": return colors.blue;
    case "accepted": return colors.green;
    case "archived": return colors.textMuted;
    default: return colors.textMuted;
  }
}

function decisionStateLabel(state: string): string {
  switch (state) {
    case "open": return "Awaiting response";
    case "waiting": return "Waiting for agent";
    case "resolved": return "Resolved";
    default: return state;
  }
}

export function PlanScreen({
  config,
  target,
  client,
  auth,
  storage,
  onBack,
}: {
  config: ControllerConfig;
  target: PlanTarget;
  client: PmClient;
  auth: DeviceAuthSession;
  storage: KeyValueStorage;
  onBack: () => void;
}) {
  const livePlan = useSyncExternalStore(client.subscribe, () =>
    client.getState().plans.get(target.planId),
  );
  const connPhase = useSyncExternalStore(client.subscribe, () => client.getState().conn);

  const [detail, setDetail] = useState<PlanDetail | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const revisionRef = useRef(0);
  const abortRef = useRef<AbortController | null>(null);
  const lastDecisionIdsRef = useRef<string>("");

  const [tab, setTab] = useState<PlanTab>("decision");

  const [drafts, setDrafts] = useState<Record<number, DecisionDraft>>({});
  const [focusedDecisionId, setFocusedDecisionId] = useState<number | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [submitted, setSubmitted] = useState(false);
  const [removedSelections, setRemovedSelections] = useState<string[]>([]);

  const [draftMessage, setDraftMessage] = useState("");
  const [sending, setSending] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const [messages, setMessages] = useState<PlanMessage[]>([]);
  const [unreadCount, setUnreadCount] = useState(0);
  const lastSeenMessageId = useRef(0);

  const planScrollRef = useRef<ScrollView>(null);
  const planScrollOffset = useRef(0);
  const saveDraftTimersRef = useRef<Map<number, ReturnType<typeof setTimeout>>>(new Map());

  useEffect(() => {
    return () => {
      for (const timer of saveDraftTimersRef.current.values()) {
        clearTimeout(timer);
      }
      saveDraftTimersRef.current.clear();
    };
  }, []);

  const resetBatchState = useCallback((d: PlanDetail) => {
    const ids = activeDecisionIds(d.plan);
    const newDrafts: Record<number, DecisionDraft> = {};
    let allSubmitted = true;
    for (const id of ids) {
      const dec = d.decisions.find((x) => x.id === id);
      if (dec) {
        newDrafts[id] = initialDraft(dec);
        if (!dec.response) allSubmitted = false;
      }
    }
    setDrafts(newDrafts);
    setFocusedDecisionId(resolveFocusedDecision(ids, null, readFocusedDecision(storage, target.planId)));
    setSubmitting(false);
    setSubmitError(null);
    setSubmitted(allSubmitted);
    setRemovedSelections([]);
  }, [storage, target.planId]);

  const focusDecision = useCallback((decisionId: number) => {
    setFocusedDecisionId(decisionId);
    writeFocusedDecision(storage, target.planId, decisionId);
    setSubmitError(null);
    setRemovedSelections([]);
  }, [storage, target.planId]);

  const loadPlan = useCallback(
    async (signal?: AbortSignal) => {
      try {
        setError(null);
        const d = await fetchPlan(auth, config.baseUrl, target.planId, signal);
        if (signal?.aborted) return;
        revisionRef.current = d.plan.revision;
        setDetail(d);
        setMessages(d.messages);
        setLoading(false);

        const idsKey = activeDecisionIds(d.plan).join(",");
        if (idsKey !== lastDecisionIdsRef.current) {
          lastDecisionIdsRef.current = idsKey;
          resetBatchState(d);
          if (idsKey) setTab("decision");
        }

        if (d.messages.length > 0) {
          const maxId = Math.max(...d.messages.map((m) => m.id));
          if (lastSeenMessageId.current === 0) lastSeenMessageId.current = maxId;
        }
      } catch (err) {
        if (signal?.aborted) return;
        setError(err instanceof Error ? err.message : String(err));
        setLoading(false);
      }
    },
    [auth, config.baseUrl, target.planId, resetBatchState],
  );

  useEffect(() => {
    const ac = new AbortController();
    abortRef.current = ac;
    void loadPlan(ac.signal);
    return () => ac.abort();
  }, [loadPlan]);

  useEffect(() => {
    if (!livePlan || loading) return;
    const liveRevision = Number(livePlan.revision);
    if (liveRevision > revisionRef.current) {
      const ac = new AbortController();
      abortRef.current?.abort();
      abortRef.current = ac;
      void loadPlan(ac.signal);
    }
  }, [livePlan, loading, loadPlan]);

  useEffect(() => {
    if (!livePlan || loading) return;
    const liveIds = (livePlan as { activeDecisionIds?: bigint[] }).activeDecisionIds;
    const liveIdsKey = liveIds?.length
      ? liveIds.map((id) => Number(id)).join(",")
      : livePlan.activeDecisionId !== undefined
        ? String(Number(livePlan.activeDecisionId))
        : "";
    const currentIdsKey = detail ? activeDecisionIds(detail.plan).join(",") : "";
    if (liveIdsKey !== currentIdsKey) {
      const ac = new AbortController();
      abortRef.current?.abort();
      abortRef.current = ac;
      void loadPlan(ac.signal);
    }
  }, [livePlan, loading, loadPlan, detail]);

  useEffect(() => {
    if (!detail || submitted) return;
    const ids = activeDecisionIds(detail.plan);
    for (const id of ids) {
      const dec = detail.decisions.find((d) => d.id === id);
      const draft = drafts[id];
      if (!dec || !draft) continue;
      const { kept, removed } = reconcileSelection(draft.selectedKeys, dec.options);
      if (removed.length > 0) {
        setDrafts((prev) => ({ ...prev, [id]: { ...prev[id], selectedKeys: kept } }));
        if (id === focusedDecisionId) setRemovedSelections(removed);
      }
    }
  }, [detail?.decisions]);

  useEffect(() => {
    if (tab === "dialogue" && messages.length > 0) {
      lastSeenMessageId.current = Math.max(...messages.map((m) => m.id));
      setUnreadCount(0);
    } else if (messages.length > 0) {
      const unseen = messages.filter((m) => m.id > lastSeenMessageId.current);
      setUnreadCount(unseen.length);
    }
  }, [tab, messages]);

  const activeDecisions = useMemo(() => {
    if (!detail) return [];
    const ids = activeDecisionIds(detail.plan);
    return ids
      .map((id) => detail.decisions.find((d) => d.id === id))
      .filter((d): d is PlanDecision => Boolean(d));
  }, [detail]);

  const activeDecision = activeDecisions.find((d) => d.id === focusedDecisionId)
    ?? activeDecisions[0]
    ?? null;

  const currentDraft = activeDecision ? drafts[activeDecision.id] ?? initialDraft(activeDecision) : null;
  const focusedIndex = activeDecisions.findIndex((d) => d.id === activeDecision?.id);
  const batchSize = activeDecisions.length;

  const isReadOnly =
    detail?.plan.state === "accepted" || detail?.plan.state === "archived";
  const isOffline = connPhase !== "online";
  const isDialogueOnly = activeDecision?.mode === "dialogue";
  const isWaiting = submitted || activeDecision?.state === "waiting";

  const batch: BatchEntry[] = activeDecisions.map((dec) => {
    const d = drafts[dec.id] ?? initialDraft(dec);
    return {
      id: dec.id,
      title: dec.title,
      complete: isDecisionComplete(dec, d.selectedKeys, d.customActive, d.customText),
    };
  });
  const allComplete = batch.every((entry) => entry.complete);


  const persistDraft = useCallback(
    (decisionId: number, draftState: DecisionDraft, delayMs: number) => {
      const dec = activeDecisions.find((d) => d.id === decisionId);
      if (!dec || isWaiting || isReadOnly) return;

      const existing = saveDraftTimersRef.current.get(decisionId);
      if (existing) {
        clearTimeout(existing);
        saveDraftTimersRef.current.delete(decisionId);
      }

      const payload = buildDecisionDraftInput(
        dec.mode,
        draftState.selectedKeys,
        draftState.customActive,
        draftState.customText,
        draftState.optionNotes,
      );

      if (delayMs === 0) {
        savePlanDraft(auth, config.baseUrl, target.planId, decisionId, payload).catch(() => {});
        return;
      }

      const timer = setTimeout(() => {
        saveDraftTimersRef.current.delete(decisionId);
        savePlanDraft(auth, config.baseUrl, target.planId, decisionId, payload).catch(() => {});
      }, delayMs);
      saveDraftTimersRef.current.set(decisionId, timer);
    },
    [activeDecisions, isWaiting, isReadOnly, auth, config.baseUrl, target.planId],
  );

  const updateDraft = useCallback(
    (decisionId: number, update: Partial<DecisionDraft>, immediate = false, persist = true) => {
      setDrafts((prev) => {
        const base = prev[decisionId] ?? initialDraft(activeDecisions.find((d) => d.id === decisionId)!);
        const updated = { ...base, ...update };
        if (persist) {
          persistDraft(decisionId, updated, immediate ? 0 : 400);
        }
        return {
          ...prev,
          [decisionId]: updated,
        };
      });
    },
    [activeDecisions, persistDraft],
  );

  const handleOptionToggle = useCallback(
    (key: string) => {
      if (!activeDecision || !currentDraft || isWaiting || isReadOnly) return;
      updateDraft(activeDecision.id, {
        selectedKeys: nextPlanSelection(activeDecision.mode, currentDraft.selectedKeys, key),
        expandedOptionKey: key,
        customActive: activeDecision.mode === "single" ? false : currentDraft.customActive,
      }, true);
      setSubmitError(null);
      setRemovedSelections([]);
    },
    [activeDecision, currentDraft, isWaiting, isReadOnly, updateDraft],
  );

  const handleCustomToggle = useCallback(() => {
    if (!activeDecision || !currentDraft || isWaiting || isReadOnly) return;
    const nextCustom = !currentDraft.customActive;
    updateDraft(activeDecision.id, {
      customActive: nextCustom,
      selectedKeys: nextCustom && activeDecision.mode === "single" ? [] : currentDraft.selectedKeys,
    }, true);
    setSubmitError(null);
  }, [activeDecision, currentDraft, isWaiting, isReadOnly, updateDraft]);

  const handleNoteChange = useCallback((key: string, text: string) => {
    if (!activeDecision || !currentDraft) return;
    updateDraft(activeDecision.id, {
      optionNotes: { ...currentDraft.optionNotes, [key]: text },
    }, false);
  }, [activeDecision, currentDraft, updateDraft]);

  const handlePrevious = useCallback(() => {
    if (focusedIndex > 0) focusDecision(activeDecisions[focusedIndex - 1].id);
  }, [focusedIndex, activeDecisions, focusDecision]);

  const handleNext = useCallback(() => {
    if (focusedIndex < batchSize - 1) focusDecision(activeDecisions[focusedIndex + 1].id);
  }, [focusedIndex, batchSize, activeDecisions, focusDecision]);

  const handleJump = useCallback((index: number) => {
    const dec = activeDecisions[index];
    if (dec && dec.id !== activeDecision?.id) focusDecision(dec.id);
  }, [activeDecisions, activeDecision, focusDecision]);

  const handleSubmit = useCallback(async () => {
    if (!activeDecisions.length || isWaiting || isReadOnly) return;

    for (const timer of saveDraftTimersRef.current.values()) {
      clearTimeout(timer);
    }
    saveDraftTimersRef.current.clear();

    for (const dec of activeDecisions) {
      const d = drafts[dec.id] ?? initialDraft(dec);
      const validationError = validateDecision(dec.mode, d.selectedKeys, d.customActive, d.customText, dec.requireSelection);
      if (validationError) {
        focusDecision(dec.id);
        setSubmitError(validationError);
        return;
      }
    }

    setSubmitting(true);
    setSubmitError(null);

    const responses = activeDecisions.map((dec) => {
      const d = drafts[dec.id] ?? initialDraft(dec);
      return {
        decisionId: dec.id,
        response: buildDecisionInput(dec.mode, d.selectedKeys, d.customActive, d.customText, d.optionNotes),
      };
    });

    setSubmitted(true);

    try {
      await submitPlanDecisions(auth, config.baseUrl, target.planId, responses);
    } catch (err) {
      setSubmitted(false);
      setSubmitError(err instanceof Error ? err.message : String(err));
    } finally {
      setSubmitting(false);
    }
  }, [activeDecisions, isWaiting, isReadOnly, drafts, auth, config.baseUrl, target.planId, focusDecision]);


  const handleSendMessage = useCallback(async () => {
    if (!draftMessage.trim() || sending) return;
    const text = draftMessage.trim();
    setSending(true);
    setSendError(null);
    try {
      const msg = await postPlanMessage(
        auth,
        config.baseUrl,
        target.planId,
        activeDecision?.id ?? null,
        text,
      );
      setMessages((prev) => [...prev, msg]);
      setDraftMessage("");
      Keyboard.dismiss();
    } catch (err) {
      setSendError(err instanceof Error ? err.message : String(err));
    } finally {
      setSending(false);
    }
  }, [draftMessage, sending, auth, config.baseUrl, target.planId, activeDecision]);


  if (loading) {
    return (
      <View style={styles.root}>
        <Header
          planName={target.planName}
          state={null}
          onBack={onBack}
          revision={null}
          markdownPath={null}
          onPathPress={() => setTab("plan")}
        />
        <View style={styles.center}>
          <ActivityIndicator color={colors.blue} />
          <Text style={styles.loadingText}>Loading plan…</Text>
        </View>
      </View>
    );
  }

  if (error) {
    return (
      <View style={styles.root}>
        <Header
          planName={target.planName}
          state={null}
          onBack={onBack}
          revision={null}
          markdownPath={null}
          onPathPress={() => setTab("plan")}
        />
        <View style={styles.center}>
          <Text style={styles.errorText}>{error}</Text>
          <Pressable style={styles.retryButton} onPress={() => { setLoading(true); void loadPlan(); }}>
            <Text style={styles.retryText}>Retry</Text>
          </Pressable>
        </View>
      </View>
    );
  }

  if (!detail) return null;

  return (
    <View style={styles.root}>
      <Header
        planName={detail.plan.name}
        state={detail.plan.state}
        onBack={onBack}
        revision={detail.plan.revision}
        markdownPath={detail.plan.markdownPath}
        onPathPress={() => setTab("plan")}
      />

      {isOffline && (
        <View style={styles.offlineBanner}>
          <Text style={styles.offlineText}>Offline — read-only</Text>
        </View>
      )}

      {isReadOnly && (
        <View style={styles.readOnlyBanner}>
          <Text style={styles.readOnlyText}>
            {detail.plan.state === "accepted" ? "Plan accepted" : "Plan archived"} — read-only
          </Text>
        </View>
      )}

      <PlanTabBar
        active={tab}
        onSelect={setTab}
        unreadCount={unreadCount}
        hasDecision={activeDecision !== null}
      />

      {tab === "decision" && (
        <DecisionTab
          decision={activeDecision}
          selectedKeys={currentDraft?.selectedKeys ?? []}
          customActive={currentDraft?.customActive ?? false}
          customText={currentDraft?.customText ?? ""}
          optionNotes={currentDraft?.optionNotes ?? {}}
          expandedOptionKey={currentDraft?.expandedOptionKey ?? null}
          isWaiting={isWaiting}
          isReadOnly={isReadOnly || isOffline}
          isDialogueOnly={isDialogueOnly}
          submitting={submitting}
          submitError={submitError}
          removedSelections={removedSelections}
          batch={batch}
          batchSize={batchSize}
          batchIndex={focusedIndex}
          batchComplete={allComplete}
          onOptionToggle={handleOptionToggle}
          onCustomToggle={handleCustomToggle}
          onCustomTextChange={(text) => {
            if (!activeDecision) return;
            const autoActivate = text.trim().length > 0;
            updateDraft(activeDecision.id, {
              customText: text,
              customActive: autoActivate ? true : (currentDraft?.customActive ?? false),
              selectedKeys: autoActivate && activeDecision.mode === "single" ? [] : (currentDraft?.selectedKeys ?? []),
            }, false);
          }}
          onNoteChange={handleNoteChange}
          onExpandOption={(key) => activeDecision && updateDraft(activeDecision.id, { expandedOptionKey: key }, false, false)}
          onPrevious={handlePrevious}
          onNext={handleNext}
          onJump={handleJump}
          onSubmit={handleSubmit}
        />
      )}

      {tab === "dialogue" && (
        <DialogueTab
          messages={messages}
          activeDecisionId={activeDecision?.id ?? null}
          draftMessage={draftMessage}
          sending={sending}
          sendError={sendError}
          isReadOnly={isReadOnly || isOffline}
          onDraftChange={setDraftMessage}
          onSend={handleSendMessage}
        />
      )}

      {tab === "plan" && (
        <PlanTab
          markdown={detail.markdown}
          isStale={isOffline}
          scrollRef={planScrollRef}
          scrollOffset={planScrollOffset}
        />
      )}
    </View>
  );
}


function Header({
  planName,
  state,
  onBack,
  revision,
  markdownPath,
  onPathPress,
}: {
  planName: string;
  state: string | null;
  onBack: () => void;
  revision: number | null;
  markdownPath: string | null;
  onPathPress: () => void;
}) {
  return (
    <View style={styles.header}>
      <Pressable
        onPress={onBack}
        style={styles.backTouch}
        accessibilityLabel="Back"
        accessibilityRole="button"
      >
        <View style={styles.backChevron} />
      </Pressable>
      <View style={styles.headerTextCol}>
        <Text style={styles.headerTitle} numberOfLines={1}>
          {planName}
        </Text>
        {state && (
          <View style={styles.headerMeta}>
            <Text style={[styles.headerState, { color: planStateColor(state) }]}>
              {planStateLabel(state)}
            </Text>
            {revision !== null && (
              <Pressable onPress={onPathPress} hitSlop={8}>
                <Text style={styles.headerRevision}>
                  r{revision}{markdownPath ? ` · ${markdownPath}` : ""}
                </Text>
              </Pressable>
            )}
          </View>
        )}
      </View>
    </View>
  );
}


function PlanTabBar({
  active,
  onSelect,
  unreadCount,
  hasDecision,
}: {
  active: PlanTab;
  onSelect: (tab: PlanTab) => void;
  unreadCount: number;
  hasDecision: boolean;
}) {
  const tabs: { name: PlanTab; label: string }[] = [
    { name: "decision", label: "Decision" },
    { name: "dialogue", label: "Dialogue" },
    { name: "plan", label: "Plan" },
  ];

  return (
    <View
      style={tabStyles.row}
      accessibilityRole="tablist"
    >
      {tabs.map((t) => (
        <Pressable
          key={t.name}
          style={tabStyles.tab}
          onPress={() => onSelect(t.name)}
          accessibilityRole="tab"
          accessibilityState={{ selected: active === t.name }}
          accessibilityLabel={
            t.name === "dialogue" && unreadCount > 0
              ? `${t.label}, ${unreadCount} unread`
              : t.label
          }
        >
          <View style={tabStyles.labelRow}>
            <Text
              style={[
                tabStyles.label,
                active === t.name && tabStyles.labelActive,
                t.name === "decision" && hasDecision && active !== t.name && tabStyles.labelAttention,
              ]}
            >
              {t.label}
            </Text>
            {t.name === "dialogue" && unreadCount > 0 && (
              <View style={tabStyles.badge}>
                <Text style={tabStyles.badgeText}>{unreadCount}</Text>
              </View>
            )}
          </View>
          {active === t.name && <View style={tabStyles.indicator} />}
        </Pressable>
      ))}
    </View>
  );
}

const tabStyles = StyleSheet.create({
  row: {
    flexDirection: "row",
    borderBottomWidth: StyleSheet.hairlineWidth,
    borderBottomColor: colors.line,
  },
  tab: {
    flex: 1,
    alignItems: "center",
    paddingVertical: 10,
  },
  labelRow: {
    flexDirection: "row",
    alignItems: "center",
    gap: 6,
  },
  label: { color: colors.textMuted, fontSize: 14 },
  labelActive: { color: colors.textBright, fontWeight: "600" },
  labelAttention: { color: colors.amber },
  indicator: {
    position: "absolute",
    bottom: 0,
    height: 2,
    width: "60%",
    backgroundColor: colors.blue,
    borderRadius: 1,
  },
  badge: {
    backgroundColor: colors.blue,
    borderRadius: 8,
    minWidth: 16,
    height: 16,
    alignItems: "center",
    justifyContent: "center",
    paddingHorizontal: 4,
  },
  badgeText: { color: "#fff", fontSize: 10, fontWeight: "700" },
});


function DecisionTab({
  decision,
  selectedKeys,
  customActive,
  customText,
  optionNotes,
  expandedOptionKey,
  isWaiting,
  isReadOnly,
  isDialogueOnly,
  submitting,
  submitError,
  removedSelections,
  batch,
  batchSize,
  batchIndex,
  batchComplete,
  onOptionToggle,
  onCustomToggle,
  onCustomTextChange,
  onNoteChange,
  onExpandOption,
  onPrevious,
  onNext,
  onJump,
  onSubmit,
}: {
  decision: PlanDecision | null;
  selectedKeys: readonly string[];
  customActive: boolean;
  customText: string;
  optionNotes: Record<string, string>;
  expandedOptionKey: string | null;
  isWaiting: boolean;
  isReadOnly: boolean;
  isDialogueOnly: boolean | undefined;
  submitting: boolean;
  submitError: string | null;
  removedSelections: string[];
  batch: BatchEntry[];
  batchSize: number;
  batchIndex: number;
  batchComplete: boolean;
  onOptionToggle: (key: string) => void;
  onCustomToggle: () => void;
  onCustomTextChange: (text: string) => void;
  onNoteChange: (key: string, text: string) => void;
  onExpandOption: (key: string | null) => void;
  onPrevious: () => void;
  onNext: () => void;
  onJump: (index: number) => void;
  onSubmit: () => void;
}) {
  if (!decision) {
    return (
      <View style={styles.center}>
        <Text style={styles.emptyText}>No active decision.</Text>
      </View>
    );
  }

  const isMulti = decision.mode === "multiple";
  const isSingle = decision.mode === "single";
  const { onFooterLayout, scrollPaddingBottom } = useFooterClearance();

  return (
    <View style={styles.flex}>
      <KeyboardAwareScrollView
        style={styles.flex}
        contentContainerStyle={decisionStyles.scrollContent}
        contentInset={{ bottom: scrollPaddingBottom }}
        scrollIndicatorInsets={{ bottom: scrollPaddingBottom }}
      >
        {batchSize > 1 && (
          <View style={decisionStyles.batchNav}>
            <Text style={decisionStyles.batchEyebrow}>
              Decision {batchIndex + 1} of {batchSize}
            </Text>
            <View
              style={decisionStyles.batchSteps}
              accessibilityRole="tablist"
              accessibilityLabel="Decisions in this batch"
            >
              {batch.map((entry, index) => {
                const isCurrent = index === batchIndex;
                return (
                  <Pressable
                    key={entry.id}
                    style={[
                      decisionStyles.batchStep,
                      entry.complete && decisionStyles.batchStepComplete,
                      isCurrent && decisionStyles.batchStepCurrent,
                    ]}
                    onPress={() => onJump(index)}
                    accessibilityRole="tab"
                    accessibilityState={{ selected: isCurrent }}
                    accessibilityLabel={`Decision ${index + 1}: ${entry.title}${entry.complete ? ", answered" : ""}`}
                    hitSlop={6}
                  >
                    <Text
                      style={[
                        decisionStyles.batchStepText,
                        entry.complete && decisionStyles.batchStepTextComplete,
                        isCurrent && decisionStyles.batchStepTextCurrent,
                      ]}
                    >
                      {index + 1}
                    </Text>
                  </Pressable>
                );
              })}
            </View>
          </View>
        )}
        <Text
          style={decisionStyles.title}
          accessibilityRole="header"
        >
          {decision.title}
        </Text>

        {decision.promptMarkdown ? (
          <PlanMarkdown
            markdown={decision.promptMarkdown}
            style={decisionStyles.prompt}
          />
        ) : null}

        {decision.state !== "open" && (
          <View style={decisionStyles.stateBanner}>
            <Text style={decisionStyles.stateText}>
              {decisionStateLabel(decision.state)}
            </Text>
          </View>
        )}

        {removedSelections.length > 0 && (
          <View style={decisionStyles.warningBanner}>
            <Text style={decisionStyles.warningText}>
              {removedSelections.length} selected option{removedSelections.length > 1 ? "s were" : " was"} removed. Please review your selection.
            </Text>
          </View>
        )}

        {/* Option list (not shown for dialogue-only) */}
        {!isDialogueOnly && decision.options.map((option) => {
          const selected = selectedKeys.includes(option.key);
          const isExpanded = expandedOptionKey === option.key;
          const note = optionNotes[option.key] ?? "";

          return (
            <View key={option.key} style={decisionStyles.optionContainer}>
              <Pressable
                style={[
                  decisionStyles.option,
                  selected && decisionStyles.optionSelected,
                  isWaiting && decisionStyles.optionDisabled,
                ]}
                onPress={() => onOptionToggle(option.key)}
                disabled={isWaiting || isReadOnly}
                accessibilityRole={isMulti ? "checkbox" : "radio"}
                accessibilityState={{
                  checked: selected,
                  disabled: isWaiting || isReadOnly,
                }}
                accessibilityLabel={option.label}
              >
                <View style={decisionStyles.optionIndicator}>
                  {isMulti ? (
                    <View
                      style={[
                        decisionStyles.checkbox,
                        selected && decisionStyles.checkboxSelected,
                      ]}
                    >
                      {selected && <Text style={decisionStyles.checkmark}>✓</Text>}
                    </View>
                  ) : (
                    <View
                      style={[
                        decisionStyles.radio,
                        selected && decisionStyles.radioSelected,
                      ]}
                    >
                      {selected && <View style={decisionStyles.radioDot} />}
                    </View>
                  )}
                </View>
                <Text
                  style={[
                    decisionStyles.optionLabel,
                    selected && decisionStyles.optionLabelSelected,
                  ]}
                >
                  {option.label}
                </Text>
                {isSingle && option.recommended ? (
                  <View style={decisionStyles.recommendedBadge}>
                    <Text style={decisionStyles.recommendedBadgeText}>
                      Recommended
                    </Text>
                  </View>
                ) : null}
              </Pressable>

              {/* Expanded detail */}
              {selected && option.detailMarkdown ? (
                <View style={decisionStyles.detailContainer}>
                  <PlanMarkdown
                    markdown={option.detailMarkdown}
                    style={decisionStyles.detailMarkdown}
                  />
                </View>
              ) : null}

              {/* Per-option note */}
              {selected && !isWaiting && !isReadOnly && (
                <Pressable
                  style={decisionStyles.noteToggle}
                  onPress={() =>
                    onExpandOption(isExpanded ? null : option.key)
                  }
                >
                  <Text style={decisionStyles.noteToggleText}>
                    {note ? "Edit note" : "Add note"}
                  </Text>
                </Pressable>
              )}
              {selected && isExpanded && !isWaiting && !isReadOnly && (
                <TextInput
                  style={decisionStyles.noteInput}
                  value={note}
                  onChangeText={(text) => onNoteChange(option.key, text)}
                  placeholder="Add a note for this option…"
                  placeholderTextColor={colors.textMuted}
                  multiline
                  textAlignVertical="top"
                  accessibilityLabel={`Note for ${option.label}`}
                />
              )}
            </View>
          );
        })}

        {/* Custom option */}
        {!isDialogueOnly && decision.allowCustom && (
          <View style={decisionStyles.customContainer}>
            <Pressable
              style={[
                decisionStyles.option,
                customActive && decisionStyles.optionSelected,
                isWaiting && decisionStyles.optionDisabled,
              ]}
              onPress={onCustomToggle}
              disabled={isWaiting || isReadOnly}
              accessibilityRole={isSingle ? "radio" : "checkbox"}
              accessibilityState={{
                checked: customActive,
                disabled: isWaiting || isReadOnly,
              }}
              accessibilityLabel="Add your own option"
            >
              <View style={decisionStyles.optionIndicator}>
                {isMulti ? (
                  <View
                    style={[
                      decisionStyles.checkbox,
                      customActive && decisionStyles.checkboxSelected,
                    ]}
                  >
                    {customActive && <Text style={decisionStyles.checkmark}>✓</Text>}
                  </View>
                ) : (
                  <View
                    style={[
                      decisionStyles.radio,
                      customActive && decisionStyles.radioSelected,
                    ]}
                  >
                    {customActive && <View style={decisionStyles.radioDot} />}
                  </View>
                )}
              </View>
              <Text
                style={[
                  decisionStyles.optionLabel,
                  customActive && decisionStyles.optionLabelSelected,
                ]}
              >
                Add your own option
              </Text>
            </Pressable>
            {customActive && !isWaiting && !isReadOnly && (
              <TextInput
                style={decisionStyles.customInput}
                value={customText}
                onChangeText={onCustomTextChange}
                placeholder="Describe your option…"
                placeholderTextColor={colors.textMuted}
                multiline
                textAlignVertical="top"
                accessibilityLabel="Custom option text"
              />
            )}
          </View>
        )}

        {/* Dialogue-only prompt */}
        {isDialogueOnly && (
          <View style={decisionStyles.dialoguePrompt}>
            <Text style={decisionStyles.dialoguePromptText}>
              This decision uses dialogue — switch to the Dialogue tab to respond.
            </Text>
          </View>
        )}

        {/* Validation / submit error */}
        {submitError && (
          <Text style={decisionStyles.errorText}>{submitError}</Text>
        )}

        {/* Resolution (after resolved) */}
        {decision.resolutionMarkdown ? (
          <View style={decisionStyles.resolution}>
            <Text style={decisionStyles.resolutionTitle}>Resolution</Text>
            <PlanMarkdown
              markdown={decision.resolutionMarkdown}
              style={decisionStyles.resolutionMarkdown}
            />
          </View>
        ) : null}
      </KeyboardAwareScrollView>

      {/* Sticky submit / waiting bar */}
      {!isReadOnly && !isDialogueOnly && decision.state === "open" && (
        <View style={decisionStyles.submitBar} onLayout={onFooterLayout}>
          {isWaiting ? (
            <View style={decisionStyles.waitingContainer}>
              <ActivityIndicator color={colors.textMuted} size="small" />
              <Text style={decisionStyles.waitingText}>Waiting for agent…</Text>
            </View>
          ) : (
            <>
              {batchSize > 1 && (
                <Text style={decisionStyles.batchHint}>
                  Nothing is sent until you submit the whole batch.
                </Text>
              )}
              <View style={decisionStyles.batchActions}>
                {batchSize > 1 && batchIndex > 0 && (
                  <Pressable
                    style={decisionStyles.batchNavButton}
                    onPress={onPrevious}
                    accessibilityRole="button"
                    accessibilityLabel="Previous decision"
                  >
                    <Text style={decisionStyles.batchNavText}>Back</Text>
                  </Pressable>
                )}
                {batchSize > 1 && batchIndex < batchSize - 1 ? (
                  <Pressable
                    style={[decisionStyles.submitButton, { flex: 1 }]}
                    onPress={onNext}
                    accessibilityRole="button"
                    accessibilityLabel="Next decision"
                  >
                    <Text style={decisionStyles.submitText}>NEXT</Text>
                  </Pressable>
                ) : (
                  <Pressable
                    style={[
                      decisionStyles.submitButton,
                      { flex: 1 },
                      (submitting || !batchComplete) && decisionStyles.submitButtonDisabled,
                    ]}
                    onPress={onSubmit}
                    disabled={submitting || !batchComplete}
                    accessibilityRole="button"
                    accessibilityLabel={batchSize > 1 ? "Submit all decisions" : "Submit decision"}
                  >
                    {submitting ? (
                      <ActivityIndicator color="#fff" size="small" />
                    ) : (
                      <Text style={decisionStyles.submitText}>
                        {batchSize > 1 ? "SUBMIT ALL" : "SUBMIT DECISION"}
                      </Text>
                    )}
                  </Pressable>
                )}
              </View>
            </>
          )}
        </View>
      )}
    </View>
  );
}

const decisionStyles = StyleSheet.create({
  scrollContent: { padding: 16 },
  batchNav: {
    marginBottom: 4,
  },
  batchEyebrow: {
    color: colors.blue,
    fontSize: 12,
    fontWeight: "600",
    marginBottom: 6,
    letterSpacing: 0.3,
  },
  batchSteps: {
    flexDirection: "row",
    flexWrap: "wrap",
    gap: 6,
    marginBottom: 8,
  },
  batchStep: {
    minWidth: 30,
    height: 30,
    paddingHorizontal: 8,
    borderRadius: 15,
    borderWidth: 1,
    borderColor: colors.line,
    backgroundColor: colors.panelAlt,
    alignItems: "center",
    justifyContent: "center",
  },
  batchStepComplete: {
    borderColor: colors.green,
    backgroundColor: "rgba(61, 214, 140, 0.12)",
  },
  batchStepCurrent: {
    borderColor: colors.blue,
    backgroundColor: "rgba(77, 163, 255, 0.16)",
  },
  batchStepText: {
    color: colors.textMuted,
    fontSize: 13,
    fontWeight: "700",
  },
  batchStepTextComplete: {
    color: colors.green,
  },
  batchStepTextCurrent: {
    color: colors.textBright,
  },
  title: {
    color: colors.textBright,
    fontSize: 18,
    fontWeight: "700",
    marginBottom: 8,
  },
  prompt: { marginBottom: 16 },
  stateBanner: {
    backgroundColor: colors.panelAlt,
    borderRadius: 8,
    padding: 10,
    marginBottom: 12,
  },
  stateText: { color: colors.amber, fontSize: 13, fontWeight: "600" },
  warningBanner: {
    backgroundColor: "rgba(255, 178, 36, 0.12)",
    borderRadius: 8,
    padding: 10,
    marginBottom: 12,
  },
  warningText: { color: colors.amber, fontSize: 13 },
  optionContainer: { marginBottom: 4 },
  option: {
    flexDirection: "row",
    alignItems: "center",
    padding: 14,
    borderRadius: 10,
    backgroundColor: colors.panel,
    borderWidth: 1,
    borderColor: colors.line,
    minHeight: 48,
  },
  optionSelected: {
    borderColor: colors.blue,
    backgroundColor: "rgba(77, 163, 255, 0.08)",
  },
  optionDisabled: { opacity: 0.6 },
  optionIndicator: { marginRight: 12 },
  radio: {
    width: 20,
    height: 20,
    borderRadius: 10,
    borderWidth: 2,
    borderColor: colors.textMuted,
    alignItems: "center",
    justifyContent: "center",
  },
  radioSelected: { borderColor: colors.blue },
  radioDot: {
    width: 10,
    height: 10,
    borderRadius: 5,
    backgroundColor: colors.blue,
  },
  checkbox: {
    width: 20,
    height: 20,
    borderRadius: 4,
    borderWidth: 2,
    borderColor: colors.textMuted,
    alignItems: "center",
    justifyContent: "center",
  },
  checkboxSelected: {
    borderColor: colors.blue,
    backgroundColor: colors.blue,
  },
  checkmark: { color: "#fff", fontSize: 13, fontWeight: "700", lineHeight: 16 },
  optionLabel: { color: colors.text, fontSize: 15, flex: 1 },
  optionLabelSelected: { color: colors.textBright, fontWeight: "600" },
  recommendedBadge: {
    paddingHorizontal: 7,
    paddingVertical: 2,
    borderRadius: 4,
    borderWidth: 1,
    borderColor: colors.blue,
    backgroundColor: "rgba(77, 163, 255, 0.12)",
    marginLeft: 8,
  },
  recommendedBadgeText: {
    color: colors.blue,
    fontSize: 10,
    fontWeight: "600",
    textTransform: "uppercase",
    letterSpacing: 0.5,
  },
  detailContainer: {
    paddingHorizontal: 16,
    paddingVertical: 8,
    marginLeft: 32,
    borderLeftWidth: 2,
    borderLeftColor: colors.blue,
    marginBottom: 4,
  },
  detailMarkdown: {},
  noteToggle: {
    marginLeft: 46,
    paddingVertical: 4,
    marginBottom: 4,
  },
  noteToggleText: { color: colors.blue, fontSize: 12 },
  noteInput: {
    marginLeft: 46,
    borderWidth: 1,
    borderColor: colors.line,
    borderRadius: 8,
    padding: 10,
    color: colors.text,
    fontSize: 14,
    minHeight: 60,
    backgroundColor: colors.panel,
    marginBottom: 8,
  },
  customContainer: { marginTop: 4, marginBottom: 4 },
  customInput: {
    marginTop: 8,
    borderWidth: 1,
    borderColor: colors.line,
    borderRadius: 8,
    padding: 12,
    color: colors.text,
    fontSize: 14,
    minHeight: 80,
    backgroundColor: colors.panel,
    marginBottom: 8,
  },
  dialoguePrompt: {
    backgroundColor: colors.panelAlt,
    borderRadius: 8,
    padding: 16,
    marginTop: 8,
  },
  dialoguePromptText: { color: colors.textMuted, fontSize: 14 },
  errorText: { color: colors.red, fontSize: 13, marginTop: 8 },
  resolution: { marginTop: 16 },
  resolutionTitle: {
    color: colors.textBright,
    fontSize: 13,
    fontWeight: "600",
    marginBottom: 6,
  },
  resolutionMarkdown: {},
  submitBar: {
    paddingHorizontal: 16,
    paddingTop: 12,
    paddingBottom: 20,
    borderTopWidth: StyleSheet.hairlineWidth,
    borderTopColor: colors.line,
    backgroundColor: colors.bg,
  },
  submitButton: {
    backgroundColor: colors.blue,
    borderRadius: 10,
    paddingVertical: 14,
    alignItems: "center",
    justifyContent: "center",
    minHeight: 48,
  },
  submitButtonDisabled: { opacity: 0.6 },
  submitText: { color: "#fff", fontSize: 16, fontWeight: "700", letterSpacing: 0.5 },
  waitingContainer: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "center",
    gap: 10,
    paddingVertical: 14,
  },
  waitingText: { color: colors.textMuted, fontSize: 15 },
  batchHint: {
    color: colors.textMuted,
    fontSize: 12,
    textAlign: "center",
    marginBottom: 8,
  },
  batchActions: {
    flexDirection: "row",
    gap: 10,
    alignItems: "center",
  },
  batchNavButton: {
    borderRadius: 10,
    paddingVertical: 14,
    paddingHorizontal: 20,
    alignItems: "center",
    justifyContent: "center",
    backgroundColor: colors.panelAlt,
    minHeight: 48,
  },
  batchNavText: {
    color: colors.text,
    fontSize: 16,
    fontWeight: "600",
  },
});


function DialogueTab({
  messages,
  activeDecisionId,
  draftMessage,
  sending,
  sendError,
  isReadOnly,
  onDraftChange,
  onSend,
}: {
  messages: readonly PlanMessage[];
  activeDecisionId: number | null;
  draftMessage: string;
  sending: boolean;
  sendError: string | null;
  isReadOnly: boolean;
  onDraftChange: (text: string) => void;
  onSend: () => void;
}) {
  const flatListRef = useRef<FlatList>(null);

  const threadMessages = useMemo(() => {
    if (activeDecisionId !== null) {
      return messages.filter(
        (m) => m.decisionId === activeDecisionId || m.decisionId === null,
      );
    }
    return messages.filter((m) => m.decisionId === null);
  }, [messages, activeDecisionId]);

  useEffect(() => {
    if (threadMessages.length > 0) {
      setTimeout(() => flatListRef.current?.scrollToEnd({ animated: true }), 100);
    }
  }, [threadMessages.length]);

  const renderMessage = useCallback(
    ({ item }: ListRenderItemInfo<PlanMessage>) => {
      const isUser = item.author === "user";
      return (
        <View
          style={[
            dialogueStyles.message,
            isUser ? dialogueStyles.messageUser : dialogueStyles.messageAgent,
          ]}
          accessibilityLabel={`${isUser ? "You" : "Agent"}: ${item.body}`}
        >
          <Text style={dialogueStyles.messageAuthor}>
            {isUser ? "You" : "Agent"}
          </Text>
          <Text style={dialogueStyles.messageBody} selectable>
            {item.body}
          </Text>
          <Text style={dialogueStyles.messageTime}>
            {new Date(item.createdAtUnixMs).toLocaleTimeString(undefined, {
              hour: "2-digit",
              minute: "2-digit",
            })}
          </Text>
        </View>
      );
    },
    [],
  );

  const keyExtractor = useCallback((item: PlanMessage) => item.id.toString(), []);

  const { onFooterLayout: onComposerLayout, scrollPaddingBottom: composerClearance } = useFooterClearance();

  return (
    <View style={styles.flex}>
      <KeyboardAwareFlatList
        listRef={flatListRef}
        data={threadMessages}
        renderItem={renderMessage}
        keyExtractor={keyExtractor}
        contentContainerStyle={dialogueStyles.listContent}
        contentInset={{ bottom: composerClearance }}
        scrollIndicatorInsets={{ bottom: composerClearance }}
        ListEmptyComponent={
          <Text style={styles.emptyText}>No messages yet.</Text>
        }
      />
      {!isReadOnly && (
        <View style={dialogueStyles.composerBar} onLayout={onComposerLayout}>
          {sendError && (
            <Text style={dialogueStyles.sendError}>{sendError}</Text>
          )}
          <View style={dialogueStyles.composerRow}>
          <TextInput
            style={dialogueStyles.composerInput}
            value={draftMessage}
            onChangeText={onDraftChange}
            placeholder="Send a message…"
            placeholderTextColor={colors.textMuted}
            multiline
            maxLength={4000}
            accessibilityLabel="Message input"
          />
          <Pressable
            style={[
              dialogueStyles.sendButton,
              (!draftMessage.trim() || sending) && dialogueStyles.sendDisabled,
            ]}
            onPress={onSend}
            disabled={!draftMessage.trim() || sending}
            accessibilityRole="button"
            accessibilityLabel="Send message"
          >
            {sending ? (
              <ActivityIndicator color="#fff" size="small" />
            ) : (
              <Text style={dialogueStyles.sendText}>Send</Text>
            )}
          </Pressable>
          </View>
        </View>
      )}
    </View>
  );
}

const dialogueStyles = StyleSheet.create({
  listContent: { padding: 16 },
  message: {
    maxWidth: "85%",
    borderRadius: 12,
    padding: 12,
    marginBottom: 8,
  },
  messageUser: {
    alignSelf: "flex-end",
    backgroundColor: colors.blue,
  },
  messageAgent: {
    alignSelf: "flex-start",
    backgroundColor: colors.panelAlt,
  },
  messageAuthor: { color: "rgba(255,255,255,0.7)", fontSize: 11, fontWeight: "600", marginBottom: 2 },
  messageBody: { color: "#fff", fontSize: 14, lineHeight: 20 },
  messageTime: { color: "rgba(255,255,255,0.5)", fontSize: 10, marginTop: 4, alignSelf: "flex-end" },
  composerBar: {
    padding: 12,
    gap: 6,
    borderTopWidth: StyleSheet.hairlineWidth,
    borderTopColor: colors.line,
    backgroundColor: colors.bg,
  },
  composerRow: {
    flexDirection: "row",
    alignItems: "flex-end",
    gap: 8,
  },
  sendError: { color: colors.red, fontSize: 12 },
  composerInput: {
    flex: 1,
    borderWidth: 1,
    borderColor: colors.line,
    borderRadius: 18,
    paddingHorizontal: 14,
    paddingVertical: 10,
    color: colors.text,
    fontSize: 14,
    maxHeight: 100,
    backgroundColor: colors.panel,
  },
  sendButton: {
    backgroundColor: colors.blue,
    borderRadius: 18,
    paddingHorizontal: 16,
    paddingVertical: 10,
    alignItems: "center",
    justifyContent: "center",
    minWidth: 56,
    minHeight: 40,
  },
  sendDisabled: { opacity: 0.4 },
  sendText: { color: "#fff", fontSize: 14, fontWeight: "600" },
});


function PlanTab({
  markdown,
  isStale,
  scrollRef,
  scrollOffset,
}: {
  markdown: string;
  isStale: boolean;
  scrollRef: React.RefObject<ScrollView | null>;
  scrollOffset: React.MutableRefObject<number>;
}) {
  return (
    <ScrollView
      ref={scrollRef}
      style={styles.flex}
      contentContainerStyle={planStyles.content}
      onScroll={(e) => {
        scrollOffset.current = e.nativeEvent.contentOffset.y;
      }}
      scrollEventThrottle={100}
      contentOffset={{ x: 0, y: scrollOffset.current }}
    >
      {isStale && (
        <View style={planStyles.staleBanner}>
          <Text style={planStyles.staleText}>Cached — may be outdated</Text>
        </View>
      )}
      <PlanMarkdown markdown={markdown} />
    </ScrollView>
  );
}

const planStyles = StyleSheet.create({
  content: { padding: 16, paddingBottom: 40 },
  staleBanner: {
    backgroundColor: "rgba(255, 178, 36, 0.12)",
    borderRadius: 8,
    padding: 10,
    marginBottom: 12,
  },
  staleText: { color: colors.amber, fontSize: 12 },
});


const styles = StyleSheet.create({
  root: { flex: 1, backgroundColor: colors.bg },
  flex: { flex: 1 },
  center: { flex: 1, alignItems: "center", justifyContent: "center", padding: 16 },
  loadingText: { color: colors.textMuted, marginTop: 12, fontSize: 14 },
  errorText: { color: colors.red, fontSize: 14, textAlign: "center" },
  retryButton: {
    marginTop: 12,
    backgroundColor: colors.panelAlt,
    borderRadius: 8,
    paddingHorizontal: 20,
    paddingVertical: 10,
  },
  retryText: { color: colors.blue, fontSize: 14 },
  emptyText: { color: colors.textMuted, fontSize: 14, textAlign: "center", marginTop: 32 },
  header: {
    flexDirection: "row",
    alignItems: "center",
    gap: 10,
    paddingHorizontal: 16,
    paddingVertical: 10,
  },
  backTouch: {
    width: 44,
    height: 44,
    alignItems: "center",
    justifyContent: "center",
    marginLeft: -12,
  },
  backChevron: {
    width: 10,
    height: 10,
    borderLeftWidth: 2.5,
    borderBottomWidth: 2.5,
    borderColor: colors.blue,
    transform: [{ rotate: "45deg" }],
  },
  headerTextCol: { flex: 1 },
  headerTitle: { color: colors.textBright, fontSize: 16, fontWeight: "600" },
  headerMeta: { flexDirection: "row", alignItems: "center", gap: 8, marginTop: 2 },
  headerState: { fontSize: 12, fontWeight: "600" },
  headerRevision: { color: colors.textMuted, fontSize: 11 },
  offlineBanner: {
    backgroundColor: "rgba(110, 120, 137, 0.15)",
    paddingHorizontal: 16,
    paddingVertical: 6,
  },
  offlineText: { color: colors.textMuted, fontSize: 12 },
  readOnlyBanner: {
    backgroundColor: "rgba(61, 214, 140, 0.08)",
    paddingHorizontal: 16,
    paddingVertical: 6,
  },
  readOnlyText: { color: colors.green, fontSize: 12 },
});
