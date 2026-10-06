import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { ItemSchema } from "../gen/pm/v1/pm_pb";
import { ITEM_SPAWN_BODY_CHARACTERS, itemSpawnPrompt } from "./itemSpawnPrompt";

describe("item spawn prompt", () => {
  it("references the board item and preserves a short description", () => {
    const prompt = itemSpawnPrompt(create(ItemSchema, {
      id: 80n, bucketId: 3n, title: "Board UX fixes", body: "Tighten the workbench.",
      url: "https://example.test/item/80",
    }));

    expect(prompt).toContain("pm:item/3/80 — Board UX fixes");
    expect(prompt).toContain("Use pm:item/3/80 for the full item details");
    expect(prompt).toContain("Item description:\nTighten the workbench.");
    expect(prompt).toContain("Source: https://example.test/item/80");
    expect(prompt).toContain("upsert_items tool (id 80)");
    expect(prompt).not.toContain("[Description truncated");
  });

  it("caps long descriptions by Unicode character without splitting an emoji", () => {
    const body = "a".repeat(ITEM_SPAWN_BODY_CHARACTERS - 1) + "😀" + "tail that stays out";
    const prompt = itemSpawnPrompt(create(ItemSchema, { id: 9n, bucketId: 1n, title: "Long item", body }));

    expect(prompt).toContain(`${"a".repeat(ITEM_SPAWN_BODY_CHARACTERS - 1)}😀`);
    expect(prompt).not.toContain("tail that stays out");
    expect(prompt).toContain("[Description truncated; read the item for the remainder.]");
  });
});
