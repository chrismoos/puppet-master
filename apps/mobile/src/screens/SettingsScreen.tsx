import { useState } from "react";
import { Platform, Pressable, StyleSheet, Switch, Text, TextInput, View } from "react-native";
import { isPlaintextController, PLAINTEXT_CONTROLLER_WARNING } from "../links";

import type { DeviceAuthSession } from "../auth/session";
import type { ControllerConfig } from "../config";
import type { ConnectionBanner } from "../status";
import { KeyboardAwareScrollView } from "../components/KeyboardAwareScrollables";
import { colors } from "../theme";

function formatDate(unixMs: number): string {
  const d = new Date(unixMs);
  return d.toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
}

export function SettingsScreen({
  config,
  auth,
  banner,
  onSave,
  onLogOut,
  onBack,
}: {
  config: ControllerConfig;
  auth: DeviceAuthSession;
  banner: ConnectionBanner;
  onSave: (config: ControllerConfig) => void;
  onLogOut: () => void;
  onBack: () => void;
}) {
  const [baseUrl, setBaseUrl] = useState(config.baseUrl);
  // deviceId() generates and stores the id on first read, so it is read
  // once here rather than on every render.
  const [deviceId] = useState(() => auth.deviceId());

  const normalizedUrl = baseUrl.trim().replace(/\/+$/, "");
  const urlChanged = normalizedUrl !== config.baseUrl;

  const deviceName = auth.deviceName();
  const enrolledAt = auth.enrolledAt();

  return (
    <View style={styles.root}>
      <View style={styles.header}>
        <Pressable onPress={onBack} hitSlop={8} style={styles.backTouch} accessibilityRole="button" accessibilityLabel="Back">
          <View style={styles.chevron} />
        </Pressable>
        <Text style={styles.title}>Settings</Text>
      </View>

      <KeyboardAwareScrollView style={styles.body}>
        <Text style={styles.sectionLabel}>Controller</Text>
        <TextInput
          style={styles.input}
          placeholder="https://controller-host"
          placeholderTextColor={colors.textMuted}
          autoCapitalize="none"
          autoCorrect={false}
          keyboardType="url"
          value={baseUrl}
          onChangeText={setBaseUrl}
        />
        {isPlaintextController(baseUrl) && (
          <Text style={styles.plaintextWarning}>{PLAINTEXT_CONTROLLER_WARNING}</Text>
        )}
        <Pressable
          style={[styles.saveButton, !urlChanged && styles.saveButtonDisabled]}
          disabled={!urlChanged}
          onPress={() => onSave({ ...config, baseUrl: normalizedUrl })}
        >
          <Text style={styles.saveButtonText}>Save</Text>
        </Pressable>

        <Text style={styles.sectionLabel}>Connection</Text>
        <View style={styles.card}>
          <View style={styles.statusRow}>
            <View style={[styles.statusDot, { backgroundColor: statusColor(banner.label) }]} />
            <Text style={styles.statusText}>{banner.label}</Text>
          </View>
        </View>

        <Text style={styles.sectionLabel}>This device</Text>
        <View style={styles.card}>
          {deviceName ? (
            <View style={styles.infoRow}>
              <Text style={styles.infoLabel}>Name</Text>
              <Text style={styles.infoValue}>{deviceName}</Text>
            </View>
          ) : null}
          {enrolledAt ? (
            <View style={styles.infoRow}>
              <Text style={styles.infoLabel}>Enrolled</Text>
              <Text style={styles.infoValue}>{formatDate(enrolledAt)}</Text>
            </View>
          ) : null}
          <View style={styles.infoStack}>
            <Text style={styles.infoLabel}>Device ID</Text>
            <Text style={styles.deviceId} selectable>
              {deviceId}
            </Text>
          </View>
        </View>

        <Text style={styles.sectionLabel}>Notifications</Text>
        <View style={styles.card}>
          <View style={styles.switchRow}>
            <View style={styles.switchLabel}>
              <Text style={styles.infoValue}>Show previews</Text>
              <Text style={styles.switchHint}>
                Shows the project name and agent headline. Without this,
                notifications say only &quot;New notification&quot;. When
                on, the content is visible to Apple on the direct path.
              </Text>
            </View>
            <Switch
              value={config.pushPreviewsEnabled}
              onValueChange={(v) =>
                onSave({ ...config, baseUrl: config.baseUrl, pushPreviewsEnabled: v })
              }
              trackColor={{ true: colors.blue }}
            />
          </View>
        </View>

        <Pressable style={styles.logOutButton} onPress={onLogOut}>
          <Text style={styles.logOutText}>Log out</Text>
        </Pressable>
      </KeyboardAwareScrollView>
    </View>
  );
}

function statusColor(label: string): string {
  switch (label) {
    case "connected":
      return colors.green;
    case "rejected":
    case "not logged in":
      return colors.red;
    default:
      return colors.textMuted;
  }
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
  title: { color: colors.textBright, fontSize: 20, fontWeight: "600" },
  body: { paddingHorizontal: 16, gap: 8 },
  sectionLabel: { color: colors.textMuted, fontSize: 12, marginTop: 12 },
  plaintextWarning: {
    color: colors.amber,
    fontSize: 12,
    lineHeight: 17,
    marginBottom: 12,
  },
  input: {
    backgroundColor: colors.surface,
    color: colors.text,
    borderRadius: 8,
    paddingHorizontal: 12,
    paddingVertical: 10,
    fontSize: 16,
  },
  saveButton: {
    alignSelf: "flex-start",
    backgroundColor: colors.blue,
    borderRadius: 6,
    paddingHorizontal: 16,
    paddingVertical: 8,
  },
  saveButtonDisabled: { opacity: 0.4 },
  saveButtonText: { color: "#fff", fontWeight: "600", fontSize: 14 },
  card: {
    backgroundColor: colors.panelAlt,
    borderRadius: 8,
    padding: 12,
    gap: 8,
  },
  statusRow: { flexDirection: "row", alignItems: "center", gap: 8 },
  statusDot: { width: 8, height: 8, borderRadius: 4 },
  statusText: { color: colors.text, fontSize: 14 },
  infoRow: { flexDirection: "row", justifyContent: "space-between" },
  infoStack: { gap: 2 },
  infoLabel: { color: colors.textMuted, fontSize: 14 },
  infoValue: { color: colors.text, fontSize: 14 },
  // The id is too long to sit opposite its label, so it gets its own line.
  deviceId: {
    color: colors.text,
    fontSize: 13,
    fontFamily: Platform.OS === "ios" ? "Menlo" : "monospace",
  },
  switchRow: { flexDirection: "row", alignItems: "center", gap: 12 },
  switchLabel: { flex: 1 },
  switchHint: { color: colors.textMuted, fontSize: 12, marginTop: 2 },
  logOutButton: {
    marginTop: 20,
    backgroundColor: "rgba(255, 93, 93, 0.12)",
    borderRadius: 8,
    padding: 12,
    alignItems: "center",
  },
  logOutText: { color: colors.red, fontWeight: "600" },
});
