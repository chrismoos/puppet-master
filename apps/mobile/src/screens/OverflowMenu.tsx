import {
  Pressable,
  StyleSheet,
  Text,
  View,
} from "react-native";
import Svg, { Path } from "react-native-svg";

import { colors } from "../theme";

function DotIcon({ color }: { color: string }) {
  return (
    <View style={iconStyles.box}>
      <View style={[iconStyles.circle, { backgroundColor: color }]} />
    </View>
  );
}

function ChevronRightIcon({ color }: { color: string }) {
  return (
    <View style={iconStyles.box}>
      <View style={[iconStyles.chevronRight, { borderColor: color }]} />
    </View>
  );
}

function FilterIcon({ color }: { color: string }) {
  return (
    <View style={iconStyles.box}>
      <View style={[iconStyles.filterTop, { backgroundColor: color }]} />
      <View style={[iconStyles.filterMid, { backgroundColor: color }]} />
      <View style={[iconStyles.filterBot, { backgroundColor: color }]} />
    </View>
  );
}

function SearchIcon({ color }: { color: string }) {
  return (
    <View style={iconStyles.box}>
      <View style={[iconStyles.searchCircle, { borderColor: color }]} />
      <View style={[iconStyles.searchHandle, { backgroundColor: color }]} />
    </View>
  );
}

function EyeIcon({ color }: { color: string }) {
  return (
    <View style={iconStyles.box}>
      <View style={[iconStyles.eyeOuter, { borderColor: color }]} />
      <View style={[iconStyles.eyePupil, { backgroundColor: color }]} />
    </View>
  );
}

function RefreshIcon({ color }: { color: string }) {
  return (
    <View style={iconStyles.box}>
      <View style={[iconStyles.refreshCircle, { borderColor: color }]} />
      <View style={[iconStyles.refreshArrow, { borderTopColor: color, borderRightColor: color }]} />
    </View>
  );
}

function GearIcon({ color }: { color: string }) {
  return (
    <View style={iconStyles.box}>
      <View style={[iconStyles.gearOuter, { borderColor: color }]} />
      <View style={[iconStyles.gearDot, { backgroundColor: color }]} />
    </View>
  );
}

function InfoIcon({ color }: { color: string }) {
  return (
    <View style={iconStyles.box}>
      <View style={[iconStyles.infoCircle, { borderColor: color }]} />
      <View style={[iconStyles.infoDot, { backgroundColor: color }]} />
      <View style={[iconStyles.infoBar, { backgroundColor: color }]} />
    </View>
  );
}

function ListIcon({ color }: { color: string }) {
  return (
    <View style={iconStyles.box}>
      <View style={[iconStyles.listLine, { backgroundColor: color, top: 4 }]} />
      <View style={[iconStyles.listLine, { backgroundColor: color, top: 9 }]} />
      <View style={[iconStyles.listLine, { backgroundColor: color, top: 14 }]} />
    </View>
  );
}

function ClockIcon({ color }: { color: string }) {
  return (
    <View style={iconStyles.box}>
      <View style={[iconStyles.clockFace, { borderColor: color }]} />
      <View style={[iconStyles.clockHand, { backgroundColor: color }]} />
      <View style={[iconStyles.clockMinute, { backgroundColor: color }]} />
    </View>
  );
}

function CopyIcon({ color }: { color: string }) {
  return (
    <View style={iconStyles.box}>
      <View style={[iconStyles.copyBack, { borderColor: color }]} />
      <View style={[iconStyles.copyFront, { borderColor: color, backgroundColor: colors.surface }]} />
    </View>
  );
}

function WarningIcon({ color }: { color: string }) {
  return (
    <View style={iconStyles.box}>
      <View style={[iconStyles.warnTriangle, { borderBottomColor: color }]} />
    </View>
  );
}

function ShieldCheckIcon({ color }: { color: string }) {
  return (
    <Svg width={20} height={20} viewBox="0 0 16 16" fill="none" stroke={color} strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
      <Path d="M8 1.5 2.5 3.5v4c0 3.3 2.3 5.9 5.5 7 3.2-1.1 5.5-3.7 5.5-7v-4z" />
      <Path d="m5.5 8 1.8 1.8L10.8 6.3" />
    </Svg>
  );
}

function XIcon({ color }: { color: string }) {
  return (
    <View style={iconStyles.box}>
      <View style={[iconStyles.xLine1, { backgroundColor: color }]} />
      <View style={[iconStyles.xLine2, { backgroundColor: color }]} />
    </View>
  );
}

