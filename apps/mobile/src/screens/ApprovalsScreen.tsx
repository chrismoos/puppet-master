import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import {
  ActivityIndicator,
  Alert,
  FlatList,
  Pressable,
  RefreshControl,
  ScrollView,
  StyleSheet,
  Text,
  View,
} from "react-native";

import {
  absoluteTime,
  approvalStatus,
  approvalWarnings,
  decisionOutcome,
  expiresIn,
  filterApprovals,
  formatArguments,
  pendingCount,
  relativeTime,
  sortApprovals,
  type ApprovalDetail,
  type ApprovalFilter,
  type ApprovalSummary,
  type ApprovalTone,
} from "@puppet-master/client-core/approvals";

import { decideApproval, fetchApproval, fetchApprovals } from "../api/approvals";
import type { DeviceAuthSession } from "../auth/session";
import type { ControllerConfig } from "../config";
import { colors } from "../theme";

const REFRESH_MS = 10_000;
const FILTERS: Array<{ value: ApprovalFilter; label: string }> = [
  { value: "all", label: "All" },
  { value: "pending", label: "Waiting" },
  { value: "decided", label: "Decided" },
];

const TONE_COLOR: Record<ApprovalTone, string> = {
  attention: colors.amber,
  good: colors.green,
  bad: colors.red,
  busy: colors.blue,
  neutral: colors.textMuted,
};

function errorText(error: unknown, fallback: string): string {
  return error instanceof Error && error.message ? error.message : fallback;
}

function useNow(intervalMs = 30_000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(timer);
  }, [intervalMs]);
  return now;
}

function StatusPill({ status }: { status: string }) {
  const { label, tone } = approvalStatus(status);
  const color = TONE_COLOR[tone];
  return (
    <View style={[styles.pill, { borderColor: color }]}>
      <Text style={[styles.pillText, { color }]}>{label}</Text>
    </View>
  );
}

function Header({ title, onBack }: { title: string; onBack: () => void }) {
  return (
    <View style={styles.header}>
      <Pressable
        onPress={onBack}
        hitSlop={8}
        style={styles.backTouch}
        accessibilityRole="button"
        accessibilityLabel="Back"
      >
        <View style={styles.chevron} />
      </Pressable>
      <Text style={styles.title} numberOfLines={1}>
        {title}
      </Text>
    </View>
  );
}

/** Approvals list, or the one approval a tap or row selected. */
export function ApprovalsScreen({
  config,
  auth,
  approvalId,
  onSelect,
  onBack,
}: {
  config: ControllerConfig;
  auth: DeviceAuthSession;
  approvalId: string | null;
  onSelect: (id: string | null) => void;
  onBack: () => void;
}) {
  const now = useNow();
  if (approvalId)
    return (
      <ApprovalDetailScreen
        key={approvalId}
        config={config}
        auth={auth}
        id={approvalId}
        now={now}
        onBack={() => onSelect(null)}
      />
    );
  return <ApprovalListScreen config={config} auth={auth} now={now} onSelect={onSelect} onBack={onBack} />;
}

