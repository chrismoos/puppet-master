import React from "react";
import {
  FlatList,
  Platform,
  ScrollView,
  type FlatListProps,
  type ScrollViewProps,
} from "react-native";
import { keyboardScrollDefaults } from "./keyboardScrollDefaults";

const scrollDefaults = keyboardScrollDefaults(Platform.OS === "ios");

export const KeyboardAwareScrollView = React.forwardRef<ScrollView, ScrollViewProps>(
  (props, ref) => (
    <ScrollView
      {...scrollDefaults}
      {...props}
      ref={ref}
    />
  ),
);
KeyboardAwareScrollView.displayName = "KeyboardAwareScrollView";

export function KeyboardAwareFlatList<T>(
  props: FlatListProps<T> & { listRef?: React.Ref<FlatList<T>> },
) {
  const { listRef, ...rest } = props;
  return (
    <FlatList<T>
      ref={listRef}
      {...scrollDefaults}
      {...rest}
    />
  );
}
