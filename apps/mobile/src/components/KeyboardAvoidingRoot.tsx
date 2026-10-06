import React, { useCallback, useEffect, useRef, useState } from "react";
import {
  Animated,
  Dimensions,
  Keyboard,
  Platform,
  View,
  type KeyboardEvent,
  type LayoutChangeEvent,
  type ViewStyle,
} from "react-native";
import {
  reduceKeyboardInset,
  initialKeyboardInsetState,
  type KeyboardInsetState,
  type KeyboardAction,
} from "./keyboardInset";
export { computeKeyboardOverlap } from "./keyboardOverlap";

export function KeyboardAvoidingRoot({
  children,
  enabled = true,
  style,
}: {
  children: React.ReactNode;
  enabled?: boolean;
  style?: ViewStyle;
}) {
  const viewRef = useRef<View>(null);
  const stateRef = useRef<KeyboardInsetState>(initialKeyboardInsetState());
  const [padding] = useState(() => new Animated.Value(0));

  const applyResult = useCallback(
    (action: KeyboardAction) => {
      const result = reduceKeyboardInset(stateRef.current, action);
      stateRef.current = result.state;
      if (result.hard) {
        padding.stopAnimation();
        padding.setValue(result.toValue);
      } else {
        Animated.timing(padding, {
          toValue: result.toValue,
          duration: result.duration,
          useNativeDriver: false,
        }).start();
      }
    },
    [padding],
  );

  const onLayout = useCallback((_event: LayoutChangeEvent) => {
    viewRef.current?.measureInWindow((_x, y, _width, height) => {
      stateRef.current = { ...stateRef.current, viewBottomY: y + height };
    });
  }, []);

  useEffect(() => {
    if (Platform.OS !== "ios" || !enabled) {
      applyResult({ type: "disable" });
      return;
    }

    const show = (e: KeyboardEvent) =>
      applyResult({
        type: "show",
        keyboardTopY: e.endCoordinates.screenY,
        duration: e.duration,
      });

    const willHide = (e: KeyboardEvent) =>
      applyResult({ type: "willHide", duration: e.duration });

    const changeFrame = (e: KeyboardEvent) =>
      applyResult({
        type: "changeFrame",
        keyboardTopY: e.endCoordinates.screenY,
        screenHeight: Dimensions.get("screen").height,
        duration: e.duration,
      });

    const didHide = () => applyResult({ type: "didHide" });

    const subscriptions = [
      Keyboard.addListener("keyboardWillShow", show),
      Keyboard.addListener("keyboardWillHide", willHide),
      Keyboard.addListener("keyboardWillChangeFrame", changeFrame),
      Keyboard.addListener("keyboardDidHide", didHide),
    ];
    return () => subscriptions.forEach((sub) => sub.remove());
  }, [enabled, padding, applyResult]);

  if (Platform.OS !== "ios" || !enabled) {
    return (
      <View ref={viewRef} style={[{ flex: 1 }, style]} onLayout={onLayout}>
        {children}
      </View>
    );
  }

  return (
    <Animated.View
      ref={viewRef as React.RefObject<View>}
      style={[{ flex: 1, paddingBottom: padding }, style]}
      onLayout={onLayout}
    >
      {children}
    </Animated.View>
  );
}
