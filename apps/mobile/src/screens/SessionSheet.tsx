import { useEffect, useState, useSyncExternalStore } from "react";
import {
  Clipboard,
  Modal,
  Pressable,
  SafeAreaView,
  ScrollView,
  StyleSheet,
  Text,
  View,
} from "react-native";
import { openExternalUrl } from "../adapters/openLink";

import type { PmClient } from "@puppet-master/client-core/ws/client";
import {
  ContextKind,
  ContextSeverity,
  type ContextField,
  type Session,
  type SessionContext,
  type SessionForward,
} from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import {
  agentLabel,
  LOCAL_WORKER_ID,
  sessionDisplayName,
} from "@puppet-master/client-core/format";
import {
  sessionLaunchMetadata,
} from "@puppet-master/client-core/state/sessionLaunch";
import type { ForwardGroup } from "@puppet-master/client-core/state/forwards";

import {
  type ActivityReport,
  parseReportsResponse,
  sortNewestFirst,
} from "@puppet-master/client-core/api/reports";

import type { ControllerConfig } from "../config";
import type { DeviceAuthSession } from "../auth/session";
import { colors, stateColor } from "../theme";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Tab names
// ---------------------------------------------------------------------------

type TabName = "glance" | "info" | "timeline" | "urls";

function reportText(r: ActivityReport): string {
  const p = r.payload as Record<string, unknown>;
  switch (r.kind) {
    case "checkpoint":
    case "user-note": {
      const parts = [p.headline, p.note].filter(Boolean);
      return parts.join(" — ") || "checkpoint";
    }
    case "status": {
      const parts = [p.task, p.phase, p.detail].filter(Boolean);
      return parts.join(": ") || "status";
    }
    case "progress":
      return `${p.percent ?? "?"}% ${p.summary ?? ""}`.trim();
    case "blocked":
      return String(p.question ?? "waiting for input");
    default:
      return r.kind;
  }
}

function formatTime(ms: number): string {
  return new Date(ms).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

// ---------------------------------------------------------------------------
// Severity → colour
// ---------------------------------------------------------------------------

function severityColor(severity: ContextSeverity): string {
  switch (severity) {
    case ContextSeverity.INFO:
      return colors.blue;
    case ContextSeverity.GOOD:
      return colors.green;
    case ContextSeverity.WARN:
      return colors.amber;
    case ContextSeverity.BAD:
      return colors.red;
    default:
      return colors.textMuted;
  }
}

// ---------------------------------------------------------------------------
// Copy helper
// ---------------------------------------------------------------------------

function copyToClipboard(text: string) {
  // React Native Clipboard is deprecated but universally available; expo-clipboard
  // is not in the dependency list, so this is the pragmatic path.
  Clipboard.setString(text);
}

function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);

  const handleCopy = () => {
    copyToClipboard(text);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };

  return (
    <Pressable onPress={handleCopy} hitSlop={8} style={copyStyles.touch}>
      <Text style={copyStyles.label}>{copied ? "copied" : "copy"}</Text>
    </Pressable>
  );
}

const copyStyles = StyleSheet.create({
  touch: { paddingHorizontal: 8, paddingVertical: 4 },
  label: { color: colors.blue, fontSize: 12 },
});

// ---------------------------------------------------------------------------
// Context chip
// ---------------------------------------------------------------------------

function ContextChip({ field }: { field: ContextField }) {
  const color = severityColor(field.severity);
  const mono = field.kind === ContextKind.CODE;
  const isUrl = field.kind === ContextKind.URL;

  return (
    <View style={chipStyles.chip}>
      <Text style={chipStyles.label}>{field.label}</Text>
      {isUrl ? (
        <Pressable onPress={() => openExternalUrl(field.value)}>
          <Text style={[chipStyles.value, chipStyles.link]} numberOfLines={1}>
            {field.value} ↗
          </Text>
        </Pressable>
      ) : (
        <Text
          style={[
            chipStyles.value,
            { color },
            mono && chipStyles.mono,
          ]}
          selectable
        >
          {field.value}
        </Text>
      )}
    </View>
  );
}