function ApprovalListScreen({
  config,
  auth,
  now,
  onSelect,
  onBack,
}: {
  config: ControllerConfig;
  auth: DeviceAuthSession;
  now: number;
  onSelect: (id: string) => void;
  onBack: () => void;
}) {
  const [approvals, setApprovals] = useState<ApprovalSummary[] | null>(null);
  const [error, setError] = useState("");
  const [filter, setFilter] = useState<ApprovalFilter>("all");
  const [refreshing, setRefreshing] = useState(false);
  const busy = useRef(false);

  const load = useCallback(async () => {
    if (busy.current) return;
    busy.current = true;
    try {
      setApprovals(sortApprovals(await fetchApprovals(auth, config.baseUrl)));
      setError("");
    } catch (failure) {
      setError(errorText(failure, "Could not load approvals"));
    } finally {
      busy.current = false;
    }
  }, [auth, config.baseUrl]);

  useEffect(() => {
    void load();
    const timer = setInterval(() => void load(), REFRESH_MS);
    return () => clearInterval(timer);
  }, [load]);

  const shown = approvals ? filterApprovals(approvals, filter) : [];
  const waiting = approvals ? pendingCount(approvals) : 0;

  return (
    <View style={styles.root} testID="approvals-screen">
      <Header title="Approvals" onBack={onBack} />
      <Text style={styles.lede}>
        Privileged connection calls agents asked you to decide.
        {approvals ? (waiting ? ` ${waiting} waiting.` : " Nothing waiting.") : ""}
      </Text>
      <View style={styles.filters} accessibilityRole="radiogroup">
        {FILTERS.map((option) => (
          <Pressable
            key={option.value}
            onPress={() => setFilter(option.value)}
            style={[styles.filter, filter === option.value && styles.filterOn]}
            accessibilityRole="radio"
            accessibilityState={{ checked: filter === option.value }}
          >
            <Text style={[styles.filterText, filter === option.value && styles.filterTextOn]}>
              {option.label}
            </Text>
          </Pressable>
        ))}
      </View>
      {error ? (
        <View style={styles.errorBox}>
          <Text style={styles.errorText}>{error}</Text>
          <Pressable onPress={() => void load()} style={styles.secondaryButton} accessibilityRole="button">
            <Text style={styles.secondaryButtonText}>Retry</Text>
          </Pressable>
        </View>
      ) : null}
      {approvals === null && !error ? (
        <View style={styles.centered}>
          <ActivityIndicator color={colors.slate} />
          <Text style={styles.muted}>Loading approvals…</Text>
        </View>
      ) : (
        <FlatList
          data={shown}
          keyExtractor={(approval) => approval.id}
          refreshControl={
            <RefreshControl
              refreshing={refreshing}
              tintColor={colors.slate}
              onRefresh={() => {
                setRefreshing(true);
                void load().finally(() => setRefreshing(false));
              }}
            />
          }
          ListEmptyComponent={
            approvals ? (
              <View style={styles.empty}>
                <Text style={styles.emptyTitle}>
                  {filter === "pending" ? "Nothing is waiting for you" : "No approvals yet"}
                </Text>
                <Text style={styles.muted}>
                  When an agent calls a connection tool whose policy requires approval, it appears
                  here and as a notification.
                </Text>
              </View>
            ) : null
          }
          renderItem={({ item }) => (
            <Pressable
              onPress={() => onSelect(item.id)}
              style={({ pressed }) => [styles.row, pressed && styles.rowPressed]}
              accessibilityRole="button"
              accessibilityLabel={`${item.tool}, ${approvalStatus(item.status).label}`}
            >
              <View style={styles.rowTop}>
                <Text style={styles.tool} numberOfLines={1}>
                  {item.tool}
                </Text>
                <StatusPill status={item.status} />
              </View>
              <Text style={styles.rowContext} numberOfLines={1}>
                {item.connection_name ?? `Connection ${item.connection_id}`} ·{" "}
                {item.project_name ?? `Project ${item.project_id}`}
              </Text>
              <View style={styles.rowTop}>
                <Text style={styles.rowMeta} numberOfLines={1}>
                  {item.session_name ?? `Session ${item.session_id}`}
                </Text>
                <Text style={styles.rowMeta}>{relativeTime(item.created_at, now)}</Text>
              </View>
            </Pressable>
          )}
        />
      )}
    </View>
  );
}

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <View style={styles.fact}>
      <Text style={styles.factLabel}>{label}</Text>
      <View>{children}</View>
    </View>
  );
}

