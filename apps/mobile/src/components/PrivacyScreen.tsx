import { useEffect, useState } from "react";
import { AppState, Image, StyleSheet, Text, View } from "react-native";

import { colors } from "../theme";

const logoSource = require("../../assets/icon.png");

/**
 * Covers the app while iOS takes its app-switcher snapshot.
 *
 * iOS screenshots an app as it leaves the foreground and shows that image in
 * the switcher and after a cold resume, so whatever was on screen is retained
 * outside the app and visible to anyone holding the phone. What is on screen
 * here is agent terminals and session detail.
 *
 * "inactive" is the state iOS passes through on the way out, and the snapshot
 * is taken during it, so the cover has to go up then rather than on
 * "background".
 */
export function PrivacyScreen() {
  const [covered, setCovered] = useState(AppState.currentState !== "active");
  useEffect(() => {
    const subscription = AppState.addEventListener("change", (state) => {
      setCovered(state !== "active");
    });
    return () => subscription.remove();
  }, []);
  if (!covered) return null;
  return (
    <View style={styles.cover} accessibilityElementsHidden importantForAccessibility="no-hide-descendants">
      <Image source={logoSource} style={styles.logo} />
      <Text style={styles.label}>Puppet Master</Text>
    </View>
  );
}

const styles = StyleSheet.create({
  cover: {
    ...StyleSheet.absoluteFillObject,
    alignItems: "center",
    backgroundColor: colors.bg,
    justifyContent: "center",
    zIndex: 1000,
  },
  logo: { height: 64, width: 64 },
  label: { color: colors.textMuted, fontSize: 14, marginTop: 12 },
});