const chipStyles = StyleSheet.create({
  chip: {
    flexDirection: "row",
    alignItems: "center",
    gap: 6,
    backgroundColor: colors.panelAlt,
    borderRadius: 8,
    paddingHorizontal: 10,
    paddingVertical: 6,
  },
  label: { color: colors.textMuted, fontSize: 12 },
  value: { fontSize: 13, flexShrink: 1 },
  link: { color: colors.blue, fontSize: 13, flexShrink: 1 },
  mono: { fontFamily: "Menlo" },
});

// ---------------------------------------------------------------------------
// Tabs
// ---------------------------------------------------------------------------

function TabBar({
  active,
  onSelect,
}: {
  active: TabName;
  onSelect: (tab: TabName) => void;
}) {
  const tabs: { name: TabName; label: string }[] = [
    { name: "glance", label: "Glance" },
    { name: "info", label: "Info" },
    { name: "timeline", label: "Timeline" },
    { name: "urls", label: "URLs" },
  ];

  return (
    <View style={tabStyles.row}>
      {tabs.map((t) => (
        <Pressable key={t.name} style={tabStyles.tab} onPress={() => onSelect(t.name)}>
          <Text style={[tabStyles.label, active === t.name && tabStyles.labelActive]}>
            {t.label}
          </Text>
          {active === t.name ? <View style={tabStyles.indicator} /> : null}
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
  label: { color: colors.textMuted, fontSize: 14 },
  labelActive: { color: colors.textBright, fontWeight: "600" },
  indicator: {
    position: "absolute",
    bottom: 0,
    height: 2,
    width: "60%",
    backgroundColor: colors.blue,
    borderRadius: 1,
  },
});

// ---------------------------------------------------------------------------
// Glance tab
// ---------------------------------------------------------------------------

function GlanceTab({
  session,
  context,
}: {
  session: Session;
  context: SessionContext | undefined;
}) {
  const glanceFields = context?.glance?.filter((f) => f.key && f.value.trim()) ?? [];
  const detailFields = context?.detail?.filter((f) => f.key && f.value.trim()) ?? [];

  return (
    <ScrollView style={contentStyles.scroll} contentContainerStyle={contentStyles.container}>
      {session.summary ? (
        <Text style={contentStyles.summary} selectable>{session.summary}</Text>
      ) : (
        <Text style={contentStyles.empty}>No summary yet.</Text>
      )}

      {session.activity ? (
        <View style={contentStyles.section}>
          <Text style={contentStyles.sectionTitle}>Activity</Text>
          <Text style={contentStyles.activity} selectable>{session.activity}</Text>
        </View>
      ) : null}

      {glanceFields.length > 0 ? (
        <View style={contentStyles.section}>
          <Text style={contentStyles.sectionTitle}>Glance</Text>
          <View style={contentStyles.chipGrid}>
            {glanceFields.map((f) => <ContextChip key={f.key} field={f} />)}
          </View>
        </View>
      ) : null}

      {detailFields.length > 0 ? (
        <View style={contentStyles.section}>
          <Text style={contentStyles.sectionTitle}>Context</Text>
          {detailFields.map((f) => (
            <View key={f.key} style={contentStyles.detailRow}>
              <Text style={contentStyles.detailLabel}>{f.label}</Text>
              {f.kind === ContextKind.URL ? (
                <Pressable style={contentStyles.detailLinkTouch} onPress={() => openExternalUrl(f.value)}>
                  <Text style={contentStyles.detailLink} numberOfLines={2}>{f.value} ↗</Text>
                </Pressable>
              ) : (
                <Text
                  style={[
                    contentStyles.detailValue,
                    f.kind === ContextKind.CODE && contentStyles.mono,
                  ]}
                  selectable
                >
                  {f.value}
                </Text>
              )}
            </View>
          ))}
        </View>
      ) : null}
    </ScrollView>
  );
}

// ---------------------------------------------------------------------------
// Info tab
// ---------------------------------------------------------------------------

function InfoTab({
  session,
  client,
}: {
  session: Session;
  client: PmClient;
}) {
  const state = client.getState();
  const project = state.projects.get(session.projectId.toString());
  const worker = state.workers.get(session.workerId.toString());
  const modelProfile = session.modelProfileId !== undefined
    ? state.modelProfiles.get(session.modelProfileId.toString())
    : undefined;

  const launch = sessionLaunchMetadata(session, project);

  return (
    <ScrollView style={contentStyles.scroll} contentContainerStyle={contentStyles.container}>
      <View style={contentStyles.section}>
        <Text style={contentStyles.sectionTitle}>Launch</Text>
        <InfoRow label="Origin" value={launch.origin} />
        <InfoRow label="Launch folder" value={session.cwd || "—"} copiable />
        {project ? <InfoRow label="Project" value={project.name} /> : null}
        {project ? <InfoRow label="Project path" value={project.path} copiable /> : null}
      </View>

      <View style={contentStyles.section}>
        <Text style={contentStyles.sectionTitle}>Worker and Host</Text>
        <InfoRow
          label="Worker"
          value={
            worker
              ? worker.id === LOCAL_WORKER_ID
                ? "local"
                : worker.name || `Worker ${worker.id}`
              : `Worker ${session.workerId} unavailable`
          }
        />
        <InfoRow
          label="Hostname"
          value={
            worker
              ? worker.id === LOCAL_WORKER_ID
                ? "Controller host"
                : worker.hostname || "Not reported"
              : "—"
          }
        />
      </View>

      <View style={contentStyles.section}>
        <Text style={contentStyles.sectionTitle}>Model</Text>
        <InfoRow label="Profile" value={modelProfile?.name ?? "inherited"} />
        <InfoRow label="Agent" value={agentLabel(session.agent)} />
      </View>

      <View style={contentStyles.section}>
        <Text style={contentStyles.sectionTitle}>Identity</Text>
        <InfoRow label="Session ID" value={session.id.toString()} copiable />
        {session.agentSessionId ? (
          <InfoRow label="Agent session" value={session.agentSessionId} copiable />
        ) : null}
      </View>
    </ScrollView>
  );
}

function InfoRow({
  label,
  value,
  copiable,
}: {
  label: string;
  value: string;
  copiable?: boolean;
}) {
  return (
    <View style={infoStyles.row}>
      <Text style={infoStyles.label}>{label}</Text>
      <Text style={infoStyles.value} selectable numberOfLines={3}>
        {value}
      </Text>
      {copiable ? <CopyButton text={value} /> : null}
    </View>
  );
}

const infoStyles = StyleSheet.create({
  row: {
    flexDirection: "row",
    alignItems: "center",
    gap: 8,
    paddingVertical: 6,
  },
  label: { color: colors.textMuted, fontSize: 13, width: 100 },
  value: { color: colors.text, fontSize: 13, flex: 1, flexWrap: "wrap" },
});

// ---------------------------------------------------------------------------
// Timeline tab
// ---------------------------------------------------------------------------

function TimelineTab({
  sessionId,
  config,
  auth,
}: {
  sessionId: bigint;
  config: ControllerConfig;
  auth: DeviceAuthSession;
}) {
  const [reports, setReports] = useState<ActivityReport[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);

    auth
      .withAccessToken(config.baseUrl, async (token) => {
        const res = await fetch(
          `${config.baseUrl}/api/sessions/${sessionId}/reports`,
          { headers: { Authorization: `Bearer ${token}` } },
        );
        if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
        return sortNewestFirst(parseReportsResponse(await res.json()));
      })
      .then((data) => {
        if (!cancelled) {
          setReports(data);
          setLoading(false);
        }
      })
      .catch((err) => {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : String(err));
          setLoading(false);
        }
      });

    return () => {
      cancelled = true;
    };
  }, [sessionId, config.baseUrl, auth]);

  if (loading) {
    return <Text style={contentStyles.empty}>Loading timeline…</Text>;
  }
  if (error) {
    return <Text style={contentStyles.error}>{error}</Text>;
  }
  if (reports.length === 0) {
    return <Text style={contentStyles.empty}>No activity yet.</Text>;
  }

  return (
    <ScrollView style={contentStyles.scroll} contentContainerStyle={contentStyles.container}>
      {reports.map((r, i) => (
        <View key={`${r.tsUnixMs}-${i}`} style={tlStyles.entry}>
          <Text style={tlStyles.time}>{formatTime(r.tsUnixMs)}</Text>
          <View style={[tlStyles.badge, { backgroundColor: kindColor(r.kind) }]}>
            <Text style={tlStyles.badgeText}>{r.kind}</Text>
          </View>
          <Text style={tlStyles.desc} numberOfLines={3}>{reportText(r)}</Text>
        </View>
      ))}
    </ScrollView>
  );
}

