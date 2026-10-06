import { describe, expect, it } from "vitest";
import { channelNote } from "./SettingsPage";

describe("channelNote", () => {
  it("names a channel other than stable", () => {
    expect(channelNote({ channel: "dev" })).toBe(" on the dev channel");
  });

  it("says nothing for stable or an unknown build", () => {
    expect(channelNote({ channel: "stable" })).toBe("");
    expect(channelNote({ channel: "" })).toBe("");
    expect(channelNote(null)).toBe("");
  });
});