function LinkIcon({ color }: { color: string }) {
  return (
    <View style={iconStyles.box}>
      <View style={[iconStyles.linkOval1, { borderColor: color }]} />
      <View style={[iconStyles.linkOval2, { borderColor: color }]} />
    </View>
  );
}

const ICON_MAP: Record<string, (props: { color: string }) => React.JSX.Element> = {
  dot: DotIcon,
  chevronRight: ChevronRightIcon,
  filter: FilterIcon,
  search: SearchIcon,
  eye: EyeIcon,
  refresh: RefreshIcon,
  gear: GearIcon,
  info: InfoIcon,
  list: ListIcon,
  clock: ClockIcon,
  copy: CopyIcon,
  warning: WarningIcon,
  shieldCheck: ShieldCheckIcon,
  x: XIcon,
  link: LinkIcon,
};

const iconStyles = StyleSheet.create({
  box: { width: 20, height: 20, alignItems: "center", justifyContent: "center" },
  // Dot
  circle: { width: 6, height: 6, borderRadius: 3 },
  // Chevron right
  chevronRight: {
    width: 8, height: 8, borderRightWidth: 2, borderBottomWidth: 2,
    transform: [{ rotate: "-45deg" }], marginLeft: -2,
  },
  // Filter (three horizontal bars, narrowing)
  filterTop: { width: 16, height: 2, borderRadius: 1 },
  filterMid: { width: 10, height: 2, borderRadius: 1, marginTop: 2 },
  filterBot: { width: 4, height: 2, borderRadius: 1, marginTop: 2 },
  // Search
  searchCircle: { width: 12, height: 12, borderRadius: 6, borderWidth: 2, marginLeft: -2, marginTop: -2 },
  searchHandle: { width: 6, height: 2, borderRadius: 1, transform: [{ rotate: "45deg" }], marginTop: -2, marginLeft: 4 },
  // Eye
  eyeOuter: {
    width: 16, height: 10, borderRadius: 5, borderWidth: 1.5,
  },
  eyePupil: { width: 4, height: 4, borderRadius: 2, position: "absolute" },
  // Refresh
  refreshCircle: { width: 14, height: 14, borderRadius: 7, borderWidth: 2 },
  refreshArrow: { position: "absolute", top: 1, right: 2, width: 5, height: 5, borderWidth: 2, borderBottomColor: "transparent", borderLeftColor: "transparent" },
  // Gear
  gearOuter: { width: 14, height: 14, borderRadius: 7, borderWidth: 2 },
  gearDot: { width: 4, height: 4, borderRadius: 2, position: "absolute" },
  // Info
  infoCircle: { width: 16, height: 16, borderRadius: 8, borderWidth: 1.5, position: "absolute" },
  infoDot: { width: 2, height: 2, borderRadius: 1, position: "absolute", top: 4 },
  infoBar: { width: 2, height: 6, borderRadius: 1, position: "absolute", top: 8 },
  // List
  listLine: { position: "absolute" as const, left: 2, width: 16, height: 2, borderRadius: 1 },
  // Clock
  clockFace: { width: 14, height: 14, borderRadius: 7, borderWidth: 1.5, position: "absolute" },
  clockHand: { width: 1.5, height: 5, position: "absolute", top: 3, left: 9.25, transformOrigin: "bottom center" },
  clockMinute: { width: 1.5, height: 3.5, position: "absolute", top: 6.5, left: 9.25, transform: [{ rotate: "90deg" }], transformOrigin: "left center" },
  // Copy
  copyBack: { width: 12, height: 12, borderRadius: 2, borderWidth: 1.5, position: "absolute", top: 1, left: 1 },
  copyFront: { width: 12, height: 12, borderRadius: 2, borderWidth: 1.5, position: "absolute", top: 5, left: 5 },
  // Warning
  warnTriangle: {
    width: 0, height: 0,
    borderLeftWidth: 9, borderRightWidth: 9, borderBottomWidth: 16,
    borderLeftColor: "transparent", borderRightColor: "transparent",
  },
  // X
  xLine1: { width: 16, height: 2, borderRadius: 1, transform: [{ rotate: "45deg" }], position: "absolute" },
  xLine2: { width: 16, height: 2, borderRadius: 1, transform: [{ rotate: "-45deg" }], position: "absolute" },
  // Link (two overlapping ovals)
  linkOval1: { width: 10, height: 6, borderRadius: 3, borderWidth: 1.5, position: "absolute", transform: [{ rotate: "45deg" }], left: 1, top: 5 },
  linkOval2: { width: 10, height: 6, borderRadius: 3, borderWidth: 1.5, position: "absolute", transform: [{ rotate: "45deg" }], left: 7, top: 7 },
});