function kindColor(kind: string): string {
  switch (kind) {
    case "checkpoint":
    case "user-note":
      return colors.blue;
    case "blocked":
      return colors.amber;
    case "progress":
      return colors.green;
    default:
      return colors.textMuted;
  }
}

const tlStyles = StyleSheet.create({
  entry: {
    flexDirection: "row",
    alignItems: "flex-start",
    gap: 8,
    paddingVertical: 8,
    borderBottomWidth: StyleSheet.hairlineWidth,
    borderBottomColor: colors.lineSoft,
  },
  time: { color: colors.textMuted, fontSize: 11, fontFamily: "Menlo", width: 70 },
  badge: {
    borderRadius: 4,
    paddingHorizontal: 6,
    paddingVertical: 1,
  },
  badgeText: { color: "#fff", fontSize: 10, fontWeight: "600" },
  desc: { color: colors.text, fontSize: 13, flex: 1 },
});

// ---------------------------------------------------------------------------
// URLs tab
// ---------------------------------------------------------------------------

function URLsTab({
  sessionId,
  groups,
  onOpenForward,
}: {
  sessionId: bigint;
  groups: readonly ForwardGroup[];
  onOpenForward: (fwd: SessionForward) => void;
}) {
  if (groups.length === 0) {
    return <Text style={contentStyles.empty}>No shared URLs.</Text>;
  }

  return (
    <ScrollView style={contentStyles.scroll} contentContainerStyle={contentStyles.container}>
      {groups.map((group) => (
        <View key={group.session.id.toString()}>
          {group.session.id === sessionId ? null : (
            <Text style={urlStyles.owner} numberOfLines={1}>
              {sessionDisplayName(group.session)}
            </Text>
          )}
          {group.forwards.map((fwd) => (
            <Pressable
              key={fwd.id.toString()}
              style={urlStyles.row}
              onPress={() => onOpenForward(fwd)}
            >
              <Text style={urlStyles.icon}>{fwd.sourcePath ? "/" : ">"}</Text>
              <View style={urlStyles.textCol}>
                <Text style={urlStyles.label} numberOfLines={1}>
                  {fwd.label || fwd.sourcePath || `Port ${fwd.workerPort}`}
                </Text>
                {fwd.sourcePath && fwd.label ? (
                  <Text style={urlStyles.path} numberOfLines={1}>
                    {fwd.sourcePath}
                  </Text>
                ) : null}
                {fwd.url ? (
                  <Text style={urlStyles.url} numberOfLines={1}>Open in Safari</Text>
                ) : (
                  <Text style={urlStyles.noUrl}>unavailable</Text>
                )}
              </View>
              {fwd.targetReachable === false ? (
                <Text style={urlStyles.offline}>
                  {fwd.sourcePath ? "not served" : "offline"}
                </Text>
              ) : null}
            </Pressable>
          ))}
        </View>
      ))}
    </ScrollView>
  );
}

