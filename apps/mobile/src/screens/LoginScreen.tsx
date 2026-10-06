import { useState } from "react";
import { Image, Platform, Pressable, StyleSheet, Text, TextInput, View } from "react-native";
import { isPlaintextController, PLAINTEXT_CONTROLLER_WARNING } from "../links";

import type { DeviceAuthSession } from "../auth/session";
import type { ControllerConfig } from "../config";
import { KeyboardAwareScrollView } from "../components/KeyboardAwareScrollables";
import { colors } from "../theme";

// eslint-disable-next-line @typescript-eslint/no-var-requires
const logoSource = require("../../assets/icon.png");

function deviceMeta(): { name: string; platform: string } {
  return {
    name: Platform.OS === "ios" ? "iOS device" : "Android device",
    platform: Platform.OS,
  };
}

export function LoginScreen({
  initial,
  auth,
  notice,
  onEnrolled,
}: {
  initial: ControllerConfig | null;
  auth: DeviceAuthSession;
  notice?: string | null;
  onEnrolled: (config: ControllerConfig) => void;
}) {
  const [baseUrl, setBaseUrl] = useState(initial?.baseUrl ?? "");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const normalizedUrl = baseUrl.trim().replace(/\/+$/, "");
  const canSubmit = Boolean(normalizedUrl && username && password) && !busy;

  const enroll = async () => {
    setBusy(true);
    setError(null);
    try {
      await auth.enroll(normalizedUrl, { username, password }, deviceMeta());
      onEnrolled({ baseUrl: normalizedUrl, sessionToken: null, pushPreviewsEnabled: false });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <KeyboardAwareScrollView
      style={styles.root}
      contentContainerStyle={styles.rootContent}
    >
      <View style={styles.brandRow}>
        <Image source={logoSource} style={styles.logo} />
        <Text style={styles.wordmark}>Puppet Master</Text>
      </View>

      {notice ? <Text style={styles.notice}>{notice}</Text> : null}

      <Text style={styles.label}>Controller</Text>
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

      <Text style={styles.label}>Username</Text>
      <TextInput
        style={styles.input}
        placeholder="username"
        placeholderTextColor={colors.textMuted}
        autoCapitalize="none"
        autoCorrect={false}
        value={username}
        onChangeText={setUsername}
      />

      <Text style={styles.label}>Password</Text>
      <TextInput
        style={styles.input}
        placeholder="password"
        placeholderTextColor={colors.textMuted}
        secureTextEntry
        value={password}
        onChangeText={setPassword}
      />

      {error ? <Text style={styles.error}>{error}</Text> : null}

      <Pressable
        style={[styles.button, !canSubmit && styles.buttonDisabled]}
        disabled={!canSubmit}
        onPress={() => void enroll()}
      >
        <Text style={styles.buttonText}>{busy ? "Enrolling\u2026" : "Sign in"}</Text>
      </Pressable>

    </KeyboardAwareScrollView>
  );
}

const styles = StyleSheet.create({
  root: { flex: 1 },
  rootContent: { flexGrow: 1, justifyContent: "center", padding: 24, gap: 10 },
  brandRow: { alignItems: "center", marginBottom: 16, gap: 8 },
  logo: { width: 56, height: 56, borderRadius: 12 },
  wordmark: { color: colors.textBright, fontSize: 22, fontWeight: "700" },
  notice: {
    color: colors.amber,
    fontSize: 13,
    backgroundColor: colors.panelAlt,
    borderRadius: 8,
    padding: 10,
    marginBottom: 4,
  },
  label: { color: colors.textMuted, fontSize: 13, marginTop: 2 },
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
  error: { color: colors.red },
  button: { backgroundColor: colors.blue, borderRadius: 8, padding: 12, alignItems: "center", marginTop: 4 },
  buttonDisabled: { opacity: 0.4 },
  buttonText: { color: "#fff", fontWeight: "600" },
});
