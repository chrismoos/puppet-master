import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Modal,
  Pressable,
  SafeAreaView,
  StyleSheet,
  Text,
  TextInput,
  View,
} from "react-native";

import { KeyboardAwareScrollView } from "../components/KeyboardAwareScrollables";
import { KeyboardAvoidingRoot } from "../components/KeyboardAvoidingRoot";

import type { PmClient } from "@puppet-master/client-core/ws/client";
import {
  AgentKind,
  PermissionMode,
  SessionRole,
} from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { LOCAL_WORKER_ID } from "@puppet-master/client-core/format";
import { agentLabel, resolveAgent } from "@puppet-master/client-core/state/agent";
import { resolveModelProfile } from "@puppet-master/client-core/state/modelProfile";
import {
  resolveProjectWorkerId,
  workerUnavailableReason,
} from "@puppet-master/client-core/state/worker";

import { colors } from "../theme";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface SpawnState {
  role: SessionRole;
  projectId: string;
  workerId: string;
  agentId: string;
  permissionMode: string;
  modelProfileId: string;
  prompt: string;
}

const INHERIT = "";

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function SegmentedControl({
  options,
  selected,
  onSelect,
}: {
  options: { value: string; label: string }[];
  selected: string;
  onSelect: (value: string) => void;
}) {
  return (
    <View style={segStyles.row}>
      {options.map((opt) => (
        <Pressable
          key={opt.value}
          style={[segStyles.segment, selected === opt.value && segStyles.segmentSelected]}
          onPress={() => onSelect(opt.value)}
        >
          <Text style={[segStyles.label, selected === opt.value && segStyles.labelSelected]}>
            {opt.label}
          </Text>
        </Pressable>
      ))}
    </View>
  );
}

const segStyles = StyleSheet.create({
  row: {
    flexDirection: "row",
    backgroundColor: colors.panelAlt,
    borderRadius: 8,
    padding: 2,
  },
  segment: {
    flex: 1,
    paddingVertical: 8,
    alignItems: "center",
    borderRadius: 6,
  },
  segmentSelected: {
    backgroundColor: colors.blue,
  },
  label: { color: colors.textMuted, fontSize: 14, fontWeight: "500" },
  labelSelected: { color: "#fff" },
});