const urlStyles = StyleSheet.create({
  row: {
    flexDirection: "row",
    alignItems: "center",
    gap: 10,
    paddingVertical: 10,
    borderBottomWidth: StyleSheet.hairlineWidth,
    borderBottomColor: colors.lineSoft,
  },
  owner: {
    color: colors.textMuted,
    fontSize: 12,
    fontWeight: "600",
    marginTop: 16,
    marginBottom: 2,
  },
  icon: { color: colors.blue, fontSize: 14, fontWeight: "700", width: 16 },
  textCol: { flex: 1, gap: 2 },
  label: { color: colors.text, fontSize: 14 },
  url: { color: colors.blue, fontSize: 12 },
  path: { color: colors.textMuted, fontSize: 11 },
  noUrl: { color: colors.textMuted, fontSize: 12 },
  offline: { color: colors.red, fontSize: 11 },
});

// ---------------------------------------------------------------------------
// Shared content styles
// ---------------------------------------------------------------------------

const contentStyles = StyleSheet.create({
  scroll: { flex: 1 },
  container: { padding: 16, paddingBottom: 40 },
  summary: { color: colors.text, fontSize: 15, lineHeight: 22 },
  activity: { color: colors.textMuted, fontSize: 14, fontStyle: "italic" },
  empty: { color: colors.textMuted, textAlign: "center", marginTop: 32, fontSize: 14 },
  error: { color: colors.red, textAlign: "center", marginTop: 32, fontSize: 13 },
  section: { marginTop: 20 },
  sectionTitle: { color: colors.textBright, fontSize: 13, fontWeight: "600", marginBottom: 8 },
  chipGrid: { flexDirection: "row", flexWrap: "wrap", gap: 8 },
  detailRow: {
    flexDirection: "row",
    alignItems: "flex-start",
    gap: 8,
    paddingVertical: 4,
  },
  detailLabel: { color: colors.textMuted, fontSize: 12, width: 80 },
  detailValue: { color: colors.text, fontSize: 13, flex: 1, flexWrap: "wrap" },
  detailLinkTouch: { flex: 1 },
  detailLink: { color: colors.blue, fontSize: 13, flexWrap: "wrap" },
  mono: { fontFamily: "Menlo" },
});

