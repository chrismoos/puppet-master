import { Image, Pressable, ScrollView, StyleSheet, Text, View } from "react-native";
import { colors } from "../theme";
import type { RailTile } from "./railModel";

/** Face text this long or longer uses the smaller size, as on the web rail. */
const LONG_FACE_CHARS = 3;
/** How strongly a bucket's own color tints its tile, and its selected tile. */
const COLOR_TINT = 0.28;
const COLOR_TINT_SELECTED = 0.45;

function tileLabel(tile: RailTile): string {
  return [
    tile.name,
    tile.needs > 0 ? `${tile.needs} ${tile.needs === 1 ? "needs" : "need"} input` : "",
    tile.working ? "working" : "",
    tile.finishedUnseen ? "finished" : "",
  ].filter(Boolean).join(", ");
}

function Face({ tile, selected }: { tile: RailTile; selected: boolean }) {
  if (tile.icon?.image) {
    return <Image source={{ uri: tile.icon.image }} style={styles.image} accessibilityIgnoresInvertColors />;
  }
  const text = tile.icon?.text || tile.letters;
  const long = [...text].length >= LONG_FACE_CHARS;
  const plainSelected = selected && !tile.icon?.color;
  return (
    <Text style={[styles.letters, long && styles.lettersLong, plainSelected && styles.lettersSelected, tile.icon?.color ? styles.lettersOnColor : null]}>
      {text}
    </Text>
  );
}

/** Bucket tiles across the top of the session list, showing the same faces
 * and indicators as the web rail. Tapping the selected bucket again shows
 * every bucket. */
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
        const color = tile.icon?.color;
        const image = Boolean(tile.icon?.image);
        return (
          <Pressable
            key={tile.id}
            onPress={() => onTap(tile.id)}
            style={[
              styles.tile,
              selected && !color && !image ? styles.tileSelected : null,
              image ? styles.tileImage : null,
              selected && (color || image) ? styles.tileRing : null,
              tile.needs > 0 && (color || image) && !selected ? styles.tileNeeds : null,
            ]}
            accessibilityRole="tab"
            accessibilityState={{ selected }}
            accessibilityLabel={tileLabel(tile)}
            accessibilityHint={selected ? "Shows every bucket" : undefined}
            testID={`bucket-tile-${tile.id}`}
          >
            {color && !image ? (
              <View style={[styles.tint, { backgroundColor: color, opacity: selected ? COLOR_TINT_SELECTED : COLOR_TINT }]} />
            ) : null}
            <Face tile={tile} selected={selected} />
            {tile.needs > 0 ? (
              <View style={styles.badge}>
                <Text style={styles.badgeText}>{tile.needs}</Text>
              </View>
            ) : null}
            {tile.working ? <View style={[styles.dot, styles.working]} /> : null}
            {tile.finishedUnseen ? <View style={[styles.dot, styles.finished]} /> : null}
          </Pressable>
        );
      })}
    </ScrollView>
  );
}

const TILE = 42;
const DOT = 10;
const BADGE = 16;

const styles = StyleSheet.create({
  scroll: { flexGrow: 0 },
  track: { gap: 10, paddingHorizontal: 16, paddingTop: 6, paddingBottom: 12 },
  tile: {
    width: TILE,
    height: TILE,
    borderRadius: 11,
    backgroundColor: colors.panelAlt,
    alignItems: "center",
    justifyContent: "center",
    borderWidth: 1,
    borderColor: "transparent",
  },
  tileSelected: { backgroundColor: colors.blue },
  tileImage: { backgroundColor: "transparent" },
  tileRing: { borderColor: colors.blue, borderWidth: 2 },
  tileNeeds: { borderColor: colors.amber },
  tint: { position: "absolute", top: 0, right: 0, bottom: 0, left: 0, borderRadius: 10 },
  image: { width: TILE - 2, height: TILE - 2, borderRadius: 10 },
  letters: { color: colors.text, fontSize: 16, fontWeight: "700", letterSpacing: -0.5 },
  lettersLong: { fontSize: 11 },
  lettersSelected: { color: "#fff" },
  lettersOnColor: { color: colors.textBright },
  badge: {
    position: "absolute",
    top: -5,
    right: -4,
    minWidth: BADGE,
    height: BADGE,
    paddingHorizontal: 4,
    borderRadius: BADGE / 2,
    backgroundColor: colors.amber,
    alignItems: "center",
    justifyContent: "center",
  },
  badgeText: { color: colors.bg, fontSize: 10, fontWeight: "700" },
  dot: {
    position: "absolute",
    bottom: -3,
    width: DOT,
    height: DOT,
    borderRadius: DOT / 2,
    borderWidth: 2,
    borderColor: colors.bg,
  },
  working: { left: -3, backgroundColor: colors.blue },
  finished: { right: -3, backgroundColor: colors.green },
});
