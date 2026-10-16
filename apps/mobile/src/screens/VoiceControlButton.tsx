import { Pressable, StyleSheet, View } from "react-native";
import Svg, { Path } from "react-native-svg";

import { colors } from "../theme";
import { controlActive, controlLabel, type VoiceControl, type VoiceUi } from "./voiceControls";

function Stroke({ children, color }: { children: React.ReactNode; color: string }) {
  return (
    <Svg
      width={22}
      height={22}
      viewBox="0 0 24 24"
      fill="none"
      stroke={color}
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      {children}
    </Svg>
  );
}

function ControlIcon({
  control,
  ui,
  color,
}: {
  control: VoiceControl;
  ui: VoiceUi;
  color: string;
}) {
  if (control === "call") {
    return ui.inCall ? (
      <Stroke color={color}>
        <Path d="M10.7 13.3a16 16 0 0 0 3.4 2.6l1.3-1.3a2 2 0 0 1 2.1-.4c.9.3 1.8.6 2.8.7a2 2 0 0 1 1.7 2v3a2 2 0 0 1-2.2 2 19.8 19.8 0 0 1-8.6-3.1 19.4 19.4 0 0 1-3.3-2.7" />
        <Path d="M8 10a19.8 19.8 0 0 1-3.1-8.6A2 2 0 0 1 6.9 1h3a2 2 0 0 1 2 1.7c.1 1 .4 1.9.7 2.8a2 2 0 0 1-.5 2.1L11 9" />
        <Path d="M22 2 2 22" />
      </Stroke>
    ) : (
      <Stroke color={color}>
        <Path d="M22 16.9v3a2 2 0 0 1-2.2 2 19.8 19.8 0 0 1-8.6-3.1 19.5 19.5 0 0 1-6-6A19.8 19.8 0 0 1 2.1 4.1 2 2 0 0 1 4.1 2h3a2 2 0 0 1 2 1.7c.1 1 .4 1.9.7 2.8a2 2 0 0 1-.5 2.1L8.1 9.9a16 16 0 0 0 6 6l1.3-1.3a2 2 0 0 1 2.1-.4c.9.3 1.8.6 2.8.7a2 2 0 0 1 1.7 2z" />
      </Stroke>
    );
  }
  if (control === "route") {
    return (
      <Stroke color={color}>
        <Path d="M11 5 6 9H2v6h4l5 4z" />
        <Path d="M15.5 8.5a5 5 0 0 1 0 7" />
        {ui.speaker && <Path d="M19 5a10 10 0 0 1 0 14" />}
      </Stroke>
    );
  }
  if (control === "talk") {
    return (
      <Stroke color={color}>
        <Path d="M12 1a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
        <Path d="M19 10v1a7 7 0 0 1-14 0v-1M12 18.5V23M8.5 23h7" />
      </Stroke>
    );
  }
  return (
    <Svg width={22} height={22} viewBox="0 0 24 24" fill={color}>
      <Path d="M7 7h10v10H7z" />
    </Svg>
  );
}

export function VoiceControlButton({
  control,
  ui,
  onPress,
}: {
  control: VoiceControl;
  ui: VoiceUi;
  onPress: () => void;
}) {
  const active = controlActive(control, ui);
  const color = active ? colors.red : colors.text;
  return (
    <Pressable
      style={[styles.button, active && styles.buttonActive]}
      accessibilityRole="button"
      accessibilityLabel={controlLabel(control, ui)}
      accessibilityState={{ busy: control === "talk" && ui.listening }}
      hitSlop={6}
      onPress={onPress}
    >
      <View style={styles.icon}>
        <ControlIcon control={control} ui={ui} color={color} />
      </View>
    </Pressable>
  );
}

const styles = StyleSheet.create({
  button: {
    width: 40,
    height: 40,
    borderRadius: 20,
    alignItems: "center",
    justifyContent: "center",
    backgroundColor: colors.panelAlt,
  },
  buttonActive: { backgroundColor: colors.surface },
  icon: { width: 22, height: 22, alignItems: "center", justifyContent: "center" },
});