// ---------------------------------------------------------------------------
// SessionSheet
// ---------------------------------------------------------------------------

export function SessionSheet({
  visible,
  sessionId,
  initialTab,
  client,
  config,
  auth,
  forwardGroups,
  onOpenForward,
  onClose,
}: {
  visible: boolean;
  sessionId: bigint;
  initialTab?: TabName;
  client: PmClient;
  config: ControllerConfig;
  auth: DeviceAuthSession;
  forwardGroups: readonly ForwardGroup[];
  onOpenForward: (fwd: SessionForward) => void;
  onClose: () => void;
}) {
  const [tab, setTab] = useState<TabName>(initialTab ?? "glance");

  useEffect(() => {
    if (visible && initialTab) setTab(initialTab);
  }, [visible, initialTab]);

  const appState = useSyncExternalStore(client.subscribe, client.getState);
  const session = appState.sessions.get(sessionId.toString());
  const context = appState.contexts?.get(sessionId.toString());

  if (!session) {
    return (
      <Modal visible={visible} animationType="slide" onRequestClose={onClose}>
        <SafeAreaView style={styles.root}>
          <View style={styles.header}>
            <Text style={styles.title}>Session</Text>
            <Pressable onPress={onClose} style={styles.closeTouch} accessibilityLabel="Close" accessibilityRole="button">
              <View style={styles.closeX1} />
              <View style={styles.closeX2} />
            </Pressable>
          </View>
          <Text style={contentStyles.empty}>Session not found.</Text>
        </SafeAreaView>
      </Modal>
    );
  }

  return (
    <Modal visible={visible} animationType="slide" onRequestClose={onClose}>
      <SafeAreaView style={styles.root}>
        <View style={styles.header}>
          <View style={[styles.stateDot, { backgroundColor: stateColor(session.state) }]} />
          <Text style={styles.title} numberOfLines={1}>
            {sessionDisplayName(session)}
          </Text>
          <Pressable onPress={onClose} style={styles.closeTouch} accessibilityLabel="Close" accessibilityRole="button">
            <View style={styles.closeX1} />
            <View style={styles.closeX2} />
          </Pressable>
        </View>
        <TabBar active={tab} onSelect={setTab} />
        {tab === "glance" ? (
          <GlanceTab session={session} context={context} />
        ) : tab === "info" ? (
          <InfoTab session={session} client={client} />
        ) : tab === "urls" ? (
          <URLsTab sessionId={sessionId} groups={forwardGroups} onOpenForward={onOpenForward} />
        ) : (
          <TimelineTab sessionId={sessionId} config={config} auth={auth} />
        )}
      </SafeAreaView>
    </Modal>
  );
}

// ---------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------

const styles = StyleSheet.create({
  root: { flex: 1, backgroundColor: colors.bg },
  header: {
    flexDirection: "row",
    alignItems: "center",
    gap: 8,
    paddingHorizontal: 16,
    paddingVertical: 12,
  },
  stateDot: { width: 8, height: 8, borderRadius: 4 },
  title: { color: colors.textBright, fontSize: 18, fontWeight: "600", flex: 1 },
  closeTouch: {
    width: 44,
    height: 44,
    alignItems: "center",
    justifyContent: "center",
  },
  closeX1: {
    position: "absolute",
    width: 18,
    height: 2,
    backgroundColor: colors.textMuted,
    borderRadius: 1,
    transform: [{ rotate: "45deg" }],
  },
  closeX2: {
    position: "absolute",
    width: 18,
    height: 2,
    backgroundColor: colors.textMuted,
    borderRadius: 1,
    transform: [{ rotate: "-45deg" }],
  },
});