// ---------------------------------------------------------------------------
// Menu item type
// ---------------------------------------------------------------------------

export interface OverflowMenuItem {
  key: string;
  label: string;
  icon: string;
  color?: string;
  badge?: string | number;
  disabled?: boolean;
  onPress: () => void;
}

// ---------------------------------------------------------------------------
// Overflow trigger button (the three-dot glyph)
// ---------------------------------------------------------------------------

export function OverflowButton({ onPress }: { onPress: () => void }) {
  return (
    <Pressable
      onPress={onPress}
      hitSlop={8}
      style={styles.trigger}
      accessibilityRole="button"
      accessibilityLabel="More options"
    >
      <View style={styles.triggerDot} />
      <View style={styles.triggerDot} />
      <View style={styles.triggerDot} />
    </Pressable>
  );
}

// ---------------------------------------------------------------------------
// Overflow menu
// ---------------------------------------------------------------------------

export function OverflowMenu({
  visible,
  items,
  onClose,
}: {
  visible: boolean;
  items: OverflowMenuItem[];
  onClose: () => void;
}) {
  if (!visible) return null;

  return (
    <>
      <Pressable
        style={StyleSheet.absoluteFill}
        onPress={onClose}
        accessibilityRole="none"
      />
      <View style={styles.menuOverlay} accessible={false} accessibilityRole="menu">
        {items.map((item) => {
          const IconComponent = ICON_MAP[item.icon];
          const itemColor = item.color ?? colors.text;
          return (
            <Pressable
              key={item.key}
              testID={`menu-${item.key}`}
              accessible
              accessibilityRole="menuitem"
              accessibilityLabel={item.label}
              accessibilityState={{ disabled: item.disabled }}
              style={({ pressed }) => [
                styles.menuItem,
                pressed && styles.menuItemPressed,
                item.disabled && styles.menuItemDisabled,
              ]}
              onPress={() => {
                if (!item.disabled) {
                  onClose();
                  item.onPress();
                }
              }}
              disabled={item.disabled}
            >
              {IconComponent ? <IconComponent color={item.disabled ? colors.textMuted : itemColor} /> : null}
              <Text
                style={[
                  styles.menuLabel,
                  { color: item.disabled ? colors.textMuted : itemColor },
                ]}
              >
                {item.label}
              </Text>
              {item.badge !== undefined ? (
                <View style={styles.badge}>
                  <Text style={styles.badgeText}>{item.badge}</Text>
                </View>
              ) : null}
            </Pressable>
          );
        })}
      </View>
    </>
  );
}

// ---------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------

const styles = StyleSheet.create({
  trigger: {
    width: 44,
    height: 44,
    alignItems: "center",
    justifyContent: "center",
    gap: 3,
  },
  triggerDot: {
    width: 4,
    height: 4,
    borderRadius: 2,
    backgroundColor: colors.textMuted,
  },
  backdrop: {
    flex: 1,
  },
  safeAnchor: {
    alignItems: "flex-end",
    paddingHorizontal: 16,
    paddingTop: 8,
  },
  menuOverlay: {
    position: "absolute",
    top: 8,
    right: 16,
    minWidth: 220,
    backgroundColor: colors.surface,
    borderRadius: 12,
    paddingVertical: 4,
    zIndex: 100,
    shadowColor: "#000",
    shadowOffset: { width: 0, height: 4 },
    shadowOpacity: 0.5,
    shadowRadius: 12,
    elevation: 8,
  },
  menu: {
    minWidth: 220,
    backgroundColor: colors.surface,
    borderRadius: 12,
    paddingVertical: 4,
    shadowColor: "#000",
    shadowOffset: { width: 0, height: 4 },
    shadowOpacity: 0.5,
    shadowRadius: 12,
    elevation: 8,
  },
  menuItem: {
    flexDirection: "row",
    alignItems: "center",
    gap: 12,
    paddingHorizontal: 16,
    paddingVertical: 12,
    minHeight: 44,
  },
  menuItemPressed: {
    backgroundColor: colors.panelAlt,
  },
  menuItemDisabled: {
    opacity: 0.4,
  },
  menuLabel: {
    fontSize: 15,
    flex: 1,
  },
  badge: {
    backgroundColor: colors.blue,
    borderRadius: 10,
    minWidth: 20,
    paddingHorizontal: 6,
    paddingVertical: 1,
    alignItems: "center",
  },
  badgeText: {
    color: "#fff",
    fontSize: 11,
    fontWeight: "600",
  },
});