function ApprovalDetailScreen({
  config,
  auth,
  id,
  now,
  onBack,
}: {
  config: ControllerConfig;
  auth: DeviceAuthSession;
  id: string;
  now: number;
  onBack: () => void;
}) {
  const [detail, setDetail] = useState<ApprovalDetail | null>(null);
  const [error, setError] = useState("");
  const [deciding, setDeciding] = useState<"approve" | "deny" | null>(null);
  const [outcome, setOutcome] = useState<{ ok: boolean; message: string } | null>(null);

  const load = useCallback(async () => {
    try {
      setDetail(await fetchApproval(auth, config.baseUrl, id));
      setError("");
    } catch (failure) {
      setError(errorText(failure, "Could not load this approval"));
    }
  }, [auth, config.baseUrl, id]);

  useEffect(() => {
    void load();
  }, [load]);

  // Follow an approved call until it settles, so its result shows here.
  useEffect(() => {
    if (!detail || !["authorized", "executing"].includes(detail.status)) return;
    const timer = setTimeout(() => void load(), 1_500);
    return () => clearTimeout(timer);
  }, [detail, load]);

  const decide = async (approve: boolean) => {
    setDeciding(approve ? "approve" : "deny");
    setOutcome(null);
    try {
      const call = await decideApproval(auth, config.baseUrl, id, approve);
      setOutcome(decisionOutcome(approve, call.status));
    } catch (failure) {
      setOutcome({ ok: false, message: errorText(failure, "The decision was not recorded") });
    } finally {
      setDeciding(null);
      await load();
    }
  };

  const confirmApprove = () => {
    if (!detail) return;
    Alert.alert(
      "Approve and execute?",
      `${detail.tool} runs once on ${detail.connection_name ?? "this connection"} with the arguments shown.`,
      [
        { text: "Cancel", style: "cancel" },
        { text: "Approve", style: "default", onPress: () => void decide(true) },
      ],
    );
  };

  if (!detail)
    return (
      <View style={styles.root} testID="approval-detail-screen">
        <Header title="Approval" onBack={onBack} />
        {error ? (
          <View style={styles.errorBox}>
            <Text style={styles.errorText}>{error}</Text>
            <Pressable onPress={() => void load()} style={styles.secondaryButton} accessibilityRole="button">
              <Text style={styles.secondaryButtonText}>Retry</Text>
            </Pressable>
          </View>
        ) : (
          <View style={styles.centered}>
            <ActivityIndicator color={colors.slate} />
            <Text style={styles.muted}>Loading approval…</Text>
          </View>
        )}
      </View>
    );

  const pending = detail.status === "pending";
  const left = pending ? expiresIn(detail.expires_at, now) : null;
  const kind =
    detail.connection_kind === "mcp" ? "MCP" : detail.connection_kind === "openapi" ? "REST" : "";
  return (
    <View style={styles.root} testID="approval-detail-screen">
      <Header title="Approval" onBack={onBack} />
      <ScrollView contentContainerStyle={styles.detailBody}>
        <Text style={styles.kicker}>CONNECTION CALL</Text>
        <Text style={styles.detailTool} selectable>
          {detail.tool}
        </Text>
        <View style={styles.pillRow}>
          <StatusPill status={detail.status} />
        </View>
        {detail.justification ? (
          <View style={styles.justification}>
            <Text style={styles.kicker}>AGENT'S JUSTIFICATION</Text>
            <Text style={styles.justificationText}>{detail.justification}</Text>
          </View>
        ) : null}
        <Fact label="Requested">
          <Text style={styles.factValue}>{absoluteTime(detail.created_at)}</Text>
          <Text style={styles.muted}>{relativeTime(detail.created_at, now)}</Text>
        </Fact>
        {pending ? (
          <Fact label="Expires">
            <Text style={styles.factValue}>{left ? `in ${left}` : "now"}</Text>
          </Fact>
        ) : null}
        {detail.decided_by ? (
          <Fact label="Decided by">
            <Text style={styles.factValue}>{detail.decided_by}</Text>
          </Fact>
        ) : null}
        <Fact label="Project">
          <Text style={styles.factValue}>{detail.project_name ?? `Project ${detail.project_id}`}</Text>
        </Fact>
        <Fact label="Session">
          <Text style={styles.factValue}>{detail.session_name ?? `Session ${detail.session_id}`}</Text>
          <Text style={styles.muted}>
            #{detail.session_id}
            {detail.session_role ? ` · ${detail.session_role}` : ""}
            {detail.session_state ? ` · ${detail.session_state}` : ""}
          </Text>
        </Fact>
        <Fact label="Connection">
          <Text style={styles.factValue}>
            {detail.connection_name ?? `Connection ${detail.connection_id}`}
          </Text>
          <Text style={styles.muted}>
            {kind}
            {kind ? " · " : ""}revision {detail.connection_revision}
          </Text>
          {detail.connection_endpoint ? (
            <Text style={styles.mono} selectable>
              {detail.connection_endpoint}
            </Text>
          ) : null}
        </Fact>
        <Fact label="Tool">
          <Text style={styles.mono}>{detail.tool}</Text>
          {detail.tool_access ? <Text style={styles.muted}>classified {detail.tool_access}</Text> : null}
          {detail.tool_description ? <Text style={styles.muted}>{detail.tool_description}</Text> : null}
        </Fact>
        <Text style={styles.sectionTitle}>Exact arguments</Text>
        <Text style={styles.muted}>Approving executes these arguments once, exactly as shown.</Text>
        <ScrollView horizontal style={styles.codeBox}>
          <Text style={styles.code} selectable testID="approval-arguments">
            {formatArguments(detail.arguments)}
          </Text>
        </ScrollView>
        {approvalWarnings(detail).map((warning) => (
          <Text key={warning} style={styles.warning}>
            {warning}
          </Text>
        ))}
        {outcome ? (
          <Text style={outcome.ok ? styles.outcome : styles.errorText}>{outcome.message}</Text>
        ) : null}
        {detail.error ? <Text style={styles.errorText}>{detail.error}</Text> : null}
        {detail.result != null ? (
          <>
            <Text style={styles.sectionTitle}>Result</Text>
            <ScrollView horizontal style={styles.codeBox}>
              <Text style={styles.code} selectable>
                {formatArguments(detail.result)}
              </Text>
            </ScrollView>
          </>
        ) : null}
      </ScrollView>
      {pending ? (
        <View style={styles.actions}>
          <Pressable
            onPress={() => void decide(false)}
            disabled={deciding !== null}
            style={[styles.actionButton, styles.denyButton, deciding !== null && styles.disabled]}
            accessibilityRole="button"
          >
            <Text style={styles.denyText}>{deciding === "deny" ? "Denying…" : "Deny"}</Text>
          </Pressable>
          <Pressable
            onPress={confirmApprove}
            disabled={deciding !== null}
            style={[styles.actionButton, styles.approveButton, deciding !== null && styles.disabled]}
            accessibilityRole="button"
          >
            <Text style={styles.approveText}>
              {deciding === "approve" ? "Approving…" : "Approve and execute"}
            </Text>
          </Pressable>
        </View>
      ) : null}
    </View>
  );
}

