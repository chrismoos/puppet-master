import { Pressable, ScrollView, StyleSheet, Text, View } from "react-native";
import { colors } from "../theme";
import type { RailDot, RailTile } from "./railModel";

const DOT_COLOR: Record<Exclude<RailDot, null>, string> = {
  needs: colors.amber,
  working: colors.blue,
  idle: colors.green,
};

const DOT_LABEL: Record<Exclude<RailDot, null>, string> = {
  needs: "needs you",
  working: "working",
  idle: "has idle sessions",
};

/** Bucket tiles across the top of the session list. Tapping the selected
 * bucket again shows every bucket. */
export function BucketRail({
  tiles,
  selectedId,
  onTap,
}: {
  tiles: readonly RailTile[];
  selectedId: string | undefined;
  onTap: (bucketId: string) => void;
}) {
  return (
    <ScrollView
      horizontal
      showsHorizontalScrollIndicator={false}
      style={styles.scroll}
      contentContainerStyle={styles.track}
      accessibilityRole="tablist"
      testID="bucket-rail"
    >
      {tiles.map((tile) => {
        const selected = tile.id === selectedId;
        const label = [tile.name, tile.dot ? DOT_LABEL[tile.dot] : ""].filter(Boolean).join(", ");
        return (
          <Pressable
            key={tile.id}
            onPress={() => onTap(tile.id)}
            style={[styles.tile, selected && styles.tileSelected]}
            accessibilityRole="tab"
            accessibilityState={{ selected }}
            accessibilityLabel={label}
            accessibilityHint={selected ? "Shows every bucket" : undefined}
            testID={`bucket-tile-${tile.id}`}
          >
            <Text style={[styles.letters, selected && styles.lettersSelected]}>{tile.letters}</Text>
            {tile.dot ? <View style={[styles.dot, { backgroundColor: DOT_COLOR[tile.dot] }]} /> : null}
          </Pressable>
        );
      })}
    </ScrollView>
  );
}

const TILE = 42;
const DOT = 11;

const styles = StyleSheet.create({
  scroll: { flexGrow: 0 },
  track: { gap: 8, paddingHorizontal: 16, paddingTop: 3, paddingBottom: 10 },
  tile: {
    width: TILE,
    height: TILE,
    borderRadius: 11,
    backgroundColor: colors.panelAlt,
    alignItems: "center",
    justifyContent: "center",
  },
  tileSelected: { backgroundColor: colors.blue },
  letters: { color: colors.text, fontSize: 16, fontWeight: "700", letterSpacing: 0.3 },
  lettersSelected: { color: "#fff" },
  dot: {
    position: "absolute",
    top: -3,
    right: -3,
    width: DOT,
    height: DOT,
    borderRadius: DOT / 2,
    borderWidth: 2,
    borderColor: colors.bg,
  },
});
