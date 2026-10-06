import { useCallback, useState } from "react";
import type { LayoutChangeEvent } from "react-native";

/** Minimum spacing between the last scroll content item and the footer. */
const CONTENT_SPACING = 16;

/**
 * Returns the bottom padding that scroll content needs to clear a fixed footer
 * sibling, derived from the footer's measured height rather than a magic number.
 *
 * Usage:
 *   const { footerHeight, onFooterLayout, scrollPaddingBottom } = useFooterClearance();
 *   <ScrollView contentContainerStyle={{ paddingBottom: scrollPaddingBottom }}>
 *   <View onLayout={onFooterLayout}> ... footer ... </View>
 */
export function useFooterClearance(spacing = CONTENT_SPACING) {
  const [footerHeight, setFooterHeight] = useState(0);

  const onFooterLayout = useCallback((event: LayoutChangeEvent) => {
    const h = event.nativeEvent.layout.height;
    setFooterHeight((prev) => (Math.abs(prev - h) > 1 ? h : prev));
  }, []);

  return {
    footerHeight,
    onFooterLayout,
    scrollPaddingBottom: footerHeight + spacing,
  };
}

/**
 * Pure calculation: the bottom padding a scroll container needs so that
 * its last child is fully visible above a fixed footer of the given height.
 */
export function computeScrollClearance(
  footerHeight: number,
  spacing = CONTENT_SPACING,
): number {
  return footerHeight + spacing;
}