function PickerField({
  label,
  value,
  options,
  onSelect,
}: {
  label: string;
  value: string;
  options: { id: string; name: string; detail?: string }[];
  onSelect: (id: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const selected = options.find((o) => o.id === value);

  return (
    <View>
      <Text style={fieldStyles.label}>{label}</Text>
      <Pressable style={fieldStyles.picker} onPress={() => setOpen(true)}>
        <Text style={fieldStyles.pickerText} numberOfLines={1}>
          {selected?.name ?? "Select\u2026"}
        </Text>
        <View style={fieldStyles.chevron} />
      </Pressable>
      <Modal visible={open} transparent animationType="fade" onRequestClose={() => setOpen(false)}>
        <Pressable style={fieldStyles.backdrop} onPress={() => setOpen(false)}>
          <SafeAreaView style={fieldStyles.listAnchor}>
            <View style={fieldStyles.list}>
              <Text style={fieldStyles.listTitle}>{label}</Text>
              <KeyboardAwareScrollView style={fieldStyles.listScroll}>
                {options.map((opt) => (
                  <Pressable
                    key={opt.id}
                    style={({ pressed }) => [
                      fieldStyles.listItem,
                      pressed && fieldStyles.listItemPressed,
                    ]}
                    onPress={() => {
                      onSelect(opt.id);
                      setOpen(false);
                    }}
                  >
                    <Text style={[fieldStyles.listItemText, opt.id === value && fieldStyles.listItemSelected]}>
                      {opt.name}
                    </Text>
                    {opt.detail ? (
                      <Text style={fieldStyles.listItemDetail}>{opt.detail}</Text>
                    ) : null}
                  </Pressable>
                ))}
                {options.length === 0 ? (
                  <Text style={fieldStyles.listEmpty}>No options available</Text>
                ) : null}
              </KeyboardAwareScrollView>
            </View>
          </SafeAreaView>
        </Pressable>
      </Modal>
    </View>
  );
}

const fieldStyles = StyleSheet.create({
  label: { color: colors.textMuted, fontSize: 12, marginBottom: 4, marginTop: 12 },
  picker: {
    flexDirection: "row",
    alignItems: "center",
    backgroundColor: colors.panelAlt,
    borderRadius: 8,
    paddingHorizontal: 12,
    paddingVertical: 12,
  },
  pickerText: { color: colors.text, fontSize: 15, flex: 1 },
  chevron: {
    width: 8,
    height: 8,
    borderRightWidth: 2,
    borderBottomWidth: 2,
    borderColor: colors.textMuted,
    transform: [{ rotate: "45deg" }],
    marginRight: 2,
  },
  backdrop: { flex: 1, backgroundColor: "rgba(0,0,0,0.5)" },
  listAnchor: { flex: 1, justifyContent: "center", paddingHorizontal: 32 },
  list: {
    backgroundColor: colors.surface,
    borderRadius: 12,
    maxHeight: 400,
    overflow: "hidden",
  },
  listTitle: {
    color: colors.textBright,
    fontSize: 16,
    fontWeight: "600",
    paddingHorizontal: 16,
    paddingVertical: 12,
    borderBottomWidth: StyleSheet.hairlineWidth,
    borderBottomColor: colors.line,
  },
  listScroll: { maxHeight: 340 },
  listItem: {
    paddingHorizontal: 16,
    paddingVertical: 12,
    minHeight: 44,
    justifyContent: "center",
  },
  listItemPressed: { backgroundColor: colors.panelAlt },
  listItemText: { color: colors.text, fontSize: 15 },
  listItemSelected: { color: colors.blue, fontWeight: "600" },
  listItemDetail: { color: colors.textMuted, fontSize: 12, marginTop: 2 },
  listEmpty: { color: colors.textMuted, fontSize: 14, textAlign: "center", paddingVertical: 24 },
});

// ---------------------------------------------------------------------------
// SpawnSheet
// ---------------------------------------------------------------------------

function permissionModeLabel(mode: PermissionMode): string {
  switch (mode) {
    case PermissionMode.DEFAULT:
      return "Default";
    case PermissionMode.AUTO:
      return "Auto-accept";
    default:
      return "Inherit";
  }
}

export function SpawnSheet({
  visible,
  client,
  onClose,
}: {
  visible: boolean;
  client: PmClient;
  onClose: () => void;
}) {
  const state = useMemo(() => client.getState(), [visible]);

  const [form, setForm] = useState<SpawnState>({
    role: SessionRole.WORKER,
    projectId: "",
    workerId: "",
    agentId: INHERIT,
    permissionMode: INHERIT,
    modelProfileId: INHERIT,
    prompt: "",
  });
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Available projects and buckets
  const projects = useMemo(
    () => [...state.projects.values()].sort((a, b) => a.name.localeCompare(b.name)),
    [state.projects],
  );
  const buckets = useMemo(
    () => new Map([...state.buckets.values()].map((b) => [b.id.toString(), b])),
    [state.buckets],
  );
  const workers = useMemo(
    () => [...state.workers.values()].sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)),
    [state.workers],
  );

  // Auto-select first project when sheet opens if none selected
  useEffect(() => {
    if (visible && !form.projectId && projects.length > 0) {
      const pid = projects[0].id.toString();
      const project = projects[0];
      const bucket = buckets.get(project.bucketId.toString());
      const wid = resolveProjectWorkerId(project, bucket).toString();
      setForm((f) => ({ ...f, projectId: pid, workerId: wid }));
    }
  }, [visible, projects, buckets]);

  // When project changes, resolve default worker
  const selectedProject = projects.find((p) => p.id.toString() === form.projectId);
  const selectedBucket = selectedProject
    ? buckets.get(selectedProject.bucketId.toString())
    : undefined;

  const onProjectChange = useCallback(
    (pid: string) => {
      const project = projects.find((p) => p.id.toString() === pid);
      const bucket = project ? buckets.get(project.bucketId.toString()) : undefined;
      const wid = resolveProjectWorkerId(project, bucket).toString();
      setForm((f) => ({ ...f, projectId: pid, workerId: wid }));
      setError(null);
    },
    [projects, buckets],
  );

  // Worker options: constrained to project's allowed workers, or all if unrestricted
  const workerOptions = useMemo(() => {
    const allowed = selectedProject?.allowedWorkerIds;
    const allowedSet =
      allowed && allowed.length > 0
        ? new Set(allowed.map((id) => id.toString()))
        : null;
    return workers
      .filter((w) => !allowedSet || allowedSet.has(w.id.toString()))
      .map((w) => ({
        id: w.id.toString(),
        name: w.id === LOCAL_WORKER_ID ? "local" : w.name || w.hostname,
        detail: w.online ? undefined : "offline",
      }));
  }, [workers, selectedProject]);

  const projectOptions = useMemo(
    () =>
      projects.map((p) => {
        const bucket = buckets.get(p.bucketId.toString());
        return { id: p.id.toString(), name: p.name, detail: bucket?.name };
      }),
    [projects, buckets],
  );

  // Agent options: inherit + each known agent from the shared table
  const resolvedAgent = resolveAgent(selectedProject, selectedBucket);
  const agentOptions = useMemo(() => {
    const inheritLabel = `Inherit (${agentLabel(resolvedAgent.agent)} from ${resolvedAgent.source})`;
    const opts: { id: string; name: string }[] = [
      { id: INHERIT, name: inheritLabel },
    ];
    for (const kind of [AgentKind.CLAUDE_CODE, AgentKind.CODEX]) {
      opts.push({ id: String(kind), name: agentLabel(kind) });
    }
    return opts;
  }, [resolvedAgent]);

  // Permission mode options
  const permissionModeOptions = useMemo(() => [
    { id: INHERIT, name: "Inherit" },
    { id: String(PermissionMode.DEFAULT), name: permissionModeLabel(PermissionMode.DEFAULT) },
    { id: String(PermissionMode.AUTO), name: permissionModeLabel(PermissionMode.AUTO) },
  ], []);

  // Model profile options: inherit + available profiles
  const resolvedProfile = resolveModelProfile(selectedProject, selectedBucket);
  const modelProfileOptions = useMemo(() => {
    const profiles = [...state.modelProfiles.values()];
    const inheritDetail = resolvedProfile
      ? `from ${resolvedProfile.source}`
      : "none configured";
    const opts: { id: string; name: string; detail?: string }[] = [
      { id: INHERIT, name: "Inherit", detail: inheritDetail },
    ];
    for (const p of profiles) {
      opts.push({ id: p.id.toString(), name: p.name || `Profile ${p.id}` });
    }
    return opts;
  }, [state.modelProfiles, resolvedProfile]);

  // Spawn validation
  const unavailableReason = form.workerId
    ? workerUnavailableReason(state.workers, BigInt(form.workerId))
    : null;
  const canSpawn = Boolean(form.projectId) && !unavailableReason && !submitting;

  const handleSpawn = async () => {
    if (!canSpawn) return;
    setSubmitting(true);
    setError(null);
    try {
      const agent = form.agentId === INHERIT ? undefined : (Number(form.agentId) as AgentKind);
      const permMode = form.permissionMode === INHERIT
        ? PermissionMode.UNSPECIFIED
        : (Number(form.permissionMode) as PermissionMode);
      const profileId = form.modelProfileId === INHERIT
        ? undefined
        : BigInt(form.modelProfileId);

      await client.spawnSession(
        BigInt(form.projectId),
        agent,
        "", // taskTitle
        form.prompt,
        "", // cwd — inherit
        permMode,
        BigInt(form.workerId),
        true, // itemsApi
        form.role === SessionRole.SUPERVISOR, // supervisorApi
        form.role,
        profileId,
      );
      setSubmitting(false);
      setForm({
        role: SessionRole.WORKER,
        projectId: form.projectId,
        workerId: form.workerId,
        agentId: INHERIT,
        permissionMode: INHERIT,
        modelProfileId: INHERIT,
        prompt: "",
      });
      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setSubmitting(false);
    }
  };

  // Reset form on open
  useEffect(() => {
    if (visible) {
      setError(null);
      setSubmitting(false);
    }
  }, [visible]);

  return (
    <Modal visible={visible} animationType="slide" onRequestClose={onClose}>
      <SafeAreaView style={styles.root}>
      <KeyboardAvoidingRoot>
        <View style={styles.header}>
          <Text style={styles.title}>New session</Text>
          <Pressable onPress={onClose} style={styles.closeTouch} accessibilityLabel="Close" accessibilityRole="button">
            <View style={styles.closeX1} />
            <View style={styles.closeX2} />
          </Pressable>
        </View>
        <KeyboardAwareScrollView
          style={styles.body}
          contentContainerStyle={styles.bodyContent}
        >
          <Text style={fieldStyles.label}>Role</Text>
          <SegmentedControl
            options={[
              { value: String(SessionRole.WORKER), label: "Worker" },
              { value: String(SessionRole.SUPERVISOR), label: "Supervisor" },
            ]}
            selected={String(form.role)}
            onSelect={(v) => setForm((f) => ({ ...f, role: Number(v) as SessionRole }))}
          />

          <PickerField
            label="Project"
            value={form.projectId}
            options={projectOptions}
            onSelect={onProjectChange}
          />

          <PickerField
            label="Worker"
            value={form.workerId}
            options={workerOptions}
            onSelect={(id) => {
              setForm((f) => ({ ...f, workerId: id }));
              setError(null);
            }}
          />

          <PickerField
            label="Agent"
            value={form.agentId}
            options={agentOptions}
            onSelect={(id) => setForm((f) => ({ ...f, agentId: id }))}
          />

          <PickerField
            label="Permission mode"
            value={form.permissionMode}
            options={permissionModeOptions}
            onSelect={(id) => setForm((f) => ({ ...f, permissionMode: id }))}
          />

          <PickerField
            label="Model profile"
            value={form.modelProfileId}
            options={modelProfileOptions}
            onSelect={(id) => setForm((f) => ({ ...f, modelProfileId: id }))}
          />

          <Text style={fieldStyles.label}>Task</Text>
          <TextInput
            style={styles.prompt}
            placeholder="What should this session do?"
            placeholderTextColor={colors.textMuted}
            value={form.prompt}
            onChangeText={(t) => setForm((f) => ({ ...f, prompt: t }))}
            multiline
            textAlignVertical="top"
          />

          {unavailableReason ? (
            <Text style={styles.warning}>{unavailableReason}</Text>
          ) : null}

          {error ? <Text style={styles.error}>{error}</Text> : null}

          <Pressable
            style={[styles.spawnButton, !canSpawn && styles.spawnButtonDisabled]}
            onPress={handleSpawn}
            disabled={!canSpawn}
          >
            <Text style={styles.spawnButtonText}>
              {submitting ? "Spawning\u2026" : "Spawn"}
            </Text>
          </Pressable>
        </KeyboardAwareScrollView>
      </KeyboardAvoidingRoot>
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
    paddingHorizontal: 16,
    paddingVertical: 12,
    borderBottomWidth: StyleSheet.hairlineWidth,
    borderBottomColor: colors.line,
  },
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
  body: { flex: 1 },
  bodyContent: { padding: 16, paddingBottom: 40 },
  prompt: {
    backgroundColor: colors.panelAlt,
    borderRadius: 8,
    color: colors.text,
    fontSize: 15,
    padding: 12,
    minHeight: 100,
  },
  warning: { color: colors.amber, fontSize: 13, marginTop: 12 },
  error: { color: colors.red, fontSize: 13, marginTop: 8 },
  spawnButton: {
    backgroundColor: colors.blue,
    borderRadius: 8,
    paddingVertical: 14,
    alignItems: "center",
    marginTop: 20,
  },
  spawnButtonDisabled: { opacity: 0.4 },
  spawnButtonText: { color: "#fff", fontSize: 16, fontWeight: "600" },
});
