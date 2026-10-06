import type { Item } from "../gen/pm/v1/pm_pb";

export const ITEM_SPAWN_BODY_CHARACTERS = 4_000;

export function itemSpawnPrompt(item: Item): string {
  const bodyCharacters = Array.from(item.body);
  const excerpt = bodyCharacters.slice(0, ITEM_SPAWN_BODY_CHARACTERS).join("");
  const truncated = bodyCharacters.length > ITEM_SPAWN_BODY_CHARACTERS;
  const reference = `pm:item/${item.bucketId.toString()}/${item.id.toString()}`;
  return [
    `You are working on ${reference} — ${item.title}.`,
    `Use ${reference} for the full item details and current board state.`,
    excerpt ? `Item description${truncated ? " excerpt" : ""}:\n${excerpt}${truncated ? "\n\n[Description truncated; read the item for the remainder.]" : ""}` : "",
    item.url ? `Source: ${item.url}` : "",
    `When the work meaningfully advances or completes, update the item over the ` +
      `upsert_items tool (id ${item.id.toString()}) so the board stays current.`,
  ].filter(Boolean).join("\n\n");
}