const styles = StyleSheet.create({
  root: { flex: 1 },
  header: {
    flexDirection: "row",
    alignItems: "center",
    gap: 8,
    paddingHorizontal: 16,
    paddingVertical: 12,
  },
  backTouch: { padding: 4 },
  chevron: {
    width: 10,
    height: 10,
    borderLeftWidth: 2,
    borderBottomWidth: 2,
    borderColor: colors.slate,
    transform: [{ rotate: "45deg" }],
  },
  title: { flex: 1, color: colors.textBright, fontSize: 20, fontWeight: "600" },
  lede: { color: colors.textMuted, fontSize: 13, paddingHorizontal: 16, marginBottom: 10 },
  filters: {
    flexDirection: "row",
    alignSelf: "flex-start",
    marginHorizontal: 16,
    marginBottom: 8,
    borderWidth: 1,
    borderColor: colors.line,
    borderRadius: 8,
    overflow: "hidden",
  },
  filter: { paddingHorizontal: 14, paddingVertical: 7 },
  filterOn: { backgroundColor: colors.surface },
  filterText: { color: colors.textMuted, fontSize: 14 },
  filterTextOn: { color: colors.textBright, fontWeight: "600" },
  row: {
    paddingHorizontal: 16,
    paddingVertical: 12,
    borderBottomWidth: StyleSheet.hairlineWidth,
    borderBottomColor: colors.line,
    gap: 4,
  },
  rowPressed: { backgroundColor: colors.panelAlt },
  rowTop: { flexDirection: "row", alignItems: "center", justifyContent: "space-between", gap: 10 },
  tool: { flex: 1, color: colors.textBright, fontFamily: "Menlo", fontSize: 14, fontWeight: "600" },
  rowContext: { color: colors.text, fontSize: 13 },
  rowMeta: { color: colors.textMuted, fontSize: 12, flexShrink: 1 },
  pill: { borderWidth: 1, borderRadius: 999, paddingHorizontal: 8, paddingVertical: 1 },
  pillText: { fontSize: 11, fontWeight: "600" },
  pillRow: { flexDirection: "row", marginTop: 6 },
  centered: { alignItems: "center", gap: 8, marginTop: 48 },
  empty: { padding: 16, gap: 6 },
  emptyTitle: { color: colors.textBright, fontSize: 15, fontWeight: "600" },
  muted: { color: colors.textMuted, fontSize: 13 },
  errorBox: { marginHorizontal: 16, marginVertical: 8, gap: 8 },
  errorText: { color: colors.red, fontSize: 14, marginVertical: 6 },
  warning: { color: colors.amber, fontSize: 14, marginTop: 10 },
  outcome: { color: colors.green, fontSize: 14, marginTop: 10 },
  secondaryButton: {
    alignSelf: "flex-start",
    borderWidth: 1,
    borderColor: colors.line,
    borderRadius: 6,
    paddingHorizontal: 14,
    paddingVertical: 7,
  },
  secondaryButtonText: { color: colors.text, fontSize: 14 },
  detailBody: { paddingHorizontal: 16, paddingBottom: 32 },
  kicker: { color: colors.textMuted, fontSize: 11, fontWeight: "600", letterSpacing: 0.6 },
  detailTool: { color: colors.textBright, fontFamily: "Menlo", fontSize: 18, fontWeight: "600", marginTop: 4 },
  justification: {
    marginTop: 14,
    borderLeftWidth: 3,
    borderLeftColor: colors.line,
    paddingLeft: 12,
    gap: 4,
  },
  justificationText: { color: colors.textBright, fontSize: 15 },
  fact: { marginTop: 14, gap: 2 },
  factLabel: { color: colors.textMuted, fontSize: 12 },
  factValue: { color: colors.text, fontSize: 15 },
  mono: { color: colors.text, fontFamily: "Menlo", fontSize: 13 },
  sectionTitle: { color: colors.textBright, fontSize: 15, fontWeight: "600", marginTop: 20 },
  codeBox: {
    marginTop: 8,
    backgroundColor: colors.panelAlt,
    borderRadius: 8,
    borderWidth: 1,
    borderColor: colors.line,
    maxHeight: 360,
  },
  code: { color: colors.text, fontFamily: "Menlo", fontSize: 12, padding: 12 },
  actions: {
    flexDirection: "row",
    gap: 10,
    paddingHorizontal: 16,
    paddingVertical: 12,
    borderTopWidth: StyleSheet.hairlineWidth,
    borderTopColor: colors.line,
    backgroundColor: colors.bg,
  },
  actionButton: { flex: 1, minHeight: 46, borderRadius: 8, alignItems: "center", justifyContent: "center" },
  approveButton: { backgroundColor: colors.blue },
  approveText: { color: "#fff", fontSize: 15, fontWeight: "600" },
  denyButton: { borderWidth: 1, borderColor: colors.line },
  denyText: { color: colors.text, fontSize: 15, fontWeight: "600" },
  disabled: { opacity: 0.5 },
});
